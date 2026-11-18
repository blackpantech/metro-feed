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

    /// `entries` dedup keys are unique per feed (design D2): a duplicate
    /// guid within one feed is rejected, the same guid in another feed is
    /// accepted.
    #[test]
    fn test_migration_unique_guid_per_feed_constraint() {
        let mut conn = open_in_memory().unwrap();

        run_migrations(&mut conn).unwrap();

        // Insert a folder and two feeds
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
        conn.execute(
            "INSERT INTO feeds (id, url, title, site_url, kind, folder_id) VALUES (2, 'https://other.example.com/feed', 'Other', 'https://other.example.com', 'rss', 1)",
            [],
        )
        .unwrap();

        // Insert an entry
        conn.execute(
            "INSERT INTO entries (feed_id, guid, url, title, is_read) VALUES (1, 'unique-guid', 'https://example.com/1', 'Entry 1', 0)",
            [],
        )
        .unwrap();

        // Try to insert duplicate guid in the same feed (should fail)
        let result = conn.execute(
            "INSERT INTO entries (feed_id, guid, url, title, is_read) VALUES (1, 'unique-guid', 'https://example.com/2', 'Entry 2', 0)",
            [],
        );
        assert!(
            result.is_err(),
            "UNIQUE constraint should prevent duplicate guid within a feed"
        );

        // The same guid in another feed is a distinct entry (should succeed)
        let result = conn.execute(
            "INSERT INTO entries (feed_id, guid, url, title, is_read) VALUES (2, 'unique-guid', 'https://example.com/1', 'Entry 1', 0)",
            [],
        );
        assert!(
            result.is_ok(),
            "the same guid in another feed should be accepted: {result:?}"
        );
    }

    /// The `entries` table carries a unique index on exactly
    /// `(feed_id, guid)`, and no unique index on `guid` alone.
    #[test]
    fn test_migration_entries_unique_index_is_feed_scoped() {
        let mut conn = open_in_memory().unwrap();

        run_migrations(&mut conn).unwrap();

        let unique_indexes: Vec<String> = conn
            .prepare("SELECT name FROM pragma_index_list('entries') WHERE \"unique\" = 1")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        let column_sets: Vec<Vec<String>> = unique_indexes
            .iter()
            .map(|index| {
                conn.prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")
                    .unwrap()
                    .query_map([index], |row| row.get(0))
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
            })
            .collect();

        assert_eq!(
            column_sets,
            vec![vec!["feed_id".to_string(), "guid".to_string()]]
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
        assert!(indexes.contains(&"idx_entries_feed_url".to_string()));
        assert!(indexes.contains(&"idx_entries_published".to_string()));
        assert!(indexes.contains(&"idx_entries_is_read".to_string()));
        assert!(indexes.contains(&"idx_feeds_next_retry".to_string()));
    }

    /// `feeds` carries the backoff state of the `backoff` spec (design D6):
    /// `consecutive_failures INTEGER DEFAULT 0` and a nullable
    /// `next_retry INTEGER` (Unix seconds).
    #[test]
    fn test_migration_feeds_backoff_columns() {
        let mut conn = open_in_memory().unwrap();

        run_migrations(&mut conn).unwrap();

        // (name, declared type, NOT NULL flag, default expression)
        let columns: Vec<(String, String, bool, Option<String>)> = conn
            .prepare("SELECT name, type, \"notnull\", dflt_value FROM pragma_table_info('feeds')")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let column = |name: &str| {
            columns
                .iter()
                .find(|column| column.0 == name)
                .unwrap_or_else(|| panic!("feeds.{name} is missing"))
        };

        let failures = column("consecutive_failures");
        assert_eq!(failures.1, "INTEGER");
        assert!(!failures.2);
        assert_eq!(failures.3.as_deref(), Some("0"));

        let next_retry = column("next_retry");
        assert_eq!(next_retry.1, "INTEGER");
        assert!(!next_retry.2);
        assert_eq!(next_retry.3, None);
    }

    /// A feed inserted without backoff state starts healthy: zero failures
    /// and no retry deadline.
    #[test]
    fn test_migration_feeds_backoff_defaults() {
        let mut conn = open_in_memory().unwrap();

        run_migrations(&mut conn).unwrap();

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

        let (failures, next_retry): (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT consecutive_failures, next_retry FROM feeds WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(failures, Some(0));
        assert_eq!(next_retry, None);
    }
}
