//! Database persistence of fetch results (the refresh "single writer").
//!
//! Worker threads never touch SQLite: they hand each [`FetchResult`] (or
//! error) to the thread that owns the [`Connection`], which calls the
//! functions below one feed at a time (design: single-writer pattern, see
//! the refresh pipeline row in the design's risk table). Each feed's update is
//! one `IMMEDIATE` transaction, so a crash or a concurrent `metro` process
//! never observes half-applied entries and validators.
//!
//! # What a successful fetch records
//!
//! | Column | `200` (modified) | `304` (not modified) |
//! |---|---|---|
//! | `kind` | parsed kind (D5, D10) | unchanged |
//! | `site_url` | parsed alternate link, kept when the feed declares none | unchanged |
//! | `title` | parsed title **only while `last_fetch IS NULL`** and non-blank (D10) | unchanged |
//! | `etag`, `last_modified` | the response's validators (may become `NULL`) | the validators reported by the fetcher (cached ones preserved) |
//! | `last_fetch` | now (D6) | now (D6) |
//! | `error_state` | cleared | cleared |
//! | `consecutive_failures`, `next_retry` | reset to `0` / `NULL` | reset to `0` / `NULL` |
//! | entries | inserted with dedup | none |
//!
//! # What a failed fetch records
//!
//! `error_state` receives the error message, `consecutive_failures` is
//! incremented, and `next_retry` is set to the failure time plus the
//! exponential backoff delay for the new counter value, computed by
//! [`next_retry_at`] (1m, 5m, 30m, 2h, 12h, then 24h for every further
//! failure; `backoff` spec, D6). Until that deadline passes, the refresh
//! pipeline skips the feed (see `refresh::partition_by_backoff`).
//! `last_fetch` is never touched by a failure (D6), so `last_fetch IS NULL`
//! keeps meaning "never fetched successfully".
//!
//! # Entry deduplication
//!
//! Deduplication is scoped **per feed**: two feeds that publish the same
//! guid or URL each keep their own entry. Each entry is inserted with
//! `INSERT OR IGNORE` keyed on [`dedup_key`] (guid → url → content hash)
//! through the `UNIQUE(feed_id, guid)` constraint (D2). In addition, an entry
//! whose non-empty URL already exists **in the same feed** is skipped even
//! when its guid is new, which is the spec's "duplicate url is skipped" rule;
//! the guard is served by the `idx_entries_feed_url` index on
//! `entries(feed_id, url)`.
//!
//! New entries get `first_seen = now` (the pruning fallback when `published`
//! is missing), `is_read = 0`, and an empty `url` / `title` when the feed
//! omits them (both columns are `NOT NULL`).

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::MetroError;
use crate::feeds::next_retry_at;
use crate::fetch::{FetchResult, FetchStatus};
use crate::models::FeedId;
use crate::refresh::dedup::{dedup_key, entry_url};

/// Insert one entry unless its key or (non-empty) URL is already stored for
/// the same feed.
///
/// `?1` (the feed) and `?3` (the URL) are reused in the `NOT EXISTS` guard,
/// so an identical URL in another feed never blocks the insert. The empty
/// string means "no URL" and never deduplicates. The key half of the dedup is
/// the `UNIQUE(feed_id, guid)` constraint behind `INSERT OR IGNORE`.
const INSERT_ENTRY_SQL: &str = "INSERT OR IGNORE INTO entries \
     (feed_id, guid, url, title, author, published, summary, content, first_seen) \
     SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9 \
     WHERE ?3 = '' \
        OR NOT EXISTS (SELECT 1 FROM entries WHERE feed_id = ?1 AND url = ?3)";

/// Current time as Unix seconds, the unit of every timestamp column.
pub(crate) fn now_unix() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// Persist a successful refresh fetch in its own `IMMEDIATE` transaction.
///
/// The parsed feed title (if any) is offered as the title candidate, so it
/// is applied only on the feed's first successful fetch (D10).
///
/// Returns the number of new entries inserted.
///
/// # Errors
///
/// - [`MetroError::NotFound`] if the feed row no longer exists (removed
///   while it was being fetched); nothing is written.
/// - [`MetroError::Generic`] if a database operation fails.
pub(crate) fn store_refresh_success(
    conn: &mut Connection,
    feed_id: FeedId,
    result: &FetchResult,
) -> Result<usize, MetroError> {
    let title_candidate = result.feed.as_ref().and_then(|feed| feed.title.as_deref());

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let new_entries = apply_fetch_success(&tx, feed_id, result, title_candidate, now_unix())?;
    tx.commit()?;
    Ok(new_entries)
}

