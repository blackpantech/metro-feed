//! Metro core library.
//!
//! Provides error types, configuration loading, database connectivity, data
//! models, the folder management API, the feed management API, URL
//! autodiscovery, HTTP feed fetching, and the threaded refresh pipeline
//! (including subscribe-time fetching).

pub mod config;
pub mod db;
pub mod discovery;
pub mod error;
pub mod feeds;
pub mod fetch;
pub mod folders;
pub mod models;
pub mod refresh;

#[cfg(feature = "test-utils")]
pub mod test_utils;

pub use error::MetroError;
