//! Feed URL autodiscovery for Metro.
//!
//! This module implements the `autodiscovery` capability of the M1 milestone:
//! given a blog homepage URL, it fetches the HTML and extracts the first feed
//! link advertised through `<link rel="alternate">` tags.
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
//! stored by the feed management API.
//!
//! # Direct feed URLs
//!
//! A homepage URL that already points at a feed is returned unchanged. This is
//! detected from the response `Content-Type`: feed media types (`rss+xml`,
//! `atom+xml`, `feed+json`, `json`, `xml`, and `text/xml`) mark the response as
//! a feed rather than an HTML document.
//!
//! # Errors
//!
//! - [`MetroError::Usage`] for an empty or non-`http(s)` homepage URL.
//! - [`MetroError::Network`] when the request fails, including the 15-second
//!   timeout, DNS failures, TLS failures, and non-success HTTP status codes.
//! - [`MetroError::NotFound`] when the response is HTML but advertises no
//!   supported feed link.
//! - [`MetroError::Generic`] when the HTML document cannot be prepared for
//!   selection.
//!
//! # Timeout
//!
//! Autodiscovery fetches a single HTML document, so it uses the 15-second
//! timeout pinned by the autodiscovery spec instead of the longer one used for
//! feed downloads.
//!
//! # Testing seam
//!
//! Callers that orchestrate discovery (such as
//! [`crate::refresh::subscribe_url`], used by `metro feeds add`) depend on
//! the [`FeedDiscoverer`] trait rather than on [`discover_feed_url`]
//! directly (dependency inversion, like `FeedFetcher` for fetching).
//! [`HttpDiscoverer`] is the production implementation; tests inject mocks
//! and never hit the network.

use std::time::Duration;

use scraper::{Html, Selector};
use ureq::ResponseExt;
use url::Url;

use crate::MetroError;
use crate::feeds::normalize_url;

/// Media types accepted on `<link type="...">` tags for feed autodiscovery.
const FEED_LINK_TYPES: &[&str] = &[
    "application/rss+xml",
    "application/atom+xml",
    "application/feed+json",
];

/// Response content types that mean the fetched URL is already a feed.
///
/// XML and JSON are included because many feed servers omit the more specific
/// `rss+xml`/`atom+xml`/`feed+json` spelling. HTML content types are
/// deliberately absent so ordinary pages are parsed for feed links.
const FEED_CONTENT_TYPES: &[&str] = &[
    "application/rss+xml",
    "application/atom+xml",
    "application/feed+json",
    "application/json",
    "application/xml",
    "text/xml",
];

/// Timeout applied to the autodiscovery HTTP request.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

/// Polite User-Agent sent with autodiscovery requests.
const USER_AGENT: &str = "metro-feed/0.1 (terminal; local-first)";

/// Source of autodiscovery results, abstracted for dependency injection.
///
/// The contract is exactly that of [`discover_feed_url`]: return the feed URL
/// advertised by (or equal to) `homepage_url`, or the documented
/// [`MetroError`] variants.
pub trait FeedDiscoverer {
    /// Resolve `homepage_url` to a feed URL.
    ///
    /// # Errors
    ///
    /// See [`discover_feed_url`].
    fn discover(&self, homepage_url: &str) -> Result<String, MetroError>;
}

/// Production [`FeedDiscoverer`]: performs the HTTP request with
/// [`discover_feed_url`].
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpDiscoverer;

impl FeedDiscoverer for HttpDiscoverer {
    fn discover(&self, homepage_url: &str) -> Result<String, MetroError> {
        discover_feed_url(homepage_url)
    }
}

/// Discover a feed URL from a blog homepage.
///
/// Fetches `homepage_url`, parses the returned HTML, and returns the first
/// `<link rel="alternate">` feed URL it advertises. When the URL already points
/// at a feed (as reported by the response `Content-Type`), the input URL is
/// returned unchanged.
///
/// # Errors
///
/// - [`MetroError::Usage`] if the homepage URL is empty, malformed, or does
///   not use the `http`/`https` scheme.
/// - [`MetroError::Network`] if the request times out or otherwise fails.
/// - [`MetroError::NotFound`] if the page advertises no supported feed link.
/// - [`MetroError::Generic`] if the HTML cannot be parsed for selection.
pub fn discover_feed_url(homepage_url: &str) -> Result<String, MetroError> {
    let homepage = parse_http_url(homepage_url)?;

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(DISCOVERY_TIMEOUT))
        .user_agent(USER_AGENT)
        .build()
        .into();

    let mut response = agent
        .get(homepage.as_str())
        .call()
        .map_err(|error| network_error(homepage.as_str(), &error))?;

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    if content_type
        .as_deref()
        .is_some_and(|value| matches_media_type(value, FEED_CONTENT_TYPES))
    {
        return Ok(homepage_url.trim().to_string());
    }

    // Relative links must be resolved against the final URL, which can differ
    // from the requested one when the server redirected.
    let effective_url = response.get_uri().to_string();
    let html = response
        .body_mut()
        .read_to_string()
        .map_err(|error| network_error(homepage.as_str(), &error))?;

    find_feed_url(&html, &effective_url)?.ok_or_else(|| {
        MetroError::NotFound(format!(
            "no feed found at {homepage_url}: the page advertises no RSS, Atom, or JSON Feed link"
        ))
    })
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

/// Map a `ureq` failure to a network error with request context.
fn network_error(url: &str, error: &ureq::Error) -> MetroError {
    MetroError::Network(format!("failed to fetch {url}: {error}"))
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
    }
}
