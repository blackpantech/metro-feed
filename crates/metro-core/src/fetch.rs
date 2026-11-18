//! HTTP feed fetching with conditional GET for Metro.
//!
//! This module implements the `feed-fetching` capability of the M1 milestone:
//! it downloads a feed over HTTP with [`ureq`], honours cached validators
//! (`ETag` / `Last-Modified`) through conditional GET, and parses the body with
//! [`feed_rs`] into format-independent [`FetchedEntry`] values.
//!
//! # Conditional GET
//!
//! When a cached `ETag` is supplied it is sent as `If-None-Match`; when a
//! cached `Last-Modified` value is supplied it is sent as `If-Modified-Since`.
//! A `304 Not Modified` answer yields a [`FetchResult`] with
//! [`FetchStatus::NotModified`], no feed metadata, an empty entry list, and the
//! cached validators preserved (a validator repeated by the server on the 304
//! takes precedence, as allowed by RFC 9110). A `2xx` answer is parsed and the
//! validators are replaced by whatever the response carries, which may be
//! `None` when the server stopped sending them.
//!
//! # Limits
//!
//! The defaults in [`FetchConfig::default`] follow the feed-fetching spec:
//!
//! - 30-second connect timeout ([`CONNECT_TIMEOUT`]).
//! - 60-second read timeout ([`READ_TIMEOUT`]), applied both to waiting for
//!   the response headers and to downloading the body.
//! - Response bodies capped at [`MAX_BODY_BYTES`].
//! - At most [`MAX_REDIRECTS`] redirects; exceeding it is an error.
//! - A polite [`USER_AGENT`] on every request.
//!
//! # Parsed fields
//!
//! [`feed_rs`] handles RSS 0.9x/1.0/2.0, Atom, and JSON Feed. Each entry is
//! normalized as follows:
//!
//! - `guid`: the entry id from the feed. `feed-rs` synthesizes a stable id
//!   when the feed omits one, so this is never empty in practice.
//! - `url`: the `alternate` link (or the link without a `rel`), falling back
//!   to the first link. Relative links are resolved against the final URL.
//! - `title`, `author`, `summary`, `content`: trimmed text, with blank values
//!   mapped to `None`. The author is the first named author; the RSS
//!   `email (Name)` convention yields `Name` without the parentheses.
//! - `published`: Unix seconds of the published date, falling back to the
//!   updated date (Atom feeds often only carry `<updated>`). Dates before the
//!   Unix epoch are dropped.
//!
//! The feed kind is inferred from the parsed format (design decision D5), not
//! from the URL or the response `Content-Type`.
//!
//! # Errors
//!
//! - [`MetroError::Usage`] for an empty, malformed, or non-`http(s)` URL.
//! - [`MetroError::Network`] for transport failures (DNS, TLS, connection,
//!   timeouts), too many redirects, oversized bodies, and non-success HTTP
//!   statuses other than `304`.
//! - [`MetroError::Generic`] when the body is not a parsable feed.
//!
//! Recording the failure on the feed row (`error_state`) is the caller's job
//! (the refresh pipeline), which keeps this module free of database access.

use std::time::Duration;

use feed_rs::model::{Entry as ParsedEntry, Feed as ParsedFeed, FeedType, Link, Text};
use feed_rs::parser::Builder as ParserBuilder;
use ureq::ResponseExt;
use url::Url;

use crate::MetroError;
use crate::models::FeedKind;

/// Default connect timeout for feed requests.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default read timeout, applied to both the response headers and the body.
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Default cap on the size of a feed response body (10 MiB).
pub const MAX_BODY_BYTES: u64 = 10 * 1024 * 1024;

/// Default maximum number of redirects followed before failing.
pub const MAX_REDIRECTS: u32 = 5;

/// Polite User-Agent sent with every feed request.
pub const USER_AGENT: &str = "metro-feed/0.1 (terminal; local-first)";

/// HTTP status code for `304 Not Modified`.
const STATUS_NOT_MODIFIED: u16 = 304;

