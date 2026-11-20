//! Feed subscription CRUD API for Metro.
//!
//! This module implements the `feed-management` capability of the M1
//! milestone on top of the v001 database schema (`feeds`, `folders`, and
//! `entries` tables).
//!
//! # Semantics
//!
//! - **Duplicate detection by normalized URL**: [`add_feed`] normalizes the
//!   URL with [`normalize_url`] before storing or comparing it. A second
//!   subscription with a URL that normalizes to the same value is reported as
//!   a duplicate by returning the existing feed's ID ([`AddFeedOutcome`])
//!   instead of creating a second row.
//! - **Folder assignment**: feeds always live in a folder of kind
//!   [`FolderKind::Feeds`]. When no folder is supplied, [`add_feed`] assigns
//!   the idempotent default `Unsorted` folder via
//!   [`crate::store::folders::ensure_default`]. Adding or moving a feed into a
//!   `Saved` folder is rejected because the two folder namespaces are
//!   separate.
//! - **Cascade deletion**: [`remove_feed`] deletes the feed row; the schema's
//!   `ON DELETE CASCADE` on `entries.feed_id` removes its entries in the same
//!   statement. Connections opened through [`crate::db::open`] enforce
//!   foreign keys, so the cascade is guaranteed.
//! - **Batch operations**: [`remove_feeds`] and [`move_feeds`] apply one
//!   operation to several feeds in a single transaction, all-or-nothing (an
//!   unknown ID changes nothing), ignoring repeated IDs, and return the
//!   affected feeds as [`FeedInfo`] so frontends can report them without a
//!   second, racy read. [`remove_feed`] and [`move_feed`] are their
//!   single-feed forms.
//! - **Provisional metadata**: the v001 schema requires `title`, `site_url`,
//!   and `kind` to be `NOT NULL`, but [`add_feed`] only receives a URL, an
//!   optional title, and an optional folder, and it never touches the
//!   network. Until the feed is first fetched (the parsed document is
//!   authoritative for kind and site URL), it stores:
//!   - `title`: the supplied title, or the URL's lowercased host,
//!   - `site_url`: the URL's origin (`scheme://host[:port]`),
//!   - `kind`: [`FeedKind::Rss`] as a placeholder,
//!   - `last_fetch`: `NULL` (never fetched successfully).
//!
//!   The first successful refresh then replaces the placeholders: `kind` and
//!  `site_url` are updated on every `200` response, and `title` is filled from the
//!  feed only while `last_fetch` is still `NULL`, so a title supplied here or set
//!  later through [`rename_feed`] is never overwritten afterwards.
//!
//!   [`add_feed`] is the network-free building block for offline and import
//!   paths. Interactive subscription (`metro feeds add`) goes through
//!   [`crate::feed_sync::subscribe_feed`] instead, which fetches the feed first
//!   and inserts the real metadata and initial entries in one transaction
//!   (users see the real kind and title at once, and dead URLs fail fast), so its rows never carry placeholders except the
//!   host title when the feed declares none.
//!
//! # Concurrency
//!
//! Every mutating function runs inside an `IMMEDIATE` transaction, so the
//! duplicate lookup and the insert, or the folder validation and the update,
//! are atomic with respect to other processes. Combined with the 5-second
//! busy timeout configured by [`crate::db::open`], concurrent `metro`
//! invocations wait for each other instead of failing fast.
//!
//! # Usage notes for callers
//!
//! Callers must pass a migrated connection (see
//! [`crate::db::migrations::run_migrations`]); the functions in this module
//! assume the `feeds`, `folders`, and `entries` tables exist.

use std::collections::HashSet;
use std::str::FromStr;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::MetroError;
use crate::db::rows::{feed_id_from_row_id, folder_id_from_row_id, timestamp_from_row};
use crate::models::{Feed, FeedId, FeedKind, FolderId, FolderKind};
use crate::store::folders::ensure_default;
use crate::url::{UrlOrigin, normalize_url, url_origin};

/// Result of [`add_feed`].
///
/// Reports both the feed ID and whether this call created the subscription.
/// A duplicate subscription is not an error: the existing feed's ID is
/// returned with `created == false`, which lets the CLI tell the user that the
/// feed was already subscribed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddFeedOutcome {
    /// The ID of the newly created or already existing feed.
    pub id: FeedId,
    /// `true` when this call inserted a new row, `false` for a duplicate.
    pub created: bool,
}

impl AddFeedOutcome {
    /// Returns `true` when [`add_feed`] created a new feed.
    #[must_use]
    pub fn is_new(&self) -> bool {
        self.created
    }
}

