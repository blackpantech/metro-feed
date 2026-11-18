//! End-to-end tests of the refresh workflow (task 14 of the `m1-feeds-core`
//! change; M1 exit criteria "50 feeds refreshed in parallel, 304s on second
//! refresh"; refresh and pruning specs).
//!
//! # What is exercised
//!
//! Only the **public** `metro-core` API is used, with the production
//! [`Fetcher`] (real `ureq` HTTP, conditional GET, `feed-rs` parsing) and a
//! real on-disk SQLite database opened with [`db::open_migrated`]. The feeds
//! are served by [`FeedServer`], a small multi-threaded HTTP/1.1 server on
//! `127.0.0.1` built on `std::net::TcpListener` (the project uses no mock-HTTP
//! crate). The server:
//!
//! - serves [`FEED_COUNT`] feeds at `/feeds/<n>`, rotating between RSS 2.0,
//!   Atom, and JSON Feed so format detection runs at scale;
//! - gives even-numbered feeds an `ETag` and odd-numbered feeds a
//!   `Last-Modified` validator, and answers `304 Not Modified` only when the
//!   request's `If-None-Match` / `If-Modified-Since` matches, so the second
//!   refresh proves that both validators are stored and sent back;
//! - dates each feed's entries relative to "now" (1, 10, and 45 days old):
//!   the 45-day-old one is already outside the default 30-day window, so it
//!   is never inserted, and the 10-day-old one ages out under a 7-day window,
//!   so pruning has something to delete;
//! - sleeps [`RESPONSE_DELAY`] per request and records the peak number of
//!   requests in flight, which shows that feeds are fetched in parallel and
//!   never by more than the worker pool size.
//!
//! # Feature gate
//!
//! The temp-database helper lives behind the `test-utils` feature, so this
//! target declares `required-features = ["test-utils"]` in
//! `crates/metro-core/Cargo.toml`. That was preferred over an inner
//! `#![cfg(feature = "test-utils")]` because Cargo then skips the target
//! entirely (no empty test binary, and `cargo test --test
//! refresh_integration` without the feature fails with an explicit "requires
//! the features: `test-utils`" message instead of silently running nothing).
//!
//! ```sh
//! cargo test -p metro-core --features test-utils --test refresh_integration
//! ```
//!
//! # Network-dependent tests
//!
//! Everything above runs on loopback only. Tests that need the **real**
//! network are marked `#[ignore = "requires network access ..."]` so the
//! default `cargo test` runs (and CI) never touch the internet. Run them
//! explicitly with:
//!
//! ```sh
//! cargo test -p metro-core --features test-utils --test refresh_integration -- --ignored
//! ```
//!
//! `cargo test ... -- --list --ignored` lists them without running anything.
//!
//! # Performance budget
//!
//! Product spec §4: refreshing 50 feeds must take under 10 seconds
//! ([`REFRESH_BUDGET`]). The measured duration is printed (visible with
//! `-- --nocapture`).

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use jiff::fmt::rfc2822::DateTimePrinter;
use metro_core::db;
use metro_core::entries::{EntryFilter, EntryLimit, ReadFilter, list_feed_entries};
use metro_core::feeds::{add_feed, list_feeds};
use metro_core::fetch::{FetchConfig, Fetcher};
use metro_core::models::{Feed, FeedId, FeedKind};
use metro_core::prune::DEFAULT_WINDOW_DAYS;
use metro_core::refresh::{
    DEFAULT_CONCURRENCY, FeedOutcome, RefreshOptions, RefreshReport, RefreshTarget, refresh_feeds,
};
use metro_core::test_utils::temp_db_path;
use rusqlite::Connection;

/// Number of feeds in the M1 exit-criteria scenario.
const FEED_COUNT: usize = 50;

/// Product spec §4: a 50-feed refresh must complete in under 10 seconds.
const REFRESH_BUDGET: Duration = Duration::from_secs(10);

/// Artificial per-request latency. Large enough that the workers overlap
/// (peak concurrency > 1) and a serial refresh would be visibly slower
/// (50 × 50 ms = 2.5 s vs. ~0.35 s with 8 workers), small enough to keep the
/// suite fast.
const RESPONSE_DELAY: Duration = Duration::from_millis(50);