/// Tunable limits for a [`Fetcher`].
///
/// [`FetchConfig::default`] returns the values required by the feed-fetching
/// spec; other values exist mainly so tests can use short timeouts and small
/// caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchConfig {
    /// Maximum time to establish a connection (including TLS).
    pub connect_timeout: Duration,
    /// Maximum time to receive the response headers, and separately the body.
    pub read_timeout: Duration,
    /// Maximum accepted response body size in bytes.
    pub max_body_bytes: u64,
    /// Maximum number of redirects followed; more is an error.
    pub max_redirects: u32,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: READ_TIMEOUT,
            max_body_bytes: MAX_BODY_BYTES,
            max_redirects: MAX_REDIRECTS,
        }
    }
}

/// Whether the server returned new content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchStatus {
    /// `2xx`: the body was downloaded and parsed.
    Modified,
    /// `304 Not Modified`: the cached copy is still current.
    NotModified,
}

/// Feed-level metadata extracted from a parsed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedMetadata {
    /// Format of the feed, inferred from the parsed document.
    pub kind: FeedKind,
    /// Feed title, if the feed declares one.
    pub title: Option<String>,
    /// Website the feed belongs to (its `alternate` link), if declared.
    pub site_url: Option<String>,
}

/// A feed entry normalized from RSS, Atom, or JSON Feed.
///
/// Unlike [`crate::models::Entry`], this carries no database identifiers: the
/// refresh pipeline assigns the feed and entry IDs when it stores the entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedEntry {
    /// Entry identifier from the feed (synthesized by `feed-rs` if absent).
    pub guid: String,
    /// Link to the entry, resolved to an absolute URL when possible.
    pub url: Option<String>,
    /// Entry title.
    pub title: Option<String>,
    /// Name of the first named author.
    pub author: Option<String>,
    /// Unix timestamp of publication (or last update as a fallback).
    pub published: Option<u64>,
    /// Summary or excerpt.
    pub summary: Option<String>,
    /// Full content body.
    pub content: Option<String>,
}

/// Outcome of a successful fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResult {
    /// Whether new content was received.
    pub status: FetchStatus,
    /// Feed metadata; `None` when [`FetchStatus::NotModified`].
    pub feed: Option<FeedMetadata>,
    /// Parsed entries; empty when [`FetchStatus::NotModified`].
    pub entries: Vec<FetchedEntry>,
    /// `ETag` to store for the next conditional request.
    pub etag: Option<String>,
    /// `Last-Modified` to store for the next conditional request.
    pub last_modified: Option<String>,
}

impl FetchResult {
    /// Return `true` when the server answered `304 Not Modified`.
    #[must_use]
    pub fn is_not_modified(&self) -> bool {
        self.status == FetchStatus::NotModified
    }
}

/// Reusable feed fetcher.
///
/// A `Fetcher` owns a configured [`ureq::Agent`], so reusing one across many
/// fetches shares its connection pool. It is `Send + Sync` and can be shared
/// by reference between worker threads.
#[derive(Debug)]
pub struct Fetcher {
    /// HTTP agent configured from a [`FetchConfig`].
    agent: ureq::Agent,
    /// Maximum accepted body size, applied when reading each response.
    max_body_bytes: u64,
}

impl Default for Fetcher {
    fn default() -> Self {
        Self::new(FetchConfig::default())
    }
}

