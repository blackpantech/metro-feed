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
}
