//! Entry read API for Metro: listing a feed's entries (`metro view`).
//!
//! This module implements the core side of the `feeds-cli` `view` command
//! (`metro view <f-id> [--all|--unread] [--limit <n>]`): the CLI only
//! translates its flags into an [`EntryFilter`] and renders the result, so the
//! TUI can list a feed's entries with exactly the same rules.
//!
//! # Semantics
//!
//! - **Scope**: one feed. An unknown feed is [`MetroError::NotFound`]
//!   (exit 3), unlike the folder-scoped read APIs that return an empty list,
//!   because the caller named the feed explicitly.
//! - **Read state** ([`ReadFilter`]): every entry ([`ReadFilter::Any`], the
//!   default) or unread entries only ([`ReadFilter::Unread`], `--unread`).
//! - **Ordering: newest first.** Entries are sorted by their *effective
//!   date*, descending: the feed's `published` date, or the `first_seen`
//!   insertion time when the feed omitted it. This is the same date
//!   [`crate::prune`] uses to measure an entry's age, so the list order and
//!   the pruning order agree. Entries with no known date at all (`published`
//!   missing and `first_seen` unset, `0`) come last. Ties (same second) are
//!   broken by entry ID, descending, so the most recently inserted entry
//!   comes first and the order is deterministic.
//! - **Size** ([`EntryLimit`]): by default only the
//!   [`DEFAULT_RECENT_LIMIT`] most recent matching entries are returned
//!   ("recent entries", `metro view f12`). [`EntryLimit::Unlimited`]
//!   (`--all`) returns every matching entry and [`EntryLimit::AtMost`]
//!   (`--limit <n>`) at most `n`. The limit applies after the read-state
//!   filter, so `--unread --limit 5` yields the 5 newest unread entries.
//!
//! Reads never write: listing entries does not mark them read (marking is
//! the job of `metro read` / `metro mark`, later milestones).

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::MetroError;
use crate::feeds::{feed_id_from_row_id, timestamp_from_row};
use crate::models::{Entry, EntryId, FeedId};

/// Number of entries [`EntryLimit::Recent`] returns.
///
/// The product spec does not pin a number for "recent entries"; 20 fits one
/// terminal screen. `--all` or `--limit <n>` override it.
pub const DEFAULT_RECENT_LIMIT: usize = 20;

/// Which entries of a feed to list, by read state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReadFilter {
    /// Read and unread entries.
    #[default]
    Any,
    /// Unread entries only.
    Unread,
}

/// How many entries to list at most.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EntryLimit {
    /// The [`DEFAULT_RECENT_LIMIT`] most recent entries.
    #[default]
    Recent,
    /// Every matching entry.
    Unlimited,
    /// At most this many entries; `0` is a usage error.
    AtMost(usize),
}

impl EntryLimit {
    /// The SQL `LIMIT` value, where `-1` means "no limit" in SQLite.
    fn sql_limit(self) -> Result<i64, MetroError> {
        let count = match self {
            EntryLimit::Recent => DEFAULT_RECENT_LIMIT,
            EntryLimit::Unlimited => return Ok(-1),
            EntryLimit::AtMost(0) => {
                return Err(MetroError::Usage(
                    "entry limit must be at least 1".to_string(),
                ));
            }
            EntryLimit::AtMost(count) => count,
        };
        // A limit beyond i64::MAX cannot be reached anyway: treat it as
        // unlimited rather than failing.
        Ok(i64::try_from(count).unwrap_or(-1))
    }
}

/// Filter for [`list_feed_entries`]. The default lists the
/// [`DEFAULT_RECENT_LIMIT`] most recent entries regardless of read state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntryFilter {
    /// Read-state filter.
    pub read: ReadFilter,
    /// Maximum number of entries.
    pub limit: EntryLimit,
}

/// Columns selected by [`list_feed_entries`], in [`StoredEntry::from_row`]
/// order.
const ENTRY_COLUMNS: &str =
    "id, feed_id, guid, url, title, author, published, summary, content, is_read";

/// Newest first by effective date (`published`, else a set `first_seen`),
/// undated entries last, ties by descending ID. SQLite sorts `NULL` lowest,
/// so `DESC` already puts undated entries last.
const ENTRY_ORDER: &str = "ORDER BY COALESCE(published, NULLIF(first_seen, 0)) DESC, id DESC";