impl Fetcher {
    /// Build a fetcher with the given limits.
    #[must_use]
    pub fn new(config: FetchConfig) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(config.connect_timeout))
            .timeout_recv_response(Some(config.read_timeout))
            .timeout_recv_body(Some(config.read_timeout))
            .max_redirects(config.max_redirects)
            .max_redirects_will_error(true)
            // Statuses are inspected explicitly so 304 and error codes can be
            // mapped precisely instead of surfacing as opaque errors.
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .build()
            .into();

        Self {
            agent,
            max_body_bytes: config.max_body_bytes,
        }
    }

    /// Fetch and parse a feed, using cached validators for conditional GET.
    ///
    /// See the [module documentation](self) for header, status, and parsing
    /// semantics.
    ///
    /// # Errors
    ///
    /// - [`MetroError::Usage`] if `url` is not an absolute `http(s)` URL.
    /// - [`MetroError::Network`] on transport failures, timeouts, too many
    ///   redirects, oversized bodies, or non-success statuses other than 304.
    /// - [`MetroError::Generic`] if the body cannot be parsed as a feed.
    pub fn fetch(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<FetchResult, MetroError> {
        let target = parse_feed_url(url)?;

        let mut request = self.agent.get(target.as_str());
        if let Some(value) = etag {
            request = request.header("If-None-Match", value);
        }
        if let Some(value) = last_modified {
            request = request.header("If-Modified-Since", value);
        }

        let mut response = request
            .call()
            .map_err(|error| network_error(target.as_str(), &error))?;

        let status = response.status().as_u16();
        let response_etag = header_value(&response, "etag");
        let response_last_modified = header_value(&response, "last-modified");

        if status == STATUS_NOT_MODIFIED {
            return Ok(FetchResult {
                status: FetchStatus::NotModified,
                feed: None,
                entries: Vec::new(),
                etag: response_etag.or_else(|| etag.map(str::to_owned)),
                last_modified: response_last_modified.or_else(|| last_modified.map(str::to_owned)),
            });
        }

        if !response.status().is_success() {
            return Err(MetroError::Network(format!(
                "failed to fetch {target}: server returned HTTP {status}"
            )));
        }

        // Relative links in the feed resolve against the final URL, which
        // differs from the requested one after a redirect.
        let effective_url = response.get_uri().to_string();
        let body = response
            .body_mut()
            .with_config()
            .limit(self.max_body_bytes)
            .read_to_vec()
            .map_err(|error| network_error(target.as_str(), &error))?;

        let (feed, entries) = parse_feed(&body, Some(&effective_url))?;

        Ok(FetchResult {
            status: FetchStatus::Modified,
            feed: Some(feed),
            entries,
            etag: response_etag,
            last_modified: response_last_modified,
        })
    }
}

/// Fetch and parse a feed with the default limits.
///
/// Convenience wrapper around [`Fetcher::fetch`] on a default [`Fetcher`].
/// Callers fetching many feeds should build one [`Fetcher`] and reuse it.
///
/// # Errors
///
/// See [`Fetcher::fetch`].
pub fn fetch_feed(
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<FetchResult, MetroError> {
    Fetcher::default().fetch(url, etag, last_modified)
}

/// Parse a feed document into metadata and normalized entries.
///
/// `base_uri` resolves relative links; pass the URL the document came from.
///
/// # Errors
///
/// Returns [`MetroError::Generic`] if the bytes are not a supported feed.
pub fn parse_feed(
    body: &[u8],
    base_uri: Option<&str>,
) -> Result<(FeedMetadata, Vec<FetchedEntry>), MetroError> {
    let parsed = ParserBuilder::new()
        .base_uri(base_uri)
        .build()
        .parse(body)
        .map_err(|error| MetroError::Generic(format!("failed to parse feed: {error}")))?;

    let metadata = feed_metadata(&parsed);
    let entries = parsed.entries.into_iter().map(fetched_entry).collect();
    Ok((metadata, entries))
}

/// Parse and validate an absolute `http(s)` feed URL.
///
/// # Errors
///
/// Returns [`MetroError::Usage`] for empty input, unparsable URLs, non-`http`
/// schemes, and URLs without a host.
fn parse_feed_url(raw: &str) -> Result<Url, MetroError> {
    let input = raw.trim();
    if input.is_empty() {
        return Err(MetroError::Usage("feed URL must not be empty".to_string()));
    }

    let url = Url::parse(input)
        .map_err(|error| MetroError::Usage(format!("invalid feed URL {input:?}: {error}")))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(MetroError::Usage(format!(
            "unsupported feed URL scheme {:?}: only http and https are supported",
            url.scheme()
        )));
    }

    if url.host_str().is_none() {
        return Err(MetroError::Usage(format!(
            "invalid feed URL {input:?}: the URL has no host"
        )));
    }

    Ok(url)
}

