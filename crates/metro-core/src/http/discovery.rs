//! Feed URL autodiscovery for Metro.
//!
//! This module implements the `autodiscovery` capability of the M1 milestone:
//! given the URL a user typed (a feed URL or a blog homepage), it makes
//! **one** request and works out which feed it designates.
//!
//! # Parse first, then scan
//!
//! The first response is classified by its **body**, whatever its
//! `Content-Type` (servers routinely serve feeds as `text/html`,
//! `text/plain`, or `application/octet-stream`):
//!
//! 1. The body is parsed as a feed ([`FetchedDocument::into_feed`]). On
//!    success the URL is a direct feed and the parsed response is returned as
//!    [`Discovery::Feed`], ready to be used as the subscription fetch (entries,
//!    metadata, and the response `ETag` / `Last-Modified`), so the feed is
//!    never downloaded twice.
//! 2. Otherwise the same body is scanned as HTML for a `<link
//!    rel="alternate">` feed link, returned as [`Discovery::Link`]; the caller
//!    then fetches that URL.
//! 3. When neither works, the error depends on what the server claimed: a
//!    response whose `Content-Type` is a feed media type
//!    (`FEED_CONTENT_TYPES`) is a broken feed and keeps the
//!    [`MetroError::Generic`] parse error, anything else is a page without a
//!    feed ([`MetroError::NotFound`]).
//!
//! # Supported feed link types
//!
//! Only the three feed formats that Metro can parse are considered:
//!
//! - RSS: `application/rss+xml`
//! - Atom: `application/atom+xml`
//! - JSON Feed: `application/feed+json`
//!
//! The `type` attribute is matched case-insensitively and media type
//! parameters such as `; charset=utf-8` are ignored. A `<link>` is only
//! accepted when its `rel` attribute contains the `alternate` token (the
//! attribute is a space-separated token list and is matched
//! case-insensitively) and it has a non-empty `href`.
//!
//! # Ordering and URL resolution
//!
//! Links are examined in document order and the first valid feed link wins, so
//! pages that advertise both RSS and Atom return whichever appears first in
//! the markup. Relative `href` values are resolved against the URL of the
//! response after redirects, which is what a browser would do. Accepted
//! feed URLs are normalized with [`normalize_url`] so they match the form
//! stored by the feed management API. The body is decoded as UTF-8 for the
//! scan, replacing invalid sequences.
//!
//! # Errors
//!
//! - [`MetroError::Usage`] for an empty or non-`http(s)` URL (before any
//!   request).
//! - [`MetroError::Network`] when the request fails, including the 15-second
//!   timeout, DNS failures, TLS failures, too many redirects, an oversized
//!   body, and non-success HTTP status codes.
//! - [`MetroError::NotFound`] when the body is neither a feed nor a page
//!   advertising a supported feed link.
//! - [`MetroError::Generic`] when a response served with a feed media type
//!   does not parse as a feed, or the HTML document cannot be prepared for
//!   selection.
//!
//! # Limits
//!
//! The request goes through a [`Fetcher`] built from [`discovery_config`]:
//! the feed-fetching limits (body cap, redirect limit, User-Agent) plus the
//! 15-second overall timeout pinned by the autodiscovery spec
//! ([`DISCOVERY_TIMEOUT`]). Since the first response may be used as the
//! feed download, it obeys the same body cap and redirect limit as any feed
//! fetch.
//!
//! # Testing seam
//!
//! Callers that orchestrate discovery (such as
//! [`crate::feed_sync::subscribe_url`], used by `metro feeds add`) depend on
//! the [`FeedDiscoverer`] trait rather than on [`HttpDiscoverer`] directly
//! (dependency inversion, like `FeedFetcher` for fetching). Tests inject
//! mocks, or point an [`HttpDiscoverer`] at a local server, and never hit
//! the network.

use std::time::Duration;

use scraper::{Html, Selector};
use url::Url;

