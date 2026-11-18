//! Entry model - represents a single feed entry (article, post, etc.).

use serde::{Deserialize, Serialize};

use super::id::{EntryId, FeedId};

/// A single entry from a feed.
///
/// Entries are the individual content items (articles, posts, videos) that
/// appear in feeds. Each entry tracks:
/// - Its source feed
/// - Unique identifier (guid from the feed)
/// - Content metadata (title, author, published date)
/// - Content itself (summary and/or full content)
/// - Read state (for tracking what the user has seen)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Unique identifier for this entry
    pub id: EntryId,
    /// ID of the feed this entry belongs to
    pub feed_id: FeedId,
    /// Unique identifier from the feed (GUID) - used for deduplication
    pub guid: String,
    /// URL of the entry (may be same as feed's site URL for some feeds)
    pub url: String,
    /// Title of the entry
    pub title: String,
    /// Author of the entry (if available)
    pub author: Option<String>,
    /// Unix timestamp of when the entry was published
    pub published: Option<u64>,
    /// Short summary or excerpt of the entry (if available)
    pub summary: Option<String>,
    /// Full content of the entry (if available)
    pub content: Option<String>,
    /// Whether the entry has been read by the user
    pub is_read: bool,
}

impl Entry {
    /// Create a new entry with the given parameters
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: EntryId,
        feed_id: FeedId,
        guid: impl Into<String>,
        url: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            id,
            feed_id,
            guid: guid.into(),
            url: url.into(),
            title: title.into(),
            author: None,
            published: None,
            summary: None,
            content: None,
            is_read: false,
        }
    }

    /// Get the entry's ID
    #[must_use]
    pub fn id(&self) -> EntryId {
        self.id
    }

    /// Get the feed ID this entry belongs to
    #[must_use]
    pub fn feed_id(&self) -> FeedId {
        self.feed_id
    }

    /// Get the entry's GUID
    #[must_use]
    pub fn guid(&self) -> &str {
        &self.guid
    }

    /// Get the entry's URL
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Get the entry's title
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Get the entry's author (if available)
    #[must_use]
    pub fn author(&self) -> Option<&str> {
        self.author.as_deref()
    }

    /// Get the entry's published timestamp
    #[must_use]
    pub fn published(&self) -> Option<u64> {
        self.published
    }

    /// Get the entry's summary (if available)
    #[must_use]
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// Get the entry's full content (if available)
    #[must_use]
    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    /// Check if the entry has been read
    #[must_use]
    pub fn is_read(&self) -> bool {
        self.is_read
    }

    /// Mark the entry as read
    pub fn mark_read(&mut self) {
        self.is_read = true;
    }

    /// Mark the entry as unread
    pub fn mark_unread(&mut self) {
        self.is_read = false;
    }

    /// Set the author
    pub fn set_author(&mut self, author: Option<String>) {
        self.author = author;
    }

    /// Set the published timestamp
    pub fn set_published(&mut self, timestamp: Option<u64>) {
        self.published = timestamp;
    }

    /// Set the summary
    pub fn set_summary(&mut self, summary: Option<String>) {
        self.summary = summary;
    }

    /// Set the full content
    pub fn set_content(&mut self, content: Option<String>) {
        self.content = content;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entry_creation() {
        let entry = Entry::new(
            EntryId(1),
            FeedId(42),
            "https://example.com/post/1",
            "https://example.com/post/1",
            "My First Post",
        );

        assert_eq!(entry.id(), EntryId(1));
        assert_eq!(entry.feed_id(), FeedId(42));
        assert_eq!(entry.guid(), "https://example.com/post/1");
        assert_eq!(entry.url(), "https://example.com/post/1");
        assert_eq!(entry.title(), "My First Post");
        assert_eq!(entry.author(), None);
        assert_eq!(entry.published(), None);
        assert_eq!(entry.summary(), None);
        assert_eq!(entry.content(), None);
        assert!(!entry.is_read());
    }

    #[test]
    fn test_entry_has_all_required_fields() {
        let entry = Entry {
            id: EntryId(100),
            feed_id: FeedId(5),
            guid: "unique-guid-123".to_string(),
            url: "https://example.com/post/1".to_string(),
            title: "Example Post".to_string(),
            author: Some("John Doe".to_string()),
            published: Some(1767225600),
            summary: Some("A brief summary".to_string()),
            content: Some("Full content here".to_string()),
            is_read: false,
        };

        // Verify all required fields are present
        assert_eq!(entry.id.0, 100);
        assert_eq!(entry.feed_id.0, 5);
        assert!(!entry.guid.is_empty());
        assert!(!entry.url.is_empty());
        assert!(!entry.title.is_empty());
        assert!(entry.author.is_some());
        assert!(entry.published.is_some());
        assert!(entry.summary.is_some());
        assert!(entry.content.is_some());
        assert!(matches!(entry.is_read, true | false));
    }

    #[test]
    fn test_entry_read_state_defaults_to_false() {
        let entry = Entry::new(
            EntryId(1),
            FeedId(1),
            "guid",
            "https://example.com",
            "Title",
        );
        assert!(!entry.is_read());
    }

    #[test]
    fn test_entry_mark_read() {
        let mut entry = Entry::new(
            EntryId(1),
            FeedId(1),
            "guid",
            "https://example.com",
            "Title",
        );
        assert!(!entry.is_read());

        entry.mark_read();
        assert!(entry.is_read());

        entry.mark_unread();
        assert!(!entry.is_read());
    }

    #[test]
    fn test_entry_setters() {
        let mut entry = Entry::new(
            EntryId(1),
            FeedId(1),
            "guid",
            "https://example.com",
            "Title",
        );

        entry.set_author(Some("Jane Doe".to_string()));
        assert_eq!(entry.author(), Some("Jane Doe"));

        entry.set_published(Some(1234567890));
        assert_eq!(entry.published(), Some(1234567890));

        entry.set_summary(Some("Summary text".to_string()));
        assert_eq!(entry.summary(), Some("Summary text"));

        entry.set_content(Some("Full content".to_string()));
        assert_eq!(entry.content(), Some("Full content"));
    }

    #[test]
    fn test_entry_json_roundtrip() {
        let entry = Entry {
            id: EntryId(4031),
            feed_id: FeedId(12),
            guid: "tag:example.com,2026:post/1".to_string(),
            url: "https://example.com/post/1".to_string(),
            title: "Example Post Title".to_string(),
            author: Some("Jane Doe".to_string()),
            published: Some(1767225600),
            summary: Some("A brief summary of the post".to_string()),
            content: Some("The full content of the post goes here.".to_string()),
            is_read: false,
        };

        let json = serde_json::to_string_pretty(&entry).unwrap();
        let parsed: Entry = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, entry.id);
        assert_eq!(parsed.feed_id, entry.feed_id);
        assert_eq!(parsed.guid, entry.guid);
        assert_eq!(parsed.url, entry.url);
        assert_eq!(parsed.title, entry.title);
        assert_eq!(parsed.author, entry.author);
        assert_eq!(parsed.published, entry.published);
        assert_eq!(parsed.summary, entry.summary);
        assert_eq!(parsed.content, entry.content);
        assert_eq!(parsed.is_read, entry.is_read);
    }

    #[test]
    fn test_entry_json_serialization_format() {
        let entry = Entry::new(
            EntryId(1),
            FeedId(1),
            "guid",
            "https://example.com",
            "Title",
        );

        let json = serde_json::to_string(&entry).unwrap();
        // Verify snake_case field names
        assert!(json.contains("\"id\":\"e1\""), "{json}");
        assert!(json.contains("\"feed_id\":\"f1\""), "{json}");
        assert!(json.contains("\"guid\""));
        assert!(json.contains("\"url\""));
        assert!(json.contains("\"title\""));
        assert!(json.contains("\"author\""));
        assert!(json.contains("\"published\""));
        assert!(json.contains("\"summary\""));
        assert!(json.contains("\"content\""));
        assert!(json.contains("\"is_read\""));
    }

    #[test]
    fn test_entry_with_optional_fields_none() {
        let entry = Entry::new(
            EntryId(1),
            FeedId(1),
            "guid",
            "https://example.com",
            "Title",
        );

        // All optional fields should be None
        assert_eq!(entry.author(), None);
        assert_eq!(entry.published(), None);
        assert_eq!(entry.summary(), None);
        assert_eq!(entry.content(), None);
    }
}