/// List the entries of one feed, newest first, filtered by `filter`.
///
/// See the [module documentation](self) for ordering and limits. `conn`
/// must be a migrated connection.
///
/// # Errors
///
/// - [`MetroError::Usage`] if the limit is [`EntryLimit::AtMost`]`(0)`.
/// - [`MetroError::NotFound`] if no feed exists with the given ID.
/// - [`MetroError::Generic`] if the database query fails or a stored row is
///   invalid (negative id or timestamp).
pub fn list_feed_entries(
    conn: &Connection,
    feed_id: FeedId,
    filter: &EntryFilter,
) -> Result<Vec<Entry>, MetroError> {
    let limit = filter.limit.sql_limit()?;
    ensure_feed_exists(conn, feed_id)?;

    let unread_clause = match filter.read {
        ReadFilter::Any => "",
        ReadFilter::Unread => "AND is_read = 0",
    };
    let sql = format!(
        "SELECT {ENTRY_COLUMNS} FROM entries \
         WHERE feed_id = ?1 {unread_clause} {ENTRY_ORDER} LIMIT ?2"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params![feed_id.inner(), limit], StoredEntry::from_row)?;

    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?.into_entry()?);
    }
    Ok(entries)
}

/// Fail with [`MetroError::NotFound`] unless the feed exists.
fn ensure_feed_exists(conn: &Connection, feed_id: FeedId) -> Result<(), MetroError> {
    conn.query_row(
        "SELECT 1 FROM feeds WHERE id = ?1",
        params![feed_id.inner()],
        |_| Ok(()),
    )
    .optional()?
    .ok_or_else(|| MetroError::NotFound(format!("feed {feed_id} not found")))
}

/// Raw entry columns as stored in SQLite, before conversion to [`Entry`].
struct StoredEntry {
    id: i64,
    feed_id: i64,
    guid: String,
    url: String,
    title: String,
    author: Option<String>,
    published: Option<i64>,
    summary: Option<String>,
    content: Option<String>,
    is_read: bool,
}