/// Detailed metadata for a single feed, as returned by [`get_feed_info`].
///
/// Unlike [`Feed`], this view joins the feed's folder name and aggregates the
/// number of stored entries, which is what `metro feeds info` displays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedInfo {
    /// Unique feed identifier.
    pub id: FeedId,
    /// Normalized feed URL.
    pub url: String,
    /// Human-readable feed title (may still be provisional until first fetch).
    pub title: String,
    /// The feed's home page URL.
    pub site_url: String,
    /// Feed format.
    pub kind: FeedKind,
    /// ID of the folder that contains the feed.
    pub folder_id: FolderId,
    /// Display name of the containing folder.
    pub folder_name: String,
    /// Unix timestamp of the last successful fetch, if any.
    pub last_fetch: Option<u64>,
    /// Error message from the last fetch, if it failed.
    pub error_state: Option<String>,
    /// Unix timestamp of the last fetch, if it failed (set with
    /// `error_state`, cleared by the next successful fetch).
    pub last_error_at: Option<u64>,
    /// Number of entries currently stored for the feed.
    pub entry_count: u64,
}

/// Parse a stored feed-kind string, reporting corruption with context.
fn feed_kind_from_text(kind_text: &str) -> Result<FeedKind, MetroError> {
    FeedKind::from_str(kind_text).map_err(|_| {
        MetroError::Generic(format!(
            "invalid feed kind {kind_text:?} stored in database"
        ))
    })
}

/// Raw feed columns as stored in SQLite, before conversion to [`Feed`].
struct StoredFeed {
    id: i64,
    url: String,
    title: String,
    site_url: String,
    kind: String,
    folder_id: i64,
    etag: Option<String>,
    last_modified: Option<String>,
    last_fetch: Option<i64>,
    error_state: Option<String>,
    last_error_at: Option<i64>,
}

impl StoredFeed {
    /// Read the eleven feed columns from a query row, in selector order.
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            url: row.get(1)?,
            title: row.get(2)?,
            site_url: row.get(3)?,
            kind: row.get(4)?,
            folder_id: row.get(5)?,
            etag: row.get(6)?,
            last_modified: row.get(7)?,
            last_fetch: row.get(8)?,
            error_state: row.get(9)?,
            last_error_at: row.get(10)?,
        })
    }

    /// Validate stored values and build a [`Feed`].
    fn into_feed(self) -> Result<Feed, MetroError> {
        Ok(Feed {
            id: feed_id_from_row_id(self.id)?,
            url: self.url,
            title: self.title,
            site_url: self.site_url,
            kind: feed_kind_from_text(&self.kind)?,
            folder_id: folder_id_from_row_id(self.folder_id)?,
            etag: self.etag,
            last_modified: self.last_modified,
            last_fetch: timestamp_from_row(self.last_fetch)?,
            error_state: self.error_state,
            last_error_at: timestamp_from_row(self.last_error_at)?,
        })
    }
}

/// Verify that a folder exists and is a feed folder.
///
/// Writers call it inside their transaction (a [`Transaction`] dereferences
/// to a [`Connection`]) so the check cannot race with a concurrent folder
/// deletion. Read-only callers, such as a subscribe dry run validating its
/// target folder before any network access, pass a plain connection.
///
/// # Errors
///
/// - [`MetroError::NotFound`] if no folder has the given ID.
/// - [`MetroError::Usage`] if the folder belongs to the saved-items
///   namespace.
/// - [`MetroError::Generic`] if the stored folder kind is invalid.
pub(crate) fn ensure_feed_folder(conn: &Connection, folder_id: FolderId) -> Result<(), MetroError> {
    let kind_text: Option<String> = conn
        .query_row(
            "SELECT kind FROM folders WHERE id = ?1",
            params![folder_id.inner()],
            |row| row.get(0),
        )
        .optional()?;

    let kind_text =
        kind_text.ok_or_else(|| MetroError::NotFound(format!("folder {folder_id} not found")))?;
    let kind = FolderKind::from_str(&kind_text).map_err(|_| {
        MetroError::Generic(format!(
            "invalid folder kind {kind_text:?} stored in database"
        ))
    })?;

    if kind != FolderKind::Feeds {
        return Err(MetroError::Usage(format!(
            "folder {folder_id} is a {kind_text} folder; feeds must belong to a feeds folder"
        )));
    }

    Ok(())
}

/// Placeholder metadata derived from a feed URL alone.
///
/// Used whenever a feed row must be written before (or without) the feed
/// declaring its own title and site URL. See the module documentation on
/// provisional metadata.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UrlPlaceholders {
    /// The URL's lowercased host, used as a fallback title.
    pub(crate) title: String,
    /// The URL's origin (`scheme://host[:port]`), used as a fallback site URL.
    pub(crate) site_url: String,
}

/// Derive placeholder title and site URL from a normalized feed URL.
///
/// # Errors
///
/// Returns [`MetroError::Usage`] if `normalized_url` is not an absolute
/// `http(s)` URL (it always is when produced by [`normalize_url`]).
pub(crate) fn url_placeholders(normalized_url: &str) -> Result<UrlPlaceholders, MetroError> {
    let UrlOrigin { host, origin } = url_origin(normalized_url)?;
    Ok(UrlPlaceholders {
        title: host,
        site_url: origin,
    })
}

