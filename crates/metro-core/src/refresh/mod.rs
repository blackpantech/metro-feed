//! Threaded feed refresh and subscribe-time fetching for Metro.
//!
//! This module implements the `refresh` capability of the M1 milestone and
//! the subscribe-time first fetch of the `feed-management` capability
//! ([`subscribe_feed`], design decision D11).
//!
//! # Architecture
//!
//! ```text
//!                 ┌────────────┐  job index (AtomicUsize)
//!  load_candidates│ due jobs   │◄──────────────────────────┐
//!  (backoff skip) └────────────┘                           │
//!                        │ &[FeedJob] (borrowed)           │
//!          ┌─────────────┼─────────────┐                   │
//!          ▼             ▼             ▼                   │
//!     worker 1  …   worker N  (std::thread::scope, N ≤ concurrency)
//!     FeedFetcher::fetch (network only, panics caught)     │
//!          │             │             │                   │
//!          └──── mpsc::Sender<FetchMessage> ───────────────┘
//!                        ▼
//!          calling thread = single DB writer (writer.rs)
//!          one IMMEDIATE transaction per feed
//!                        │
//!                        ▼
//!          RefreshEvent progress channel (optional) + RefreshReport
//! ```
//!
//! - **Worker pool** (D1): plain `std::thread` scoped threads, no async
//!   runtime. The pool size defaults to [`DEFAULT_CONCURRENCY`] (8) and is
//!   capped by the number of due feeds. Workers pull the next job from a
//!   shared atomic index, so a slow feed never blocks the others.
//! - **Single writer**: workers never touch SQLite. The thread that called
//!   [`refresh_feeds`] owns the [`Connection`] and persists each result as it
//!   arrives (see `writer` for exactly which columns are written on `200`,
//!   `304`, and failure). This avoids write contention between workers.
//! - **Failure isolation**: a failing fetch (including a panic inside the
//!   fetcher) only fails that feed. The failure is recorded on the feed row
//!   and in the [`RefreshReport`]; the remaining feeds are still fetched. A
//!   **database** error, by contrast, is systemic: the writer stops the
//!   workers and [`refresh_feeds`] returns the error.
//! - **Backoff** (D6): a failure sets `next_retry` to the failure time plus
//!   the exponential delay for the feed's new `consecutive_failures` value
//!   ([`crate::feeds::next_retry_at`]: 1m, 5m, 30m, 2h, 12h, 24h max). Feeds
//!   whose `next_retry` lies in the future are not fetched (not even when
//!   targeted explicitly) and are listed in [`RefreshReport::skipped`]; a
//!   success resets the counter and clears the deadline.
//! - **Entry dedup**: guid → url → content hash, see `dedup` and `writer`.
//! - **Progress**: when [`RefreshOptions::progress`] is set, a
//!   [`RefreshEvent::Started`] event is sent once, then one
//!   [`RefreshEvent::FeedFinished`] per fetched feed in completion order.
//!   Progress is advisory: a dropped receiver never fails the refresh.
//! - **Pruning** (pruning spec): once every fetched feed has been persisted,
//!   [`prune_entries`] deletes entries older than
//!   [`RefreshOptions::prune_window_days`] (default
//!   [`DEFAULT_WINDOW_DAYS`], 30). It runs whenever the refresh completes,
//!   including partial failure, all feeds failing, or nothing being due, and
//!   it prunes **all** feeds, not only the refreshed ones. The count is
//!   reported in [`RefreshReport::pruned`]. It does not run when the refresh
//!   aborts with an error (invalid options, unknown target, database error).
//!   The window is validated before anything is fetched.
//!
//! # Testing seam
//!
//! The network is abstracted behind the [`FeedFetcher`] trait (dependency
//! inversion). [`Fetcher`] implements it for production; unit tests inject
//! mock fetchers and never hit the network.
//!
//! # Out of scope here
//!
//! The CLI maps [`RefreshReport::exit_status`] to process exit codes.

