//! Feed model - represents a subscribed feed.

use serde::{Deserialize, Serialize};

use super::enums::FeedKind;
use super::id::{FeedId, FolderId};

/// A subscribed feed.
///
/// Feeds are the core unit of content ingestion. Each feed tracks:
/// - Its URL and metadata (title, site URL, kind)
/// - Which folder it belongs to
/// - HTTP caching state (etag, `last_modified`)
/// - Fetch history (`last_fetch` timestamp of the last successful fetch)
/// - Error state and its timestamp (`error_state`, `last_error_at`), set
///   when the last fetch failed and cleared by the next successful one
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feed {
    /// Unique identifier for this feed
    pub id: FeedId,
    /// The URL of the feed (normalized for deduplication)
    pub url: String,
    /// Human-readable title of the feed
    pub title: String,
    /// URL of the feed's home page / site
    pub site_url: String,
    /// The format/type of the feed
    pub kind: FeedKind,
    /// ID of the folder this feed belongs to
    pub folder_id: FolderId,
    /// `ETag` for HTTP conditional GET (304 handling)
    pub etag: Option<String>,
    /// Last-Modified header for HTTP conditional GET
    pub last_modified: Option<String>,
    /// Unix timestamp of the last successful fetch
    pub last_fetch: Option<u64>,
    /// Error message if the last fetch failed (None if successful)
    pub error_state: Option<String>,
    /// Unix timestamp of the last failed fetch, set together with
    /// `error_state` (None if the last fetch succeeded or none failed yet)
    pub last_error_at: Option<u64>,
}

