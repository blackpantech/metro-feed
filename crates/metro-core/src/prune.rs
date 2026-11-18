//! Age-based entry pruning (the `pruning` capability of M1).
//!
//! Feed entries are a rolling window, not an archive: entries older than
//! `feeds.window_days` (spec: 30 days by default) are deleted. In M1 the
//! window is not read from the config file yet (config wiring is planned for
//! M4); callers pass it explicitly and [`DEFAULT_WINDOW_DAYS`] is the
//! documented default. [`crate::refresh::refresh_feeds`] runs pruning after
//! every refresh (pruning spec: "Pruning runs after refresh").
//!
//! # Which date decides an entry's age
//!
//! An entry's age is measured from its **`published`** date. When the feed
//! omitted it (`published IS NULL`), the entry's **`first_seen`** date (set by
//! the refresh writer to the insertion time) is used instead (design decision
//! D7, open question 1: an explicit `first_seen` column rather than `ROWID`).
//!
//! `first_seen = 0` is the schema's "unset" marker (`DEFAULT 0`). An entry
//! with neither a `published` date nor a set `first_seen` has **no known
//! age** and is kept: deleting it would be a guess. The refresh writer
//! always sets `first_seen`, so only rows written outside the pipeline can
//! end up in that state.
//!
//! # Boundaries
//!
//! The cutoff is `now - window_days * 86 400` seconds. An entry is pruned when
//! its effective date is **strictly before** the cutoff, so an entry exactly
//! `window_days` old is kept. Entries dated in the future are never pruned.
//!
//! # What is (not) exempt
//!
//! - **Unread entries are not exempt** (product spec decision 7; pruning
//!   spec): they are deleted like read ones. The "warn before expiry" hint
//!   and `prune --dry-run` are deferred to M4.
//! - **Saved items are never touched**: they are independent snapshots in
//!   their own table (M3), so a `DELETE FROM entries` cannot affect them.
//!
//! # Atomicity
//!
//! Like every other mutation in `metro-core`, the delete runs in an
//! `IMMEDIATE` transaction, so a concurrent `metro` process never observes a
//! partially applied prune and the returned count matches what was deleted.

use rusqlite::{Connection, TransactionBehavior, params};

use crate::MetroError;

/// Default retention window in days (`feeds.window_days`, product spec §
/// "Feed window"; hardcoded in M1).
pub const DEFAULT_WINDOW_DAYS: u32 = 30;

/// Number of seconds in one day, the unit conversion for the window.
const SECONDS_PER_DAY: i64 = 86_400;

/// Delete every entry whose effective date (`published`, else a set
/// `first_seen`) is strictly before `?1`.
///
/// `NULLIF(first_seen, 0)` maps the "unset" marker to `NULL`; a `NULL`
/// effective date makes the comparison `NULL`, so such entries are kept.
const PRUNE_SQL: &str = "DELETE FROM entries \
     WHERE COALESCE(published, NULLIF(first_seen, 0)) < ?1";

/// Delete entries older than `window_days` days, measured from now.
///
/// See the [module documentation](self) for the age rules. `conn` must be a
/// migrated connection.
///
/// Returns the number of entries deleted.
///
/// # Errors
///
/// - [`MetroError::Usage`] if `window_days` is `0` (it would delete every
///   entry dated before this second, which is never a sensible window).
/// - [`MetroError::Generic`] if a database operation fails.
pub fn prune_entries(conn: &mut Connection, window_days: u32) -> Result<usize, MetroError> {
    prune_entries_at(conn, window_days, jiff::Timestamp::now().as_second())
}

/// [`prune_entries`] with an explicit "now" (Unix seconds).
///
/// Exists so tests can use synthetic dates without depending on the clock.
///
/// # Errors
///
/// See [`prune_entries`].
pub(crate) fn prune_entries_at(
    conn: &mut Connection,
    window_days: u32,
    now: i64,
) -> Result<usize, MetroError> {
    let cutoff = cutoff(window_days, now)?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let deleted = tx.execute(PRUNE_SQL, params![cutoff])?;
    tx.commit()?;
    Ok(deleted)
}

