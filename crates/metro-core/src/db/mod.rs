//! Database connectivity for Metro.

use crate::MetroError;
use rusqlite::Connection;
use std::path::Path;

pub mod migrations;

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