impl Feed {
    /// Create a new feed with the given parameters
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: FeedId,
        url: impl Into<String>,
        title: impl Into<String>,
        site_url: impl Into<String>,
        kind: FeedKind,
        folder_id: FolderId,
    ) -> Self {
        Self {
            id,
            url: url.into(),
            title: title.into(),
            site_url: site_url.into(),
            kind,
            folder_id,
            etag: None,
            last_modified: None,
            last_fetch: None,
            error_state: None,
            last_error_at: None,
        }
    }

    /// Get the feed's ID
    #[must_use]
    pub fn id(&self) -> FeedId {
        self.id
    }

    /// Get the feed's URL
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Get the feed's title
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Get the feed's site URL
    #[must_use]
    pub fn site_url(&self) -> &str {
        &self.site_url
    }

    /// Get the feed's kind
    #[must_use]
    pub fn kind(&self) -> FeedKind {
        self.kind
    }

    /// Get the folder ID this feed belongs to
    #[must_use]
    pub fn folder_id(&self) -> FolderId {
        self.folder_id
    }

    /// Get the `ETag` for conditional GET
    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }

    /// Get the Last-Modified header for conditional GET
    #[must_use]
    pub fn last_modified(&self) -> Option<&str> {
        self.last_modified.as_deref()
    }

    /// Get the last fetch timestamp
    #[must_use]
    pub fn last_fetch(&self) -> Option<u64> {
        self.last_fetch
    }

    /// Get the error state (None if no error)
    #[must_use]
    pub fn error_state(&self) -> Option<&str> {
        self.error_state.as_deref()
    }

    /// Get the Unix timestamp of the last failed fetch (None if no error)
    #[must_use]
    pub fn last_error_at(&self) -> Option<u64> {
        self.last_error_at
    }

    /// Returns true if the feed has an error state
    #[must_use]
    pub fn has_error(&self) -> bool {
        self.error_state.is_some()
    }

    /// Returns true if the feed has never been fetched
    #[must_use]
    pub fn is_unfetched(&self) -> bool {
        self.last_fetch.is_none()
    }

    /// Update the `ETag`
    pub fn set_etag(&mut self, etag: Option<String>) {
        self.etag = etag;
    }

    /// Update the Last-Modified header
    pub fn set_last_modified(&mut self, last_modified: Option<String>) {
        self.last_modified = last_modified;
    }

    /// Update the last fetch timestamp
    pub fn set_last_fetch(&mut self, timestamp: u64) {
        self.last_fetch = Some(timestamp);
    }

    /// Update the error state
    pub fn set_error_state(&mut self, error: Option<String>) {
        self.error_state = error;
    }

    /// Update the failure timestamp
    pub fn set_last_error_at(&mut self, timestamp: Option<u64>) {
        self.last_error_at = timestamp;
    }

    /// Clear the error state and its timestamp (mark as successful)
    pub fn clear_error(&mut self) {
        self.error_state = None;
        self.last_error_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feed_creation() {
        let feed = Feed::new(
            FeedId(1),
            "https://example.com/feed.xml",
            "Example Blog",
            "https://example.com",
            FeedKind::Rss,
            FolderId(1),
        );

        assert_eq!(feed.id(), FeedId(1));
        assert_eq!(feed.url(), "https://example.com/feed.xml");
        assert_eq!(feed.title(), "Example Blog");
        assert_eq!(feed.site_url(), "https://example.com");
        assert_eq!(feed.kind(), FeedKind::Rss);
        assert_eq!(feed.folder_id(), FolderId(1));
    }

    #[test]
    fn test_feed_has_all_required_metadata() {
        let feed = Feed {
            id: FeedId(1),
            url: "https://example.com/feed.xml".to_string(),
            title: "Example Blog".to_string(),
            site_url: "https://example.com".to_string(),
            kind: FeedKind::Rss,
            folder_id: FolderId(1),
            etag: Some("\"abc123\"".to_string()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_string()),
            last_fetch: Some(1_445_414_880),
            error_state: None,
            last_error_at: None,
        };

        // Verify all required fields are present
        assert_eq!(feed.id.0, 1);
        assert_ne!(feed.url, "");
        assert_ne!(feed.title, "");
        assert_ne!(feed.site_url, "");
        assert!(matches!(
            feed.kind,
            FeedKind::Rss | FeedKind::Atom | FeedKind::Json | FeedKind::Youtube
        ));
        assert_eq!(feed.folder_id.0, 1);
        assert!(feed.etag.is_some());
        assert!(feed.last_modified.is_some());
        assert!(feed.last_fetch.is_some());
    }

    #[test]
    fn test_feed_kind_rss() {
        let feed = Feed::new(
            FeedId(1),
            "https://example.com/feed.xml",
            "Example",
            "https://example.com",
            FeedKind::Rss,
            FolderId(1),
        );
        assert_eq!(feed.kind(), FeedKind::Rss);
    }

    #[test]
    fn test_feed_error_state_nullable() {
        // Feed with no error (never failed)
        let mut feed = Feed::new(
            FeedId(1),
            "https://example.com/feed.xml",
            "Example",
            "https://example.com",
            FeedKind::Rss,
            FolderId(1),
        );
        assert_eq!(feed.error_state(), None);
        assert!(!feed.has_error());

        assert_eq!(feed.last_error_at(), None);

        // Feed with error
        feed.set_error_state(Some("Connection timeout".to_string()));
        feed.set_last_error_at(Some(1_767_229_200));
        assert_eq!(feed.error_state(), Some("Connection timeout"));
        assert_eq!(feed.last_error_at(), Some(1_767_229_200));
        assert!(feed.has_error());

        // Clear error (and its timestamp)
        feed.clear_error();
        assert_eq!(feed.error_state(), None);
        assert_eq!(feed.last_error_at(), None);
        assert!(!feed.has_error());
    }

    #[test]
    fn test_feed_unfetched() {
        let mut feed = Feed::new(
            FeedId(1),
            "https://example.com/feed.xml",
            "Example",
            "https://example.com",
            FeedKind::Rss,
            FolderId(1),
        );
        assert!(feed.is_unfetched());

        feed.set_last_fetch(1_234_567_890);
        assert!(!feed.is_unfetched());
    }

    #[test]
    fn test_feed_json_roundtrip() {
        let feed = Feed {
            id: FeedId(42),
            url: "https://example.com/feed.xml".to_string(),
            title: "Example Blog".to_string(),
            site_url: "https://example.com".to_string(),
            kind: FeedKind::Atom,
            folder_id: FolderId(2),
            etag: Some("\"xyz789\"".to_string()),
            last_modified: Some("Thu, 01 Jan 2026 00:00:00 GMT".to_string()),
            last_fetch: Some(1_767_225_600),
            error_state: Some("HTTP 500".to_string()),
            last_error_at: Some(1_767_229_200),
        };

        let json = serde_json::to_string_pretty(&feed).unwrap();
        let parsed: Feed = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, feed.id);
        assert_eq!(parsed.url, feed.url);
        assert_eq!(parsed.title, feed.title);
        assert_eq!(parsed.site_url, feed.site_url);
        assert_eq!(parsed.kind, feed.kind);
        assert_eq!(parsed.folder_id, feed.folder_id);
        assert_eq!(parsed.etag, feed.etag);
        assert_eq!(parsed.last_modified, feed.last_modified);
        assert_eq!(parsed.last_fetch, feed.last_fetch);
        assert_eq!(parsed.error_state, feed.error_state);
        assert_eq!(parsed.last_error_at, feed.last_error_at);
    }

    #[test]
    fn test_feed_json_serialization_format() {
        let feed = Feed::new(
            FeedId(1),
            "https://example.com/feed.xml",
            "Example",
            "https://example.com",
            FeedKind::Rss,
            FolderId(1),
        );

        let json = serde_json::to_string(&feed).unwrap();
        // Verify snake_case field names
        assert!(json.contains("\"id\":\"f1\""), "{json}");
        assert!(json.contains("\"url\""));
        assert!(json.contains("\"title\""));
        assert!(json.contains("\"site_url\""));
        assert!(json.contains("\"kind\""));
        assert!(json.contains("\"folder_id\":\"fo1\""), "{json}");
        assert!(json.contains("\"etag\""));
        assert!(json.contains("\"last_modified\""));
        assert!(json.contains("\"last_fetch\""));
        assert!(json.contains("\"error_state\""));
        assert!(json.contains("\"last_error_at\""));
    }
}