/// Map a `ureq` failure to a network error with request context.
fn network_error(url: &str, error: &ureq::Error) -> MetroError {
    let detail = match error {
        ureq::Error::Timeout(_) => format!("request timed out ({error})"),
        ureq::Error::TooManyRedirects => "too many redirects".to_string(),
        ureq::Error::BodyExceedsLimit(limit) => {
            format!("response body exceeds the {limit}-byte limit")
        }
        other => other.to_string(),
    };
    MetroError::Network(format!("failed to fetch {url}: {detail}"))
}

/// Read a response header as an owned, non-empty string.
fn header_value<B>(response: &ureq::http::Response<B>, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Extract feed-level metadata from a parsed feed.
fn feed_metadata(feed: &ParsedFeed) -> FeedMetadata {
    FeedMetadata {
        kind: feed_kind(&feed.feed_type),
        title: text_content(feed.title.as_ref()),
        site_url: alternate_link(&feed.links).map(|link| link.href.trim().to_owned()),
    }
}

/// Map a `feed-rs` format to Metro's [`FeedKind`].
fn feed_kind(feed_type: &FeedType) -> FeedKind {
    match feed_type {
        FeedType::Atom => FeedKind::Atom,
        FeedType::JSON => FeedKind::Json,
        FeedType::RSS0 | FeedType::RSS1 | FeedType::RSS2 => FeedKind::Rss,
    }
}

/// Normalize one parsed entry, consuming it to avoid copying its strings.
fn fetched_entry(entry: ParsedEntry) -> FetchedEntry {
    let url = alternate_link(&entry.links)
        .or_else(|| entry.links.first())
        .map(|link| link.href.trim().to_owned())
        .filter(|href| !href.is_empty());

    let author = entry
        .authors
        .into_iter()
        .filter_map(|person| person.name)
        .map(|name| author_name(&name).to_owned())
        .find(|name| !name.is_empty());

    let published = entry
        .published
        .or(entry.updated)
        .and_then(|date| u64::try_from(date.timestamp()).ok());

    FetchedEntry {
        guid: entry.id,
        url,
        title: text_content(entry.title.as_ref()),
        author,
        published,
        summary: text_content(entry.summary.as_ref()),
        content: entry.content.and_then(|content| non_blank(content.body)),
    }
}

/// Pick the link pointing at the human-readable page.
///
/// Prefers `rel="alternate"`, then a link without `rel` (RSS `<link>` and JSON
/// Feed URLs carry none). `self`, `hub`, and other relations are skipped.
fn alternate_link(links: &[Link]) -> Option<&Link> {
    links
        .iter()
        .find(|link| {
            link.rel
                .as_deref()
                .is_some_and(|rel| rel.eq_ignore_ascii_case("alternate"))
        })
        .or_else(|| links.iter().find(|link| link.rel.is_none()))
}

/// Return trimmed text content, or `None` when absent or blank.
fn text_content(text: Option<&Text>) -> Option<String> {
    text.map(|text| text.content.trim())
        .filter(|content| !content.is_empty())
        .map(str::to_owned)
}

/// Clean an author name.
///
/// RSS `<author>` holds `email (Name)`; `feed-rs` splits off the email but
/// keeps the parentheses around the name, so they are removed here.
fn author_name(raw: &str) -> &str {
    let name = raw.trim();
    name.strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .map_or(name, str::trim)
}

/// Trim an owned optional string, mapping blank values to `None`.
fn non_blank(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;

    const RSS_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Example Blog</title>
    <link>https://blog.example.com/</link>
    <description>Posts</description>
    <item>
      <title>  First Post  </title>
      <link>https://blog.example.com/posts/1</link>
      <guid>post-1</guid>
      <author>jane@example.com (Jane Doe)</author>
      <pubDate>Thu, 01 Jan 2026 00:00:00 GMT</pubDate>
      <description>Summary of the first post</description>
    </item>
    <item>
      <title>Second Post</title>
      <link>/posts/2</link>
      <guid>post-2</guid>
      <description>   </description>
    </item>
  </channel>
</rss>"#;

    const ATOM_FEED: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Atom Blog</title>
  <link rel="self" href="https://atom.example.com/atom.xml"/>
  <link rel="alternate" href="https://atom.example.com/"/>
  <id>urn:uuid:feed</id>
  <updated>2026-01-02T00:00:00Z</updated>
  <entry>
    <title>Atom Entry</title>
    <link rel="alternate" href="https://atom.example.com/entry"/>
    <id>urn:uuid:entry-1</id>
    <updated>2026-01-02T00:00:00Z</updated>
    <author><name>John Roe</name></author>
    <summary>Atom summary</summary>
    <content type="html">&lt;p&gt;Atom content&lt;/p&gt;</content>
  </entry>
</feed>"#;

    const JSON_FEED: &str = r#"{
  "version": "https://jsonfeed.org/version/1.1",
  "title": "JSON Blog",
  "home_page_url": "https://json.example.com/",
  "feed_url": "https://json.example.com/feed.json",
  "items": [
    {
      "id": "json-1",
      "url": "https://json.example.com/posts/1",
      "title": "JSON Item",
      "summary": "JSON summary",
      "content_text": "JSON content",
      "date_published": "2026-01-01T00:00:00Z",
      "authors": [{ "name": "Ada" }]
    }
  ]
}"#;

    /// 2026-01-01T00:00:00Z as Unix seconds.
    const JAN_1_2026: u64 = 1_767_225_600;
    /// 2026-01-02T00:00:00Z as Unix seconds.
    const JAN_2_2026: u64 = JAN_1_2026 + 86_400;

    /// A canned HTTP response written by the test server.
    struct Reply {
        status: &'static str,
        headers: Vec<(&'static str, String)>,
        body: String,
        delay: Option<Duration>,
    }

    impl Reply {
        fn new(status: &'static str, body: &str) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: body.to_owned(),
                delay: None,
            }
        }

        fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
            self.headers.push((name, value.into()));
            self
        }
    }

    /// Spawn a local HTTP server answering `connections` requests.
    ///
    /// Each request's lowercase header block is sent on the returned channel
    /// before `respond` builds the reply, so tests can assert on what the
    /// fetcher sent. Returns the server base URL (`http://127.0.0.1:<port>`).
    fn serve<F>(connections: usize, respond: F) -> (String, mpsc::Receiver<String>)
    where
        F: Fn(&str) -> Reply + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            for stream in listener.incoming().take(connections) {
                let Ok(stream) = stream else { return };
                let request = read_request_head(&stream);
                let reply = respond(&request);
                let _ = sender.send(request);
                write_reply(stream, &reply);
            }
        });

        (base, receiver)
    }

    /// Read the request line and headers (lowercased) from a connection.
    fn read_request_head(stream: &TcpStream) -> String {
        let mut reader = BufReader::new(stream);
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            head.push_str(&line.to_ascii_lowercase());
        }
        head
    }

    /// Write a reply and close the connection.
    fn write_reply(mut stream: TcpStream, reply: &Reply) {
        if let Some(delay) = reply.delay {
            thread::sleep(delay);
        }
        let mut out = format!(
            "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            reply.status,
            reply.body.len()
        );
        for (name, value) in &reply.headers {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str("\r\n");
        out.push_str(&reply.body);
        let _ = stream.write_all(out.as_bytes());
    }

    /// Fetcher with short timeouts so failing tests finish quickly.
    fn test_fetcher() -> Fetcher {
        Fetcher::new(FetchConfig {
            connect_timeout: Duration::from_secs(2),
            read_timeout: Duration::from_secs(2),
            ..FetchConfig::default()
        })
    }

    #[test]
    fn test_default_config_matches_spec() {
        let config = FetchConfig::default();
        assert_eq!(config.connect_timeout, Duration::from_secs(30));
        assert_eq!(config.read_timeout, Duration::from_secs(60));
        assert_eq!(config.max_redirects, 5);
        assert_eq!(config.max_body_bytes, 10 * 1024 * 1024);
        assert_eq!(USER_AGENT, "metro-feed/0.1 (terminal; local-first)");
    }

    #[test]
    fn test_full_fetch_without_cached_values() {
        let (base, requests) = serve(1, |_| {
            Reply::new("200 OK", RSS_FEED)
                .header("Content-Type", "application/rss+xml")
                .header("ETag", "\"v1\"")
                .header("Last-Modified", "Thu, 01 Jan 2026 00:00:00 GMT")
        });

        let result = test_fetcher()
            .fetch(&format!("{base}/feed.xml"), None, None)
            .unwrap();

        let request = requests.recv().unwrap();
        assert!(request.starts_with("get /feed.xml http/1.1"));
        assert!(request.contains("user-agent: metro-feed/0.1 (terminal; local-first)"));
        assert!(!request.contains("if-none-match"));
        assert!(!request.contains("if-modified-since"));

        assert_eq!(result.status, FetchStatus::Modified);
        assert!(!result.is_not_modified());
        assert_eq!(result.etag.as_deref(), Some("\"v1\""));
        assert_eq!(
            result.last_modified.as_deref(),
            Some("Thu, 01 Jan 2026 00:00:00 GMT")
        );
        let feed = result.feed.unwrap();
        assert_eq!(feed.kind, FeedKind::Rss);
        assert_eq!(feed.title.as_deref(), Some("Example Blog"));
        assert_eq!(result.entries.len(), 2);
        // Relative entry links resolve against the URL that was fetched.
        assert_eq!(
            result.entries[1].url.as_deref(),
            Some(format!("{base}/posts/2").as_str())
        );
    }

    #[test]
    fn test_not_modified_preserves_cached_validators() {
        let (base, requests) = serve(1, |_| Reply::new("304 Not Modified", ""));

        let result = test_fetcher()
            .fetch(
                &format!("{base}/feed.xml"),
                Some("\"v1\""),
                Some("Thu, 01 Jan 2026 00:00:00 GMT"),
            )
            .unwrap();

        let request = requests.recv().unwrap();
        assert!(request.contains("if-none-match: \"v1\""));
        assert!(request.contains("if-modified-since: thu, 01 jan 2026 00:00:00 gmt"));

        assert_eq!(result.status, FetchStatus::NotModified);
        assert!(result.is_not_modified());
        assert!(result.feed.is_none());
        assert!(result.entries.is_empty());
        assert_eq!(result.etag.as_deref(), Some("\"v1\""));
        assert_eq!(
            result.last_modified.as_deref(),
            Some("Thu, 01 Jan 2026 00:00:00 GMT")
        );
    }

    #[test]
    fn test_not_modified_prefers_validators_repeated_by_server() {
        let (base, _requests) = serve(1, |_| {
            Reply::new("304 Not Modified", "").header("ETag", "\"v1b\"")
        });

        let result = test_fetcher()
            .fetch(&format!("{base}/feed.xml"), Some("\"v1\""), None)
            .unwrap();

        assert!(result.is_not_modified());
        assert_eq!(result.etag.as_deref(), Some("\"v1b\""));
        assert_eq!(result.last_modified, None);
    }

    #[test]
    fn test_new_content_updates_validators() {
        let (base, _requests) = serve(1, |_| {
            Reply::new("200 OK", ATOM_FEED)
                .header("ETag", "\"v2\"")
                .header("Last-Modified", "Fri, 02 Jan 2026 00:00:00 GMT")
        });

        let result = test_fetcher()
            .fetch(
                &format!("{base}/atom.xml"),
                Some("\"v1\""),
                Some("Thu, 01 Jan 2026 00:00:00 GMT"),
            )
            .unwrap();

        assert_eq!(result.status, FetchStatus::Modified);
        assert_eq!(result.etag.as_deref(), Some("\"v2\""));
        assert_eq!(
            result.last_modified.as_deref(),
            Some("Fri, 02 Jan 2026 00:00:00 GMT")
        );
        assert_eq!(result.entries.len(), 1);
    }

    #[test]
    fn test_new_content_without_validators_clears_them() {
        let (base, _requests) = serve(1, |_| Reply::new("200 OK", RSS_FEED));

        let result = test_fetcher()
            .fetch(&format!("{base}/feed.xml"), Some("\"v1\""), Some("old"))
            .unwrap();

        assert_eq!(result.etag, None);
        assert_eq!(result.last_modified, None);
    }

    #[test]
    fn test_http_error_status_is_network_error() {
        let (base, _requests) = serve(1, |_| Reply::new("500 Internal Server Error", "boom"));

        let error = test_fetcher()
            .fetch(&format!("{base}/feed.xml"), None, None)
            .unwrap_err();

        assert!(matches!(&error, MetroError::Network(message) if message.contains("HTTP 500")));
        assert_eq!(error.exit_code(), 4);
    }

    #[test]
    fn test_invalid_body_is_parse_error() {
        let (base, _requests) = serve(1, |_| Reply::new("200 OK", "this is not a feed"));

        let error = test_fetcher()
            .fetch(&format!("{base}/feed.xml"), None, None)
            .unwrap_err();

        assert!(matches!(&error, MetroError::Generic(message) if message.contains("parse")));
    }

    #[test]
    fn test_read_timeout_is_network_error() {
        let (base, _requests) = serve(1, |_| Reply {
            delay: Some(Duration::from_millis(1500)),
            ..Reply::new("200 OK", RSS_FEED)
        });
        let fetcher = Fetcher::new(FetchConfig {
            read_timeout: Duration::from_millis(200),
            ..FetchConfig::default()
        });

        let error = fetcher
            .fetch(&format!("{base}/feed.xml"), None, None)
            .unwrap_err();

        assert!(matches!(&error, MetroError::Network(message) if message.contains("timed out")));
    }

    #[test]
    fn test_redirects_are_followed_within_limit() {
        let (base, _requests) = serve(2, |request| {
            if request.starts_with("get /old") {
                Reply::new("301 Moved Permanently", "").header("Location", "/new")
            } else {
                Reply::new("200 OK", RSS_FEED)
            }
        });

        let result = test_fetcher()
            .fetch(&format!("{base}/old"), None, None)
            .unwrap();

        assert_eq!(result.entries.len(), 2);
        // Relative links resolve against the post-redirect URL.
        assert_eq!(
            result.entries[1].url.as_deref(),
            Some(format!("{base}/posts/2").as_str())
        );
    }

    #[test]
    fn test_redirect_limit_is_enforced() {
        // Six hops: /r0 -> /r1 -> ... -> /r6, one more than the limit of 5.
        let (base, _requests) = serve(MAX_REDIRECTS as usize + 1, |request| {
            let hop = request
                .strip_prefix("get /r")
                .and_then(|rest| rest.split(' ').next())
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(0);
            Reply::new("302 Found", "").header("Location", format!("/r{}", hop + 1))
        });

        let error = test_fetcher()
            .fetch(&format!("{base}/r0"), None, None)
            .unwrap_err();

        assert!(
            matches!(&error, MetroError::Network(message) if message.contains("too many redirects"))
        );
    }

    #[test]
    fn test_body_size_cap_is_enforced() {
        let (base, _requests) = serve(1, |_| Reply::new("200 OK", RSS_FEED));
        let fetcher = Fetcher::new(FetchConfig {
            max_body_bytes: 64,
            ..FetchConfig::default()
        });

        let error = fetcher
            .fetch(&format!("{base}/feed.xml"), None, None)
            .unwrap_err();

        assert!(matches!(&error, MetroError::Network(message) if message.contains("limit")));
    }

    #[test]
    fn test_fetch_rejects_invalid_urls_without_network() {
        for url in ["", "   ", "example.com/feed.xml", "ftp://example.com/feed"] {
            assert!(
                matches!(fetch_feed(url, None, None), Err(MetroError::Usage(_))),
                "expected usage error for {url:?}"
            );
        }
    }

    #[test]
    fn test_parse_rss_feed() {
        let (feed, entries) = parse_feed(RSS_FEED.as_bytes(), None).unwrap();

        assert_eq!(feed.kind, FeedKind::Rss);
        assert_eq!(feed.title.as_deref(), Some("Example Blog"));
        assert_eq!(feed.site_url.as_deref(), Some("https://blog.example.com/"));

        let first = &entries[0];
        assert_eq!(first.guid, "post-1");
        assert_eq!(
            first.url.as_deref(),
            Some("https://blog.example.com/posts/1")
        );
        assert_eq!(first.title.as_deref(), Some("First Post"));
        assert_eq!(first.author.as_deref(), Some("Jane Doe"));
        assert_eq!(first.published, Some(JAN_1_2026));
        assert_eq!(first.summary.as_deref(), Some("Summary of the first post"));

        // Blank descriptions are normalized away.
        assert_eq!(entries[1].summary, None);
        assert_eq!(entries[1].published, None);
    }

    #[test]
    fn test_parse_atom_feed() {
        let (feed, entries) = parse_feed(ATOM_FEED.as_bytes(), None).unwrap();

        assert_eq!(feed.kind, FeedKind::Atom);
        assert_eq!(feed.title.as_deref(), Some("Atom Blog"));
        // The `self` link is skipped in favour of `alternate`.
        assert_eq!(feed.site_url.as_deref(), Some("https://atom.example.com/"));

        let entry = &entries[0];
        assert_eq!(entry.guid, "urn:uuid:entry-1");
        assert_eq!(entry.url.as_deref(), Some("https://atom.example.com/entry"));
        assert_eq!(entry.title.as_deref(), Some("Atom Entry"));
        assert_eq!(entry.author.as_deref(), Some("John Roe"));
        // No <published>: the updated date is used instead.
        assert_eq!(entry.published, Some(JAN_2_2026));
        assert_eq!(entry.summary.as_deref(), Some("Atom summary"));
        assert!(
            entry
                .content
                .as_deref()
                .is_some_and(|c| c.contains("Atom content"))
        );
    }

    #[test]
    fn test_parse_json_feed() {
        let (feed, entries) = parse_feed(JSON_FEED.as_bytes(), None).unwrap();

        assert_eq!(feed.kind, FeedKind::Json);
        assert_eq!(feed.title.as_deref(), Some("JSON Blog"));

        let entry = &entries[0];
        assert_eq!(entry.guid, "json-1");
        assert_eq!(
            entry.url.as_deref(),
            Some("https://json.example.com/posts/1")
        );
        assert_eq!(entry.title.as_deref(), Some("JSON Item"));
        assert_eq!(entry.author.as_deref(), Some("Ada"));
        assert_eq!(entry.published, Some(JAN_1_2026));
        assert_eq!(entry.summary.as_deref(), Some("JSON summary"));
        assert_eq!(entry.content.as_deref(), Some("JSON content"));
    }

    #[test]
    fn test_parse_invalid_documents_fail() {
        for body in ["", "not xml at all", "<html><body>page</body></html>", "{}"] {
            assert!(
                matches!(
                    parse_feed(body.as_bytes(), None),
                    Err(MetroError::Generic(_))
                ),
                "expected parse error for {body:?}"
            );
        }
    }

    #[test]
    fn test_author_name_strips_rss_parentheses() {
        assert_eq!(author_name("(Jane Doe)"), "Jane Doe");
        assert_eq!(author_name("  ( Jane Doe )  "), "Jane Doe");
        assert_eq!(author_name("Jane Doe"), "Jane Doe");
        assert_eq!(author_name("Jane (JD) Doe"), "Jane (JD) Doe");
        assert_eq!(author_name("()"), "");
    }

    #[test]
    fn test_parse_feed_generates_missing_guid() {
        let rss = r#"<rss version="2.0"><channel><title>t</title>
            <item><title>No guid</title><link>https://example.com/a</link></item>
            </channel></rss>"#;
        let (_, entries) = parse_feed(rss.as_bytes(), None).unwrap();
        assert!(!entries[0].guid.is_empty());
    }
}
