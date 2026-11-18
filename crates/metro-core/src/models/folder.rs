//! Folder model - represents a collection of feeds or saved items.

use serde::{Deserialize, Serialize};

use super::enums::FolderKind;
use super::id::FolderId;

/// A folder containing feeds or saved items.
///
/// Folders provide organization for feed subscriptions. There are two kinds:
/// - `Feeds`: Contains feed subscriptions (RSS, Atom, JSON feeds)
/// - `Saved`: Contains saved/bookmarked items (future feature, M3)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// Unique identifier for this folder
    pub id: FolderId,
    /// Human-readable name for the folder (case-insensitive for comparison)
    pub name: String,
    /// The kind of folder - determines what it can contain
    pub kind: FolderKind,
}

impl Folder {
    /// Create a new folder with the given ID, name, and kind
    #[must_use]
    pub fn new(id: FolderId, name: impl Into<String>, kind: FolderKind) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
        }
    }

    /// Get the folder's ID
    #[must_use]
    pub fn id(&self) -> FolderId {
        self.id
    }

    /// Get the folder's name
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the folder's kind
    #[must_use]
    pub fn kind(&self) -> FolderKind {
        self.kind
    }

    /// Returns true if this folder contains feeds (as opposed to saved items)
    #[must_use]
    pub fn is_feeds_folder(&self) -> bool {
        self.kind == FolderKind::Feeds
    }

    /// Returns true if this folder contains saved items
    #[must_use]
    pub fn is_saved_folder(&self) -> bool {
        self.kind == FolderKind::Saved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_folder_creation() {
        let folder = Folder::new(FolderId(1), "Tech News", FolderKind::Feeds);
        assert_eq!(folder.id(), FolderId(1));
        assert_eq!(folder.name(), "Tech News");
        assert_eq!(folder.kind(), FolderKind::Feeds);
    }

    #[test]
    fn test_folder_has_required_fields() {
        let folder = Folder {
            id: FolderId(42),
            name: "My Feeds".to_string(),
            kind: FolderKind::Feeds,
        };
        // Verify all required fields are present
        assert_eq!(folder.id.0, 42);
        assert!(!folder.name.is_empty());
        assert!(matches!(folder.kind, FolderKind::Feeds | FolderKind::Saved));
    }

    #[test]
    fn test_folder_kind_feeds() {
        let folder = Folder::new(FolderId(1), "News", FolderKind::Feeds);
        assert_eq!(folder.kind(), FolderKind::Feeds);
        assert!(folder.is_feeds_folder());
        assert!(!folder.is_saved_folder());
    }

    #[test]
    fn test_folder_kind_saved() {
        let folder = Folder::new(FolderId(2), "Bookmarks", FolderKind::Saved);
        assert_eq!(folder.kind(), FolderKind::Saved);
        assert!(!folder.is_feeds_folder());
        assert!(folder.is_saved_folder());
    }

    #[test]
    fn test_folder_json_roundtrip() {
        let folder = Folder {
            id: FolderId(12),
            name: "Tech Blogs".to_string(),
            kind: FolderKind::Feeds,
        };

        let json = serde_json::to_string_pretty(&folder).unwrap();
        let parsed: Folder = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, folder.id);
        assert_eq!(parsed.name, folder.name);
        assert_eq!(parsed.kind, folder.kind);
    }

    #[test]
    fn test_folder_json_serialization_format() {
        let folder = Folder {
            id: FolderId(1),
            name: "News".to_string(),
            kind: FolderKind::Feeds,
        };

        let json = serde_json::to_string(&folder).unwrap();
        // Verify snake_case field names
        assert!(json.contains("\"id\":\"fo1\""), "{json}");
        assert!(json.contains("\"name\""));
        assert!(json.contains("\"kind\""));
    }
}