/// Trim and validate an explicitly supplied feed title.
///
/// `None` stays `None` (the caller picks a fallback); a blank explicit title
/// is rejected so no blank display name is ever stored silently.
///
/// # Errors
///
/// Returns [`MetroError::Usage`] if `title` is `Some` but blank.
pub(crate) fn validate_explicit_title(title: Option<&str>) -> Result<Option<&str>, MetroError> {
    match title.map(str::trim) {
        Some("") => Err(MetroError::Usage(
            "feed title must not be empty".to_string(),
        )),
        other => Ok(other),
    }
}

/// Look up a subscription by its already-normalized URL.
///
/// # Errors
///
/// Returns [`MetroError::Generic`] if the query fails or the stored row id
/// is out of range.
pub(crate) fn find_feed_by_url(
    conn: &Connection,
    normalized_url: &str,
) -> Result<Option<FeedId>, MetroError> {
    let row_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM feeds WHERE url = ?1",
            params![normalized_url],
            |row| row.get(0),
        )
        .optional()?;
    row_id.map(feed_id_from_row_id).transpose()
}

/// Column values for a new feed row, see [`insert_feed`].
#[derive(Debug)]
pub(crate) struct NewFeed<'a> {
    /// URL already normalized with [`normalize_url`].
    pub(crate) url: &'a str,
    /// Final display title.
    pub(crate) title: &'a str,
    /// Site URL (declared by the feed, or the URL placeholder).
    pub(crate) site_url: &'a str,
    /// Feed format.
    pub(crate) kind: FeedKind,
    /// Target folder; validated to be an existing feed folder.
    pub(crate) folder_id: FolderId,
}

/// Insert a new feed row inside the caller's write transaction.
///
/// Validates the target folder with [`ensure_feed_folder`] first. The caller
/// is responsible for the duplicate check (in the same transaction) and for
/// committing. Fetch state (`etag`, `last_modified`, `last_fetch`,
/// `error_state`, `last_error_at`, backoff columns) starts out empty.
///
/// # Errors
///
/// - [`MetroError::NotFound`] / [`MetroError::Usage`] from
///   [`ensure_feed_folder`].
/// - [`MetroError::Generic`] if the insert fails (including a `UNIQUE`
///   violation when the caller skipped the duplicate check).
pub(crate) fn insert_feed(tx: &Transaction<'_>, feed: &NewFeed<'_>) -> Result<FeedId, MetroError> {
    ensure_feed_folder(tx, feed.folder_id)?;

    tx.execute(
        "INSERT INTO feeds (url, title, site_url, kind, folder_id) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            feed.url,
            feed.title,
            feed.site_url,
            feed.kind.as_str(),
            feed.folder_id.inner()
        ],
    )?;

    feed_id_from_row_id(tx.last_insert_rowid())
}

/// Add a feed subscription, or report the existing one when the URL is a
/// duplicate.
///
/// The URL is normalized with [`normalize_url`] before it is compared and
/// stored. A duplicate returns the existing feed's ID with
/// [`AddFeedOutcome::created`] set to `false`; it never modifies the existing
/// subscription.
///
/// When `folder_id` is `None` the feed is assigned to the idempotent
/// `Unsorted` feed folder. When a folder is supplied it must exist and be of
/// kind [`FolderKind::Feeds`].
///
/// `title` is trimmed. An explicitly empty title is rejected so the CLI does
/// not silently store a blank display name; callers that have no title pass
/// `None` and receive the URL host as provisional title.
///
/// # Errors
///
/// - [`MetroError::Usage`] for an invalid or empty URL, an explicitly empty
///   title, or a folder that is not a feed folder.
/// - [`MetroError::NotFound`] when a supplied folder does not exist.
/// - [`MetroError::Generic`] if the database operation fails.
pub fn add_feed(
    conn: &mut Connection,
    url: &str,
    title: Option<&str>,
    folder_id: Option<FolderId>,
) -> Result<AddFeedOutcome, MetroError> {
    let normalized = normalize_url(url)?;
    let placeholders = url_placeholders(&normalized)?;
    let explicit_title = validate_explicit_title(title)?;
    let title = explicit_title.unwrap_or(placeholders.title.as_str());

    // Creating the default folder runs in its own transaction (folder
    // creation is idempotent), so resolve it before taking the write lock for
    // the subscription.
    let target_folder = match folder_id {
        Some(folder_id) => folder_id,
        None => ensure_default(conn)?,
    };

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    if let Some(id) = find_feed_by_url(&tx, &normalized)? {
        // No rows were written; release the write lock explicitly.
        tx.rollback()?;
        return Ok(AddFeedOutcome { id, created: false });
    }

    let id = insert_feed(
        &tx,
        &NewFeed {
            url: &normalized,
            title,
            site_url: &placeholders.site_url,
            kind: FeedKind::Rss,
            folder_id: target_folder,
        },
    )?;
    tx.commit()?;

    Ok(AddFeedOutcome { id, created: true })
}

