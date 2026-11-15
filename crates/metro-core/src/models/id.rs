//! Typed ID wrappers for compile-time safety.
//!
//! Each ID type is a newtype wrapper around `u64` that implements:
//! - `Display` with a prefix format (`f<N>`, `e<N>`, `s<N>`)
//! - `FromStr` with helpful error messages when the wrong format is used
//! - `serde::{Serialize, Deserialize}` for JSON serialization
//!
//! These types prevent mixing up different ID types at compile time.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Folder ID wrapper - displays as `f<N>`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FolderId(pub u64);

impl FolderId {
    /// Create a new `FolderId`
    #[must_use]
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw numeric ID
    #[must_use]
    pub fn inner(&self) -> u64 {
        self.0
    }
}

impl fmt::Display for FolderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "f{}", self.0)
    }
}

impl FromStr for FolderId {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix('f') {
            rest.parse::<u64>()
                .map(Self)
                .map_err(|_| IdParseError::InvalidNumber(s.to_string(), "f"))
        } else {
            Err(IdParseError::MissingPrefix(s.to_string(), "f"))
        }
    }
}

/// Feed ID wrapper - displays as `f<N>` (same prefix as `FolderId`, but distinct type)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FeedId(pub u64);

impl FeedId {
    /// Create a new `FeedId`
    #[must_use]
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw numeric ID
    #[must_use]
    pub fn inner(&self) -> u64 {
        self.0
    }
}

impl fmt::Display for FeedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "f{}", self.0)
    }
}

impl FromStr for FeedId {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix('f') {
            rest.parse::<u64>()
                .map(Self)
                .map_err(|_| IdParseError::InvalidNumber(s.to_string(), "f"))
        } else {
            Err(IdParseError::MissingPrefix(s.to_string(), "f"))
        }
    }
}

/// Entry ID wrapper - displays as `e<N>`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryId(pub u64);

impl EntryId {
    /// Create a new `EntryId`
    #[must_use]
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw numeric ID
    #[must_use]
    pub fn inner(&self) -> u64 {
        self.0
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "e{}", self.0)
    }
}

impl FromStr for EntryId {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix('e') {
            rest.parse::<u64>()
                .map(Self)
                .map_err(|_| IdParseError::InvalidNumber(s.to_string(), "e"))
        } else {
            Err(IdParseError::MissingPrefix(s.to_string(), "e"))
        }
    }
}

/// Saved item ID wrapper - displays as `s<N>`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SavedItemId(pub u64);

impl SavedItemId {
    /// Create a new `SavedItemId`
    #[must_use]
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw numeric ID
    #[must_use]
    pub fn inner(&self) -> u64 {
        self.0
    }
}

impl fmt::Display for SavedItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s{}", self.0)
    }
}

impl FromStr for SavedItemId {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix('s') {
            rest.parse::<u64>()
                .map(Self)
                .map_err(|_| IdParseError::InvalidNumber(s.to_string(), "s"))
        } else {
            Err(IdParseError::MissingPrefix(s.to_string(), "s"))
        }
    }
}

/// Error type for ID parsing failures
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdParseError {
    /// The string had the correct prefix but the number was invalid
    InvalidNumber(String, &'static str),
    /// The string was missing the expected prefix
    MissingPrefix(String, &'static str),
}

impl fmt::Display for IdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdParseError::InvalidNumber(s, prefix) => {
                write!(
                    f,
                    "invalid number in ID: {s:?} (expected format: {prefix}<number>)"
                )
            }
            IdParseError::MissingPrefix(s, prefix) => {
                write!(
                    f,
                    "missing '{prefix}' prefix in ID: {s:?} (expected format: {prefix}<number>)"
                )
            }
        }
    }
}

impl std::error::Error for IdParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_folder_id_display() {
        assert_eq!(FolderId(12).to_string(), "f12");
        assert_eq!(FolderId(0).to_string(), "f0");
        assert_eq!(FolderId(4031).to_string(), "f4031");
    }

    #[test]
    fn test_feed_id_display() {
        assert_eq!(FeedId(12).to_string(), "f12");
        assert_eq!(FeedId(0).to_string(), "f0");
    }

    #[test]
    fn test_entry_id_display() {
        assert_eq!(EntryId(12).to_string(), "e12");
        assert_eq!(EntryId(4031).to_string(), "e4031");
        assert_eq!(EntryId(0).to_string(), "e0");
    }

    #[test]
    fn test_saved_item_id_display() {
        assert_eq!(SavedItemId(88).to_string(), "s88");
        assert_eq!(SavedItemId(0).to_string(), "s0");
    }

    #[test]
    fn test_folder_id_from_str() {
        assert_eq!("f12".parse::<FolderId>().unwrap(), FolderId(12));
        assert_eq!("f0".parse::<FolderId>().unwrap(), FolderId(0));
        assert_eq!("f4031".parse::<FolderId>().unwrap(), FolderId(4031));
    }

    #[test]
    fn test_entry_id_from_str() {
        assert_eq!("e12".parse::<EntryId>().unwrap(), EntryId(12));
        assert_eq!("e4031".parse::<EntryId>().unwrap(), EntryId(4031));
    }

    #[test]
    fn test_id_from_str_missing_prefix() {
        let result = "123".parse::<EntryId>();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, IdParseError::MissingPrefix(_, "e")));
        // Check the error message contains helpful hint
        let msg = err.to_string();
        assert!(msg.contains("e"));
    }

    #[test]
    fn test_id_from_str_invalid_number() {
        let result = "eabc".parse::<EntryId>();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, IdParseError::InvalidNumber(_, "e")));
    }

    #[test]
    fn test_id_from_str_whitespace() {
        assert_eq!("  f12  ".parse::<FolderId>().unwrap(), FolderId(12));
        assert_eq!("  e4031  ".parse::<EntryId>().unwrap(), EntryId(4031));
    }

    #[test]
    fn test_id_json_roundtrip() {
        // FolderId
        let folder_id = FolderId(42);
        let json = serde_json::to_string(&folder_id).unwrap();
        assert_eq!(json, "42");
        let parsed: FolderId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, folder_id);

        // EntryId
        let entry_id = EntryId(4031);
        let json = serde_json::to_string(&entry_id).unwrap();
        assert_eq!(json, "4031");
        let parsed: EntryId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, entry_id);

        // SavedItemId
        let saved_id = SavedItemId(88);
        let json = serde_json::to_string(&saved_id).unwrap();
        assert_eq!(json, "88");
        let parsed: SavedItemId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, saved_id);
    }

    #[test]
    fn test_id_types_not_interchangeable() {
        // This test verifies compile-time type safety.
        // If this compiles, the types are correctly distinct.
        let folder_id = FolderId(1);
        let feed_id = FeedId(1);
        let entry_id = EntryId(1);

        // These would not compile if we tried to use them interchangeably:
        // let _: FeedId = folder_id;  // Compile error
        // let _: EntryId = feed_id;   // Compile error

        // But we can compare their inner values
        assert_eq!(folder_id.inner(), feed_id.inner());
        assert_eq!(feed_id.inner(), entry_id.inner());
    }
}
