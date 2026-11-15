//! Metro core library.
//!
//! Provides error types, configuration loading, database connectivity, and data models.

pub mod config;
pub mod db;
pub mod error;
pub mod models;

#[cfg(feature = "test-utils")]
pub mod test_utils;

pub use error::MetroError;