mod dedup;
mod subscribe;
mod writer;

#[cfg(all(test, feature = "test-utils"))]
mod test_support;

use std::any::Any;
use std::collections::HashSet;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::MetroError;
use crate::feeds::feed_id_from_row_id;
use crate::fetch::{FetchResult, Fetcher};
use crate::models::{FeedId, FolderId};
use crate::prune::{DEFAULT_WINDOW_DAYS, prune_entries, validate_window};
use crate::refresh::writer::{now_unix, store_refresh_failure, store_refresh_success};

pub use subscribe::{FeedPreview, SubscribeOutcome, subscribe_feed};

/// Default number of worker threads used by [`refresh_feeds`].
pub const DEFAULT_CONCURRENCY: usize = 8;

/// Source of feed documents, abstracted for dependency injection.
///
/// The contract is exactly that of [`Fetcher::fetch`]: perform a conditional
/// GET with the cached validators and return the parsed result, a
/// [`MetroError::Network`] for transport/HTTP failures, or a
/// [`MetroError::Generic`] for unparsable bodies. Implementations must be
/// [`Sync`] because one fetcher is shared by reference across all workers.
pub trait FeedFetcher: Sync {
    /// Fetch `url`, sending `etag` / `last_modified` as conditional headers.
    ///
    /// # Errors
    ///
    /// See [`Fetcher::fetch`].
    fn fetch(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<FetchResult, MetroError>;
}

impl FeedFetcher for Fetcher {
    fn fetch(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<FetchResult, MetroError> {
        // Resolves to the inherent method, which takes precedence over the
        // trait method of the same name.
        Fetcher::fetch(self, url, etag, last_modified)
    }
}

/// Which feeds [`refresh_feeds`] considers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshTarget<'a> {
    /// Every subscribed feed.
    All,
    /// Every feed in one folder (an unknown folder yields no feeds, like
    /// [`crate::feeds::list_feeds`]).
    Folder(FolderId),
    /// Specific feeds. Duplicate IDs are refreshed once; an unknown ID fails
    /// the whole call with [`MetroError::NotFound`] before anything is
    /// fetched.
    Feeds(&'a [FeedId]),
}

/// Tuning knobs for [`refresh_feeds`].
#[derive(Debug, Default, Clone, Copy)]
pub struct RefreshOptions<'a> {
    /// Worker pool size; `None` means [`DEFAULT_CONCURRENCY`]. `Some(0)` is a
    /// usage error.
    pub concurrency: Option<usize>,
    /// Optional channel receiving [`RefreshEvent`]s while the refresh runs.
    pub progress: Option<&'a Sender<RefreshEvent>>,
    /// Retention window (days) applied by the post-refresh prune; `None`
    /// means [`DEFAULT_WINDOW_DAYS`] (30, `feeds.window_days`, hardcoded in
    /// M1). `Some(0)` is a usage error.
    pub prune_window_days: Option<u32>,
}

/// Compact per-feed status carried by progress events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedStatus {
    /// `200`: the feed was parsed and `new_entries` entries were inserted.
    Updated {
        /// Number of entries inserted (after deduplication).
        new_entries: usize,
    },
    /// `304 Not Modified`.
    NotModified,
    /// The fetch (or its persistence) failed; details are in the report.
    Failed,
}

/// Progress event sent on [`RefreshOptions::progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshEvent {
    /// Sent once before any fetch starts.
    Started {
        /// Number of feeds that will be fetched.
        total: usize,
        /// Number of feeds skipped because they are in backoff.
        skipped: usize,
        /// Number of worker threads that will run.
        workers: usize,
    },
    /// Sent after each feed has been fetched and persisted.
    FeedFinished {
        /// The feed that finished.
        feed_id: FeedId,
        /// How many feeds have finished so far, including this one.
        completed: usize,
        /// Number of feeds being fetched (same as [`RefreshEvent::Started`]).
        total: usize,
        /// What happened to the feed.
        status: FeedStatus,
    },
}