/// SQL selector shared by [`list_feeds`] queries.
const FEED_COLUMNS: &str = "f.id, f.url, f.title, f.site_url, f.kind, f.folder_id, \
                            f.etag, f.last_modified, f.last_fetch, f.error_state, \
                            f.last_error_at";

/// SQL sort order shared by [`list_feeds`] queries.
///
/// Feeds are grouped by folder (case-insensitive folder name) and sorted by
/// title, with the stored casing and row id as deterministic tie-breakers.
const FEED_ORDER: &str = "ORDER BY lower(fo.name), fo.name, lower(f.title), f.title, f.id";

/// List feeds, optionally restricted to one folder.
///
/// The result is sorted by folder name and then by feed title, which
/// preserves the spec's "grouped by folder" ordering while keeping the return
/// type a flat `Vec<Feed>` for the CLI to group. Passing `Some(folder_id)`
/// returns only that folder's feeds (sorted by title); an unknown folder
/// yields an empty list rather than an error, matching the other read APIs.
///
/// # Errors
///
/// - [`MetroError::Generic`] if the database query fails or a stored row is
///   invalid (unknown feed kind, negative id, or negative timestamp).
pub fn list_feeds(conn: &Connection, folder_id: Option<FolderId>) -> Result<Vec<Feed>, MetroError> {
    if let Some(folder_id) = folder_id {
        let sql = format!(
            "SELECT {FEED_COLUMNS} FROM feeds AS f \
             JOIN folders AS fo ON fo.id = f.folder_id \
             WHERE f.folder_id = ?1 {FEED_ORDER}"
        );
        let folder_row_id = folder_id.inner();
        query_feeds(conn, &sql, &[&folder_row_id])
    } else {
        let sql = format!(
            "SELECT {FEED_COLUMNS} FROM feeds AS f \
             JOIN folders AS fo ON fo.id = f.folder_id {FEED_ORDER}"
        );
        query_feeds(conn, &sql, &[])
    }
}

/// Run a feed selector and convert every row into a [`Feed`].
///
/// `sql` must select exactly the eleven [`FEED_COLUMNS`]. Keeping the execution
/// and row conversion in one helper keeps the filtered and unfiltered
/// [`list_feeds`] queries in sync.
fn query_feeds(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Feed>, MetroError> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map(params, StoredFeed::from_row)?;

    let mut feeds = Vec::new();
    for row in rows {
        feeds.push(row?.into_feed()?);
    }
    Ok(feeds)
}

/// Remove a feed and, by cascade, all of its entries.
///
/// Equivalent to [`remove_feeds`] with a single ID.
///
/// # Errors
///
/// - [`MetroError::NotFound`] if no feed exists with the given ID.
/// - [`MetroError::Generic`] if the database delete fails.
pub fn remove_feed(conn: &mut Connection, id: FeedId) -> Result<(), MetroError> {
    remove_feeds(conn, &[id]).map(|_| ())
}

/// Remove several feeds (and, by cascade, their entries) atomically.
///
/// All deletions run in one `IMMEDIATE` transaction: either every listed feed
/// is removed or, when any ID does not exist, none is (`metro feeds remove
/// f1 f2 f99` must not half-apply). An ID listed more than once is removed
/// once. An empty slice is a no-op.
///
/// Returns what was removed: one [`FeedInfo`] per distinct ID, in argument
/// order, read inside the transaction just before the deletion (so
/// `entry_count` is the number of entries deleted with the feed).
///
/// # Errors
///
/// - [`MetroError::NotFound`] for the first ID (in argument order) that does
///   not exist; nothing is deleted.
/// - [`MetroError::Generic`] if the database delete fails or a stored row is
///   invalid.
pub fn remove_feeds(conn: &mut Connection, ids: &[FeedId]) -> Result<Vec<FeedInfo>, MetroError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let mut removed = Vec::with_capacity(ids.len());
    for id in unique_ids(ids) {
        // `get_feed_info` reports a missing feed as `NotFound`; returning
        // early drops `tx`, which rolls back the deletions made so far.
        removed.push(get_feed_info(&tx, id)?);
        tx.execute("DELETE FROM feeds WHERE id = ?1", params![id.inner()])?;
    }

    tx.commit()?;
    Ok(removed)
}

/// Move a feed to another feed folder.
///
/// Equivalent to [`move_feeds`] with a single ID. The target folder must
/// exist and be of kind [`FolderKind::Feeds`]; moving a feed into the
/// saved-items namespace is rejected.
///
/// # Errors
///
/// - [`MetroError::NotFound`] if the feed or the target folder does not exist.
/// - [`MetroError::Usage`] if the target folder is not a feed folder.
/// - [`MetroError::Generic`] if the database update fails or a stored row is
///   invalid.
pub fn move_feed(conn: &mut Connection, id: FeedId, folder_id: FolderId) -> Result<(), MetroError> {
    move_feeds(conn, &[id], folder_id).map(|_| ())
}

