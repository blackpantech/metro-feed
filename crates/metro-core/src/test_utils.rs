//! Test utilities for Metro.
//!
//! Only available when the `test-utils` feature is enabled.
//!
//! - **Temp databases**: [`temp_db_path`] / [`cleanup_temp_db`].
//! - **Shared feed fixtures**: [`fixtures_dir`], [`fixture_path`], and
//!   [`load_fixture`] read the RSS / Atom / JSON Feed documents kept in the
//!   repository-level `test-fixtures/` directory, so parser tests run without
//!   network access. The directory is located from this crate's
//!   `CARGO_MANIFEST_DIR` at compile time, which makes it independent of the
//!   working directory `cargo test` runs from (it always runs from the source
//!   tree, so the path is valid whenever these helpers are compiled).
//! - **SQL fixtures**: [`fixture_loader`] loads every `*.sql` file of a
//!   directory.

use std::env;
use std::io;
use std::path::{Path, PathBuf};

/// Name of the repository-level directory holding shared test fixtures.
const FIXTURES_DIR_NAME: &str = "test-fixtures";

/// Create a temporary database path for testing.
///
/// The database file is created in the system temp directory
/// with a unique name. Callers should delete the file after the test.
pub fn temp_db_path() -> PathBuf {
    let id = uuid_simple();
    env::temp_dir().join(format!("metro_test_{}.db", id))
}

/// Generate a simple unique ID for temp files.
fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{}_{}", duration.as_secs(), duration.subsec_nanos())
}

/// Clean up a temporary database file.
pub fn cleanup_temp_db(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
}

/// Absolute path of the repository-level `test-fixtures/` directory.
///
/// `metro-core` lives at `<repo>/crates/metro-core`, so the directory is two
/// levels above this crate's manifest.
#[must_use]
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(FIXTURES_DIR_NAME)
}

/// Absolute path of one shared fixture, e.g. `fixture_path("rss2.xml")`.
#[must_use]
pub fn fixture_path(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

/// Read one shared fixture as raw bytes (what [`crate::fetch::parse_feed`]
/// takes), e.g. `load_fixture("atom.xml")`.
///
/// # Errors
///
/// Returns the I/O error of the failed read, with the fixture path added to
/// its message so a missing or misnamed fixture is easy to diagnose.
pub fn load_fixture(name: &str) -> io::Result<Vec<u8>> {
    let path = fixture_path(name);
    std::fs::read(&path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to read fixture {}: {error}", path.display()),
        )
    })
}

/// Load fixtures from a directory.
///
/// Returns a map of fixture name to SQL content.
pub fn fixture_loader(dir: &str) -> std::collections::HashMap<String, String> {
    let mut fixtures = std::collections::HashMap::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "sql")
                && let Ok(content) = std::fs::read_to_string(&path)
            {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                fixtures.insert(name, content);
            }
        }
    }

    fixtures
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{migrations, open};
    use migrations::run_migrations;

    #[test]
    fn test_temp_db_factory() {
        let path = temp_db_path();
        assert!(path.starts_with(env::temp_dir()));
        assert!(path.to_string_lossy().contains("metro_test_"));

        // Clean up
        cleanup_temp_db(&path);
    }

    #[test]
    fn test_temp_db_with_migrations() {
        let path = temp_db_path();

        // Open and migrate
        let mut conn = open(&path).unwrap();
        run_migrations(&mut conn).unwrap();

        // Verify migrated
        let version = migrations::get_version(&conn).unwrap();
        assert_eq!(version, 1);

        // Clean up
        cleanup_temp_db(&path);
    }

    #[test]
    fn test_fixtures_dir_exists() {
        let dir = fixtures_dir();
        assert!(dir.is_dir(), "missing fixtures dir {}", dir.display());
    }

    #[test]
    fn test_load_fixture_reads_bytes() {
        let bytes = load_fixture("rss2.xml").unwrap();
        assert!(bytes.starts_with(b"<?xml"));
    }

    #[test]
    fn test_load_missing_fixture_reports_path() {
        let error = load_fixture("does-not-exist.xml").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("does-not-exist.xml"));
    }
}