/// Final outcome of one fetched feed.
#[derive(Debug)]
pub enum FeedOutcome {
    /// `200`: the feed was parsed and `new_entries` entries were inserted.
    Updated {
        /// Number of entries inserted (after deduplication).
        new_entries: usize,
    },
    /// `304 Not Modified`.
    NotModified,
    /// The fetch failed (the message is also stored in `error_state`), or
    /// the feed was removed while it was being fetched.
    Failed {
        /// The error, keeping its exit-code variant.
        error: MetroError,
    },
}

impl FeedOutcome {
    /// The compact status used by progress events.
    #[must_use]
    pub fn status(&self) -> FeedStatus {
        match self {
            FeedOutcome::Updated { new_entries } => FeedStatus::Updated {
                new_entries: *new_entries,
            },
            FeedOutcome::NotModified => FeedStatus::NotModified,
            FeedOutcome::Failed { .. } => FeedStatus::Failed,
        }
    }

    /// Returns `true` for [`FeedOutcome::Failed`].
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(self, FeedOutcome::Failed { .. })
    }
}

/// Result of refreshing one feed.
#[derive(Debug)]
pub struct FeedRefresh {
    /// The refreshed feed.
    pub feed_id: FeedId,
    /// What happened.
    pub outcome: FeedOutcome,
}

/// A feed that was not fetched because it is in backoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkippedFeed {
    /// The skipped feed.
    pub feed_id: FeedId,
    /// Unix timestamp after which the feed will be fetched again.
    pub next_retry: u64,
}

/// Summary returned by [`refresh_feeds`].
#[derive(Debug)]
pub struct RefreshReport {
    /// Number of worker threads that ran (`min(concurrency, due feeds)`).
    pub workers: usize,
    /// One result per fetched feed, sorted by feed ID.
    pub results: Vec<FeedRefresh>,
    /// Feeds skipped because of backoff, sorted by feed ID.
    pub skipped: Vec<SkippedFeed>,
    /// Number of entries deleted by the post-refresh prune (all feeds).
    pub pruned: usize,
}

impl RefreshReport {
    /// Total number of new entries inserted across all feeds.
    #[must_use]
    pub fn new_entries(&self) -> usize {
        self.results
            .iter()
            .map(|result| match result.outcome {
                FeedOutcome::Updated { new_entries } => new_entries,
                FeedOutcome::NotModified | FeedOutcome::Failed { .. } => 0,
            })
            .sum()
    }

    /// Iterate over the failed feeds with their errors.
    pub fn failures(&self) -> impl Iterator<Item = (FeedId, &MetroError)> {
        self.results
            .iter()
            .filter_map(|result| match &result.outcome {
                FeedOutcome::Failed { error } => Some((result.feed_id, error)),
                FeedOutcome::Updated { .. } | FeedOutcome::NotModified => None,
            })
    }

    /// Number of feeds fetched successfully (`200` or `304`).
    #[must_use]
    pub fn succeeded(&self) -> usize {
        self.results
            .iter()
            .filter(|result| !result.outcome.is_failure())
            .count()
    }

    /// Map the report to the refresh exit-code contract.
    ///
    /// - `Ok(())` (exit 0) when no fetched feed failed (including when
    ///   nothing was fetched).
    /// - [`MetroError::PartialSuccess`] (exit 5) when at least one feed
    ///   failed and at least one succeeded.
    /// - [`MetroError::Generic`] (exit 1) when every fetched feed failed.
    ///
    /// # Errors
    ///
    /// Returns the error variants listed above.
    pub fn exit_status(&self) -> Result<(), MetroError> {
        let failed = self.results.len() - self.succeeded();
        if failed == 0 {
            Ok(())
        } else if failed == self.results.len() {
            Err(MetroError::Generic(format!(
                "all {failed} feeds failed to refresh"
            )))
        } else {
            Err(MetroError::PartialSuccess(format!(
                "{failed} of {} feeds failed to refresh",
                self.results.len()
            )))
        }
    }
}

