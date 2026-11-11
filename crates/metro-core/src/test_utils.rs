//! Test utilities for Metro.
//!
//! Only available when the `test-utils` feature is enabled.

use std::env;
use std::path::PathBuf;

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

/// Load fixtures from a directory.
///
/// Returns a map of fixture name to SQL content.
#[expect(dead_code, reason = "Fixture loader for future tests")]
pub fn fixture_loader(dir: &str) -> std::collections::HashMap<String, String> {
    let mut fixtures = std::collections::HashMap::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "sql") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    fixtures.insert(name, content);
                }
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
}
