//! `metro refresh` and `metro view` commands.
//!
//! ```text
//! metro refresh [<f-id>...] [--folder <name>] [--jobs <n>]
//! metro view    <f-id> [--all|--unread] [--limit <n>]
//! ```
//!
//! Both are thin adapters over `metro-core` ("two frontends, one core"):
//! `refresh` calls `metro_core::feed_sync::refresh_feeds` and `view` calls
//! `metro_core::store::entries::list_feed_entries`. This module only maps flags to
//! core arguments and renders the results.
//!
//! # `refresh`
//!
//! - **Target**: every feed by default, the listed feeds (`f12 f13`), or one
//!   feed folder (`--folder`, resolved in the feed namespace with
//!   `metro_core::store::folders::find_folder`). Feed IDs and `--folder` are
//!   mutually exclusive (a usage error, exit 2).
//! - **Workers**: `--jobs <n>` sets the worker pool size; without it the
//!   core default (`DEFAULT_CONCURRENCY`, 8) applies. `--jobs 0` is rejected
//!   by the core as a usage error (exit 2).
//! - **Backoff and pruning** happen inside the core: feeds in backoff are
//!   skipped (and reported), and old entries are pruned after the refresh.
//! - **Progress** goes to **stderr**, and only when stderr is a terminal and
//!   `--json` is not set: a single `Refreshing feeds: 10/50` line rewritten
//!   in place (`\r`) from the core's progress channel by a helper thread.
//!   stdout carries only the report, so pipes and `--json` stay clean.
//!   Progress is advisory: a write failure on stderr stops the progress
//!   line, never the refresh.
//! - **Report** (stdout): one row per *failed* feed (`id  failed  title
//!   error`) and per *skipped* feed (`id  skipped  title  retry after
//!   <time>`), then a summary line with the totals. Feeds that were updated
//!   or not modified are only counted, so a 50-feed refresh stays readable.
//!   With `--json`: one object with the totals and a `feeds` array holding
//!   every fetched and skipped feed with its `status` (`updated`,
//!   `not_modified`, `failed`, `skipped`).
//!
//! ## Exit codes (`refresh` spec, MVP spec §7)
//!
//! The report is always printed first; the exit code then comes from
//! `RefreshReport::exit_status` and the error is printed to stderr:
//!
//! | Situation                                         | Exit |
//! |---------------------------------------------------|------|
//! | no fetched feed failed (also: nothing to fetch, only skipped feeds) | 0 |
//! | some fetched feeds failed, at least one succeeded | 5 (`PartialSuccess`) |
//! | every fetched feed failed                         | 1 (`Generic`) |
//! | unknown feed ID or `--folder`                     | 3 (nothing fetched) |
//! | `--jobs 0`, IDs together with `--folder`          | 2 |
//!
//! # `view`
//!
//! Lists one feed's entries, newest first. The flags map to an
//! `EntryFilter`:
//!
//! | Flags          | Read state | How many |
//! |----------------|------------|----------|
//! | (none)         | all        | the `DEFAULT_RECENT_LIMIT` (20) most recent |
//! | `--all`        | all        | every entry |
//! | `--unread`     | unread     | the 20 most recent unread |
//! | `--limit <n>`  | (as above) | at most `n` (overrides the default and `--all`) |
//!
//! `--all` and `--unread` are mutually exclusive (exit 2); `--limit 0` is a
//! usage error (exit 2) and an unknown feed is not found (exit 3).
//!
//! Output: one entry per line, `id  published  read-state  title  url`
//! (published as RFC 3339 UTC, `-` when the feed gave no date). With
//! `--json`: NDJSON, one `Entry` object per line (`"id":"e4031"`,
//! `"feed_id":"f12"`: prefixed strings, as in text). Listing never marks
//! entries read. Titles and URLs come from remote feeds and are sanitized by
//! the shared [`Output`].

use std::collections::HashMap;
use std::fmt::Display;
use std::io::Write;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use clap::Args;
use metro_core::MetroError;
use metro_core::feed_sync::{
    FeedFetcher, FeedOutcome, RefreshEvent, RefreshOptions, RefreshReport, RefreshTarget,
    refresh_feeds,
};
use metro_core::http::client::Fetcher;
use metro_core::models::{Entry, FeedId, FolderKind};
use metro_core::store::entries::{EntryFilter, EntryLimit, ReadFilter, list_feed_entries};
use metro_core::store::feeds::list_feeds;
use metro_core::store::folders::find_folder;
use rusqlite::Connection;
use serde::{Serialize, Serializer};