/// Refresh feeds in parallel and persist the results.
///
/// See the [module documentation](self) for the pipeline. `conn` must be a
/// migrated connection; it is used only by the calling thread.
///
/// # Errors
///
/// - [`MetroError::Usage`] if `options.concurrency` or
///   `options.prune_window_days` is `Some(0)`.
/// - [`MetroError::NotFound`] if a [`RefreshTarget::Feeds`] ID does not
///   exist (nothing is fetched).
/// - [`MetroError::Generic`] if a database operation fails or a worker
///   thread cannot be spawned. Individual **fetch** failures are not errors:
///   they are reported in [`RefreshReport::results`].
pub fn refresh_feeds<F: FeedFetcher>(
    conn: &mut Connection,
    fetcher: &F,
    target: RefreshTarget<'_>,
    options: &RefreshOptions<'_>,
) -> Result<RefreshReport, MetroError> {
    let pool_size = resolve_concurrency(options.concurrency)?;
    let window_days = options.prune_window_days.unwrap_or(DEFAULT_WINDOW_DAYS);
    validate_window(window_days)?;
    let candidates = load_candidates(conn, target)?;
    let (due, mut skipped) = partition_by_backoff(candidates, now_unix());
    let workers = pool_size.min(due.len());

    emit(
        options.progress,
        RefreshEvent::Started {
            total: due.len(),
            skipped: skipped.len(),
            workers,
        },
    );

    let mut results = run_pool(conn, fetcher, &due, workers, options.progress)?;
    results.sort_by_key(|result| result.feed_id.inner());
    skipped.sort_by_key(|skipped| skipped.feed_id.inner());

    // Runs after every entry has been inserted (pruning spec: "Pruning runs
    // after refresh", also on partial failure).
    let pruned = prune_entries(conn, window_days)?;

    Ok(RefreshReport {
        workers,
        results,
        skipped,
        pruned,
    })
}

/// Validate the requested pool size.
fn resolve_concurrency(requested: Option<usize>) -> Result<usize, MetroError> {
    match requested {
        None => Ok(DEFAULT_CONCURRENCY),
        Some(0) => Err(MetroError::Usage(
            "refresh concurrency must be at least 1".to_string(),
        )),
        Some(size) => Ok(size),
    }
}

/// Everything a worker needs to fetch one feed, loaded before the pool runs.
#[derive(Debug)]
struct FeedJob {
    /// Feed being fetched.
    id: FeedId,
    /// Stored (normalized) feed URL.
    url: String,
    /// Cached `ETag` for conditional GET.
    etag: Option<String>,
    /// Cached `Last-Modified` for conditional GET.
    last_modified: Option<String>,
    /// Backoff deadline (Unix seconds), if any.
    next_retry: Option<i64>,
}

/// Columns selected for a [`FeedJob`], in [`StoredJob::from_row`] order.
const JOB_COLUMNS: &str = "id, url, etag, last_modified, next_retry";

/// Raw [`JOB_COLUMNS`] values, before the row id is validated.
struct StoredJob {
    id: i64,
    url: String,
    etag: Option<String>,
    last_modified: Option<String>,
    next_retry: Option<i64>,
}

impl StoredJob {
    /// Read the job columns from a row selecting [`JOB_COLUMNS`].
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            url: row.get(1)?,
            etag: row.get(2)?,
            last_modified: row.get(3)?,
            next_retry: row.get(4)?,
        })
    }

    /// Validate the row id and build a [`FeedJob`].
    fn into_job(self) -> Result<FeedJob, MetroError> {
        Ok(FeedJob {
            id: feed_id_from_row_id(self.id)?,
            url: self.url,
            etag: self.etag,
            last_modified: self.last_modified,
            next_retry: self.next_retry,
        })
    }
}

