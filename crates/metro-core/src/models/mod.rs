//! Data models for Metro.
//!
//! This module provides type-safe in-memory representations of the core domain entities:
//! - [`Folder`]: A collection of feeds or saved items
//! - [`Feed`]: A subscribed feed (`RSS`, `Atom`, `JSON`, `YouTube`)
//! - [`Entry`]: A single entry/article from a feed
//!
//! It also includes:
//! - Typed ID wrappers ([`FolderId`] `fo3`, [`FeedId`] `f12`, [`EntryId`] `e4031`,
//!   [`SavedItemId`] `s3`) for compile-time safety. They display, parse and
//!   serialize to JSON in that prefixed string form (see [`id`])
//! - Enumerations ([`FolderKind`], [`FeedKind`]) for type-safe categorization

pub mod entry;
pub mod enums;
pub mod feed;
pub mod folder;
pub mod id;

pub use entry::Entry;
pub use enums::{FeedKind, FolderKind};
pub use feed::Feed;
pub use folder::Folder;
pub use id::{EntryId, FeedId, FolderId, SavedItemId};
