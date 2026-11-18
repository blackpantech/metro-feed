//! Feed document parsing for Metro.
//!
//! This module turns the bytes of a feed document into format-independent
//! [`FeedMetadata`] and [`FetchedEntry`] values with [`feed_rs`]. It performs
//! no I/O: the HTTP client ([`crate::http::client`]) feeds it response
//! bodies, and tests and fixtures can call [`parse_feed`] on bytes directly.
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
//!
//! # Errors
//!
//! [`parse_feed`] returns [`MetroError::Generic`] when the body is not a
//! parsable feed.

use feed_rs::model::{Entry as ParsedEntry, Feed as ParsedFeed, FeedType, Link, Text};
use feed_rs::parser::Builder as ParserBuilder;

use crate::MetroError;
use crate::models::FeedKind;

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

/// Sample feed documents shared by the parser and HTTP client tests.
#[cfg(test)]
pub(crate) mod samples {
    pub(crate) const RSS_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
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

    pub(crate) const ATOM_FEED: &str = r#"<?xml version="1.0" encoding="utf-8"?>
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

    pub(crate) const JSON_FEED: &str = r#"{
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
    pub(crate) const JAN_1_2026: u64 = 1_767_225_600;
    /// 2026-01-02T00:00:00Z as Unix seconds.
    pub(crate) const JAN_2_2026: u64 = JAN_1_2026 + 86_400;
}

#[cfg(test)]
mod tests {
    use super::samples::{ATOM_FEED, JAN_1_2026, JAN_2_2026, JSON_FEED, RSS_FEED};
    use super::*;

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
        assert_ne!(entries[0].guid, "");
    }
}