/// Load the feeds selected by `target`, ordered by ID.
fn load_candidates(
    conn: &Connection,
    target: RefreshTarget<'_>,
) -> Result<Vec<FeedJob>, MetroError> {
    match target {
        RefreshTarget::All => query_jobs(
            conn,
            &format!("SELECT {JOB_COLUMNS} FROM feeds ORDER BY id"),
            &[],
        ),
        RefreshTarget::Folder(folder_id) => {
            let folder_row_id = folder_id.inner();
            query_jobs(
                conn,
                &format!("SELECT {JOB_COLUMNS} FROM feeds WHERE folder_id = ?1 ORDER BY id"),
                &[&folder_row_id],
            )
        }
        RefreshTarget::Feeds(ids) => {
            let sql = format!("SELECT {JOB_COLUMNS} FROM feeds WHERE id = ?1");
            let mut statement = conn.prepare(&sql)?;
            let mut seen = HashSet::new();
            let mut jobs = Vec::with_capacity(ids.len());
            for id in ids {
                if !seen.insert(id.inner()) {
                    continue;
                }
                let stored = statement
                    .query_row(params![id.inner()], StoredJob::from_row)
                    .optional()?
                    .ok_or_else(|| MetroError::NotFound(format!("feed {id} not found")))?;
                jobs.push(stored.into_job()?);
            }
            Ok(jobs)
        }
    }
}

/// Run a job selector and convert every row.
fn query_jobs(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<FeedJob>, MetroError> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map(params, StoredJob::from_row)?;
    let mut jobs = Vec::new();
    for row in rows {
        jobs.push(row?.into_job()?);
    }
    Ok(jobs)
}

/// Split candidates into due jobs and feeds still in backoff at `now`.
///
/// A feed is in backoff when `next_retry > now`; `NULL` or a past/present
/// deadline means it is due (backoff spec: "Skip feeds in backoff").
fn partition_by_backoff(candidates: Vec<FeedJob>, now: i64) -> (Vec<FeedJob>, Vec<SkippedFeed>) {
    let mut due = Vec::with_capacity(candidates.len());
    let mut skipped = Vec::new();
    for job in candidates {
        match job.next_retry {
            Some(deadline) if deadline > now => skipped.push(SkippedFeed {
                feed_id: job.id,
                // `deadline > now >= 0` in practice; clamp corrupt values.
                next_retry: u64::try_from(deadline).unwrap_or_default(),
            }),
            _ => due.push(job),
        }
    }
    (due, skipped)
}

/// A fetch result handed from a worker to the writer.
struct FetchMessage {
    /// Index of the job in the due-jobs slice.
    index: usize,
    /// The fetch outcome (panics already converted to errors).
    fetched: Result<FetchResult, MetroError>,
}

/// Fetch `jobs` on `workers` scoped threads and persist results as they
/// arrive on the calling thread.
fn run_pool<F: FeedFetcher>(
    conn: &mut Connection,
    fetcher: &F,
    jobs: &[FeedJob],
    workers: usize,
    progress: Option<&Sender<RefreshEvent>>,
) -> Result<Vec<FeedRefresh>, MetroError> {
    if jobs.is_empty() {
        return Ok(Vec::new());
    }

    let next_job = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let (sender, receiver) = mpsc::channel::<FetchMessage>();

    thread::scope(|scope| {
        for worker in 0..workers {
            // mpsc is multi-producer by cloning the sender; there is no
            // borrow-based alternative for handing one to each thread.
            let sender = sender.clone();
            let (next_job, stop) = (&next_job, &stop);
            let spawned = thread::Builder::new()
                .name(format!("metro-refresh-{worker}"))
                .spawn_scoped(scope, move || {
                    worker_loop(fetcher, jobs, next_job, stop, &sender);
                });
            if let Err(error) = spawned {
                stop.store(true, Ordering::Relaxed);
                return Err(MetroError::Generic(format!(
                    "failed to spawn refresh worker: {error}"
                )));
            }
        }
        // Only worker-owned senders remain, so the receiver ends once every
        // worker has finished.
        drop(sender);

        let written = write_results(conn, jobs, &receiver, progress);
        if written.is_err() {
            // Stop handing out jobs; in-flight fetches finish and are
            // discarded when the scope joins the workers.
            stop.store(true, Ordering::Relaxed);
        }
        written
    })
}