use crate::output::{Output, format_timestamp, plural};

/// Arguments of `metro refresh`.
#[derive(Debug, Args)]
pub struct RefreshArgs {
    /// Feeds to refresh (e.g. f12); default: every feed
    #[arg(value_name = "FEED_ID", conflicts_with = "folder")]
    pub ids: Vec<FeedId>,
    /// Only refresh the feeds of this feed folder
    #[arg(long, value_name = "NAME")]
    pub folder: Option<String>,
    /// Number of parallel fetch workers [default: 8]
    #[arg(short = 'j', long, value_name = "N")]
    pub jobs: Option<usize>,
}

/// Arguments of `metro view`.
#[derive(Debug, Args)]
pub struct ViewArgs {
    /// Feed whose entries to list (e.g. f12)
    #[arg(value_name = "FEED_ID")]
    pub id: FeedId,
    /// List every entry instead of only the 20 most recent
    #[arg(long, conflicts_with = "unread")]
    pub all: bool,
    /// Only list unread entries
    #[arg(long)]
    pub unread: bool,
    /// List at most N entries
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

impl ViewArgs {
    /// Translate the flags into the core filter (see the module table).
    fn filter(&self) -> EntryFilter {
        let read = if self.unread {
            ReadFilter::Unread
        } else {
            ReadFilter::Any
        };
        let limit = match (self.limit, self.all) {
            (Some(count), _) => EntryLimit::AtMost(count),
            (None, true) => EntryLimit::Unlimited,
            (None, false) => EntryLimit::Recent,
        };
        EntryFilter { read, limit }
    }
}

/// Run `metro refresh` with the production HTTP fetcher.
///
/// `progress` receives the progress line when set (the caller decides when
/// progress is appropriate: stderr is a terminal and `--json` is off).
///
/// # Errors
///
/// See the module's exit-code table. The report is written to `output`
/// before a partial or total failure is returned.
pub fn run_refresh(
    args: &RefreshArgs,
    conn: &mut Connection,
    output: &mut Output<'_>,
    progress: Option<&mut (dyn Write + Send)>,
) -> Result<(), MetroError> {
    refresh(args, conn, output, &Fetcher::default(), progress)
}

/// Run `metro view`.
///
/// # Errors
///
/// [`MetroError::NotFound`] for an unknown feed, [`MetroError::Usage`] for
/// `--limit 0`, [`MetroError::Generic`] for database or output failures.
pub fn run_view(
    args: &ViewArgs,
    conn: &Connection,
    output: &mut Output<'_>,
) -> Result<(), MetroError> {
    let entries = list_feed_entries(conn, args.id, &args.filter())?;
    output.list(&entries, entry_columns)
}

/// Text columns of one entry in `view`: `id  published  state  title  url`.
fn entry_columns(entry: &Entry) -> Vec<String> {
    vec![
        entry.id().to_string(),
        entry
            .published()
            .map_or_else(|| "-".to_string(), format_timestamp),
        if entry.is_read() { "read" } else { "unread" }.to_string(),
        entry.title().to_string(),
        entry.url().to_string(),
    ]
}

/// `refresh` with an injected fetcher, so tests never reach the network.
fn refresh<F: FeedFetcher>(
    args: &RefreshArgs,
    conn: &mut Connection,
    output: &mut Output<'_>,
    fetcher: &F,
    progress: Option<&mut (dyn Write + Send)>,
) -> Result<(), MetroError> {
    // Resolve the folder before anything is fetched: an unknown folder
    // fails without network access.
    let folder = args
        .folder
        .as_deref()
        .map(|name| find_folder(conn, name, FolderKind::Feeds))
        .transpose()?;
    let target = match &folder {
        Some(folder) => RefreshTarget::Folder(folder.id()),
        None if args.ids.is_empty() => RefreshTarget::All,
        None => RefreshTarget::Feeds(&args.ids),
    };

    let report = refresh_with_progress(conn, fetcher, target, args.jobs, progress)?;
    render_report(conn, output, &report)?;
    report.exit_status()
}

/// Run the core refresh, rendering its progress events to `progress` (when
/// set) from a scoped helper thread while the calling thread does the work.
fn refresh_with_progress<F: FeedFetcher>(
    conn: &mut Connection,
    fetcher: &F,
    target: RefreshTarget<'_>,
    jobs: Option<usize>,
    progress: Option<&mut (dyn Write + Send)>,
) -> Result<RefreshReport, MetroError> {
    let Some(sink) = progress else {
        let options = RefreshOptions {
            concurrency: jobs,
            ..RefreshOptions::default()
        };
        return refresh_feeds(conn, fetcher, target, &options);
    };

    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        let renderer = scope.spawn(move || render_progress(&receiver, sink));
        let result = {
            // Moved into this block so it is dropped when the refresh
            // returns, which ends the renderer's receive loop.
            let sender = sender;
            let options = RefreshOptions {
                concurrency: jobs,
                progress: Some(&sender),
                ..RefreshOptions::default()
            };
            refresh_feeds(conn, fetcher, target, &options)
        };
        // Progress is advisory: a panicked renderer does not fail the
        // refresh, whose results are already persisted.
        let _ = renderer.join();
        result
    })
}