/// Check that `window_days` is usable as a retention window.
///
/// Shared with the refresh pipeline so an invalid window is rejected before
/// any feed is fetched.
///
/// # Errors
///
/// Returns [`MetroError::Usage`] if `window_days` is `0`.
pub(crate) fn validate_window(window_days: u32) -> Result<(), MetroError> {
    if window_days == 0 {
        return Err(MetroError::Usage(
            "the pruning window must be at least 1 day".to_string(),
        ));
    }
    Ok(())
}

/// Compute the pruning cutoff (Unix seconds) for `window_days` before `now`.
///
/// Saturating arithmetic keeps absurd inputs from overflowing: the result
/// simply moves towards `i64::MIN`, which prunes nothing.
///
/// # Errors
///
/// Returns [`MetroError::Usage`] if `window_days` is `0`.
fn cutoff(window_days: u32, now: i64) -> Result<i64, MetroError> {
    validate_window(window_days)?;
    let window_seconds = i64::from(window_days).saturating_mul(SECONDS_PER_DAY);
    Ok(now.saturating_sub(window_seconds))
}

#[cfg(test)]
mod pure_tests {
    use super::*;

    #[test]
    fn test_cutoff_is_window_before_now() {
        assert_eq!(cutoff(30, 3_000_000).unwrap(), 3_000_000 - 30 * 86_400);
        assert_eq!(cutoff(1, 86_400).unwrap(), 0);
    }

    #[test]
    fn test_cutoff_saturates() {
        assert_eq!(cutoff(u32::MAX, i64::MIN).unwrap(), i64::MIN);
    }

