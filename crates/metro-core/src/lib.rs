//! Metro core library.
//!
//! Provides error types, configuration loading, database connectivity, data
//! models, the folder management API, the feed management API, the entry
//! listing API, URL
//! autodiscovery, HTTP feed fetching, the threaded refresh pipeline
//! (including subscribe-time fetching), and age-based entry pruning.

pub mod config;
pub mod db;
pub mod discovery;
pub mod entries;
pub mod error;
pub mod feeds;
pub mod fetch;
pub mod folders;
pub mod models;
pub mod prune;
pub mod refresh;

#[cfg(feature = "test-utils")]
pub mod test_utils;

pub use error::MetroError;
