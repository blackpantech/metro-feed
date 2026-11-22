//! HTTP feed fetching with conditional GET for Metro.
//!
//! This module implements the `feed-fetching` capability of the M1 milestone:
//! it downloads a feed over HTTP with [`ureq`], honours cached validators
//! (`ETag` / `Last-Modified`) through conditional GET, and hands the body to
//! [`parse_feed`] (see [`crate::parse`] for how entries are normalized).
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
//! - No overall deadline ([`FetchConfig::total_timeout`] is `None`). The
//!   autodiscovery request sets one (15 seconds, see
//!   [`crate::http::discovery`]) on top of the other limits.
//!
//! # Raw documents
//!
//! [`Fetcher::get`] performs an unconditional GET under the same limits and
//! returns the raw [`FetchedDocument`] (final URL after redirects,
//! `Content-Type`, validators, body) without parsing it. Autodiscovery uses it
//! so the first response of `metro feeds add` can be tried as a feed
//! ([`FetchedDocument::into_feed`]) and, when that fails, scanned as HTML,
//! without a second request. [`Fetcher::fetch`] is built on the same request
//! path, so both apply identical limits, status mapping, and parsing.
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

use ureq::ResponseExt;
use url::Url;

use crate::MetroError;
use crate::parse::{FeedMetadata, FetchedEntry, parse_feed};

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
    /// Optional deadline for the whole request (connect, redirects, headers,
    /// and body together). `None` means only the per-phase timeouts apply.
    pub total_timeout: Option<Duration>,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: READ_TIMEOUT,
            max_body_bytes: MAX_BODY_BYTES,
            max_redirects: MAX_REDIRECTS,
            total_timeout: None,
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

/// A successful (`2xx`) response, downloaded but not parsed.
///
/// Returned by [`Fetcher::get`]. The body has already been read under the
/// fetcher's size cap and timeouts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedDocument {
    /// URL of the response after redirects; relative links resolve against
    /// it.
    pub final_url: String,
    /// Response `Content-Type` header, if present and non-empty.
    pub content_type: Option<String>,
    /// Response `ETag`, to store for the next conditional request.
    pub etag: Option<String>,
    /// Response `Last-Modified`, to store for the next conditional request.
    pub last_modified: Option<String>,
    /// Raw response body.
    pub body: Vec<u8>,
}

impl FetchedDocument {
    /// Parse the body as a feed, turning the document into the
    /// [`FetchResult`] a [`Fetcher::fetch`] of the same response would have
    /// produced ([`FetchStatus::Modified`], the response validators, links
    /// resolved against [`FetchedDocument::final_url`]).
    ///
    /// # Errors
    ///
    /// When the body is not a supported feed, returns a [`NotAFeed`] that
    /// carries the [`MetroError::Generic`] parse error **and** gives the
    /// document back, so the caller can still inspect it (for example scan
    /// it as HTML) without downloading it again.
    pub fn into_feed(self) -> Result<FetchResult, NotAFeed> {
        match parse_feed(&self.body, Some(&self.final_url)) {
            Ok((feed, entries)) => Ok(FetchResult {
                status: FetchStatus::Modified,
                feed: Some(feed),
                entries,
                etag: self.etag,
                last_modified: self.last_modified,
            }),
            Err(error) => Err(NotAFeed {
                error,
                document: Box::new(self),
            }),
        }
    }
}

/// Failure of [`FetchedDocument::into_feed`]: the parse error plus the
/// untouched document.
#[derive(Debug)]
pub struct NotAFeed {
    /// The [`MetroError::Generic`] parse error.
    pub error: MetroError,
    /// The document whose body failed to parse (boxed to keep the `Err`
    /// variant small).
    pub document: Box<FetchedDocument>,
}