impl StoredEntry {
    /// Read the [`ENTRY_COLUMNS`] of one row.
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            feed_id: row.get(1)?,
            guid: row.get(2)?,
            url: row.get(3)?,
            title: row.get(4)?,
            author: row.get(5)?,
            published: row.get(6)?,
            summary: row.get(7)?,
            content: row.get(8)?,
            is_read: row.get(9)?,
        })
    }

    /// Convert to the typed model, rejecting out-of-range stored values.
    fn into_entry(self) -> Result<Entry, MetroError> {
        let id = u64::try_from(self.id).map(EntryId::new).map_err(|_| {
            MetroError::Generic(format!("entry row id {} is out of range", self.id))
        })?;
        Ok(Entry {
            id,
            feed_id: feed_id_from_row_id(self.feed_id)?,
            guid: self.guid,
            url: self.url,
            title: self.title,
            author: self.author,
            published: timestamp_from_row(self.published)?,
            summary: self.summary,
            content: self.content,
            is_read: self.is_read,
        })
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;

    #[test]
    fn test_sql_limit_values() {
        assert_eq!(EntryLimit::Recent.sql_limit().unwrap(), 20);
        assert_eq!(EntryLimit::Unlimited.sql_limit().unwrap(), -1);
        assert_eq!(EntryLimit::AtMost(3).sql_limit().unwrap(), 3);
        assert_eq!(EntryLimit::AtMost(usize::MAX).sql_limit().unwrap(), -1);
        assert!(matches!(
            EntryLimit::AtMost(0).sql_limit(),
            Err(MetroError::Usage(_))
        ));
    }

    #[test]
    fn test_default_filter_is_recent_any() {
        let filter = EntryFilter::default();
        assert_eq!(filter.read, ReadFilter::Any);
        assert_eq!(filter.limit, EntryLimit::Recent);
    }
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use crate::feeds::add_feed;

    fn test_conn() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    fn feed(conn: &mut Connection, url: &str) -> FeedId {
        add_feed(conn, url, None, None).unwrap().id
    }

    /// Insert an entry with explicit dates and read state; returns its ID.
    fn insert(
        conn: &Connection,
        feed_id: FeedId,
        guid: &str,
        published: Option<i64>,
        first_seen: i64,
        is_read: bool,
    ) -> EntryId {
        conn.execute(
            "INSERT INTO entries (feed_id, guid, url, title, published, first_seen, is_read) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                feed_id.inner(),
                guid,
                format!("https://example.com/{guid}"),
                format!("Entry {guid}"),
                published,
                first_seen,
                is_read
            ],
        )
        .unwrap();
        EntryId::new(u64::try_from(conn.last_insert_rowid()).unwrap())
    }

    fn guids(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(Entry::guid).collect()
    }

    #[test]
    fn test_entries_are_newest_first_by_effective_date() {
        let mut conn = test_conn();
        let id = feed(&mut conn, "https://example.com/feed.xml");
        insert(&conn, id, "old", Some(100), 999, false);
        insert(&conn, id, "undated", None, 0, false);
        insert(&conn, id, "seen", None, 300, false);
        insert(&conn, id, "new", Some(500), 1, false);
        // Same second as "new", inserted later: ties by descending ID.
        insert(&conn, id, "new-later", Some(500), 1, false);

        let entries = list_feed_entries(&conn, id, &EntryFilter::default()).unwrap();
        assert_eq!(
            guids(&entries),
            ["new-later", "new", "seen", "old", "undated"]
        );
    }

    #[test]
    fn test_entry_fields_round_trip() {
        let mut conn = test_conn();
        let id = feed(&mut conn, "https://example.com/feed.xml");
        let entry_id = insert(&conn, id, "g", Some(42), 7, true);

        let entries = list_feed_entries(&conn, id, &EntryFilter::default()).unwrap();
        let [entry] = entries.as_slice() else {
            panic!("expected one entry, got {entries:?}");
        };
        assert_eq!(entry.id(), entry_id);
        assert_eq!(entry.feed_id(), id);
        assert_eq!(entry.url(), "https://example.com/g");
        assert_eq!(entry.title(), "Entry g");
        assert_eq!(entry.published(), Some(42));
        assert!(entry.is_read());
    }

    #[test]
    fn test_unread_filter() {
        let mut conn = test_conn();
        let id = feed(&mut conn, "https://example.com/feed.xml");
        insert(&conn, id, "read", Some(300), 1, true);
        insert(&conn, id, "unread-old", Some(100), 1, false);
        insert(&conn, id, "unread-new", Some(200), 1, false);

        let filter = EntryFilter {
            read: ReadFilter::Unread,
            ..EntryFilter::default()
        };
        let entries = list_feed_entries(&conn, id, &filter).unwrap();
        assert_eq!(guids(&entries), ["unread-new", "unread-old"]);
    }

    #[test]
    fn test_limits() {
        let mut conn = test_conn();
        let id = feed(&mut conn, "https://example.com/feed.xml");
        for n in 0..25 {
            insert(&conn, id, &format!("g{n:02}"), Some(n), 1, n % 2 == 0);
        }

        let recent = list_feed_entries(&conn, id, &EntryFilter::default()).unwrap();
        assert_eq!(recent.len(), DEFAULT_RECENT_LIMIT);
        assert_eq!(recent.first().map(Entry::guid), Some("g24"));

        let all = EntryFilter {
            limit: EntryLimit::Unlimited,
            ..EntryFilter::default()
        };
        assert_eq!(list_feed_entries(&conn, id, &all).unwrap().len(), 25);

        // The limit applies after the unread filter.
        let unread_three = EntryFilter {
            read: ReadFilter::Unread,
            limit: EntryLimit::AtMost(3),
        };
        let entries = list_feed_entries(&conn, id, &unread_three).unwrap();
        assert_eq!(guids(&entries), ["g23", "g21", "g19"]);
    }

    #[test]
    fn test_only_the_requested_feed_is_listed() {
        let mut conn = test_conn();
        let first = feed(&mut conn, "https://one.example.com/feed.xml");
        let second = feed(&mut conn, "https://two.example.com/feed.xml");
        insert(&conn, first, "a", Some(1), 1, false);
        insert(&conn, second, "b", Some(2), 1, false);

        let entries = list_feed_entries(&conn, first, &EntryFilter::default()).unwrap();
        assert_eq!(guids(&entries), ["a"]);
        // A feed without entries is an empty list, not an error.
        let empty = feed(&mut conn, "https://three.example.com/feed.xml");
        assert_eq!(
            list_feed_entries(&conn, empty, &EntryFilter::default()).unwrap(),
            []
        );
    }

    #[test]
    fn test_unknown_feed_is_not_found() {
        let conn = test_conn();
        let err = list_feed_entries(&conn, FeedId::new(9), &EntryFilter::default()).unwrap_err();
        assert!(matches!(err, MetroError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn test_zero_limit_is_usage_error_before_lookup() {
        let conn = test_conn();
        let filter = EntryFilter {
            limit: EntryLimit::AtMost(0),
            ..EntryFilter::default()
        };
        let err = list_feed_entries(&conn, FeedId::new(9), &filter).unwrap_err();
        assert!(matches!(err, MetroError::Usage(_)), "{err:?}");
    }
}
