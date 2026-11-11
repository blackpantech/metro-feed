//! Metro core library.
//!
//! Provides error types, configuration loading, and database connectivity.

pub mod config;
pub mod db;
pub mod error;

#[cfg(feature = "test-utils")]
pub mod test_utils;

pub use error::MetroError;