/// Worker body: fetch jobs until none remain or the writer asks to stop.
fn worker_loop<F: FeedFetcher>(
    fetcher: &F,
    jobs: &[FeedJob],
    next_job: &AtomicUsize,
    stop: &AtomicBool,
    results: &Sender<FetchMessage>,
) {
    while !stop.load(Ordering::Relaxed) {
        let index = next_job.fetch_add(1, Ordering::Relaxed);
        let Some(job) = jobs.get(index) else {
            return;
        };
        let outcome = fetch_isolated(fetcher, job);
        if results
            .send(FetchMessage {
                index,
                fetched: outcome,
            })
            .is_err()
        {
            // The writer is gone (it hit a database error).
            return;
        }
    }
}

/// Fetch one job, converting a panic in the fetcher into an error so a
/// single misbehaving feed cannot take down the refresh.
fn fetch_isolated<F: FeedFetcher>(fetcher: &F, job: &FeedJob) -> Result<FetchResult, MetroError> {
    panic::catch_unwind(AssertUnwindSafe(|| {
        fetcher.fetch(&job.url, job.etag.as_deref(), job.last_modified.as_deref())
    }))
    .unwrap_or_else(|payload| {
        Err(MetroError::Generic(format!(
            "fetching {} panicked: {}",
            job.url,
            panic_message(payload.as_ref())
        )))
    })
}

/// Best-effort extraction of a panic payload's message.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

/// Writer loop: persist each fetch result and emit progress.
fn write_results(
    conn: &mut Connection,
    jobs: &[FeedJob],
    receiver: &Receiver<FetchMessage>,
    progress: Option<&Sender<RefreshEvent>>,
) -> Result<Vec<FeedRefresh>, MetroError> {
    let total = jobs.len();
    let mut results = Vec::with_capacity(total);

    for message in receiver {
        let job = jobs.get(message.index).ok_or_else(|| {
            MetroError::Generic(format!(
                "refresh worker reported unknown job index {}",
                message.index
            ))
        })?;
        let outcome = persist(conn, job.id, message.fetched)?;

        emit(
            progress,
            RefreshEvent::FeedFinished {
                feed_id: job.id,
                completed: results.len() + 1,
                total,
                status: outcome.status(),
            },
        );
        results.push(FeedRefresh {
            feed_id: job.id,
            outcome,
        });
    }

    Ok(results)
}

/// Persist one fetch outcome and classify it.
///
/// Fetch errors and "feed removed meanwhile" become [`FeedOutcome::Failed`];
/// only database errors propagate.
fn persist(
    conn: &mut Connection,
    feed_id: FeedId,
    fetched: Result<FetchResult, MetroError>,
) -> Result<FeedOutcome, MetroError> {
    match fetched {
        Ok(result) => match store_refresh_success(conn, feed_id, &result) {
            Ok(new_entries) if result.is_not_modified() => {
                debug_assert_eq!(new_entries, 0, "a 304 carries no entries");
                Ok(FeedOutcome::NotModified)
            }
            Ok(new_entries) => Ok(FeedOutcome::Updated { new_entries }),
            Err(error @ MetroError::NotFound(_)) => Ok(FeedOutcome::Failed { error }),
            Err(error) => Err(error),
        },
        Err(error) => {
            store_refresh_failure(conn, feed_id, &error.to_string())?;
            Ok(FeedOutcome::Failed { error })
        }
    }
}

/// Send a progress event, ignoring a disconnected receiver.
fn emit(progress: Option<&Sender<RefreshEvent>>, event: RefreshEvent) {
    if let Some(sender) = progress {
        // Progress is advisory; a caller that stopped listening must not
        // abort the refresh.
        let _ = sender.send(event);
    }
}

#[cfg(all(test, feature = "test-utils"))]
mod tests;