/// Draw the progress line until the sending side is dropped.
///
/// Writes `\rRefreshing feeds: <done>/<total>` after each event and a final
/// newline, so the summary starts on its own line. Nothing is drawn when no
/// feed is due. The first write error stops drawing (the events are still
/// drained so the channel never fills up).
fn render_progress(receiver: &Receiver<RefreshEvent>, sink: &mut (dyn Write + Send)) {
    let mut drawing = true;
    let mut drew = false;
    for event in receiver {
        let (completed, total) = match event {
            RefreshEvent::Started { total, .. } => (0, total),
            RefreshEvent::FeedFinished {
                completed, total, ..
            } => (completed, total),
        };
        if drawing && total > 0 {
            drawing = write!(sink, "\rRefreshing feeds: {completed}/{total}")
                .and_then(|()| sink.flush())
                .is_ok();
            drew |= drawing;
        }
    }
    if drew {
        let _ = writeln!(sink);
    }
}

/// JSON shape of the `refresh` report.
#[derive(Debug, Serialize)]
struct RefreshRecord<'a> {
    /// Feeds fetched (successfully or not); skipped feeds are not counted.
    fetched: usize,
    /// Fetched feeds that succeeded (`200` or `304`).
    succeeded: usize,
    /// Fetched feeds that failed.
    failed: usize,
    /// Feeds skipped because of backoff.
    skipped: usize,
    /// Entries inserted across all feeds.
    new_entries: usize,
    /// Entries deleted by the post-refresh prune.
    pruned: usize,
    /// Worker threads that ran.
    workers: usize,
    /// Every fetched or skipped feed, by feed ID.
    feeds: Vec<FeedRecord<'a>>,
}

/// One feed of the `refresh` JSON report.
#[derive(Debug, Serialize)]
struct FeedRecord<'a> {
    /// The feed.
    id: FeedId,
    /// Its title (absent if the feed was removed during the refresh).
    title: Option<&'a str>,
    /// What happened, tagged by `status`.
    #[serde(flatten)]
    status: FeedStatusRecord<'a>,
}

/// Per-feed status in the `refresh` JSON report.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum FeedStatusRecord<'a> {
    /// `200`: parsed, `new_entries` inserted.
    Updated {
        /// Entries inserted.
        new_entries: usize,
    },
    /// `304 Not Modified`.
    NotModified,
    /// The fetch failed.
    Failed {
        /// The error, serialized as its message (also stored as the feed's
        /// error state).
        #[serde(serialize_with = "serialize_display")]
        error: &'a MetroError,
        /// Exit code of the error's kind (4 network, 1 unparsable …).
        exit_code: i32,
    },
    /// Not fetched: the feed is in backoff.
    Skipped {
        /// Unix timestamp after which the feed is fetched again.
        next_retry: u64,
    },
}

/// Serialize a value as its `Display` text.
fn serialize_display<T: Display, S: Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

