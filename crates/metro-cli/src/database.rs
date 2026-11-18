//! Database access for CLI commands.
//!
//! Every command that touches data opens the database through [`open`], which
//! is a thin adapter over `metro-core`: path resolution (the global
//! `--db <path>` flag, else `metro.db` in the platform data directory) and
//! "open + create parent directory + migrate" both live in
//! `metro_core::db`, so the TUI opens exactly the same file the same way.
//!
//! The database is opened lazily by the commands that need it, never during
//! argument parsing, so `--help`, `--version` and usage errors never create
//! files.
//!
//! The default `Unsorted` folder is *not* created here: per the
//! `folder-management` spec it appears on first use (the first feed added
//! without `--folder`), which `metro_core::feeds` handles. Keeping `open`
//! free of writes also keeps read-only commands from taking the write lock.

use std::path::Path;

use metro_core::MetroError;
use metro_core::db::{open_migrated, resolve_path};
use rusqlite::Connection;

/// Open the migrated database, honoring the `--db` override.
///
/// # Errors
///
/// [`MetroError::Generic`] when the path cannot be resolved, the directory or
/// database cannot be created/opened, or a migration fails.
pub fn open(db_override: Option<&Path>) -> Result<Connection, MetroError> {
    let path = resolve_path(db_override)?;
    open_migrated(&path)
}