/// Move several feeds to one feed folder atomically.
///
/// Every feed must exist and the target folder must exist and be of kind
/// [`FolderKind::Feeds`]. All updates run in one `IMMEDIATE` transaction, so
/// either every feed is moved or none is. Feeds are checked in argument order
/// before the folder, matching [`move_feed`]. An ID listed more than once is
/// moved once; moving a feed into the folder it already lives in succeeds.
///
/// Returns one [`FeedInfo`] per distinct ID, in argument order, read after
/// the move inside the same transaction.
///
/// # Errors
///
/// - [`MetroError::NotFound`] for the first missing feed, or a missing target
///   folder; nothing is moved.
/// - [`MetroError::Usage`] if the target folder is not a feed folder.
/// - [`MetroError::Generic`] if the database update fails or a stored row is
///   invalid.
pub fn move_feeds(
    conn: &mut Connection,
    ids: &[FeedId],
    folder_id: FolderId,
) -> Result<Vec<FeedInfo>, MetroError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let ids = unique_ids(ids);
    for id in &ids {
        let existing_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM feeds WHERE id = ?1",
                params![id.inner()],
                |row| row.get(0),
            )
            .optional()?;
        if existing_id.is_none() {
            return Err(MetroError::NotFound(format!("feed {id} not found")));
        }
    }

    ensure_feed_folder(&tx, folder_id)?;

    let mut moved = Vec::with_capacity(ids.len());
    for id in &ids {
        tx.execute(
            "UPDATE feeds SET folder_id = ?1 WHERE id = ?2",
            params![folder_id.inner(), id.inner()],
        )?;
        moved.push(get_feed_info(&tx, *id)?);
    }
    tx.commit()?;
    Ok(moved)
}

/// Return `ids` without repetitions, keeping the first occurrence's order.
///
/// Batch operations use it so a repeated argument (`f12 f12`) is not
/// reported as "not found" the second time round.
fn unique_ids(ids: &[FeedId]) -> Vec<FeedId> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

/// Rename a feed.
///
/// The title is trimmed and must not be empty. Renaming to the current title
/// succeeds as a no-op.
///
/// # Errors
///
/// - [`MetroError::Usage`] if the trimmed title is empty.
/// - [`MetroError::NotFound`] if no feed exists with the given ID.
/// - [`MetroError::Generic`] if the database update fails.
pub fn rename_feed(conn: &mut Connection, id: FeedId, title: &str) -> Result<(), MetroError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(MetroError::Usage(
            "feed title must not be empty".to_string(),
        ));
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM feeds WHERE id = ?1",
            params![id.inner()],
            |row| row.get(0),
        )
        .optional()?;
    if existing_id.is_none() {
        return Err(MetroError::NotFound(format!("feed {id} not found")));
    }

    tx.execute(
        "UPDATE feeds SET title = ?1 WHERE id = ?2",
        params![title, id.inner()],
    )?;
    tx.commit()?;
    Ok(())
}

