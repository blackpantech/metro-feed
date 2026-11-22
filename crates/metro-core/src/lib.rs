//! Metro core library.
//!
//! All product logic shared by the CLI and TUI frontends. Modules are grouped
//! by the question they answer, and dependencies point one way
//! (`feed_sync` → `http` / `parse` / `store` → `db` / `url` / `models`):
//!
//! - [`db`]: how we connect (open, pragmas, migrations, row decoders).
//! - [`store`]: what we persist and the rules on it (folders, feeds, entries,
//!   pruning).
//! - [`http`]: how we talk to the network (the fetch client, autodiscovery).
//! - [`parse`]: turning feed bytes into entries, with no I/O.
//! - [`url`]: feed URL normalization, with no I/O.
//! - [`feed_sync`]: getting fetched data into the database (refresh,
//!   subscribe, dedup, backoff).
//! - [`models`], [`error`], [`config`]: shared types, errors, configuration.

pub mod config;
pub mod db;
pub mod error;
pub mod feed_sync;
pub mod http;
pub mod models;
pub mod parse;
pub mod store;
pub mod url;

#[cfg(feature = "test-utils")]
pub mod test_utils;

pub use error::MetroError;