/// Write the refresh report (rows for problems, then totals).
fn render_report(
    conn: &Connection,
    output: &mut Output<'_>,
    report: &RefreshReport,
) -> Result<(), MetroError> {
    // One query for every title instead of one per reported feed.
    let feeds = list_feeds(conn, None)?;
    let titles: HashMap<FeedId, &str> = feeds.iter().map(|f| (f.id(), f.title())).collect();
    let title = |id: FeedId| titles.get(&id).copied();

    let mut records: Vec<FeedRecord<'_>> = report
        .results
        .iter()
        .map(|result| FeedRecord {
            id: result.feed_id,
            title: title(result.feed_id),
            status: match &result.outcome {
                FeedOutcome::Updated { new_entries } => FeedStatusRecord::Updated {
                    new_entries: *new_entries,
                },
                FeedOutcome::NotModified => FeedStatusRecord::NotModified,
                FeedOutcome::Failed { error } => FeedStatusRecord::Failed {
                    error,
                    exit_code: error.exit_code(),
                },
            },
        })
        .chain(report.skipped.iter().map(|skipped| FeedRecord {
            id: skipped.feed_id,
            title: title(skipped.feed_id),
            status: FeedStatusRecord::Skipped {
                next_retry: skipped.next_retry,
            },
        }))
        .collect();
    records.sort_by_key(|record| record.id.inner());

    let rows: Vec<Vec<String>> = records.iter().filter_map(problem_row).collect();

    let fetched = report.results.len();
    let succeeded = report.succeeded();
    let record = RefreshRecord {
        fetched,
        succeeded,
        failed: fetched - succeeded,
        skipped: report.skipped.len(),
        new_entries: report.new_entries(),
        pruned: report.pruned,
        workers: report.workers,
        feeds: records,
    };
    let message = summary_message(&record);
    output.report(&record, &rows, &message)
}

/// Human row for a failed or skipped feed; `None` for successful feeds.
fn problem_row(record: &FeedRecord<'_>) -> Option<Vec<String>> {
    let (state, detail) = match &record.status {
        FeedStatusRecord::Failed { error, .. } => ("failed", error.to_string()),
        FeedStatusRecord::Skipped { next_retry } => (
            "skipped",
            format!("in backoff, retry after {}", format_timestamp(*next_retry)),
        ),
        FeedStatusRecord::Updated { .. } | FeedStatusRecord::NotModified => return None,
    };
    Some(vec![
        record.id.to_string(),
        state.to_string(),
        record.title.unwrap_or("-").to_string(),
        detail,
    ])
}