/// Get detailed metadata for one feed.
///
/// Includes the containing folder's display name and the number of stored
/// entries, which `metro feeds info` reports.
///
/// # Errors
///
/// - [`MetroError::NotFound`] if no feed exists with the given ID.
/// - [`MetroError::Generic`] if the database query fails or a stored row is
///   invalid (unknown feed kind, negative id, or negative timestamp/count).
pub fn get_feed_info(conn: &Connection, id: FeedId) -> Result<FeedInfo, MetroError> {
    let row = conn
        .query_row(
            "SELECT f.id, f.url, f.title, f.site_url, f.kind, f.folder_id, fo.name, \
                    f.last_fetch, f.error_state, f.last_error_at, \
                    (SELECT COUNT(*) FROM entries AS e WHERE e.feed_id = f.id) \
             FROM feeds AS f JOIN folders AS fo ON fo.id = f.folder_id \
             WHERE f.id = ?1",
            params![id.inner()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            },
        )
        .optional()?;

    let Some((
        feed_row_id,
        url,
        title,
        site_url,
        kind_text,
        folder_row_id,
        folder_name,
        last_fetch,
        error_state,
        last_error_at,
        entry_count,
    )) = row
    else {
        return Err(MetroError::NotFound(format!("feed {id} not found")));
    };

    Ok(FeedInfo {
        id: feed_id_from_row_id(feed_row_id)?,
        url,
        title,
        site_url,
        kind: feed_kind_from_text(&kind_text)?,
        folder_id: folder_id_from_row_id(folder_row_id)?,
        folder_name,
        last_fetch: timestamp_from_row(last_fetch)?,
        error_state,
        last_error_at: timestamp_from_row(last_error_at)?,
        entry_count: u64::try_from(entry_count).map_err(|_| {
            MetroError::Generic(format!(
                "negative entry count {entry_count} stored in database"
            ))
        })?,
    })
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use crate::store::folders::{create_folder, list_folders};

    /// Open a migrated in-memory database for feed tests.
    fn test_conn() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    /// Insert an entry row for a feed using direct SQL.
    fn insert_entry_row(conn: &Connection, feed_id: FeedId, guid: &str) {
        conn.execute(
            "INSERT INTO entries (feed_id, guid, url, title) VALUES (?1, ?2, ?3, ?4)",
            params![
                feed_id.inner(),
                guid,
                format!("https://example.com/entries/{guid}"),
                format!("Entry {guid}")
            ],
        )
        .unwrap();
    }

    /// Count rows in a table, used to verify deletes.
    fn row_count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[test]
    fn test_add_feed_returns_new_id_and_default_folder() {
        let mut conn = test_conn();

        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();
        assert!(outcome.created);
        assert!(outcome.is_new());

        let feeds = list_feeds(&conn, None).unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].id(), outcome.id);
        assert_eq!(feeds[0].url(), "https://example.com/feed.xml");
        assert_eq!(feeds[0].title(), "example.com");
        assert_eq!(feeds[0].site_url(), "https://example.com");
        assert_eq!(feeds[0].kind(), FeedKind::Rss);

        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name(), "Unsorted");
        assert_eq!(feeds[0].folder_id(), folders[0].id());
    }

    #[test]
    fn test_add_feed_with_title_and_explicit_folder() {
        let mut conn = test_conn();
        let folder_id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();

        let outcome = add_feed(
            &mut conn,
            "https://example.com/feed.xml",
            Some("  My Blog  "),
            Some(folder_id),
        )
        .unwrap();

        let feeds = list_feeds(&conn, Some(folder_id)).unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].id(), outcome.id);
        assert_eq!(feeds[0].title(), "My Blog");
        assert_eq!(feeds[0].folder_id(), folder_id);
    }

    #[test]
    fn test_add_feed_rejects_explicitly_empty_title() {
        let mut conn = test_conn();

        assert!(matches!(
            add_feed(&mut conn, "https://example.com/feed.xml", Some("   "), None),
            Err(MetroError::Usage(_))
        ));
        assert_eq!(row_count(&conn, "feeds"), 0);
    }

    #[test]
    fn test_add_feed_duplicate_returns_existing_id() {
        let mut conn = test_conn();

        let first = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();
        let second = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();

        assert_eq!(first.id, second.id);
        assert!(!second.created);
        assert!(!second.is_new());
        assert_eq!(row_count(&conn, "feeds"), 1);
    }

    #[test]
    fn test_add_feed_duplicate_detection_normalizes_url() {
        let mut conn = test_conn();

        let first = add_feed(
            &mut conn,
            "https://Example.com/feed.xml?utm_source=rss#top",
            None,
            None,
        )
        .unwrap();
        let second = add_feed(&mut conn, "https://example.com/feed.xml/", None, None).unwrap();

        assert_eq!(first.id, second.id);
        assert!(!second.created);
        assert_eq!(row_count(&conn, "feeds"), 1);
        assert_eq!(
            list_feeds(&conn, None).unwrap()[0].url(),
            "https://example.com/feed.xml"
        );
    }

    #[test]
    fn test_add_feed_to_nonexistent_folder_not_found() {
        let mut conn = test_conn();

        assert!(matches!(
            add_feed(
                &mut conn,
                "https://example.com/feed.xml",
                None,
                Some(FolderId::new(999))
            ),
            Err(MetroError::NotFound(_))
        ));
        assert_eq!(row_count(&conn, "feeds"), 0);
    }

    #[test]
    fn test_add_feed_to_saved_folder_rejected() {
        let mut conn = test_conn();
        let saved_id = create_folder(&mut conn, "Saved", FolderKind::Saved).unwrap();

        assert!(matches!(
            add_feed(
                &mut conn,
                "https://example.com/feed.xml",
                None,
                Some(saved_id)
            ),
            Err(MetroError::Usage(_))
        ));
        assert_eq!(row_count(&conn, "feeds"), 0);
    }

    #[test]
    fn test_add_feed_invalid_url_usage_error() {
        let mut conn = test_conn();

        assert!(matches!(
            add_feed(&mut conn, "not a url", None, None),
            Err(MetroError::Usage(_))
        ));
        assert_eq!(row_count(&conn, "feeds"), 0);
    }

    #[test]
    fn test_list_feeds_empty() {
        let conn = test_conn();
        assert_eq!(list_feeds(&conn, None).unwrap(), []);
    }

    #[test]
    fn test_list_feeds_sorted_by_folder_then_title() {
        let mut conn = test_conn();
        let zeta_id = create_folder(&mut conn, "Zeta", FolderKind::Feeds).unwrap();
        let alpha_id = create_folder(&mut conn, "Alpha", FolderKind::Feeds).unwrap();

        add_feed(
            &mut conn,
            "https://example.com/zulu.xml",
            Some("Zulu"),
            Some(zeta_id),
        )
        .unwrap();
        add_feed(
            &mut conn,
            "https://example.com/alpha.xml",
            Some("alpha"),
            Some(alpha_id),
        )
        .unwrap();
        add_feed(
            &mut conn,
            "https://example.com/zulu2.xml",
            Some("Alpha"),
            Some(alpha_id),
        )
        .unwrap();

        let feeds = list_feeds(&conn, None).unwrap();
        let titles: Vec<&str> = feeds.iter().map(Feed::title).collect();
        assert_eq!(titles, vec!["Alpha", "alpha", "Zulu"]);
        assert_eq!(feeds[0].folder_id(), alpha_id);
        assert_eq!(feeds[2].folder_id(), zeta_id);
    }

    #[test]
    fn test_list_feeds_filters_by_folder() {
        let mut conn = test_conn();
        let first = create_folder(&mut conn, "First", FolderKind::Feeds).unwrap();
        let second = create_folder(&mut conn, "Second", FolderKind::Feeds).unwrap();

        add_feed(&mut conn, "https://example.com/one.xml", None, Some(first)).unwrap();
        add_feed(&mut conn, "https://example.com/two.xml", None, Some(second)).unwrap();

        let feeds = list_feeds(&conn, Some(first)).unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].folder_id(), first);

        // Unknown folder filters yield an empty list.
        assert_eq!(list_feeds(&conn, Some(FolderId::new(999))).unwrap(), []);
    }

    #[test]
    fn test_list_feeds_maps_optional_columns() {
        let mut conn = test_conn();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();
        conn.execute(
            "UPDATE feeds SET etag = ?1, last_modified = ?2, last_fetch = ?3, error_state = ?4, \
                 last_error_at = ?5 \
             WHERE id = ?6",
            params![
                "\"abc\"",
                "Wed, 21 Oct 2015 07:28:00 GMT",
                1_445_414_880_i64,
                "timeout",
                1_445_418_480_i64,
                outcome.id.inner()
            ],
        )
        .unwrap();

        let feeds = list_feeds(&conn, None).unwrap();
        let feed = &feeds[0];
        assert_eq!(feed.etag(), Some("\"abc\""));
        assert_eq!(feed.last_modified(), Some("Wed, 21 Oct 2015 07:28:00 GMT"));
        assert_eq!(feed.last_fetch(), Some(1_445_414_880));
        assert_eq!(feed.error_state(), Some("timeout"));
        assert_eq!(feed.last_error_at(), Some(1_445_418_480));
    }

    #[test]
    fn test_remove_feed_deletes_feed_and_entries() {
        let mut conn = test_conn();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();
        insert_entry_row(&conn, outcome.id, "guid-1");
        insert_entry_row(&conn, outcome.id, "guid-2");

        remove_feed(&mut conn, outcome.id).unwrap();

        assert_eq!(row_count(&conn, "feeds"), 0);
        assert_eq!(row_count(&conn, "entries"), 0);
    }

    #[test]
    fn test_remove_feed_nonexistent_not_found() {
        let mut conn = test_conn();
        assert!(matches!(
            remove_feed(&mut conn, FeedId::new(999)),
            Err(MetroError::NotFound(_))
        ));
    }

    #[test]
    fn test_move_feed_to_another_folder() {
        let mut conn = test_conn();
        let target = create_folder(&mut conn, "Target", FolderKind::Feeds).unwrap();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();

        move_feed(&mut conn, outcome.id, target).unwrap();

        let feeds = list_feeds(&conn, Some(target)).unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].id(), outcome.id);
    }

    #[test]
    fn test_move_feed_missing_feed_or_folder_not_found() {
        let mut conn = test_conn();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();
        let target = create_folder(&mut conn, "Target", FolderKind::Feeds).unwrap();

        assert!(matches!(
            move_feed(&mut conn, FeedId::new(999), target),
            Err(MetroError::NotFound(_))
        ));
        assert!(matches!(
            move_feed(&mut conn, outcome.id, FolderId::new(999)),
            Err(MetroError::NotFound(_))
        ));
    }

    #[test]
    fn test_move_feed_to_saved_folder_rejected() {
        let mut conn = test_conn();
        let saved_id = create_folder(&mut conn, "Saved", FolderKind::Saved).unwrap();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();

        assert!(matches!(
            move_feed(&mut conn, outcome.id, saved_id),
            Err(MetroError::Usage(_))
        ));
    }

    #[test]
    fn test_remove_feeds_removes_all_and_tolerates_repeats() {
        let mut conn = test_conn();
        let one = add_feed(&mut conn, "https://example.com/one.xml", None, None).unwrap();
        let two = add_feed(&mut conn, "https://example.com/two.xml", None, None).unwrap();
        let three = add_feed(&mut conn, "https://example.com/three.xml", None, None).unwrap();
        insert_entry_row(&conn, two.id, "guid-1");

        let removed = remove_feeds(&mut conn, &[one.id, two.id, one.id]).unwrap();
        let removed: Vec<(FeedId, u64)> = removed.iter().map(|f| (f.id, f.entry_count)).collect();
        assert_eq!(removed, vec![(one.id, 0), (two.id, 1)]);

        let remaining: Vec<FeedId> = list_feeds(&conn, None)
            .unwrap()
            .iter()
            .map(Feed::id)
            .collect();
        assert_eq!(remaining, vec![three.id]);
        assert_eq!(row_count(&conn, "entries"), 0);
        // An empty batch is a no-op.
        assert_eq!(remove_feeds(&mut conn, &[]).unwrap(), []);
        assert_eq!(row_count(&conn, "feeds"), 1);
    }

    #[test]
    fn test_remove_feeds_is_all_or_nothing() {
        let mut conn = test_conn();
        let one = add_feed(&mut conn, "https://example.com/one.xml", None, None).unwrap();

        let error = remove_feeds(&mut conn, &[one.id, FeedId::new(999)]).unwrap_err();

        assert!(matches!(error, MetroError::NotFound(_)));
        assert!(error.to_string().contains("f999"), "{error}");
        assert_eq!(row_count(&conn, "feeds"), 1);
    }

    #[test]
    fn test_move_feeds_moves_all_or_nothing() {
        let mut conn = test_conn();
        let target = create_folder(&mut conn, "Target", FolderKind::Feeds).unwrap();
        let one = add_feed(&mut conn, "https://example.com/one.xml", None, None).unwrap();
        let two = add_feed(&mut conn, "https://example.com/two.xml", None, None).unwrap();

        let error = move_feeds(&mut conn, &[one.id, FeedId::new(999)], target).unwrap_err();
        assert!(matches!(error, MetroError::NotFound(_)));
        assert_eq!(list_feeds(&conn, Some(target)).unwrap(), []);

        let moved = move_feeds(&mut conn, &[one.id, two.id, two.id], target).unwrap();
        assert_eq!(moved.len(), 2);
        assert!(
            moved
                .iter()
                .all(|f| f.folder_id == target && f.folder_name == "Target")
        );
        assert_eq!(list_feeds(&conn, Some(target)).unwrap().len(), 2);
    }

    #[test]
    fn test_rename_feed_updates_title() {
        let mut conn = test_conn();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();

        rename_feed(&mut conn, outcome.id, "  New Title  ").unwrap();

        assert_eq!(list_feeds(&conn, None).unwrap()[0].title(), "New Title");
    }

    #[test]
    fn test_rename_feed_nonexistent_not_found() {
        let mut conn = test_conn();
        assert!(matches!(
            rename_feed(&mut conn, FeedId::new(999), "Title"),
            Err(MetroError::NotFound(_))
        ));
    }

    #[test]
    fn test_rename_feed_empty_title_rejected() {
        let mut conn = test_conn();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();

        assert!(matches!(
            rename_feed(&mut conn, outcome.id, "   "),
            Err(MetroError::Usage(_))
        ));
        assert_eq!(list_feeds(&conn, None).unwrap()[0].title(), "example.com");
    }

    #[test]
    fn test_get_feed_info_returns_complete_metadata() {
        let mut conn = test_conn();
        let folder_id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        let outcome = add_feed(
            &mut conn,
            "https://example.com/feed.xml",
            Some("My Blog"),
            Some(folder_id),
        )
        .unwrap();
        insert_entry_row(&conn, outcome.id, "guid-1");
        insert_entry_row(&conn, outcome.id, "guid-2");
        conn.execute(
            "UPDATE feeds SET last_fetch = ?1, error_state = ?2, last_error_at = ?3 WHERE id = ?4",
            params![
                1_445_414_880_i64,
                "timeout",
                1_445_418_480_i64,
                outcome.id.inner()
            ],
        )
        .unwrap();

        let info = get_feed_info(&conn, outcome.id).unwrap();
        assert_eq!(info.id, outcome.id);
        assert_eq!(info.url, "https://example.com/feed.xml");
        assert_eq!(info.title, "My Blog");
        assert_eq!(info.site_url, "https://example.com");
        assert_eq!(info.kind, FeedKind::Rss);
        assert_eq!(info.folder_id, folder_id);
        assert_eq!(info.folder_name, "Tech");
        assert_eq!(info.last_fetch, Some(1_445_414_880));
        assert_eq!(info.error_state.as_deref(), Some("timeout"));
        assert_eq!(info.last_error_at, Some(1_445_418_480));
        assert_eq!(info.entry_count, 2);
    }

    #[test]
    fn test_get_feed_info_defaults_for_unfetched_feed() {
        let mut conn = test_conn();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();

        let info = get_feed_info(&conn, outcome.id).unwrap();
        assert_eq!(info.folder_name, "Unsorted");
        assert_eq!(info.last_fetch, None);
        assert_eq!(info.error_state, None);
        assert_eq!(info.last_error_at, None);
        assert_eq!(info.entry_count, 0);
    }

    #[test]
    fn test_get_feed_info_nonexistent_not_found() {
        let conn = test_conn();
        assert!(matches!(
            get_feed_info(&conn, FeedId::new(999)),
            Err(MetroError::NotFound(_))
        ));
    }
}
