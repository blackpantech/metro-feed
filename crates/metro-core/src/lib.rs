//! Metro core library.
//!
//! Provides error types, configuration loading, database connectivity, data
//! models, the folder management API, and the feed management API.

pub mod config;
pub mod db;
pub mod discovery;
pub mod error;
pub mod feeds;
pub mod folders;
pub mod models;

#[cfg(feature = "test-utils")]
pub mod test_utils;

pub use error::MetroError;