/// Outcome of the shared request path behind [`Fetcher::fetch`] and
/// [`Fetcher::get`].
enum RawResponse {
    /// `304 Not Modified`, with the validators the server repeated (if any).
    NotModified {
        etag: Option<String>,
        last_modified: Option<String>,
    },
    /// A `2xx` response with its body downloaded.
    Document(FetchedDocument),
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
            .timeout_global(config.total_timeout)
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
        match self.request(url, etag, last_modified)? {
            RawResponse::NotModified {
                etag: response_etag,
                last_modified: response_last_modified,
            } => Ok(FetchResult {
                status: FetchStatus::NotModified,
                feed: None,
                entries: Vec::new(),
                etag: response_etag.or_else(|| etag.map(str::to_owned)),
                last_modified: response_last_modified.or_else(|| last_modified.map(str::to_owned)),
            }),
            RawResponse::Document(document) => {
                document.into_feed().map_err(|failure| failure.error)
            }
        }
    }

    /// Download `url` unconditionally without parsing the body.
    ///
    /// Applies the same limits and status mapping as [`Fetcher::fetch`]; see
    /// [Raw documents](self#raw-documents).
    ///
    /// # Errors
    ///
    /// - [`MetroError::Usage`] if `url` is not an absolute `http(s)` URL.
    /// - [`MetroError::Network`] on transport failures, timeouts, too many
    ///   redirects, oversized bodies, or any non-success status (a `304` to
    ///   this unconditional request included).
    pub fn get(&self, url: &str) -> Result<FetchedDocument, MetroError> {
        match self.request(url, None, None)? {
            RawResponse::Document(document) => Ok(document),
            RawResponse::NotModified { .. } => Err(MetroError::Network(format!(
                "failed to fetch {}: server returned HTTP 304 to an unconditional request",
                url.trim()
            ))),
        }
    }

    /// Send one GET (with optional conditional headers), map its status, and
    /// download the body of a `2xx` answer under the size cap.
    ///
    /// # Errors
    ///
    /// [`MetroError::Usage`] for an invalid URL, [`MetroError::Network`] for
    /// transport failures, limits, and non-success statuses other than 304.
    fn request(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<RawResponse, MetroError> {
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
            return Ok(RawResponse::NotModified {
                etag: response_etag,
                last_modified: response_last_modified,
            });
        }

        if !response.status().is_success() {
            return Err(MetroError::Network(format!(
                "failed to fetch {target}: server returned HTTP {status}"
            )));
        }

        // Relative links in the document resolve against the final URL,
        // which differs from the requested one after a redirect.
        let final_url = response.get_uri().to_string();
        let content_type = header_value(&response, "content-type");
        let body = response
            .body_mut()
            .with_config()
            .limit(self.max_body_bytes)
            .read_to_vec()
            .map_err(|error| network_error(target.as_str(), &error))?;

        Ok(RawResponse::Document(FetchedDocument {
            final_url,
            content_type,
            etag: response_etag,
            last_modified: response_last_modified,
            body,
        }))
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::fmt::Write as _;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;

    use crate::models::FeedKind;
    use crate::parse::samples::{ATOM_FEED, RSS_FEED};

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
            write!(out, "{name}: {value}\r\n").unwrap();
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
        assert_eq!(config.total_timeout, None);
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
        assert_eq!(result.entries, []);
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
    fn test_get_returns_raw_document_after_redirect() {
        let (base, requests) = serve(2, |request| {
            if request.starts_with("get /old") {
                Reply::new("301 Moved Permanently", "").header("Location", "/new")
            } else {
                Reply::new("200 OK", "<html></html>")
                    .header("Content-Type", "text/html; charset=utf-8")
                    .header("ETag", "\"v1\"")
                    .header("Last-Modified", "Thu, 01 Jan 2026 00:00:00 GMT")
            }
        });

        let document = test_fetcher().get(&format!("{base}/old")).unwrap();

        let request = requests.recv().unwrap();
        assert!(!request.contains("if-none-match"));
        assert!(!request.contains("if-modified-since"));
        assert_eq!(document.final_url, format!("{base}/new"));
        assert_eq!(
            document.content_type.as_deref(),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(document.etag.as_deref(), Some("\"v1\""));
        assert_eq!(
            document.last_modified.as_deref(),
            Some("Thu, 01 Jan 2026 00:00:00 GMT")
        );
        assert_eq!(document.body, b"<html></html>");
    }

    #[test]
    fn test_get_maps_error_statuses_and_limits() {
        let (base, _requests) = serve(1, |_| Reply::new("304 Not Modified", ""));
        let error = test_fetcher().get(&base).unwrap_err();
        assert!(matches!(&error, MetroError::Network(message) if message.contains("304")));

        let (base, _requests) = serve(1, |_| Reply::new("404 Not Found", "gone"));
        let error = test_fetcher().get(&base).unwrap_err();
        assert!(matches!(&error, MetroError::Network(message) if message.contains("HTTP 404")));

        let (base, _requests) = serve(1, |_| Reply::new("200 OK", RSS_FEED));
        let error = Fetcher::new(FetchConfig {
            max_body_bytes: 64,
            ..FetchConfig::default()
        })
        .get(&base)
        .unwrap_err();
        assert!(matches!(&error, MetroError::Network(message) if message.contains("limit")));
    }

    #[test]
    fn test_total_timeout_is_enforced() {
        let (base, _requests) = serve(1, |_| Reply {
            delay: Some(Duration::from_millis(1500)),
            ..Reply::new("200 OK", RSS_FEED)
        });
        let fetcher = Fetcher::new(FetchConfig {
            total_timeout: Some(Duration::from_millis(200)),
            ..FetchConfig::default()
        });

        let error = fetcher.get(&base).unwrap_err();

        assert!(matches!(&error, MetroError::Network(message) if message.contains("timed out")));
    }

    #[test]
    fn test_into_feed_parses_or_gives_the_document_back() {
        let document = FetchedDocument {
            final_url: "https://blog.example.com/feed".to_string(),
            content_type: Some("text/plain".to_string()),
            etag: Some("\"v1\"".to_string()),
            last_modified: None,
            body: RSS_FEED.as_bytes().to_vec(),
        };
        let result = document.into_feed().unwrap();
        assert_eq!(result.status, FetchStatus::Modified);
        assert_eq!(result.etag.as_deref(), Some("\"v1\""));
        assert_eq!(result.entries.len(), 2);
        // Relative links resolve against the final URL.
        assert_eq!(
            result.entries[1].url.as_deref(),
            Some("https://blog.example.com/posts/2")
        );

        let page = FetchedDocument {
            final_url: "https://blog.example.com/".to_string(),
            content_type: None,
            etag: None,
            last_modified: None,
            body: b"<html></html>".to_vec(),
        };
        let failure = page.into_feed().unwrap_err();
        assert!(matches!(failure.error, MetroError::Generic(_)));
        assert_eq!(failure.document.body, b"<html></html>");
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
}
