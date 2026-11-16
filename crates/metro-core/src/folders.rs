//! Folder CRUD API for Metro.
//!
//! This module implements the `folder-management` capability of the M1
//! milestone on top of the v001 database schema (`folders` and `feeds`
//! tables).
//!
//! # Semantics
//!
//! - **Flat namespace**: folders have no hierarchy. Feed folders and
//!   saved-item folders live in separate namespaces distinguished by
//!   [`FolderKind`], so a `Feeds` folder and a `Saved` folder may share the
//!   same name.
//! - **Case-insensitive names**: two folders of the same kind whose names
//!   differ only in ASCII casing are the same folder. The display casing of
//!   the first creation is preserved in the database; a later creation with
//!   different casing resolves to the existing folder.
//! - **Idempotent creation**: [`create_folder`] returns the existing folder's
//!   ID when a folder with the same (kind, name) already exists instead of
//!   creating a duplicate. This is what makes [`ensure_default`] safe to call
//!   repeatedly.
//! - **Safe deletion**: [`delete_folder`] refuses to delete a folder that
//!   still contains feeds. The v001 schema cascades `feeds` → `entries`
//!   deletions, so this guard is what prevents a folder deletion from
//!   silently destroying subscriptions and their entries.
//!
//! # Case-insensitivity limitation
//!
//! Comparisons use SQLite's `lower()`, which lowercases ASCII characters
//! only. Names that differ solely by non-ASCII casing (for example `"Öl"`
//! vs `"öl"`) are treated as distinct. This is an accepted M1 limitation.
//!
//! # Concurrency
//!
//! Every mutating function runs inside an `IMMEDIATE` transaction: the write
//! lock is taken before the read, so each check-then-write sequence
//! (duplicate lookup → insert, conflict check → rename, emptiness check →
//! delete) is atomic with respect to other processes. Combined with the
//! 5-second busy timeout configured by [`crate::db::open`], concurrent
//! writers wait for each other instead of failing fast.
//!
//! # Usage notes for callers
//!
//! Callers must pass a migrated connection (see
//! [`crate::db::migrations::run_migrations`]); the functions in this module
//! assume the `folders` and `feeds` tables exist.

use std::str::FromStr;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::MetroError;
use crate::models::{Folder, FolderId, FolderKind};

/// Name of the default folder that receives feeds added without an explicit
/// folder.
///
/// The default folder is of kind [`FolderKind::Feeds`] and is created
/// idempotently by [`ensure_default`].
pub const DEFAULT_FOLDER_NAME: &str = "Unsorted";

/// Convert a non-negative SQLite row id into a [`FolderId`].
///
/// SQLite stores integers as signed 64-bit values while folder IDs are
/// unsigned. Row ids are always non-negative, so this conversion only fails
/// on a corrupted database.
fn folder_id_from_row_id(row_id: i64) -> Result<FolderId, MetroError> {
    u64::try_from(row_id)
        .map(FolderId::new)
        .map_err(|_| MetroError::Generic(format!("folder row id {row_id} is out of range")))
}

/// Build a [`Folder`] from its stored database columns.
///
/// Returns an error when the stored kind text is not a valid folder kind or
/// the stored row id is negative; both indicate a corrupted or unmigrated
/// database.
fn folder_from_columns(row_id: i64, name: String, kind_text: &str) -> Result<Folder, MetroError> {
    let kind = FolderKind::from_str(kind_text).map_err(|_| {
        MetroError::Generic(format!(
            "invalid folder kind {kind_text:?} stored in database"
        ))
    })?;
    let id = folder_id_from_row_id(row_id)?;
    Ok(Folder::new(id, name, kind))
}

/// Insert a new folder row inside an open transaction and return its ID.
///
/// The name must already be trimmed and validated non-empty by the caller.
/// The caller owns the transaction: on error it rolls back and the row is
/// never persisted; on success the caller commits.
fn insert_folder(
    tx: &Transaction<'_>,
    name: &str,
    kind: FolderKind,
) -> Result<FolderId, MetroError> {
    tx.execute(
        "INSERT INTO folders (name, kind) VALUES (?1, ?2)",
        params![name, kind.as_str()],
    )?;
    folder_id_from_row_id(tx.last_insert_rowid())
}