/// `Last-Modified` value served by odd-numbered feeds.
const LAST_MODIFIED: &str = "Wed, 21 Oct 2015 07:28:00 GMT";

/// Seconds in one day.
const DAY: i64 = 86_400;

/// Entries served by every feed: `(label, age in days)`.
///
/// - `fresh` survives every window used here;
/// - `aging` survives the default 30-day window but not a 7-day one;
/// - `stale` is older than the default 30-day window, so it is never
///   inserted (refresh skips out-of-window entries).
const ENTRY_AGES: [(&str, i64); 3] = [("fresh", 1), ("aging", 10), ("stale", 45)];

/// Number of [`ENTRY_AGES`] inside the default window, i.e. the entries a
/// first refresh inserts per feed.
fn entries_in_default_window() -> usize {
    ENTRY_AGES
        .iter()
        .filter(|(_, age_days)| *age_days < i64::from(DEFAULT_WINDOW_DAYS))
        .count()
}

// ---------------------------------------------------------------------------
// Local HTTP feed server
// ---------------------------------------------------------------------------

/// Counters shared between the server threads and the test.
#[derive(Default)]
struct ServerStats {
    /// `200` responses with a feed body.
    full: AtomicUsize,
    /// `304 Not Modified` responses.
    not_modified: AtomicUsize,
    /// Requests currently being answered.
    in_flight: AtomicUsize,
    /// Highest value `in_flight` reached.
    peak_in_flight: AtomicUsize,
}

/// Multi-threaded loopback HTTP server serving [`FEED_COUNT`] feeds.
///
/// One accept thread hands each connection to its own handler thread, so
/// parallel fetches are really answered in parallel. Dropping the server
/// stops the accept loop.
struct FeedServer {
    /// `http://127.0.0.1:<port>`.
    base: String,
    /// Bound address, used to wake the accept loop on shutdown.
    addr: SocketAddr,
    /// Response counters.
    stats: Arc<ServerStats>,
    /// Set on drop; the accept loop exits on its next connection.
    stop: Arc<AtomicBool>,
    /// The accept thread, joined on drop.
    accept_thread: Option<JoinHandle<()>>,
}

impl FeedServer {
    /// Bind an ephemeral loopback port and start serving.
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stats = Arc::new(ServerStats::default());
        let stop = Arc::new(AtomicBool::new(false));
        // Entry dates are relative to the server start so every refresh in a
        // test sees the same documents (stable validators, stable guids).
        let now = Timestamp::now().as_second();

        let accept_thread = {
            let stats = Arc::clone(&stats);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let stats = Arc::clone(&stats);
                    thread::spawn(move || handle_connection(stream, &stats, now));
                }
            })
        };

        Self {
            base: format!("http://{addr}"),
            addr,
            stats,
            stop,
            accept_thread: Some(accept_thread),
        }
    }

    /// URL of feed number `index`.
    fn feed_url(&self, index: usize) -> String {
        format!("{}/feeds/{index}", self.base)
    }

    fn full_responses(&self) -> usize {
        self.stats.full.load(Ordering::SeqCst)
    }

    fn not_modified_responses(&self) -> usize {
        self.stats.not_modified.load(Ordering::SeqCst)
    }

    fn peak_in_flight(&self) -> usize {
        self.stats.peak_in_flight.load(Ordering::SeqCst)
    }
}

impl Drop for FeedServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking `accept` so the loop observes the flag.
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
    }
}

/// A parsed request head: path and lowercased header lines.
struct Request {
    path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    /// First value of header `name` (lowercase).
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Read the request line and headers. A `GET` has no body, so the head is
/// the whole request.
fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let path = request_line.split_whitespace().nth(1)?.to_owned();

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 || line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    Some(Request { path, headers })
}

