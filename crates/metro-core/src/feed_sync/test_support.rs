//! Test helpers shared by the refresh and subscribe tests.
//!
//! Provides a [`MockFetcher`] implementing [`FeedFetcher`] (so no test hits
//! the network), builders for canned [`FetchResult`]s, and small database
//! helpers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, params};

use crate::MetroError;
use crate::db::migrations::run_migrations;
use crate::db::open_in_memory;
use crate::feed_sync::FeedFetcher;
use crate::http::client::{FetchResult, FetchStatus};
use crate::models::{FeedId, FeedKind};
use crate::parse::{FeedMetadata, FetchedEntry};

/// One recorded call to [`MockFetcher::fetch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FetchCall {
    pub(crate) url: String,
    pub(crate) etag: Option<String>,
    pub(crate) last_modified: Option<String>,
}

/// A fetcher answering from a closure, recording calls and concurrency.
pub(crate) struct MockFetcher<R> {
    respond: R,
    delay: Duration,
    calls: Mutex<Vec<FetchCall>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl<R> MockFetcher<R>
where
    R: Fn(&str, Option<&str>, Option<&str>) -> Result<FetchResult, MetroError> + Sync,
{
    /// Build a mock answering every fetch with `respond(url, etag, lm)`.
    pub(crate) fn new(respond: R) -> Self {
        Self {
            respond,
            delay: Duration::ZERO,
            calls: Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        }
    }

    /// Sleep for `delay` inside each fetch, to make overlap observable.
    pub(crate) fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// All recorded calls, in call order.
    pub(crate) fn calls(&self) -> Vec<FetchCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Highest number of fetches observed running at the same time.
    pub(crate) fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }
}

impl<R> FeedFetcher for MockFetcher<R>
where
    R: Fn(&str, Option<&str>, Option<&str>) -> Result<FetchResult, MetroError> + Sync,
{
    fn fetch(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<FetchResult, MetroError> {
        let running = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(running, Ordering::SeqCst);
        self.calls.lock().unwrap().push(FetchCall {
            url: url.to_string(),
            etag: etag.map(str::to_string),
            last_modified: last_modified.map(str::to_string),
        });
        if !self.delay.is_zero() {
            thread::sleep(self.delay);
        }
        let result = (self.respond)(url, etag, last_modified);
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        result
    }
}

/// Open a migrated in-memory database.
pub(crate) fn test_conn() -> Connection {
    let mut conn = open_in_memory().unwrap();
    run_migrations(&mut conn).unwrap();
    conn
}

/// Count rows in a table.
pub(crate) fn row_count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// Count the entries stored for one feed.
pub(crate) fn entry_count(conn: &Connection, feed_id: FeedId) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM entries WHERE feed_id = ?1",
        params![feed_id.inner()],
        |row| row.get(0),
    )
    .unwrap()
}

/// A publication date (Unix seconds) one day before the first call, stable
/// for the whole test process.
///
/// Fixture entries must stay inside the pruning window, otherwise the
/// post-refresh prune would delete them; a fixed calendar date would expire
/// as the real clock advances. Being computed once keeps content hashes of
/// guidless fixture entries identical across calls.
pub(crate) fn recent_published() -> u64 {
    static PUBLISHED: OnceLock<u64> = OnceLock::new();
    *PUBLISHED.get_or_init(|| u64::try_from(jiff::Timestamp::now().as_second() - 86_400).unwrap())
}

/// A fetched entry with a guid and optional URL, published recently (see
/// [`recent_published`]).
pub(crate) fn entry(guid: &str, url: Option<&str>) -> FetchedEntry {
    FetchedEntry {
        guid: guid.to_string(),
        url: url.map(str::to_string),
        title: Some(format!("Entry {guid}")),
        author: None,
        published: Some(recent_published()),
        summary: None,
        content: None,
    }
}

/// A `200` result with the given metadata and entries, and no validators.
pub(crate) fn modified(
    kind: FeedKind,
    title: Option<&str>,
    site_url: Option<&str>,
    entries: Vec<FetchedEntry>,
) -> FetchResult {
    FetchResult {
        status: FetchStatus::Modified,
        feed: Some(FeedMetadata {
            kind,
            title: title.map(str::to_string),
            site_url: site_url.map(str::to_string),
        }),
        entries,
        etag: None,
        last_modified: None,
    }
}

/// A `304` result carrying no validators.
pub(crate) fn not_modified() -> FetchResult {
    FetchResult {
        status: FetchStatus::NotModified,
        feed: None,
        entries: Vec::new(),
        etag: None,
        last_modified: None,
    }
}

/// An Atom `200` result titled "Atom Blog" with two entries and validators.
pub(crate) fn atom_result() -> FetchResult {
    let mut result = modified(
        FeedKind::Atom,
        Some("Atom Blog"),
        Some("https://atom.example.com/"),
        vec![
            entry("urn:uuid:entry-1", Some("https://atom.example.com/1")),
            entry("urn:uuid:entry-2", Some("https://atom.example.com/2")),
        ],
    );
    result.etag = Some("\"atom-v1\"".to_string());
    result.last_modified = Some("Fri, 02 Jan 2026 00:00:00 GMT".to_string());
    result
}