/// The summary line, e.g. `Refreshed 3 feeds: 2 ok, 1 failed, 0 skipped;
/// 5 new entries, 0 pruned`.
fn summary_message(record: &RefreshRecord<'_>) -> String {
    format!(
        "Refreshed {} {}: {} ok, {} failed, {} skipped; {} new {}, {} pruned",
        record.fetched,
        plural(record.fetched, "feed", "feeds"),
        record.succeeded,
        record.failed,
        record.skipped,
        record.new_entries,
        plural(record.new_entries, "entry", "entries"),
        record.pruned,
    )
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::{Cli, Commands};

    fn parse(args: &[&str]) -> Commands {
        let command_line = std::iter::once("metro").chain(args.iter().copied());
        Cli::try_parse_from(command_line).unwrap().command.unwrap()
    }

    fn parse_refresh(args: &[&str]) -> RefreshArgs {
        match parse(&[&["refresh"], args].concat()) {
            Commands::Refresh(args) => args,
            other => panic!("expected refresh, got {other:?}"),
        }
    }

    fn parse_view(args: &[&str]) -> ViewArgs {
        match parse(&[&["view"], args].concat()) {
            Commands::View(args) => args,
            other => panic!("expected view, got {other:?}"),
        }
    }

    fn parse_error_code(args: &[&str]) -> i32 {
        let command_line = std::iter::once("metro").chain(args.iter().copied());
        Cli::try_parse_from(command_line).unwrap_err().exit_code()
    }

    #[test]
    fn test_parse_refresh_defaults() {
        let args = parse_refresh(&[]);
        assert_eq!(args.ids, []);
        assert_eq!(args.folder, None);
        assert_eq!(args.jobs, None);
    }

    #[test]
    fn test_parse_refresh_ids_folder_and_jobs() {
        let args = parse_refresh(&["f12", "f13", "--jobs", "4"]);
        assert_eq!(args.ids, vec![FeedId(12), FeedId(13)]);
        assert_eq!(args.jobs, Some(4));

        let args = parse_refresh(&["--folder", "Tech", "-j", "2"]);
        assert_eq!(args.folder.as_deref(), Some("Tech"));
        assert_eq!(args.jobs, Some(2));
    }

    #[test]
    fn test_parse_refresh_usage_errors() {
        for args in [
            &["refresh", "f1", "--folder", "Tech"][..],
            &["refresh", "12"][..],
            &["refresh", "--jobs", "many"][..],
            &["refresh", "--jobs", "-1"][..],
        ] {
            assert_eq!(parse_error_code(args), 2, "args {args:?}");
        }
    }

    #[test]
    fn test_parse_view_flags_map_to_filter() {
        let cases = [
            (&["f1"][..], ReadFilter::Any, EntryLimit::Recent),
            (&["f1", "--all"][..], ReadFilter::Any, EntryLimit::Unlimited),
            (
                &["f1", "--unread"][..],
                ReadFilter::Unread,
                EntryLimit::Recent,
            ),
            (
                &["f1", "--limit", "10"][..],
                ReadFilter::Any,
                EntryLimit::AtMost(10),
            ),
            (
                &["f1", "--all", "--limit", "3"][..],
                ReadFilter::Any,
                EntryLimit::AtMost(3),
            ),
            (
                &["f1", "--unread", "--limit", "5"][..],
                ReadFilter::Unread,
                EntryLimit::AtMost(5),
            ),
        ];
        for (args, read, limit) in cases {
            let view = parse_view(args);
            assert_eq!(view.id, FeedId(1));
            assert_eq!(view.filter(), EntryFilter { read, limit }, "args {args:?}");
        }
    }

    #[test]
    fn test_parse_view_usage_errors() {
        for args in [
            &["view"][..],
            &["view", "12"][..],
            &["view", "e1"][..],
            &["view", "f1", "--all", "--unread"][..],
            &["view", "f1", "--limit", "x"][..],
        ] {
            assert_eq!(parse_error_code(args), 2, "args {args:?}");
        }
    }

    #[test]
    fn test_render_progress_draws_one_line() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(RefreshEvent::Started {
                total: 2,
                skipped: 0,
                workers: 2,
            })
            .unwrap();
        for completed in 1..=2 {
            sender
                .send(RefreshEvent::FeedFinished {
                    feed_id: FeedId(completed as u64),
                    completed,
                    total: 2,
                    status: metro_core::feed_sync::FeedStatus::NotModified,
                })
                .unwrap();
        }
        drop(sender);
        let mut sink = Vec::new();
        render_progress(&receiver, &mut sink);
        assert_eq!(
            String::from_utf8(sink).unwrap(),
            "\rRefreshing feeds: 0/2\rRefreshing feeds: 1/2\rRefreshing feeds: 2/2\n"
        );
    }

    #[test]
    fn test_render_progress_is_silent_without_due_feeds() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(RefreshEvent::Started {
                total: 0,
                skipped: 3,
                workers: 0,
            })
            .unwrap();
        drop(sender);
        let mut sink = Vec::new();
        render_progress(&receiver, &mut sink);
        assert_eq!(sink, b"");
    }
}

/// Command tests against an in-memory database with a mock fetcher (no test
/// reaches the network).
#[cfg(all(test, feature = "test-utils"))]
mod command_tests {
    use metro_core::db::migrations::run_migrations;
    use metro_core::db::open_in_memory;
    use metro_core::http::client::{FetchResult, FetchStatus};
    use metro_core::models::FeedKind;
    use metro_core::parse::{FeedMetadata, FetchedEntry};
    use metro_core::store::feeds::add_feed;
    use metro_core::store::folders::create_folder;
    use rusqlite::params;

    use super::*;
    use crate::output::OutputFormat;

    fn test_conn() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    /// Fetcher answering every request from a closure.
    struct MockFetcher<R>(R);