/// The validator a feed uses for conditional GET.
enum Validator {
    ETag(String),
    LastModified(&'static str),
}

/// Even feeds use an `ETag`, odd feeds `Last-Modified`.
fn validator_for(index: usize) -> Validator {
    if index.is_multiple_of(2) {
        Validator::ETag(format!("\"feed-{index}-v1\""))
    } else {
        Validator::LastModified(LAST_MODIFIED)
    }
}

/// Answer one connection, counting it in the concurrency statistics.
fn handle_connection(mut stream: TcpStream, stats: &ServerStats, now: i64) {
    let Some(request) = read_request(&stream) else {
        return;
    };
    let in_flight = stats.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    stats.peak_in_flight.fetch_max(in_flight, Ordering::SeqCst);
    thread::sleep(RESPONSE_DELAY);

    let response = build_response(&request, stats, now);
    let _ = stream.write_all(response.as_bytes());
    stats.in_flight.fetch_sub(1, Ordering::SeqCst);
}

/// Route a request to a feed and apply conditional-GET semantics.
fn build_response(request: &Request, stats: &ServerStats, now: i64) -> String {
    let index = request
        .path
        .strip_prefix("/feeds/")
        .and_then(|rest| rest.parse::<usize>().ok())
        .filter(|index| *index < FEED_COUNT);
    let Some(index) = index else {
        return http_response("404 Not Found", &[], "text/plain", "not found");
    };

    let (validator_header, validator_value, matches) = match validator_for(index) {
        Validator::ETag(etag) => {
            let matches = request.header("if-none-match") == Some(etag.as_str());
            ("ETag", etag, matches)
        }
        Validator::LastModified(date) => {
            // The document never changes, so an exact echo of our own
            // Last-Modified value means "not modified since".
            let matches = request.header("if-modified-since") == Some(date);
            ("Last-Modified", date.to_owned(), matches)
        }
    };
    let headers = [(validator_header, validator_value.as_str())];

    if matches {
        stats.not_modified.fetch_add(1, Ordering::SeqCst);
        return http_response("304 Not Modified", &headers, "text/plain", "");
    }

    stats.full.fetch_add(1, Ordering::SeqCst);
    let (content_type, body) = feed_document(index, now);
    http_response("200 OK", &headers, content_type, &body)
}

/// Serialize a complete `Connection: close` HTTP/1.1 response.
fn http_response(status: &str, headers: &[(&str, &str)], content_type: &str, body: &str) -> String {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        let _ = write!(out, "{name}: {value}\r\n");
    }
    out.push_str("\r\n");
    out.push_str(body);
    out
}

/// Expected format of feed number `index`.
fn kind_for(index: usize) -> FeedKind {
    match index % 3 {
        0 => FeedKind::Rss,
        1 => FeedKind::Atom,
        _ => FeedKind::Json,
    }
}

/// Title declared by feed number `index`.
fn title_for(index: usize) -> String {
    format!("Feed {index}")
}

/// Guid of entry `label` in feed number `index`, unique across all test
/// feeds (dedup itself only requires uniqueness within one feed).
fn guid_for(index: usize, label: &str) -> String {
    format!("feed-{index}-{label}")
}