use crate::MetroError;
use crate::http::client::{FetchConfig, FetchResult, FetchedDocument, Fetcher, NotAFeed};
use crate::url::normalize_url;

/// Media types accepted on `<link type="...">` tags for feed autodiscovery.
const FEED_LINK_TYPES: &[&str] = &[
    "application/rss+xml",
    "application/atom+xml",
    "application/feed+json",
];

/// Response content types with which a server claims to serve a feed.
///
/// They no longer decide whether a response **is** a feed (the body does,
/// see [Parse first, then scan](self#parse-first-then-scan)); they only
/// decide which error a response that is neither a feed nor a page with a
/// feed link gets: a broken feed (parse error) rather than "no feed found".
/// XML and JSON are included because many feed servers omit the more
/// specific `rss+xml`/`atom+xml`/`feed+json` spelling.
const FEED_CONTENT_TYPES: &[&str] = &[
    "application/rss+xml",
    "application/atom+xml",
    "application/feed+json",
    "application/json",
    "application/xml",
    "text/xml",
];

/// Overall timeout applied to the autodiscovery HTTP request.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

/// What the URL given to [`FeedDiscoverer::discover`] turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discovery {
    /// The URL serves a feed. Carries the parsed first response
    /// ([`FetchStatus::Modified`](crate::http::client::FetchStatus::Modified), with
    /// its validators), to be used as the subscription fetch instead of
    /// requesting the URL again.
    Feed(FetchResult),
    /// The URL serves a page advertising this (normalized) feed URL, which
    /// still has to be fetched.
    Link(String),
}

/// Source of autodiscovery results, abstracted for dependency injection.
///
/// The contract is that of [`HttpDiscoverer`]: classify the response of
/// `url` (see [Parse first, then scan](self#parse-first-then-scan)) or
/// return the documented [`MetroError`] variants.
pub trait FeedDiscoverer {
    /// Resolve `url` to a feed: the feed itself, or the feed URL it links to.
    ///
    /// # Errors
    ///
    /// See the [module documentation](self#errors).
    fn discover(&self, url: &str) -> Result<Discovery, MetroError>;
}

/// Limits for the autodiscovery request: [`FetchConfig::default`] plus the
/// [`DISCOVERY_TIMEOUT`] overall deadline.
#[must_use]
pub fn discovery_config() -> FetchConfig {
    FetchConfig {
        total_timeout: Some(DISCOVERY_TIMEOUT),
        ..FetchConfig::default()
    }
}

/// Production [`FeedDiscoverer`]: one HTTP request through a [`Fetcher`].
#[derive(Debug)]
pub struct HttpDiscoverer {
    /// Fetcher performing the single autodiscovery request.
    fetcher: Fetcher,
}

impl Default for HttpDiscoverer {
    /// A discoverer using [`discovery_config`].
    fn default() -> Self {
        Self::new(discovery_config())
    }
}

impl HttpDiscoverer {
    /// Build a discoverer with custom limits (tests use short timeouts).
    #[must_use]
    pub fn new(config: FetchConfig) -> Self {
        Self {
            fetcher: Fetcher::new(config),
        }
    }
}

impl FeedDiscoverer for HttpDiscoverer {
    fn discover(&self, url: &str) -> Result<Discovery, MetroError> {
        // Validated here for the homepage-specific usage messages.
        parse_http_url(url)?;
        let document = self.fetcher.get(url)?;
        classify(document, url.trim())
    }
}

/// Discover a feed URL from a blog homepage.
///
/// Convenience wrapper around [`HttpDiscoverer::default`]: returns the input
/// URL (trimmed) when it serves a feed, else the first feed link its page
/// advertises. Callers that go on to fetch the feed should use
/// [`FeedDiscoverer::discover`] instead, which hands back the already
/// downloaded feed and so saves a request.
///
/// # Errors
///
/// See the [module documentation](self#errors).
pub fn discover_feed_url(homepage_url: &str) -> Result<String, MetroError> {
    match HttpDiscoverer::default().discover(homepage_url)? {
        Discovery::Feed(_) => Ok(homepage_url.trim().to_string()),
        Discovery::Link(feed_url) => Ok(feed_url),
    }
}