/// Apply a successful fetch to an existing feed row inside `tx`.
///
/// Shared by the refresh writer and `subscribe_feed`, so both store
/// metadata, validators, `last_fetch`, and entries identically. See the
/// module documentation for the exact column updates.
///
/// `title_candidate` is written to `title` only while `last_fetch IS NULL`
/// and only when it is not blank. `subscribe_feed` passes `None` because it
/// already inserted the final title.
///
/// Returns the number of new entries inserted. The caller commits.
///
/// # Errors
///
/// - [`MetroError::NotFound`] if no feed row has `feed_id`.
/// - [`MetroError::Generic`] if a database operation fails.
pub(crate) fn apply_fetch_success(
    tx: &Transaction<'_>,
    feed_id: FeedId,
    result: &FetchResult,
    title_candidate: Option<&str>,
    now: i64,
) -> Result<usize, MetroError> {
    let title_candidate = title_candidate
        .map(str::trim)
        .filter(|title| !title.is_empty());

    let updated = match (result.status, result.feed.as_ref()) {
        (FetchStatus::Modified, Some(metadata)) => {
            let site_url = metadata
                .site_url
                .as_deref()
                .map(str::trim)
                .filter(|site_url| !site_url.is_empty());
            tx.execute(
                "UPDATE feeds SET \
                     kind = ?1, \
                     site_url = COALESCE(?2, site_url), \
                     title = CASE WHEN last_fetch IS NULL AND ?3 IS NOT NULL \
                                  THEN ?3 ELSE title END, \
                     etag = ?4, \
                     last_modified = ?5, \
                     last_fetch = ?6, \
                     error_state = NULL, \
                     consecutive_failures = 0, \
                     next_retry = NULL \
                 WHERE id = ?7",
                params![
                    metadata.kind.as_str(),
                    site_url,
                    title_candidate,
                    result.etag.as_deref(),
                    result.last_modified.as_deref(),
                    now,
                    feed_id.inner()
                ],
            )?
        }
        // A 304, or (defensively) a 200 without feed metadata: record the
        // successful contact without touching kind, site URL, or title.
        _ => tx.execute(
            "UPDATE feeds SET \
                 etag = ?1, \
                 last_modified = ?2, \
                 last_fetch = ?3, \
                 error_state = NULL, \
                 consecutive_failures = 0, \
                 next_retry = NULL \
             WHERE id = ?4",
            params![
                result.etag.as_deref(),
                result.last_modified.as_deref(),
                now,
                feed_id.inner()
            ],
        )?,
    };

    if updated == 0 {
        return Err(MetroError::NotFound(format!(
            "feed {feed_id} was removed during refresh"
        )));
    }

    insert_entries(tx, feed_id, result, now)
}

/// Insert the fetched entries with deduplication; returns how many were new.
fn insert_entries(
    tx: &Transaction<'_>,
    feed_id: FeedId,
    result: &FetchResult,
    now: i64,
) -> Result<usize, MetroError> {
    let mut statement = tx.prepare_cached(INSERT_ENTRY_SQL)?;
    let mut inserted = 0;

    for entry in &result.entries {
        let key = dedup_key(entry);
        // Timestamps beyond i64 cannot be stored; treat them as unknown so
        // pruning falls back to `first_seen`.
        let published = entry
            .published
            .and_then(|seconds| i64::try_from(seconds).ok());

        inserted += statement.execute(params![
            feed_id.inner(),
            key.as_ref(),
            entry_url(entry),
            entry.title.as_deref().unwrap_or_default(),
            entry.author.as_deref(),
            published,
            entry.summary.as_deref(),
            entry.content.as_deref(),
            now
        ])?;
    }

    Ok(inserted)
}

/// Record a failed fetch in its own `IMMEDIATE` transaction.
///
/// Sets `error_state` to `message`, increments `consecutive_failures`, and
/// sets `next_retry` to the backoff deadline from [`next_retry_at`] for the
/// incremented counter, relative to the failure time. `last_fetch` is left
/// untouched (D6). A feed that was removed meanwhile is silently ignored:
/// there is no row left to annotate.
///
/// # Errors
///
/// Returns [`MetroError::Generic`] if a database operation fails.
pub(crate) fn store_refresh_failure(
    conn: &mut Connection,
    feed_id: FeedId,
    message: &str,
) -> Result<(), MetroError> {
    let now = now_unix();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let previous: Option<Option<i64>> = tx
        .query_row(
            "SELECT consecutive_failures FROM feeds WHERE id = ?1",
            params![feed_id.inner()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(previous) = previous else {
        // The feed was removed while it was being fetched.
        return Ok(());
    };

    // The column is nullable with a default of 0; treat NULL and corrupt
    // negative values as "no previous failures".
    let failures = previous
        .and_then(|count| u32::try_from(count).ok())
        .unwrap_or_default()
        .saturating_add(1);
    let next_retry = next_retry_at(failures, now);

    tx.execute(
        "UPDATE feeds SET error_state = ?1, consecutive_failures = ?2, next_retry = ?3 \
         WHERE id = ?4",
        params![message, failures, next_retry, feed_id.inner()],
    )?;
    tx.commit()?;
    Ok(())
}