/// Build the body of feed number `index` in its format.
fn feed_document(index: usize, now: i64) -> (&'static str, String) {
    let title = title_for(index);
    let site = format!("https://site-{index}.example.com/");
    let entries = ENTRY_AGES.iter().map(|(label, age_days)| {
        let published = Timestamp::from_second(now - age_days * DAY).unwrap();
        (
            guid_for(index, label),
            format!("{site}posts/{label}"),
            format!("{title} {label}"),
            published,
        )
    });

    let mut body = String::new();
    match kind_for(index) {
        FeedKind::Rss => {
            let printer = DateTimePrinter::new();
            let _ = write!(
                body,
                "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel>\
                 <title>{title}</title><link>{site}</link><description>d</description>"
            );
            for (guid, url, entry_title, published) in entries {
                let date = printer.timestamp_to_rfc9110_string(&published).unwrap();
                let _ = write!(
                    body,
                    "<item><title>{entry_title}</title><link>{url}</link>\
                     <guid isPermaLink=\"false\">{guid}</guid><pubDate>{date}</pubDate></item>"
                );
            }
            body.push_str("</channel></rss>");
            ("application/rss+xml", body)
        }
        FeedKind::Atom => {
            let _ = write!(
                body,
                "<?xml version=\"1.0\"?><feed xmlns=\"http://www.w3.org/2005/Atom\">\
                 <title>{title}</title><link rel=\"alternate\" href=\"{site}\"/>\
                 <id>urn:feed:{index}</id><updated>{}</updated>",
                Timestamp::from_second(now).unwrap()
            );
            for (guid, url, entry_title, published) in entries {
                let _ = write!(
                    body,
                    "<entry><title>{entry_title}</title><link rel=\"alternate\" href=\"{url}\"/>\
                     <id>{guid}</id><published>{published}</published>\
                     <updated>{published}</updated></entry>"
                );
            }
            body.push_str("</feed>");
            ("application/atom+xml", body)
        }
        // `kind_for` only yields RSS, Atom, or JSON Feed; YouTube is not a
        // wire format of its own.
        FeedKind::Json | FeedKind::Youtube => {
            let items: Vec<String> = entries
                .map(|(guid, url, entry_title, published)| {
                    format!(
                        "{{\"id\":\"{guid}\",\"url\":\"{url}\",\"title\":\"{entry_title}\",\
                         \"content_text\":\"c\",\"date_published\":\"{published}\"}}"
                    )
                })
                .collect();
            let _ = write!(
                body,
                "{{\"version\":\"https://jsonfeed.org/version/1.1\",\"title\":\"{title}\",\
                 \"home_page_url\":\"{site}\",\"items\":[{}]}}",
                items.join(",")
            );
            ("application/feed+json", body)
        }
    }
}

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

/// Deletes a temp database and its WAL sidecar files on drop.
struct TempDb(PathBuf);

impl Drop for TempDb {
    fn drop(&mut self) {
        let path = self.0.as_os_str();
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.to_owned();
            file.push(suffix);
            let _ = std::fs::remove_file(file);
        }
    }
}

/// A migrated temp database subscribed to every feed of a running server.
///
/// Field order matters: the connection is dropped before the database files
/// are removed (required on Windows).
struct Harness {
    conn: Connection,
    _db: TempDb,
    server: FeedServer,
    /// Feed IDs, indexed by feed number.
    feed_ids: Vec<FeedId>,
}

impl Harness {
    fn new() -> Self {
        let server = FeedServer::start();
        let path = temp_db_path();
        let mut conn = db::open_migrated(&path).unwrap();
        let db = TempDb(path);

        let feed_ids = (0..FEED_COUNT)
            .map(|index| {
                let outcome = add_feed(&mut conn, &server.feed_url(index), None, None).unwrap();
                assert!(outcome.is_new());
                outcome.id
            })
            .collect();

        Self {
            conn,
            _db: db,
            server,
            feed_ids,
        }
    }

    /// Refresh every feed with the production fetcher and return the report
    /// together with the wall-clock duration.
    fn refresh(&mut self, prune_window_days: Option<u32>) -> (RefreshReport, Duration) {
        // Short timeouts: a hung loopback request should fail the test fast
        // instead of waiting for the 30 s / 60 s production defaults.
        let fetcher = Fetcher::new(FetchConfig {
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(5),
            ..FetchConfig::default()
        });
        let options = RefreshOptions {
            prune_window_days,
            ..RefreshOptions::default()
        };

        let started = Instant::now();
        let report = refresh_feeds(&mut self.conn, &fetcher, RefreshTarget::All, &options).unwrap();
        (report, started.elapsed())
    }

    /// The stored feed row of feed number `index`.
    fn feed(&self, index: usize) -> Feed {
        let id = self.feed_ids[index];
        list_feeds(&self.conn, None)
            .unwrap()
            .into_iter()
            .find(|feed| feed.id() == id)
            .unwrap()
    }

    /// Guids currently stored for feed number `index`.
    fn guids(&self, index: usize) -> HashSet<String> {
        let filter = EntryFilter {
            read: ReadFilter::Any,
            limit: EntryLimit::Unlimited,
        };
        list_feed_entries(&self.conn, self.feed_ids[index], &filter)
            .unwrap()
            .into_iter()
            .map(|entry| entry.guid().to_owned())
            .collect()
    }
}