/// Classify a downloaded response: feed, page with a feed link, or error.
///
/// `requested_url` only appears in the "no feed found" message. See
/// [Parse first, then scan](self#parse-first-then-scan).
///
/// # Errors
///
/// [`MetroError::Generic`] for a broken feed (served with a feed media type)
/// or an unusable selector, [`MetroError::NotFound`] when the body is
/// neither a feed nor a page with a feed link.
fn classify(document: FetchedDocument, requested_url: &str) -> Result<Discovery, MetroError> {
    let NotAFeed {
        error: parse_error,
        document,
    } = match document.into_feed() {
        Ok(result) => return Ok(Discovery::Feed(result)),
        Err(failure) => failure,
    };

    let html = String::from_utf8_lossy(&document.body);
    if let Some(feed_url) = find_feed_url(&html, &document.final_url)? {
        return Ok(Discovery::Link(feed_url));
    }

    if document
        .content_type
        .as_deref()
        .is_some_and(|value| matches_media_type(value, FEED_CONTENT_TYPES))
    {
        // The server claims a feed but sent something unparsable: report the
        // parse error rather than a misleading "no feed found".
        return Err(parse_error);
    }

    Err(MetroError::NotFound(format!(
        "no feed found at {requested_url}: the page advertises no RSS, Atom, or JSON Feed link"
    )))
}

/// Parse and validate an absolute `http(s)` URL.
///
/// # Errors
///
/// Returns [`MetroError::Usage`] for empty input, URLs that fail to parse, and
/// URLs whose scheme is not `http` or `https`.
fn parse_http_url(raw: &str) -> Result<Url, MetroError> {
    let input = raw.trim();
    if input.is_empty() {
        return Err(MetroError::Usage(
            "homepage URL must not be empty".to_string(),
        ));
    }

    let url = Url::parse(input)
        .map_err(|error| MetroError::Usage(format!("invalid homepage URL {input:?}: {error}")))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(MetroError::Usage(format!(
            "unsupported homepage URL scheme {:?}: only http and https are supported",
            url.scheme()
        )));
    }

    if url.host_str().is_none() {
        return Err(MetroError::Usage(format!(
            "invalid homepage URL {input:?}: the URL has no host"
        )));
    }

    Ok(url)
}

/// Return `true` when a media type matches one of `supported`.
///
/// Parameters such as `; charset=utf-8` are ignored and the comparison is
/// case-insensitive, matching how HTTP media types are compared.
fn matches_media_type(value: &str, supported: &[&str]) -> bool {
    let media_type = value.split(';').next().map_or("", str::trim);
    supported
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(media_type))
}

/// Find the first valid feed link in an HTML document.
///
/// `base_url` is used to resolve relative `href` values. Returns `Ok(None)`
/// when no `<link rel="alternate">` advertises a supported feed type.
///
/// # Errors
///
/// Returns [`MetroError::Usage`] if `base_url` is not an absolute `http(s)`
/// URL and [`MetroError::Generic`] if the `link` selector cannot be built.
fn find_feed_url(html: &str, base_url: &str) -> Result<Option<String>, MetroError> {
    let base = parse_http_url(base_url)?;
    let selector = Selector::parse("link")
        .map_err(|error| MetroError::Generic(format!("failed to build HTML selector: {error}")))?;
    let document = Html::parse_document(html);

    for element in document.select(&selector) {
        let value = element.value();

        let has_alternate_rel = value
            .attr("rel")
            .is_some_and(|rel| rel.split_ascii_whitespace().any(is_alternate));
        if !has_alternate_rel {
            continue;
        }

        let Some(link_type) = value.attr("type") else {
            continue;
        };
        if !matches_media_type(link_type, FEED_LINK_TYPES) {
            continue;
        }

        let Some(href) = value.attr("href") else {
            continue;
        };
        let href = href.trim();
        // A fragment-only link points at the current page, not at a feed.
        if href.is_empty() || href.starts_with('#') {
            continue;
        }

        let Ok(resolved) = base.join(href) else {
            continue;
        };
        if !matches!(resolved.scheme(), "http" | "https") || resolved.host_str().is_none() {
            continue;
        }

        // Normalization both validates the URL and returns it in the canonical
        // form used by the feed management API. A link that fails validation is
        // skipped so the next candidate can be considered.
        if let Ok(normalized) = normalize_url(resolved.as_str()) {
            return Ok(Some(normalized));
        }
    }

    Ok(None)
}

