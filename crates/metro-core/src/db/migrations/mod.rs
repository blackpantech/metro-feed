//! Database migrations for Metro.

use crate::MetroError;
use rusqlite::Connection;

/// Get the current schema version from `user_version` pragma.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn get_version(conn: &Connection) -> Result<i32, MetroError> {
    let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(version)
}

/// Run all pending migrations.
///
/// Migrations are executed in order within a transaction.
/// If any migration fails, the transaction is rolled back.
///
/// # Errors
///
/// Returns an error if any migration fails or the database cannot be updated.
pub fn run_migrations(conn: &mut Connection) -> Result<(), MetroError> {
    let current_version = get_version(conn)?;

    // Migrations list: (version, sql)
    let migrations: Vec<(i32, &str)> = vec![(1, include_str!("v001__init.sql"))];

    for (version, sql) in migrations {
        if version <= current_version {
            continue; // Already applied
        }

        // Run migration in a transaction
        let tx = conn.transaction()?;
        let result = tx.execute_batch(sql);

        if let Err(e) = result {
            // Rollback on failure
            tx.rollback()?;
            return Err(MetroError::Generic(format!(
                "Migration v{version:03} failed: {e}"
            )));
        }

        // Update version and commit
        tx.execute_batch(&format!("PRAGMA user_version = {version}"))?;
        tx.commit()?;
    }

    Ok(())
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn test_fresh_db_version_zero() {
        let conn = open_in_memory().unwrap();
        let version = get_version(&conn).unwrap();
        assert_eq!(version, 0);
    }

    #[test]
    fn test_migration_advances_version() {
        let mut conn = open_in_memory().unwrap();

        run_migrations(&mut conn).unwrap();

        let version = get_version(&conn).unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn test_migration_idempotent() {
        let mut conn = open_in_memory().unwrap();

        // Run migrations twice
        run_migrations(&mut conn).unwrap();
        run_migrations(&mut conn).unwrap();

        // Should still be at version 1
        let version = get_version(&conn).unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn test_migration_creates_all_tables() {
        let mut conn = open_in_memory().unwrap();

        run_migrations(&mut conn).unwrap();

        // Verify all tables exist
        let tables: Vec<String> = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('folders', 'feeds', 'entries') ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(tables, vec!["entries", "feeds", "folders"]);
    }

    #[test]
    fn test_migration_fk_enforcement() {
        let mut conn = open_in_memory().unwrap();

        run_migrations(&mut conn).unwrap();

        // Insert a folder first
        conn.execute(
            "INSERT INTO folders (id, name, kind) VALUES (1, 'Test Folder', 'feeds')",
            [],
        )
        .unwrap();

        // Try to insert a feed with invalid folder_id (should fail due to FK)
        let result = conn.execute(
            "INSERT INTO feeds (id, url, title, site_url, kind, folder_id) VALUES (1, 'https://example.com/feed', 'Test', 'https://example.com', 'rss', 999)",
            [],
        );

        assert!(
            result.is_err(),
            "FK constraint should prevent inserting feed with non-existent folder_id"
        );

        // Insert feed with valid folder_id
        let result = conn.execute(
            "INSERT INTO feeds (id, url, title, site_url, kind, folder_id) VALUES (1, 'https://example.com/feed', 'Test', 'https://example.com', 'rss', 1)",
            [],
        );
        assert!(result.is_ok());

        // Try to insert an entry with invalid feed_id (should fail due to FK)
        let entry_result = conn.execute(
            "INSERT INTO entries (feed_id, guid, url, title, is_read) VALUES (999, 'guid1', 'https://example.com/1', 'Test Entry', 0)",
            [],
        );
        assert!(
            entry_result.is_err(),
            "FK constraint should prevent inserting entry with non-existent feed_id"
        );
    }

    #[test]
    fn test_migration_fk_cascade_delete() {
        let mut conn = open_in_memory().unwrap();

        run_migrations(&mut conn).unwrap();

        // Insert a folder
        conn.execute(
            "INSERT INTO folders (id, name, kind) VALUES (1, 'Test Folder', 'feeds')",
            [],
        )
        .unwrap();

        // Insert a feed in that folder
        conn.execute(
            "INSERT INTO feeds (id, url, title, site_url, kind, folder_id) VALUES (1, 'https://example.com/feed', 'Test', 'https://example.com', 'rss', 1)",
            [],
        )
        .unwrap();

        // Insert an entry for that feed
        conn.execute(
            "INSERT INTO entries (feed_id, guid, url, title, is_read) VALUES (1, 'guid1', 'https://example.com/1', 'Test Entry', 0)",
            [],
        )
        .unwrap();

        // Verify entry exists
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Delete the feed - entries should be cascade deleted
        conn.execute("DELETE FROM feeds WHERE id = 1", []).unwrap();

        // Verify entry is gone
        let count_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count_after, 0);
    }

    #[test]
    fn test_migration_unique_guid_constraint() {
        let mut conn = open_in_memory().unwrap();

        run_migrations(&mut conn).unwrap();

        // Insert a folder and feed
        conn.execute(
            "INSERT INTO folders (id, name, kind) VALUES (1, 'Test', 'feeds')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO feeds (id, url, title, site_url, kind, folder_id) VALUES (1, 'https://example.com/feed', 'Test', 'https://example.com', 'rss', 1)",
            [],
        )
        .unwrap();

        // Insert an entry
        conn.execute(
            "INSERT INTO entries (feed_id, guid, url, title, is_read) VALUES (1, 'unique-guid', 'https://example.com/1', 'Entry 1', 0)",
            [],
        )
        .unwrap();

        // Try to insert duplicate guid (should fail)
        let result = conn.execute(
            "INSERT INTO entries (feed_id, guid, url, title, is_read) VALUES (1, 'unique-guid', 'https://example.com/2', 'Entry 2', 0)",
            [],
        );

        assert!(
            result.is_err(),
            "UNIQUE constraint should prevent duplicate guid"
        );
    }

    #[test]
    fn test_migration_indexes_exist() {
        let mut conn = open_in_memory().unwrap();

        run_migrations(&mut conn).unwrap();

        // Verify indexes exist
        let indexes: Vec<String> = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%' ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(indexes.contains(&"idx_feeds_folder_id".to_string()));
        assert!(indexes.contains(&"idx_entries_feed_id".to_string()));
        assert!(indexes.contains(&"idx_entries_published".to_string()));
        assert!(indexes.contains(&"idx_entries_is_read".to_string()));
        assert!(indexes.contains(&"idx_feeds_next_retry".to_string()));
    }
}