    #[test]
    fn test_zero_window_is_usage_error() {
        let error = cutoff(0, 1_000).unwrap_err();
        assert!(matches!(error, MetroError::Usage(_)));
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn test_default_window_is_thirty_days() {
        assert_eq!(DEFAULT_WINDOW_DAYS, 30);
    }
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;

    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use crate::feeds::add_feed;
    use crate::models::FeedId;

    /// Synthetic "now": 2026-03-01T00:00:00Z.
    const NOW: i64 = 1_772_323_200;
    const DAY: i64 = 86_400;

    fn test_conn() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    fn new_feed(conn: &mut Connection) -> FeedId {
        add_feed(conn, "https://example.com/feed.xml", None, None)
            .unwrap()
            .id
    }

    /// Insert an entry with explicit dates and read flag.
    fn insert_entry(
        conn: &Connection,
        feed_id: FeedId,
        guid: &str,
        published: Option<i64>,
        first_seen: Option<i64>,
        is_read: bool,
    ) {
        conn.execute(
            "INSERT INTO entries (feed_id, guid, url, title, published, is_read, first_seen) \
             VALUES (?1, ?2, '', ?2, ?3, ?4, ?5)",
            params![feed_id.inner(), guid, published, is_read, first_seen],
        )
        .unwrap();
    }

    /// Remaining entry guids, sorted.
    fn remaining(conn: &Connection) -> Vec<String> {
        let mut statement = conn
            .prepare("SELECT guid FROM entries ORDER BY guid")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn test_old_entries_are_pruned_and_recent_preserved() {
        let mut conn = test_conn();
        let feed = new_feed(&mut conn);
        insert_entry(&conn, feed, "old", Some(NOW - 45 * DAY), Some(NOW), true);
        insert_entry(&conn, feed, "recent", Some(NOW - 2 * DAY), Some(NOW), true);

        let pruned = prune_entries_at(&mut conn, 30, NOW).unwrap();

        assert_eq!(pruned, 1);
        assert_eq!(remaining(&conn), vec!["recent"]);
    }

    #[test]
    fn test_published_takes_precedence_over_first_seen() {
        let mut conn = test_conn();
        let feed = new_feed(&mut conn);
        // Old publication date but seen just now: still pruned by `published`.
        insert_entry(
            &conn,
            feed,
            "old-pub",
            Some(NOW - 31 * DAY),
            Some(NOW),
            false,
        );
        // Recent publication date but seen long ago: kept by `published`.
        insert_entry(
            &conn,
            feed,
            "new-pub",
            Some(NOW - DAY),
            Some(NOW - 90 * DAY),
            false,
        );

        assert_eq!(prune_entries_at(&mut conn, 30, NOW).unwrap(), 1);
        assert_eq!(remaining(&conn), vec!["new-pub"]);
    }

    #[test]
    fn test_first_seen_is_fallback_without_published() {
        let mut conn = test_conn();
        let feed = new_feed(&mut conn);
        insert_entry(&conn, feed, "seen-old", None, Some(NOW - 40 * DAY), false);
        insert_entry(&conn, feed, "seen-new", None, Some(NOW - 10 * DAY), false);

        assert_eq!(prune_entries_at(&mut conn, 30, NOW).unwrap(), 1);
        assert_eq!(remaining(&conn), vec!["seen-new"]);
    }

    #[test]
    fn test_entries_without_any_date_are_kept() {
        let mut conn = test_conn();
        let feed = new_feed(&mut conn);
        insert_entry(&conn, feed, "unset", None, Some(0), false);
        insert_entry(&conn, feed, "null", None, None, false);

        assert_eq!(prune_entries_at(&mut conn, 30, NOW).unwrap(), 0);
        assert_eq!(remaining(&conn), vec!["null", "unset"]);
    }

    #[test]
    fn test_unread_old_entries_are_pruned() {
        let mut conn = test_conn();
        let feed = new_feed(&mut conn);
        insert_entry(
            &conn,
            feed,
            "unread",
            Some(NOW - 60 * DAY),
            Some(NOW),
            false,
        );
        insert_entry(&conn, feed, "read", Some(NOW - 60 * DAY), Some(NOW), true);

        assert_eq!(prune_entries_at(&mut conn, 30, NOW).unwrap(), 2);
        assert!(remaining(&conn).is_empty());
    }

    #[test]
    fn test_window_boundary_is_exclusive() {
        let mut conn = test_conn();
        let feed = new_feed(&mut conn);
        let cutoff = NOW - 30 * DAY;
        insert_entry(&conn, feed, "at-cutoff", Some(cutoff), Some(NOW), false);
        insert_entry(
            &conn,
            feed,
            "past-cutoff",
            Some(cutoff - 1),
            Some(NOW),
            false,
        );
        insert_entry(&conn, feed, "future", Some(NOW + DAY), Some(NOW), false);

        assert_eq!(prune_entries_at(&mut conn, 30, NOW).unwrap(), 1);
        assert_eq!(remaining(&conn), vec!["at-cutoff", "future"]);
    }

    #[test]
    fn test_prune_count_is_returned_and_is_zero_when_idle() {
        let mut conn = test_conn();
        let feed = new_feed(&mut conn);
        for index in 0..5 {
            insert_entry(
                &conn,
                feed,
                &format!("old-{index}"),
                Some(NOW - 100 * DAY),
                Some(NOW),
                false,
            );
        }

        assert_eq!(prune_entries_at(&mut conn, 30, NOW).unwrap(), 5);
        assert_eq!(prune_entries_at(&mut conn, 30, NOW).unwrap(), 0);
    }

    #[test]
    fn test_custom_window_is_respected() {
        let mut conn = test_conn();
        let feed = new_feed(&mut conn);
        insert_entry(&conn, feed, "8-days", Some(NOW - 8 * DAY), Some(NOW), false);
        insert_entry(&conn, feed, "3-days", Some(NOW - 3 * DAY), Some(NOW), false);

        assert_eq!(prune_entries_at(&mut conn, 7, NOW).unwrap(), 1);
        assert_eq!(remaining(&conn), vec!["3-days"]);
    }

    #[test]
    fn test_prune_entries_uses_the_current_time() {
        let mut conn = test_conn();
        let feed = new_feed(&mut conn);
        let now = jiff::Timestamp::now().as_second();
        insert_entry(&conn, feed, "old", Some(now - 31 * DAY), Some(now), false);
        insert_entry(&conn, feed, "new", Some(now - DAY), Some(now), false);

        assert_eq!(prune_entries(&mut conn, DEFAULT_WINDOW_DAYS).unwrap(), 1);
        assert_eq!(remaining(&conn), vec!["new"]);
    }

    #[test]
    fn test_zero_window_deletes_nothing() {
        let mut conn = test_conn();
        let feed = new_feed(&mut conn);
        insert_entry(&conn, feed, "old", Some(NOW - 60 * DAY), Some(NOW), false);

        let error = prune_entries_at(&mut conn, 0, NOW).unwrap_err();

        assert!(matches!(error, MetroError::Usage(_)));
        assert_eq!(remaining(&conn), vec!["old"]);
    }
}