    impl<R: Fn(&str) -> Result<FetchResult, MetroError> + Sync> FeedFetcher for MockFetcher<R> {
        fn fetch(
            &self,
            url: &str,
            _etag: Option<&str>,
            _last_modified: Option<&str>,
        ) -> Result<FetchResult, MetroError> {
            (self.0)(url)
        }
    }

    /// `/fail` URLs fail with a network error, `/same` URLs are not
    /// modified, everything else returns two dated entries.
    fn respond(url: &str) -> Result<FetchResult, MetroError> {
        if url.ends_with("/fail") {
            return Err(MetroError::Network(format!("HTTP 500 for {url}")));
        }
        if url.ends_with("/same") {
            return Ok(FetchResult {
                status: FetchStatus::NotModified,
                feed: None,
                entries: Vec::new(),
                etag: None,
                last_modified: None,
            });
        }
        let now = u64::try_from(jiff::Timestamp::now().as_second()).unwrap();
        let entry = |n: u64| FetchedEntry {
            guid: format!("{url}#{n}"),
            url: Some(format!("{url}/{n}")),
            title: Some(format!("Entry {n}")),
            author: None,
            published: Some(now - 3600 * n),
            summary: None,
            content: None,
        };
        Ok(FetchResult {
            status: FetchStatus::Modified,
            feed: Some(FeedMetadata {
                kind: FeedKind::Rss,
                title: Some("Blog".to_string()),
                site_url: None,
            }),
            entries: vec![entry(1), entry(2)],
            etag: None,
            last_modified: None,
        })
    }

    fn args(ids: &[FeedId], folder: Option<&str>) -> RefreshArgs {
        RefreshArgs {
            ids: ids.to_vec(),
            folder: folder.map(str::to_string),
            jobs: None,
        }
    }

    /// Run `refresh` and return (rendered stdout, result).
    fn exec_refresh(
        conn: &mut Connection,
        format: OutputFormat,
        args: &RefreshArgs,
        progress: Option<&mut (dyn Write + Send)>,
    ) -> (String, Result<(), MetroError>) {
        let mut buf = Vec::new();
        let result = refresh(
            args,
            conn,
            &mut Output::new(&mut buf, format),
            &MockFetcher(respond),
            progress,
        );
        (String::from_utf8(buf).unwrap(), result)
    }

    fn exec_view(conn: &Connection, format: OutputFormat, args: &ViewArgs) -> String {
        let mut buf = Vec::new();
        run_view(args, conn, &mut Output::new(&mut buf, format)).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn feed(conn: &mut Connection, url: &str) -> FeedId {
        add_feed(conn, url, None, None).unwrap().id
    }

    #[test]
    fn test_refresh_all_success_exits_ok() {
        let mut conn = test_conn();
        feed(&mut conn, "https://a.example.com/feed");
        feed(&mut conn, "https://b.example.com/same");

        let (out, result) = exec_refresh(&mut conn, OutputFormat::Tabular, &args(&[], None), None);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            out,
            "Refreshed 2 feeds: 2 ok, 0 failed, 0 skipped; 2 new entries, 0 pruned\n"
        );
    }

    #[test]
    fn test_refresh_partial_failure_reports_and_exits_5() {
        let mut conn = test_conn();
        feed(&mut conn, "https://a.example.com/feed");
        let failing = feed(&mut conn, "https://b.example.com/fail");

        let (out, result) = exec_refresh(&mut conn, OutputFormat::Tabular, &args(&[], None), None);
        let err = result.unwrap_err();
        assert_eq!(err.exit_code(), 5, "{err}");
        assert_eq!(
            out,
            format!(
                "{failing}\tfailed\tb.example.com\tNetwork error: HTTP 500 for \
                 https://b.example.com/fail\n\
                 Refreshed 2 feeds: 1 ok, 1 failed, 0 skipped; 2 new entries, 0 pruned\n"
            )
        );

        // Second refresh: the failed feed is in backoff (skipped, not a
        // failure), so the refresh succeeds.
        let (out, result) = exec_refresh(&mut conn, OutputFormat::Tabular, &args(&[], None), None);
        assert!(result.is_ok(), "{result:?}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert!(
            lines[0].starts_with(&format!(
                "{failing}\tskipped\tb.example.com\tin backoff, retry after "
            )),
            "{out}"
        );
        assert_eq!(
            lines[1],
            "Refreshed 1 feed: 1 ok, 0 failed, 1 skipped; 0 new entries, 0 pruned"
        );
    }

