//! Metro core library.
//!
//! Provides error types, configuration loading, database connectivity, data
//! models, and the folder management API.

pub mod config;
pub mod db;
pub mod error;
pub mod folders;
pub mod models;

#[cfg(feature = "test-utils")]
pub mod test_utils;

pub use error::MetroError;