/// Return `true` when a `rel` token is the `alternate` keyword.
fn is_alternate(token: &str) -> bool {
    token.eq_ignore_ascii_case("alternate")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::http::client::FetchStatus;

    /// Wrap a fragment of markup in a minimal HTML document.
    fn document(head: &str) -> String {
        format!("<!doctype html><html><head>{head}</head><body><p>Blog</p></body></html>")
    }

    /// Run autodiscovery link extraction against the example blog.
    fn find(head: &str) -> Option<String> {
        find_feed_url(&document(head), "https://blog.example.com").unwrap()
    }

    #[test]
    fn test_find_rss_feed_link() {
        let head = r#"<link rel="alternate" type="application/rss+xml" href="https://blog.example.com/feed.xml">"#;
        assert_eq!(
            find(head).as_deref(),
            Some("https://blog.example.com/feed.xml")
        );
    }

    #[test]
    fn test_find_atom_feed_link() {
        let head = r#"<link rel="alternate" type="application/atom+xml" href="https://blog.example.com/atom.xml">"#;
        assert_eq!(
            find(head).as_deref(),
            Some("https://blog.example.com/atom.xml")
        );
    }

    #[test]
    fn test_find_json_feed_link() {
        let head = r#"<link rel="alternate" type="application/feed+json" href="https://blog.example.com/feed.json">"#;
        assert_eq!(
            find(head).as_deref(),
            Some("https://blog.example.com/feed.json")
        );
    }

    #[test]
    fn test_find_link_is_case_insensitive_and_ignores_parameters() {
        let head = r#"<link REL="Alternate" TYPE="Application/RSS+XML; charset=utf-8" HREF="https://blog.example.com/feed.xml">"#;
        assert_eq!(
            find(head).as_deref(),
            Some("https://blog.example.com/feed.xml")
        );
    }

    #[test]
    fn test_find_link_accepts_multiple_rel_tokens() {
        let head = r#"<link rel="alternate feed" type="application/atom+xml" href="/atom.xml">"#;
        assert_eq!(
            find(head).as_deref(),
            Some("https://blog.example.com/atom.xml")
        );
    }

    #[test]
    fn test_find_link_resolves_relative_hrefs() {
        let html = document(r#"<link rel="alternate" type="application/rss+xml" href="feed.xml">"#);
        let found = find_feed_url(&html, "https://blog.example.com/posts/").unwrap();
        assert_eq!(
            found.as_deref(),
            Some("https://blog.example.com/posts/feed.xml")
        );

        let html = document(
            r#"<link rel="alternate" type="application/rss+xml" href="//cdn.example.com/feed.xml">"#,
        );
        let found = find_feed_url(&html, "https://blog.example.com").unwrap();
        assert_eq!(found.as_deref(), Some("https://cdn.example.com/feed.xml"));
    }

    #[test]
    fn test_find_link_returns_first_valid_in_document_order() {
        let head = concat!(
            r#"<link rel="stylesheet" type="text/css" href="/style.css">"#,
            r#"<link rel="alternate" type="text/html" href="/index.html">"#,
            r#"<link rel="alternate" type="application/atom+xml" href="/atom.xml">"#,
            r#"<link rel="alternate" type="application/rss+xml" href="/rss.xml">"#,
        );
        assert_eq!(
            find(head).as_deref(),
            Some("https://blog.example.com/atom.xml")
        );
    }

    #[test]
    fn test_find_link_skips_incomplete_or_unsupported_tags() {
        let head = concat!(
            r#"<link rel="alternate" type="application/rss+xml">"#,
            r#"<link rel="alternate" href="/feed.xml">"#,
            r#"<link rel="alternate" type="application/xhtml+xml" href="/page">"#,
            r##"<link rel="alternate" type="application/rss+xml" href="#top">"##,
            r#"<link rel="alternate" type="application/rss+xml" href="mailto:feed@example.com">"#,
            r#"<link rel="alternate" type="application/rss+xml" href="  ">"#,
        );
        assert_eq!(find(head), None);
    }

    #[test]
    fn test_find_link_returns_none_without_feed_links() {
        let head =
            r#"<link rel="stylesheet" href="/style.css"><link rel="icon" href="/favicon.ico">"#;
        assert_eq!(find(head), None);
    }

    #[test]
    fn test_find_link_normalizes_discovered_url() {
        let head = r#"<link rel="alternate" type="application/rss+xml" href="https://Blog.Example.com/feed/?utm_source=page">"#;
        assert_eq!(find(head).as_deref(), Some("https://blog.example.com/feed"));
    }

    #[test]
    fn test_find_link_rejects_invalid_base_url() {
        assert!(matches!(
            find_feed_url("<html></html>", "not a url"),
            Err(MetroError::Usage(_))
        ));
        assert!(matches!(
            find_feed_url("<html></html>", "ftp://example.com"),
            Err(MetroError::Usage(_))
        ));
    }

    #[test]
    fn test_matches_media_type() {
        assert!(matches_media_type("application/rss+xml", FEED_LINK_TYPES));
        assert!(matches_media_type(
            " application/atom+xml ; charset=utf-8",
            FEED_LINK_TYPES
        ));
        assert!(matches_media_type("application/feed+json", FEED_LINK_TYPES));
        assert!(!matches_media_type("text/html", FEED_LINK_TYPES));
        assert!(!matches_media_type("application/json", FEED_LINK_TYPES));
    }

    #[test]
    fn test_feed_content_types() {
        assert!(matches_media_type(
            "application/rss+xml; charset=utf-8",
            FEED_CONTENT_TYPES
        ));
        assert!(matches_media_type("application/json", FEED_CONTENT_TYPES));
        assert!(matches_media_type("text/xml", FEED_CONTENT_TYPES));
        assert!(!matches_media_type("text/html", FEED_CONTENT_TYPES));
        assert!(!matches_media_type(
            "application/xhtml+xml",
            FEED_CONTENT_TYPES
        ));
    }

    const RSS: &str = "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel>\
        <title>Local Blog</title><link>/</link><description>d</description>\
        <item><title>One</title><link>/1</link><guid>g1</guid></item>\
        </channel></rss>";

    /// Spawn a local HTTP server answering every request with `200 OK`, the
    /// given `Content-Type`, an `ETag`, and `body`. Returns the base URL and
    /// a counter of requests served.
    fn serve(
        content_type: &'static str,
        body: &'static str,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        serve_status("200 OK", content_type, body)
    }

    /// [`serve`] with an explicit status line.
    fn serve_status(
        status: &'static str,
        content_type: &'static str,
        body: &'static str,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&served);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(&stream);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nETag: \"v1\"\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (base, served)
    }

    /// Discoverer with short timeouts so failing tests finish quickly.
    fn discoverer() -> HttpDiscoverer {
        HttpDiscoverer::new(FetchConfig {
            connect_timeout: Duration::from_secs(2),
            read_timeout: Duration::from_secs(2),
            ..discovery_config()
        })
    }

    fn served(counter: &std::sync::atomic::AtomicUsize) -> usize {
        counter.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn test_discovery_config_adds_overall_timeout() {
        let config = discovery_config();
        assert_eq!(config.total_timeout, Some(Duration::from_secs(15)));
        assert_eq!(
            FetchConfig {
                total_timeout: None,
                ..config
            },
            FetchConfig::default()
        );
    }

    /// A feed is recognized by its body whatever the `Content-Type`, with a
    /// single request, and comes back parsed with its validators.
    #[test]
    fn test_direct_feed_is_detected_whatever_the_content_type() {
        for content_type in [
            "text/html; charset=utf-8",
            "text/plain",
            "application/octet-stream",
            "application/rss+xml",
        ] {
            let (base, counter) = serve(content_type, RSS);

            let found = discoverer().discover(&format!("{base}/feed")).unwrap();

            let Discovery::Feed(result) = found else {
                panic!("expected a feed for {content_type}, got {found:?}");
            };
            assert_eq!(result.status, FetchStatus::Modified);
            assert_eq!(
                result.feed.and_then(|feed| feed.title).as_deref(),
                Some("Local Blog")
            );
            assert_eq!(result.entries.len(), 1);
            // Relative entry links resolve against the served URL.
            assert_eq!(
                result.entries[0].url.as_deref(),
                Some(format!("{base}/1").as_str())
            );
            assert_eq!(result.etag.as_deref(), Some("\"v1\""));
            assert_eq!(served(&counter), 1, "{content_type}");
        }
    }

    #[test]
    fn test_html_page_with_feed_link_is_discovered() {
        let (base, counter) = serve(
            "text/html",
            "<html><head><link rel=\"alternate\" type=\"application/atom+xml\" \
             href=\"/atom.xml\"></head></html>",
        );

        let found = discoverer().discover(&format!("{base}/")).unwrap();

        assert_eq!(found, Discovery::Link(format!("{base}/atom.xml")));
        assert_eq!(served(&counter), 1);
    }

    #[test]
    fn test_html_page_without_feed_link_is_not_found() {
        let (base, _counter) = serve("text/html", "<html><head></head><body>hi</body></html>");

        let error = discoverer().discover(&base).unwrap_err();

        assert!(matches!(&error, MetroError::NotFound(message) if message.contains("no feed")));
        assert_eq!(error.exit_code(), 3);
    }

    /// A body served as a feed that does not parse keeps the parse error
    /// (exit 1); the same body served as anything else is "no feed found".
    #[test]
    fn test_broken_feed_keeps_parse_error() {
        let (base, _counter) = serve("application/xml", "not a feed");
        let error = discoverer().discover(&base).unwrap_err();
        assert!(matches!(&error, MetroError::Generic(message) if message.contains("parse")));
        assert_eq!(error.exit_code(), 1);

        let (base, _counter) = serve("text/plain", "not a feed");
        let error = discoverer().discover(&base).unwrap_err();
        assert_eq!(error.exit_code(), 3);
    }

    #[test]
    fn test_http_error_status_is_network_error() {
        let (base, _counter) = serve_status("404 Not Found", "text/html", "missing");

        let error = discoverer().discover(&base).unwrap_err();

        assert!(matches!(&error, MetroError::Network(message) if message.contains("HTTP 404")));
        assert_eq!(error.exit_code(), 4);
    }

    #[test]
    fn test_discover_feed_url_returns_input_for_direct_feed() {
        let (base, _counter) = serve("text/plain", RSS);
        let url = format!("{base}/feed.xml");

        assert_eq!(discover_feed_url(&format!(" {url} ")).unwrap(), url);
    }

    #[test]
    fn test_discover_rejects_invalid_urls_without_network() {
        assert!(matches!(discover_feed_url(""), Err(MetroError::Usage(_))));
        assert!(matches!(
            discover_feed_url("example.com"),
            Err(MetroError::Usage(_))
        ));
        assert!(matches!(
            discover_feed_url("ftp://example.com"),
            Err(MetroError::Usage(_))
        ));
        assert!(matches!(
            HttpDiscoverer::default().discover("not a url"),
            Err(MetroError::Usage(_))
        ));
    }
}