    #[test]
    fn test_refresh_total_failure_exits_1() {
        let mut conn = test_conn();
        feed(&mut conn, "https://a.example.com/fail");
        let (_, result) = exec_refresh(&mut conn, OutputFormat::Tabular, &args(&[], None), None);
        assert_eq!(result.unwrap_err().exit_code(), 1);
    }

    #[test]
    fn test_refresh_without_feeds_is_ok() {
        let mut conn = test_conn();
        let (out, result) = exec_refresh(&mut conn, OutputFormat::Human, &args(&[], None), None);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            out,
            "Refreshed 0 feeds: 0 ok, 0 failed, 0 skipped; 0 new entries, 0 pruned\n"
        );
    }

    #[test]
    fn test_refresh_targets_ids_and_folder() {
        let mut conn = test_conn();
        let tech = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        let one = feed(&mut conn, "https://a.example.com/feed");
        let two = add_feed(&mut conn, "https://b.example.com/feed", None, Some(tech))
            .unwrap()
            .id;

        let (out, result) = exec_refresh(&mut conn, OutputFormat::Json, &args(&[one], None), None);
        assert!(result.is_ok(), "{result:?}");
        let value: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        assert_eq!(value["fetched"], 1);
        assert_eq!(value["feeds"][0]["id"], one.to_string());

        let (out, result) = exec_refresh(
            &mut conn,
            OutputFormat::Json,
            &args(&[], Some("tech")),
            None,
        );
        assert!(result.is_ok(), "{result:?}");
        let value: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        assert_eq!(value["fetched"], 1);
        assert_eq!(value["feeds"][0]["id"], two.to_string());
    }

    #[test]
    fn test_refresh_lookup_and_option_errors_fetch_nothing() {
        let mut conn = test_conn();
        let (out, result) = exec_refresh(
            &mut conn,
            OutputFormat::Human,
            &args(&[], Some("Ghost")),
            None,
        );
        assert_eq!(result.unwrap_err().exit_code(), 3);
        assert_eq!(out, "");

        let (out, result) = exec_refresh(
            &mut conn,
            OutputFormat::Human,
            &args(&[FeedId(9)], None),
            None,
        );
        assert_eq!(result.unwrap_err().exit_code(), 3);
        assert_eq!(out, "");

        let mut zero_jobs = args(&[], None);
        zero_jobs.jobs = Some(0);
        let (_, result) = exec_refresh(&mut conn, OutputFormat::Human, &zero_jobs, None);
        assert_eq!(result.unwrap_err().exit_code(), 2);
    }

    #[test]
    fn test_refresh_json_report_lists_every_feed() {
        let mut conn = test_conn();
        let ok = feed(&mut conn, "https://a.example.com/feed");
        let same = feed(&mut conn, "https://b.example.com/same");
        let failing = feed(&mut conn, "https://c.example.com/fail");

        let (out, result) = exec_refresh(&mut conn, OutputFormat::Json, &args(&[], None), None);
        assert_eq!(result.unwrap_err().exit_code(), 5);
        assert_eq!(out.lines().count(), 1, "{out}");
        let value: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        assert_eq!(value["fetched"], 3);
        assert_eq!(value["succeeded"], 2);
        assert_eq!(value["failed"], 1);
        assert_eq!(value["skipped"], 0);
        assert_eq!(value["new_entries"], 2);
        assert_eq!(value["pruned"], 0);
        let feeds = value["feeds"].as_array().unwrap();
        assert_eq!(feeds[0]["id"], ok.to_string());
        assert_eq!(feeds[0]["status"], "updated");
        assert_eq!(feeds[0]["new_entries"], 2);
        assert_eq!(feeds[0]["title"], "Blog");
        assert_eq!(feeds[1]["id"], same.to_string());
        assert_eq!(feeds[1]["status"], "not_modified");
        assert_eq!(feeds[2]["id"], failing.to_string());
        assert_eq!(feeds[2]["status"], "failed");
        assert_eq!(feeds[2]["exit_code"], 4);
        assert!(
            feeds[2]["error"].as_str().unwrap().contains("HTTP 500"),
            "{value}"
        );

        // Second run: the failing feed is reported as skipped.
        let (out, _) = exec_refresh(&mut conn, OutputFormat::Json, &args(&[], None), None);
        let value: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        assert_eq!(value["skipped"], 1);
        assert_eq!(value["feeds"][2]["status"], "skipped");
        assert!(value["feeds"][2]["next_retry"].as_u64().is_some());
    }

    #[test]
    fn test_refresh_progress_goes_to_the_progress_sink_only() {
        let mut conn = test_conn();
        feed(&mut conn, "https://a.example.com/feed");
        feed(&mut conn, "https://b.example.com/feed");

        let mut progress = Vec::new();
        let (out, result) = exec_refresh(
            &mut conn,
            OutputFormat::Tabular,
            &args(&[], None),
            Some(&mut progress),
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(out.starts_with("Refreshed 2 feeds"), "{out}");
        let progress = String::from_utf8(progress).unwrap();
        assert!(
            progress.starts_with("\rRefreshing feeds: 0/2"),
            "{progress:?}"
        );
        assert!(
            progress.ends_with("\rRefreshing feeds: 2/2\n"),
            "{progress:?}"
        );
    }

    fn view_args(id: FeedId, all: bool, unread: bool, limit: Option<usize>) -> ViewArgs {
        ViewArgs {
            id,
            all,
            unread,
            limit,
        }
    }

    #[test]
    fn test_view_lists_newest_first_with_read_state() {
        let mut conn = test_conn();
        let id = feed(&mut conn, "https://a.example.com/feed");
        let (_, result) = exec_refresh(&mut conn, OutputFormat::Json, &args(&[], None), None);
        assert!(result.is_ok(), "{result:?}");
        // Mark the older entry read through SQL (`metro mark` is a later
        // milestone).
        conn.execute(
            "UPDATE entries SET is_read = 1 WHERE title = ?1",
            params!["Entry 2"],
        )
        .unwrap();

        let out = exec_view(
            &conn,
            OutputFormat::Tabular,
            &view_args(id, false, false, None),
        );
        let rows: Vec<Vec<&str>> = out.lines().map(|l| l.split('\t').collect()).collect();
        assert_eq!(rows.len(), 2, "{out}");
        assert_eq!(rows[0][0], "e1");
        assert!(rows[0][1].ends_with('Z'), "{out}");
        assert_eq!(
            &rows[0][2..],
            ["unread", "Entry 1", "https://a.example.com/feed/1"]
        );
        assert_eq!(&rows[1][2..4], ["read", "Entry 2"]);

        let out = exec_view(
            &conn,
            OutputFormat::Tabular,
            &view_args(id, false, true, None),
        );
        assert_eq!(out.lines().count(), 1);
        assert!(out.contains("Entry 1"), "{out}");

        let out = exec_view(
            &conn,
            OutputFormat::Tabular,
            &view_args(id, true, false, Some(1)),
        );
        assert_eq!(out.lines().count(), 1);

        let out = exec_view(&conn, OutputFormat::Json, &view_args(id, true, false, None));
        let lines: Vec<serde_json::Value> = out
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["id"], "e1");
        assert_eq!(lines[0]["feed_id"], id.to_string());
        assert_eq!(lines[0]["is_read"], false);
        assert_eq!(lines[1]["is_read"], true);
    }

    #[test]
    fn test_view_undated_entry_and_errors() {
        let mut conn = test_conn();
        let id = feed(&mut conn, "https://a.example.com/feed");
        conn.execute(
            "INSERT INTO entries (feed_id, guid, url, title) VALUES (?1, 'g', 'u', 'T')",
            params![id.inner()],
        )
        .unwrap();
        let out = exec_view(
            &conn,
            OutputFormat::Tabular,
            &view_args(id, false, false, None),
        );
        assert_eq!(out, "e1\t-\tunread\tT\tu\n");

        let mut buf = Vec::new();
        let mut output = Output::new(&mut buf, OutputFormat::Human);
        let err = run_view(
            &view_args(FeedId(9), false, false, None),
            &conn,
            &mut output,
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 3);
        let err = run_view(&view_args(id, false, false, Some(0)), &conn, &mut output).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(buf, b"");
    }
}