/// Expected guid set of feed number `index` for the given labels.
fn expected_guids(index: usize, labels: &[&str]) -> HashSet<String> {
    labels.iter().map(|label| guid_for(index, label)).collect()
}

/// Assert every result has the expected outcome and that each feed appears.
fn assert_outcomes(
    harness: &Harness,
    report: &RefreshReport,
    expected: impl Fn(&FeedOutcome) -> bool,
) {
    assert_eq!(report.results.len(), FEED_COUNT);
    assert!(report.skipped.is_empty(), "skipped: {:?}", report.skipped);
    let fetched: HashSet<FeedId> = report.results.iter().map(|result| result.feed_id).collect();
    let subscribed: HashSet<FeedId> = harness.feed_ids.iter().copied().collect();
    assert_eq!(fetched, subscribed);
    for result in &report.results {
        assert!(
            expected(&result.outcome),
            "feed {} had outcome {:?}",
            result.feed_id,
            result.outcome
        );
    }
    assert!(report.exit_status().is_ok());
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// M1 exit criterion / refresh spec "50 feeds refresh in parallel" / product
/// spec §4 budget.
#[test]
fn fifty_feeds_refresh_in_parallel_within_budget() {
    let mut harness = Harness::new();

    let (report, elapsed) = harness.refresh(None);
    println!("first refresh of {FEED_COUNT} feeds: {elapsed:?}");

    assert!(
        elapsed < REFRESH_BUDGET,
        "refresh of {FEED_COUNT} feeds took {elapsed:?} (budget {REFRESH_BUDGET:?})"
    );
    assert_eq!(report.workers, DEFAULT_CONCURRENCY);
    assert_outcomes(
        &harness,
        &report,
        |outcome| matches!(outcome, FeedOutcome::Updated { new_entries } if *new_entries == entries_in_default_window()),
    );
    assert_eq!(
        report.new_entries(),
        FEED_COUNT * entries_in_default_window()
    );

    // Every feed was downloaded exactly once, in parallel, never by more
    // than the worker pool size.
    assert_eq!(harness.server.full_responses(), FEED_COUNT);
    assert_eq!(harness.server.not_modified_responses(), 0);
    let peak = harness.server.peak_in_flight();
    println!("peak concurrent requests: {peak}");
    assert!(peak > 1, "feeds were fetched serially (peak {peak})");
    assert!(peak <= DEFAULT_CONCURRENCY, "peak {peak} exceeds the pool");

    // Metadata from the first successful fetch: format-detected kind, the
    // declared title, and the validator the server sent.
    for index in 0..FEED_COUNT {
        let feed = harness.feed(index);
        assert_eq!(feed.kind(), kind_for(index), "feed {index}");
        assert_eq!(feed.title(), title_for(index));
        assert!(feed.last_fetch().is_some());
        assert_eq!(feed.error_state(), None);
        match validator_for(index) {
            Validator::ETag(etag) => assert_eq!(feed.etag(), Some(etag.as_str())),
            Validator::LastModified(date) => assert_eq!(feed.last_modified(), Some(date)),
        }
    }
}

/// M1 exit criterion "304s on second refresh" / feed-fetching spec "304 Not
/// Modified returns empty entries": both `ETag` and `Last-Modified` are
/// stored and sent back.
#[test]
fn second_refresh_gets_not_modified_via_etag_and_last_modified() {
    let mut harness = Harness::new();
    let (first, _) = harness.refresh(None);
    assert!(first.exit_status().is_ok());
    let before: Vec<HashSet<String>> = (0..FEED_COUNT).map(|index| harness.guids(index)).collect();

    let (report, elapsed) = harness.refresh(None);
    println!("second (conditional) refresh of {FEED_COUNT} feeds: {elapsed:?}");

    assert!(elapsed < REFRESH_BUDGET, "second refresh took {elapsed:?}");
    assert_outcomes(&harness, &report, |outcome| {
        matches!(outcome, FeedOutcome::NotModified)
    });
    assert_eq!(report.new_entries(), 0);
    assert_eq!(report.pruned, 0);

    // Every feed answered 304: even feeds matched If-None-Match, odd feeds
    // matched If-Modified-Since. No body was downloaded twice.
    assert_eq!(harness.server.not_modified_responses(), FEED_COUNT);
    assert_eq!(harness.server.full_responses(), FEED_COUNT);

    for (index, guids) in before.iter().enumerate() {
        assert_eq!(&harness.guids(index), guids, "feed {index} entries changed");
        let feed = harness.feed(index);
        // 304 leaves metadata and the cached validators untouched.
        assert_eq!(feed.kind(), kind_for(index));
        assert_eq!(feed.title(), title_for(index));
        match validator_for(index) {
            Validator::ETag(etag) => assert_eq!(feed.etag(), Some(etag.as_str())),
            Validator::LastModified(date) => assert_eq!(feed.last_modified(), Some(date)),
        }
    }
}

/// Pruning spec "Pruning runs after refresh": entries already outside the
/// window are never inserted (so nothing churns), and stored entries that
/// age out are deleted, also after a refresh where every feed answered 304
/// (pruning covers all feeds, not only updated ones).
#[test]
fn refresh_prunes_entries_outside_the_window() {
    let mut harness = Harness::new();

    // Default 30-day window: the 45-day-old entry of every feed is skipped at
    // insert time, so it is neither counted as new nor pruned.
    let (first, _) = harness.refresh(None);
    assert!(first.exit_status().is_ok());
    assert_eq!(
        first.new_entries(),
        FEED_COUNT * entries_in_default_window()
    );
    assert_eq!(first.pruned, 0);
    for index in 0..FEED_COUNT {
        assert_eq!(
            harness.guids(index),
            expected_guids(index, &["fresh", "aging"]),
            "feed {index}"
        );
    }

    // 7-day window on a refresh that downloads nothing: the stored
    // 10-day-old entries have aged out of it and are pruned, and only they
    // are counted.
    let (second, _) = harness.refresh(Some(7));
    assert_outcomes(&harness, &second, |outcome| {
        matches!(outcome, FeedOutcome::NotModified)
    });
    assert_eq!(second.pruned, FEED_COUNT);
    for index in 0..FEED_COUNT {
        assert_eq!(
            harness.guids(index),
            expected_guids(index, &["fresh"]),
            "feed {index}"
        );
    }
}

/// Real-network smoke test: subscribe to a well-known public feed and
/// refresh it twice through the full pipeline (DNS, TLS, redirects, real
/// server validators).
///
/// Ignored by default so `cargo test` and CI never depend on the internet.
/// Run with `-- --ignored` (see the module documentation). The assertions
/// avoid anything the publisher controls (entry count, titles) beyond "it
/// parses and has entries".
#[test]
#[ignore = "requires network access; run with `cargo test -p metro-core --features test-utils --test refresh_integration -- --ignored`"]
fn real_network_refresh_smoke() {
    /// The Rust blog's Atom feed: long-lived, served over HTTPS with validators.
    const PUBLIC_FEED: &str = "https://blog.rust-lang.org/feed.xml";

    let path = temp_db_path();
    let mut conn = db::open_migrated(&path).unwrap();
    let _db = TempDb(path);
    let feed_id = add_feed(&mut conn, PUBLIC_FEED, None, None).unwrap().id;
    let fetcher = Fetcher::default();
    let options = RefreshOptions::default();

    let first = refresh_feeds(
        &mut conn,
        &fetcher,
        RefreshTarget::Feeds(&[feed_id]),
        &options,
    )
    .unwrap();
    assert_eq!(first.results.len(), 1);
    assert!(
        matches!(first.results[0].outcome, FeedOutcome::Updated { new_entries } if new_entries > 0),
        "first fetch: {:?}",
        first.results[0].outcome
    );

    // The second refresh must succeed; whether it is a 304 depends on the
    // publisher's caching, so only success is asserted.
    let second = refresh_feeds(
        &mut conn,
        &fetcher,
        RefreshTarget::Feeds(&[feed_id]),
        &options,
    )
    .unwrap();
    assert!(second.exit_status().is_ok(), "{:?}", second.results);
    drop(conn);
}
