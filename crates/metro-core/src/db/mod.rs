//! Database connectivity for Metro.
//!
//! - [`open`] opens a connection with the mandatory pragmas (WAL, busy
//!   timeout, foreign keys) but does not migrate it.
//! - [`open_migrated`] is the frontend entry point: it creates the parent
//!   directory if needed, opens the database and applies pending migrations,
//!   so callers get a connection every core API can use.
//! - [`resolve_path`] / [`default_path`] decide *which* file to open: an
//!   explicit override (the CLI's `--db <path>` global flag) wins, otherwise
//!   `metro.db` in the platform data directory ([`Config::data_dir`]).

use crate::MetroError;
use crate::config::Config;
use crate::db::migrations::run_migrations;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub mod migrations;

/// File name of the database inside the data directory.
pub const DB_FILE_NAME: &str = "metro.db";

/// Default database path: `<data dir>/metro.db`.
///
/// Returns `None` when the platform home directory cannot be determined.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    Config::data_dir().map(|dir| dir.join(DB_FILE_NAME))
}

/// Resolve the database path: the explicit override when given, otherwise
/// [`default_path`].
///
/// # Errors
///
/// - [`MetroError::Generic`] when no override is given and the platform
///   data directory cannot be determined.
pub fn resolve_path(override_path: Option<&Path>) -> Result<PathBuf, MetroError> {
    override_path
        .map(Path::to_path_buf)
        .or_else(default_path)
        .ok_or_else(|| {
            MetroError::Generic(
                "cannot determine the data directory; pass --db <path> explicitly".to_string(),
            )
        })
}

/// Open the database at `path`, creating its parent directory if needed,
/// and apply all pending migrations.
///
/// This is what frontends call before using any core API, which all assume
/// a migrated connection.
///
/// # Errors
///
/// - [`MetroError::Generic`] if the parent directory cannot be created, the
///   database cannot be opened or configured, or a migration fails.
pub fn open_migrated(path: &Path) -> Result<Connection, MetroError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| {
            MetroError::Generic(format!(
                "failed to create database directory {}: {e}",
                parent.display()
            ))
        })?;
    }
    let mut conn = open(path)?;
    run_migrations(&mut conn)?;
    Ok(conn)
}

/// Open a database connection with recommended settings:
/// - WAL mode
/// - Busy timeout (5 seconds)
/// - Foreign keys enabled
///
/// # Errors
///
/// Returns an error if the database cannot be opened or configured.
pub fn open<P: AsRef<Path>>(path: P) -> Result<Connection, MetroError> {
    let conn = Connection::open(path)?;

    // Enable WAL mode
    conn.execute_batch("PRAGMA journal_mode = WAL")?;

    // Set busy timeout (5 seconds = 5000ms)
    conn.execute_batch("PRAGMA busy_timeout = 5000")?;

    // Enable foreign keys
    conn.execute_batch("PRAGMA foreign_keys = ON")?;

    Ok(conn)
}

/// Open an in-memory database connection for testing.
#[cfg(feature = "test-utils")]
pub fn open_in_memory() -> Result<Connection, MetroError> {
    let conn = Connection::open(":memory:")?;

    // Enable WAL mode (will be ignored for :memory:, but consistent)
    conn.execute_batch("PRAGMA journal_mode = WAL")?;

    // Set busy timeout
    conn.execute_batch("PRAGMA busy_timeout = 5000")?;

    // Enable foreign keys
    conn.execute_batch("PRAGMA foreign_keys = ON")?;

    Ok(conn)
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_wal_mode() {
        let temp_path = env::temp_dir().join("metro_test_wal.db");
        let _ = std::fs::remove_file(&temp_path); // Clean up if exists

        let conn = open(&temp_path).unwrap();

        let result: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();

        assert_eq!(result, "wal");

        let _ = std::fs::remove_file(&temp_path);
    }

    #[test]
    fn test_resolve_path_prefers_override() {
        let explicit = Path::new("custom.db");
        assert_eq!(
            resolve_path(Some(explicit)).unwrap(),
            PathBuf::from("custom.db")
        );
    }

    #[test]
    fn test_default_path_file_name() {
        let path = default_path().unwrap();
        assert!(path.ends_with(DB_FILE_NAME));
    }

    #[test]
    fn test_open_migrated_creates_parent_and_migrates() {
        let dir = crate::test_utils::temp_db_path().with_extension("dir");
        let path = dir.join("nested").join(DB_FILE_NAME);

        let conn = open_migrated(&path).unwrap();
        assert_eq!(migrations::get_version(&conn).unwrap(), 1);
        drop(conn);

        // Re-opening an already-migrated database is a no-op migration.
        let conn = open_migrated(&path).unwrap();
        assert_eq!(migrations::get_version(&conn).unwrap(), 1);
        drop(conn);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_foreign_keys_enabled() {
        let temp_path = env::temp_dir().join("metro_test_fk.db");
        let _ = std::fs::remove_file(&temp_path);

        let conn = open(&temp_path).unwrap();

        // Create tables with FK
        conn.execute("CREATE TABLE parent (id INTEGER PRIMARY KEY)", [])
            .unwrap();
        conn.execute(
            "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, FOREIGN KEY(parent_id) REFERENCES parent(id))",
            [],
        ).unwrap();

        // Try to insert orphan child (should fail)
        let result = conn.execute("INSERT INTO child (parent_id) VALUES (999)", []);
        assert!(result.is_err());

        let _ = std::fs::remove_file(&temp_path);
    }
}