/// Create a folder, or return the existing folder's ID if it already exists.
///
/// The name is trimmed before use and must be non-empty. Names are compared
/// case-insensitively (ASCII only) within the given kind, so this function
/// is idempotent: creating `"Tech"` twice yields the same folder, and a
/// `Feeds` folder never collides with a `Saved` folder of the same name. The
/// display casing of the first creation is preserved for later listings.
///
/// # Errors
///
/// - [`MetroError::Usage`] if the trimmed name is empty.
/// - [`MetroError::Generic`] if the database lookup or insert fails.
pub fn create_folder(
    conn: &mut Connection,
    name: &str,
    kind: FolderKind,
) -> Result<FolderId, MetroError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(MetroError::Usage(
            "folder name must not be empty".to_string(),
        ));
    }

    // IMMEDIATE takes the write lock up front so the duplicate check and the
    // insert are atomic with respect to other processes.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM folders WHERE kind = ?1 AND lower(name) = lower(?2)",
            params![kind.as_str(), name],
            |row| row.get(0),
        )
        .optional()?;

    let folder_id =
        existing_id.map_or_else(|| insert_folder(&tx, name, kind), folder_id_from_row_id)?;

    tx.commit()?;
    Ok(folder_id)
}

/// List all folders of the given kind, sorted by name.
///
/// Sorting is alphabetical and case-insensitive (`ORDER BY lower(name)`),
/// with the stored display casing as a deterministic tie-breaker.
///
/// # Errors
///
/// - [`MetroError::Generic`] if the database query fails or a stored row is
///   invalid (unknown kind text or negative row id).
pub fn list_folders(conn: &Connection, kind: FolderKind) -> Result<Vec<Folder>, MetroError> {
    let mut stmt = conn
        .prepare("SELECT id, name, kind FROM folders WHERE kind = ?1 ORDER BY lower(name), name")?;
    let rows = stmt.query_map(params![kind.as_str()], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut folders = Vec::new();
    for row in rows {
        let (row_id, name, kind_text) = row?;
        folders.push(folder_from_columns(row_id, name, &kind_text)?);
    }
    Ok(folders)
}

/// Rename a folder.
///
/// The new name is trimmed before use and must be non-empty. Renaming to the
/// folder's own name (including a differently-cased variant of it) succeeds
/// as a no-op. Renaming fails when another folder of the same kind already
/// uses the new name (compared case-insensitively, ASCII only).
///
/// # Errors
///
/// - [`MetroError::Usage`] if the trimmed new name is empty.
/// - [`MetroError::NotFound`] if no folder exists with the given ID.
/// - [`MetroError::Generic`] if the new name conflicts with an existing
///   folder of the same kind, or if the database update fails.
pub fn rename_folder(
    conn: &mut Connection,
    id: FolderId,
    new_name: &str,
) -> Result<(), MetroError> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err(MetroError::Usage(
            "folder name must not be empty".to_string(),
        ));
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // The conflict check needs the folder's kind; fetch it first.
    let kind_text: Option<String> = tx
        .query_row(
            "SELECT kind FROM folders WHERE id = ?1",
            params![id.inner()],
            |row| row.get(0),
        )
        .optional()?;

    let kind_text =
        kind_text.ok_or_else(|| MetroError::NotFound(format!("folder {id} not found")))?;

    let conflicting_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM folders WHERE kind = ?1 AND lower(name) = lower(?2) AND id != ?3",
            params![kind_text, new_name, id.inner()],
            |row| row.get(0),
        )
        .optional()?;

    if conflicting_id.is_some() {
        return Err(MetroError::Generic(format!(
            "cannot rename folder {id} to {new_name:?}: a folder of the same kind already uses that name"
        )));
    }

    tx.execute(
        "UPDATE folders SET name = ?1 WHERE id = ?2",
        params![new_name, id.inner()],
    )?;
    tx.commit()?;
    Ok(())
}

