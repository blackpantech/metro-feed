//! Enumerations for folder and feed types.

use serde::{Deserialize, Serialize};

/// Folder kind - distinguishes between feed folders and saved items
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderKind {
    /// A folder containing feed subscriptions
    Feeds,
    /// A folder containing saved items (bookmarks)
    Saved,
}

impl FolderKind {
    /// Returns all possible folder kinds
    pub const ALL: &[FolderKind] = &[FolderKind::Feeds, FolderKind::Saved];

    /// Returns a string representation suitable for display
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            FolderKind::Feeds => "feeds",
            FolderKind::Saved => "saved",
        }
    }
}

impl std::fmt::Display for FolderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for FolderKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "feeds" => Ok(FolderKind::Feeds),
            "saved" => Ok(FolderKind::Saved),
            other => Err(format!("unknown folder kind: {other}")),
        }
    }
}

/// Feed kind - the format/type of the feed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedKind {
    /// RSS feed (any version)
    Rss,
    /// Atom feed
    Atom,
    /// JSON Feed
    Json,
    /// `YouTube` channel feed
    Youtube,
}

impl FeedKind {
    /// Returns a string representation suitable for display
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            FeedKind::Rss => "rss",
            FeedKind::Atom => "atom",
            FeedKind::Json => "json",
            FeedKind::Youtube => "youtube",
        }
    }
}

impl std::fmt::Display for FeedKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for FeedKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "rss" => Ok(FeedKind::Rss),
            "atom" => Ok(FeedKind::Atom),
            "json" => Ok(FeedKind::Json),
            "youtube" => Ok(FeedKind::Youtube),
            other => Err(format!("unknown feed kind: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_folder_kind_display() {
        assert_eq!(FolderKind::Feeds.to_string(), "feeds");
        assert_eq!(FolderKind::Saved.to_string(), "saved");
    }

    #[test]
    fn test_folder_kind_from_str() {
        assert_eq!("feeds".parse::<FolderKind>().unwrap(), FolderKind::Feeds);
        assert_eq!("saved".parse::<FolderKind>().unwrap(), FolderKind::Saved);
        assert_eq!("FEEDS".parse::<FolderKind>().unwrap(), FolderKind::Feeds);
        assert_eq!("Saved".parse::<FolderKind>().unwrap(), FolderKind::Saved);
    }

    #[test]
    fn test_folder_kind_from_str_invalid() {
        assert!("invalid".parse::<FolderKind>().is_err());
        assert!("".parse::<FolderKind>().is_err());
    }

    #[test]
    fn test_feed_kind_display() {
        assert_eq!(FeedKind::Rss.to_string(), "rss");
        assert_eq!(FeedKind::Atom.to_string(), "atom");
        assert_eq!(FeedKind::Json.to_string(), "json");
        assert_eq!(FeedKind::Youtube.to_string(), "youtube");
    }

    #[test]
    fn test_feed_kind_from_str() {
        assert_eq!("rss".parse::<FeedKind>().unwrap(), FeedKind::Rss);
        assert_eq!("atom".parse::<FeedKind>().unwrap(), FeedKind::Atom);
        assert_eq!("json".parse::<FeedKind>().unwrap(), FeedKind::Json);
        assert_eq!("youtube".parse::<FeedKind>().unwrap(), FeedKind::Youtube);
        assert_eq!("RSS".parse::<FeedKind>().unwrap(), FeedKind::Rss);
        assert_eq!("Atom".parse::<FeedKind>().unwrap(), FeedKind::Atom);
    }

    #[test]
    fn test_feed_kind_from_str_invalid() {
        assert!("invalid".parse::<FeedKind>().is_err());
        assert!("".parse::<FeedKind>().is_err());
    }

    #[test]
    fn test_folder_kind_json_roundtrip() {
        let kind = FolderKind::Feeds;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#""feeds""#);
        let parsed: FolderKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);

        let kind = FolderKind::Saved;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#""saved""#);
        let parsed: FolderKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);
    }

    #[test]
    fn test_feed_kind_json_roundtrip() {
        let kind = FeedKind::Rss;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#""rss""#);
        let parsed: FeedKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);

        let kind = FeedKind::Youtube;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#""youtube""#);
        let parsed: FeedKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);
    }
}
