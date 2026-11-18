//! Persistence and the product rules that apply to it.
//!
//! Each module exposes free functions over an already-migrated
//! [`rusqlite::Connection`] (see [`crate::db`]): `&Connection` for reads,
//! `&mut Connection` for writes. Every mutation runs in an `IMMEDIATE`
//! transaction so check-then-write sequences stay atomic across processes.
//! These are not plain CRUD wrappers: they enforce the invariants of the
//! domain (the undeletable `Unsorted` folder, refusing to delete non-empty
//! folders, feed deduplication by normalized URL, the prune window).
//!
//! - [`folders`]: feed and saved-item folders.
//! - [`feeds`]: feed subscriptions.
//! - [`entries`]: entry listing for `metro view`.
//! - [`prune`]: age-based entry deletion and the shared window cutoff.

pub mod entries;
pub mod feeds;
pub mod folders;
pub mod prune;