/// Delete a folder.
///
/// Deletion is refused while the folder still contains feeds, so the
/// schema's `ON DELETE CASCADE` never silently removes subscriptions or
/// their entries. Once the saved-items feature lands (M3), saved folders
/// will gain an equivalent emptiness rule of their own.
///
/// # Errors
///
/// - [`MetroError::NotFound`] if no folder exists with the given ID.
/// - [`MetroError::Generic`] if the folder still contains feeds, or if the
///   database delete fails.
pub fn delete_folder(conn: &mut Connection, id: FolderId) -> Result<(), MetroError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM folders WHERE id = ?1",
            params![id.inner()],
            |row| row.get(0),
        )
        .optional()?;

    if existing_id.is_none() {
        return Err(MetroError::NotFound(format!("folder {id} not found")));
    }

    let feed_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM feeds WHERE folder_id = ?1",
        params![id.inner()],
        |row| row.get(0),
    )?;

    if feed_count > 0 {
        return Err(MetroError::Generic(format!(
            "cannot delete folder {id}: it still contains {feed_count} feed(s); move or remove them first"
        )));
    }

    tx.execute("DELETE FROM folders WHERE id = ?1", params![id.inner()])?;
    tx.commit()?;
    Ok(())
}

/// Ensure the default `Unsorted` folder exists, returning its ID.
///
/// Creates the default folder of kind [`FolderKind::Feeds`] on first use and
/// returns the existing folder's ID on every subsequent call. Callers that
/// add a feed without an explicit folder use this to obtain the target.
///
/// # Errors
///
/// Same as [`create_folder`].
pub fn ensure_default(conn: &mut Connection) -> Result<FolderId, MetroError> {
    create_folder(conn, DEFAULT_FOLDER_NAME, FolderKind::Feeds)
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;

    /// Open a migrated in-memory database for folder tests.
    fn test_conn() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    /// Insert a minimal feed row into the given folder using direct SQL.
    ///
    /// Folder deletion tests build the "non-empty folder" state directly so
    /// they stay focused on folder semantics instead of depending on the feed
    /// management API.
    fn insert_feed_row(conn: &Connection, folder_row_id: u64) {
        conn.execute(
            "INSERT INTO feeds (url, title, site_url, kind, folder_id) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                format!("https://example.com/feed/{folder_row_id}"),
                "Example Feed",
                "https://example.com",
                "rss",
                folder_row_id
            ],
        )
        .unwrap();
    }

    #[test]
    fn test_create_folder_returns_new_id() {
        let mut conn = test_conn();
        let id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        assert!(id.inner() > 0);

        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].id(), id);
        assert_eq!(folders[0].name(), "Tech");
    }

    #[test]
    fn test_create_folder_duplicate_returns_existing_id() {
        let mut conn = test_conn();
        let first = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        let second = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        assert_eq!(first, second);

        // No duplicate row was created.
        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 1);
    }

    #[test]
    fn test_create_folder_same_name_different_kinds_are_separate() {
        let mut conn = test_conn();
        let feeds_id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        let saved_id = create_folder(&mut conn, "Tech", FolderKind::Saved).unwrap();
        assert_ne!(feeds_id, saved_id);

        // Each namespace holds exactly one folder.
        assert_eq!(list_folders(&conn, FolderKind::Feeds).unwrap().len(), 1);
        assert_eq!(list_folders(&conn, FolderKind::Saved).unwrap().len(), 1);
    }

    #[test]
    fn test_create_folder_case_insensitive_preserves_first_casing() {
        let mut conn = test_conn();
        let first = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        let second = create_folder(&mut conn, "TECH", FolderKind::Feeds).unwrap();
        assert_eq!(first, second);

        // The display casing of the first creation is preserved.
        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name(), "Tech");
    }

    #[test]
    fn test_create_folder_trims_surrounding_whitespace() {
        let mut conn = test_conn();
        let id = create_folder(&mut conn, "  Tech  ", FolderKind::Feeds).unwrap();

        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name(), "Tech");
        assert_eq!(folders[0].id(), id);
    }

    #[test]
    fn test_create_folder_empty_name_rejected() {
        let mut conn = test_conn();
        assert!(matches!(
            create_folder(&mut conn, "", FolderKind::Feeds),
            Err(MetroError::Usage(_))
        ));
        assert!(matches!(
            create_folder(&mut conn, "   ", FolderKind::Feeds),
            Err(MetroError::Usage(_))
        ));
    }

    #[test]
    fn test_list_folders_sorted_case_insensitive() {
        let mut conn = test_conn();
        create_folder(&mut conn, "banana", FolderKind::Feeds).unwrap();
        create_folder(&mut conn, "Apple", FolderKind::Feeds).unwrap();
        create_folder(&mut conn, "Cherry", FolderKind::Feeds).unwrap();

        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        let names: Vec<&str> = folders.iter().map(Folder::name).collect();
        assert_eq!(names, vec!["Apple", "banana", "Cherry"]);
    }

    #[test]
    fn test_list_folders_empty() {
        let conn = test_conn();
        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 0);
    }

    #[test]
    fn test_list_folders_filters_by_kind() {
        let mut conn = test_conn();
        create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        create_folder(&mut conn, "Tech", FolderKind::Saved).unwrap();

        let feeds = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].kind(), FolderKind::Feeds);

        let saved = list_folders(&conn, FolderKind::Saved).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].kind(), FolderKind::Saved);
    }

    #[test]
    fn test_rename_folder_succeeds() {
        let mut conn = test_conn();
        let id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();

        rename_folder(&mut conn, id, "Technology").unwrap();

        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name(), "Technology");
        assert_eq!(folders[0].id(), id);
    }

    #[test]
    fn test_rename_folder_to_duplicate_name_fails() {
        let mut conn = test_conn();
        let tech_id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        create_folder(&mut conn, "News", FolderKind::Feeds).unwrap();

        // Renaming "Tech" to the existing "News" conflicts...
        assert!(matches!(
            rename_folder(&mut conn, tech_id, "News"),
            Err(MetroError::Generic(_))
        ));
        // ...including with different casing (case-insensitive names).
        assert!(matches!(
            rename_folder(&mut conn, tech_id, "NEWS"),
            Err(MetroError::Generic(_))
        ));

        // Both folders are untouched.
        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 2);
    }

    #[test]
    fn test_rename_folder_to_same_kind_name_in_other_namespace_ok() {
        // A Saved folder named "News" does not conflict with renaming a
        // Feeds folder to "News": separate namespaces.
        let mut conn = test_conn();
        let feeds_id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        create_folder(&mut conn, "News", FolderKind::Saved).unwrap();

        rename_folder(&mut conn, feeds_id, "News").unwrap();

        let feeds = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].name(), "News");
    }

    #[test]
    fn test_rename_folder_to_own_name_is_noop() {
        let mut conn = test_conn();
        let id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();

        rename_folder(&mut conn, id, "tech").unwrap();

        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].id(), id);
    }

    #[test]
    fn test_rename_folder_nonexistent_not_found() {
        let mut conn = test_conn();
        assert!(matches!(
            rename_folder(&mut conn, FolderId::new(999), "Whatever"),
            Err(MetroError::NotFound(_))
        ));
    }

    #[test]
    fn test_rename_folder_empty_name_rejected() {
        let mut conn = test_conn();
        let id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        assert!(matches!(
            rename_folder(&mut conn, id, "   "),
            Err(MetroError::Usage(_))
        ));
    }

    #[test]
    fn test_delete_folder_empty_succeeds() {
        let mut conn = test_conn();
        let id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();

        delete_folder(&mut conn, id).unwrap();

        assert_eq!(list_folders(&conn, FolderKind::Feeds).unwrap().len(), 0);
    }

    #[test]
    fn test_delete_folder_non_empty_fails() {
        let mut conn = test_conn();
        let id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        insert_feed_row(&conn, id.inner());

        let result = delete_folder(&mut conn, id);
        assert!(matches!(result, Err(MetroError::Generic(ref msg)) if msg.contains("feed")));

        // The folder and its feed are still there.
        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 1);
        let feed_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM feeds", [], |row| row.get(0))
            .unwrap();
        assert_eq!(feed_count, 1);
    }

    #[test]
    fn test_delete_folder_nonexistent_not_found() {
        let mut conn = test_conn();
        assert!(matches!(
            delete_folder(&mut conn, FolderId::new(999)),
            Err(MetroError::NotFound(_))
        ));
    }

    #[test]
    fn test_ensure_default_creates_unsorted_feeds_folder() {
        let mut conn = test_conn();
        let id = ensure_default(&mut conn).unwrap();
        assert!(id.inner() > 0);

        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name(), DEFAULT_FOLDER_NAME);
        assert_eq!(folders[0].kind(), FolderKind::Feeds);
        assert_eq!(folders[0].id(), id);
    }

    #[test]
    fn test_ensure_default_is_idempotent() {
        let mut conn = test_conn();
        let first = ensure_default(&mut conn).unwrap();
        let second = ensure_default(&mut conn).unwrap();
        assert_eq!(first, second);

        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 1);
    }
}
