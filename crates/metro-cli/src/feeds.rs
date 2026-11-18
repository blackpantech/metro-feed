//! `metro feeds …` commands.
//!
//! ```text
//! metro feeds add    <url> [--folder <name>] [--title <t>] [--dry-run]
//! metro feeds list         [--folder <name>]
//! metro feeds remove <f-id>...
//! metro feeds move   <f-id>... --folder <name>
//! metro feeds rename <f-id> <title>
//! metro feeds info   <f-id>
//! ```
//!
//! Feeds are addressed by their typed ID (`f12`, parsed with [`FeedId`]'s
//! `FromStr`). A bare number or another prefix is rejected by `clap` as a
//! usage error (exit 2) with a hint about the expected `f<number>` form.
//! `--folder <name>` is always resolved in the **feed** folder namespace
//! (MVP spec §7) with `metro_core::folders::find_folder` (case-insensitive,
//! `NotFound` → exit 3).
//!
//! This module only parses arguments and renders results. Every rule lives in
//! `metro-core` and errors are returned untouched, so their [`MetroError`]
//! variant decides the exit code:
//!
//! | Situation                                    | Core error  | Exit |
//! |----------------------------------------------|-------------|------|
//! | invalid URL, blank `--title`                 | `Usage`     | 2    |
//! | unknown feed ID or `--folder`                | `NotFound`  | 3    |
//! | homepage advertises no feed (autodiscovery)  | `NotFound`  | 3    |
//! | discovery / feed download failed             | `Network`   | 4    |
//! | body is not a parsable feed                  | `Generic`   | 1    |
//!
//! # `feeds add`
//!
//! Delegates to `metro_core::refresh::subscribe_url`, which validates the
//! input, reports an already-subscribed URL without network access, runs
//! autodiscovery (a feed URL passes through, a homepage yields its advertised
//! feed) and then `subscribe_feed`, which fetches the feed and stores it with
//! its real kind, title, site URL and initial entries in one transaction.
//! Nothing is written when anything fails. `--dry-run` performs discovery and
//! the fetch and shows the detected metadata, but writes nothing (not even
//! the default `Unsorted` folder). A duplicate is not an error (exit 0): the
//! existing feed and the folder it lives in are reported.
//!
//! The network is injected ([`FeedDiscoverer`] / [`FeedFetcher`]) so tests
//! never leave the machine; [`run`] wires the production implementations.
//!
//! # Batch commands
//!
//! `remove` and `move` accept several IDs and are all-or-nothing
//! (`metro_core::feeds::remove_feeds` / `move_feeds` run in one transaction):
//! one unknown ID fails the command and changes nothing.
//!
//! # Output
//!
//! - `list`: one feed per line. Without `--folder` the list covers every
//!   feed folder, grouped (sorted) by folder with the folder name as first
//!   column: `folder  id  title  url`. With `--folder` it is flat: `id  title
//!   url`. With `--json`: NDJSON, one `Feed` object per line plus its
//!   `folder_name`.
//! - `add`: a status line, or one JSON object whose `status` is
//!   `subscribed`, `duplicate` or `dry_run`. `subscribed` and `duplicate`
//!   carry the stored feed (`FeedInfo` fields), `dry_run` the detected
//!   metadata (`FeedPreview` fields) and the target `folder_name`.
//! - `remove` / `move`: one status line (or JSON `FeedInfo` object) per
//!   distinct feed, in argument order. `remove` reports each feed as it was
//!   just before deletion, `move` as it is after the move.
//! - `rename`: a status line, or the renamed feed as a `FeedInfo` object.
//! - `info`: `label  value` lines (tab-separated when piped), or one
//!   `FeedInfo` object.
//!
//! Feed titles and URLs come from remote documents; the shared [`Output`]
//! sanitizes them before printing (terminal control sequences are removed).

use std::borrow::Cow;
use std::collections::HashMap;

use clap::{Args, Subcommand};
use metro_core::MetroError;
use metro_core::discovery::{FeedDiscoverer, HttpDiscoverer};
use metro_core::feeds::{
    FeedInfo, get_feed_info, list_feeds, move_feeds, remove_feeds, rename_feed,
};
use metro_core::fetch::Fetcher;
use metro_core::folders::{DEFAULT_FOLDER_NAME, find_folder, list_folders};
use metro_core::models::{Feed, FeedId, Folder, FolderId, FolderKind};
use metro_core::refresh::{FeedFetcher, FeedPreview, SubscribeOutcome, subscribe_url};
use rusqlite::Connection;
use serde::Serialize;

use crate::output::Output;

/// Arguments of `metro feeds`.
#[derive(Debug, Args)]
pub struct FeedsArgs {
    /// Feed operation to run.
    #[command(subcommand)]
    pub command: FeedsCommand,
}

/// `metro feeds` subcommands.
#[derive(Debug, Subcommand)]
pub enum FeedsCommand {
    /// Subscribe to a feed URL, or a blog homepage (autodiscovery)
    Add(AddArgs),
    /// List feeds, grouped by folder (flat with --folder)
    List {
        /// Only list the feeds of this feed folder
        #[arg(long, value_name = "NAME")]
        folder: Option<String>,
    },
    /// Remove feeds and all their entries
    Remove {
        /// Feeds to remove (e.g. f12)
        #[arg(required = true, value_name = "FEED_ID")]
        ids: Vec<FeedId>,
    },
    /// Move feeds to another feed folder
    Move {
        /// Feeds to move (e.g. f12)
        #[arg(required = true, value_name = "FEED_ID")]
        ids: Vec<FeedId>,
        /// Destination feed folder
        #[arg(long, value_name = "NAME")]
        folder: String,
    },
    /// Change a feed's display title
    Rename {
        /// Feed to rename (e.g. f12)
        #[arg(value_name = "FEED_ID")]
        id: FeedId,
        /// New title
        title: String,
    },
    /// Show a feed's URL, last fetch, error state and entry count
    Info {
        /// Feed to describe (e.g. f12)
        #[arg(value_name = "FEED_ID")]
        id: FeedId,
    },
}

/// Arguments of `metro feeds add`.
#[derive(Debug, Args)]
pub struct AddArgs {
    /// Feed URL or blog homepage URL
    pub url: String,
    /// Feed folder to subscribe into (default: Unsorted)
    #[arg(long, value_name = "NAME")]
    pub folder: Option<String>,
    /// Title to use instead of the feed's own
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,
    /// Fetch the feed and show what would be stored, without saving anything
    #[arg(long)]
    pub dry_run: bool,
}

/// JSON shape of a `feeds list` record: the feed plus its folder's name.
#[derive(Debug, Serialize)]
struct ListedFeed<'a> {
    /// The stored feed.
    #[serde(flatten)]
    feed: &'a Feed,
    /// Display name of the containing folder.
    folder_name: &'a str,
}

/// JSON shape of a `feeds add` result, tagged by `status`.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum AddRecord<'a> {
    /// A new subscription was stored.
    Subscribed {
        /// The stored feed.
        #[serde(flatten)]
        feed: &'a FeedInfo,
        /// Entries inserted by the initial fetch.
        new_entries: usize,
    },
    /// The feed was already subscribed; nothing changed.
    Duplicate {
        /// The existing feed.
        #[serde(flatten)]
        feed: &'a FeedInfo,
    },
    /// Dry run: what would be stored; nothing changed.
    DryRun {
        /// The detected metadata.
        #[serde(flatten)]
        feed: &'a FeedPreview,
        /// Folder the feed would be stored in.
        folder_name: &'a str,
    },
}

/// Run a `metro feeds` subcommand against a migrated connection, with the
/// production network implementations.
///
/// # Errors
///
/// Any [`MetroError`] from `metro-core` (see the module table for the exit
/// codes), or [`MetroError::Generic`] if writing output fails.
pub fn run(
    args: &FeedsArgs,
    conn: &mut Connection,
    output: &mut Output<'_>,
) -> Result<(), MetroError> {
    match &args.command {
        // Only `add` touches the network, so only it builds an HTTP agent.
        FeedsCommand::Add(add_args) => {
            add(add_args, conn, output, &HttpDiscoverer, &Fetcher::default())
        }
        FeedsCommand::List { folder } => list(conn, output, folder.as_deref()),
        FeedsCommand::Remove { ids } => remove(conn, output, ids),
        FeedsCommand::Move { ids, folder } => move_to(conn, output, ids, folder),
        FeedsCommand::Rename { id, title } => rename(conn, output, *id, title),
        FeedsCommand::Info { id } => info(conn, output, *id),
    }
}

/// `feeds add`: autodiscovery + subscribe-time fetch, through injected
/// network implementations.
fn add<D: FeedDiscoverer, F: FeedFetcher>(
    args: &AddArgs,
    conn: &mut Connection,
    output: &mut Output<'_>,
    discoverer: &D,
    fetcher: &F,
) -> Result<(), MetroError> {
    // Resolve the folder name first: an unknown folder fails before any
    // network access.
    let folder = args
        .folder
        .as_deref()
        .map(|name| find_folder(conn, name, FolderKind::Feeds))
        .transpose()?;

    let outcome = subscribe_url(
        conn,
        discoverer,
        fetcher,
        &args.url,
        args.title.as_deref(),
        folder.as_ref().map(Folder::id),
        args.dry_run,
    )?;

    match outcome {
        SubscribeOutcome::Subscribed {
            id, new_entries, ..
        } => {
            let feed = get_feed_info(conn, id)?;
            let message = format!(
                "Subscribed to {} \"{}\" ({}, {new_entries} new {}) in folder \"{}\"",
                feed.id,
                feed.title,
                feed.kind,
                plural(new_entries, "entry", "entries"),
                feed.folder_name
            );
            output.outcome(
                &AddRecord::Subscribed {
                    feed: &feed,
                    new_entries,
                },
                &message,
            )
        }
        SubscribeOutcome::Duplicate { id } => {
            let feed = get_feed_info(conn, id)?;
            let message = format!(
                "Already subscribed: {} \"{}\" in folder \"{}\" ({})",
                feed.id, feed.title, feed.folder_name, feed.url
            );
            output.outcome(&AddRecord::Duplicate { feed: &feed }, &message)
        }
        SubscribeOutcome::DryRun { feed } => {
            let folder_name = folder.as_ref().map_or(DEFAULT_FOLDER_NAME, Folder::name);
            let message = format!(
                "Dry run: would subscribe to \"{}\" ({}, {} {}) at {} in folder \"{folder_name}\"; \
                 nothing was saved",
                feed.title,
                feed.kind,
                feed.entry_count,
                plural(feed.entry_count, "entry", "entries"),
                feed.url
            );
            output.outcome(
                &AddRecord::DryRun {
                    feed: &feed,
                    folder_name,
                },
                &message,
            )
        }
    }
}

/// `feeds list`: grouped by folder, or flat for one `--folder`.
fn list(
    conn: &Connection,
    output: &mut Output<'_>,
    folder: Option<&str>,
) -> Result<(), MetroError> {
    if let Some(name) = folder {
        let folder = find_folder(conn, name, FolderKind::Feeds)?;
        let feeds = list_feeds(conn, Some(folder.id()))?;
        let records: Vec<ListedFeed<'_>> = feeds
            .iter()
            .map(|feed| ListedFeed {
                feed,
                folder_name: folder.name(),
            })
            .collect();
        return output.list(&records, |record| feed_columns(record.feed));
    }

    // `list_feeds` already orders feeds by folder name then title; the folder
    // names come from the same database, so every feed's folder is present.
    let folders = list_folders(conn, FolderKind::Feeds)?;
    let names: HashMap<FolderId, &str> = folders
        .iter()
        .map(|folder| (folder.id(), folder.name()))
        .collect();
    let feeds = list_feeds(conn, None)?;
    let records = feeds
        .iter()
        .map(|feed| {
            let folder_name = names.get(&feed.folder_id()).copied().ok_or_else(|| {
                MetroError::Generic(format!(
                    "feed {} belongs to unknown folder {}",
                    feed.id(),
                    feed.folder_id().inner()
                ))
            })?;
            Ok(ListedFeed { feed, folder_name })
        })
        .collect::<Result<Vec<_>, MetroError>>()?;
    output.list(&records, |record| {
        let mut columns = vec![record.folder_name.to_string()];
        columns.extend(feed_columns(record.feed));
        columns
    })
}

/// Text columns of one feed in `feeds list`: `id  title  url`.
fn feed_columns(feed: &Feed) -> Vec<String> {
    vec![
        feed.id().to_string(),
        feed.title().to_string(),
        feed.url().to_string(),
    ]
}

/// `feeds remove`: delete every listed feed (all-or-nothing).
fn remove(
    conn: &mut Connection,
    output: &mut Output<'_>,
    ids: &[FeedId],
) -> Result<(), MetroError> {
    for feed in remove_feeds(conn, ids)? {
        let message = format!(
            "Removed feed {} \"{}\" and its {} {}",
            feed.id,
            feed.title,
            feed.entry_count,
            plural(feed.entry_count, "entry", "entries")
        );
        output.outcome(&feed, &message)?;
    }
    Ok(())
}

/// `feeds move`: move every listed feed to one feed folder (all-or-nothing).
fn move_to(
    conn: &mut Connection,
    output: &mut Output<'_>,
    ids: &[FeedId],
    folder: &str,
) -> Result<(), MetroError> {
    let folder = find_folder(conn, folder, FolderKind::Feeds)?;
    for feed in move_feeds(conn, ids, folder.id())? {
        let message = format!(
            "Moved feed {} \"{}\" to folder \"{}\"",
            feed.id, feed.title, feed.folder_name
        );
        output.outcome(&feed, &message)?;
    }
    Ok(())
}

/// `feeds rename`: change a feed's title.
fn rename(
    conn: &mut Connection,
    output: &mut Output<'_>,
    id: FeedId,
    title: &str,
) -> Result<(), MetroError> {
    rename_feed(conn, id, title)?;
    // Re-read so the result carries the title exactly as stored (trimmed).
    let feed = get_feed_info(conn, id)?;
    let message = format!("Renamed feed {} to \"{}\"", feed.id, feed.title);
    output.outcome(&feed, &message)
}

/// `feeds info`: labelled metadata of one feed.
fn info(conn: &Connection, output: &mut Output<'_>, id: FeedId) -> Result<(), MetroError> {
    let feed = get_feed_info(conn, id)?;
    let fields = [
        ("ID", Cow::Owned(feed.id.to_string())),
        ("Title", Cow::Borrowed(feed.title.as_str())),
        ("URL", Cow::Borrowed(feed.url.as_str())),
        ("Site", Cow::Borrowed(feed.site_url.as_str())),
        ("Kind", Cow::Borrowed(feed.kind.as_str())),
        ("Folder", Cow::Borrowed(feed.folder_name.as_str())),
        (
            "Last fetch",
            feed.last_fetch.map_or(Cow::Borrowed("never"), |at| {
                Cow::Owned(format_timestamp(at))
            }),
        ),
        (
            "Error",
            Cow::Borrowed(feed.error_state.as_deref().unwrap_or("none")),
        ),
        ("Entries", Cow::Owned(feed.entry_count.to_string())),
    ];
    output.details(&feed, &fields)
}

/// Format Unix seconds as an RFC 3339 UTC timestamp
/// (`2026-01-02T00:00:00Z`), falling back to the raw number when it is out
/// of the representable range.
fn format_timestamp(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| jiff::Timestamp::from_second(seconds).ok())
        .map_or_else(|| seconds.to_string(), |timestamp| timestamp.to_string())
}

/// Pick the singular or plural form of a noun for `count`.
///
/// Generic over the count type so both `usize` fetch counts and `u64`
/// database counts can be passed without casts.
fn plural<'a, N>(count: N, singular: &'a str, plural: &'a str) -> &'a str
where
    N: TryInto<u64>,
{
    if count.try_into().ok() == Some(1) {
        singular
    } else {
        plural
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::{Cli, Commands};

    /// Parse `metro feeds …` and return the feeds subcommand.
    fn parse(args: &[&str]) -> FeedsCommand {
        let argv = ["metro", "feeds"].iter().chain(args).copied();
        match Cli::try_parse_from(argv).unwrap().command {
            Some(Commands::Feeds(feeds)) => feeds.command,
            other => panic!("expected feeds command, got {other:?}"),
        }
    }

    /// Parse `metro <args>` and return the usage error's exit code.
    fn parse_error_code(args: &[&str]) -> i32 {
        let argv = std::iter::once("metro").chain(args.iter().copied());
        Cli::try_parse_from(argv).unwrap_err().exit_code()
    }

    #[test]
    fn test_parse_add_with_all_flags() {
        match parse(&[
            "add",
            "https://example.com/feed.xml",
            "--folder",
            "Tech",
            "--title",
            "My Feed",
            "--dry-run",
        ]) {
            FeedsCommand::Add(args) => {
                assert_eq!(args.url, "https://example.com/feed.xml");
                assert_eq!(args.folder.as_deref(), Some("Tech"));
                assert_eq!(args.title.as_deref(), Some("My Feed"));
                assert!(args.dry_run);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn test_parse_add_defaults() {
        match parse(&["add", "https://example.com"]) {
            FeedsCommand::Add(args) => {
                assert_eq!(args.folder, None);
                assert_eq!(args.title, None);
                assert!(!args.dry_run);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn test_parse_list() {
        assert!(matches!(
            parse(&["list"]),
            FeedsCommand::List { folder: None }
        ));
        match parse(&["list", "--folder", "Tech"]) {
            FeedsCommand::List { folder } => assert_eq!(folder.as_deref(), Some("Tech")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn test_parse_remove_and_move_take_typed_ids() {
        match parse(&["remove", "f12", "f13"]) {
            FeedsCommand::Remove { ids } => assert_eq!(ids, vec![FeedId(12), FeedId(13)]),
            other => panic!("unexpected {other:?}"),
        }
        match parse(&["move", "f12", "--folder", "Tech"]) {
            FeedsCommand::Move { ids, folder } => {
                assert_eq!(ids, vec![FeedId(12)]);
                assert_eq!(folder, "Tech");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn test_parse_rename_and_info() {
        match parse(&["rename", "f7", "New Title"]) {
            FeedsCommand::Rename { id, title } => {
                assert_eq!(id, FeedId(7));
                assert_eq!(title, "New Title");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            parse(&["info", "f7"]),
            FeedsCommand::Info { id: FeedId(7) }
        ));
    }

    #[test]
    fn test_parse_rejects_bare_or_wrong_ids_with_hint() {
        let err = Cli::try_parse_from(["metro", "feeds", "info", "12"]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("f<number>"), "{err}");
        for args in [
            &["feeds", "remove", "e12"][..],
            &["feeds", "rename", "fx", "Title"][..],
        ] {
            assert_eq!(parse_error_code(args), 2, "args {args:?}");
        }
    }

    #[test]
    fn test_parse_rejects_missing_args() {
        for args in [
            &["feeds"][..],
            &["feeds", "add"][..],
            &["feeds", "remove"][..],
            &["feeds", "move", "f1"][..],
            &["feeds", "move", "--folder", "Tech"][..],
            &["feeds", "rename", "f1"][..],
            &["feeds", "info"][..],
        ] {
            assert_eq!(parse_error_code(args), 2, "args {args:?}");
        }
    }

    #[test]
    fn test_format_timestamp() {
        assert_eq!(format_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_timestamp(1_767_312_000), "2026-01-02T00:00:00Z");
        assert_eq!(format_timestamp(u64::MAX), u64::MAX.to_string());
    }

    #[test]
    fn test_plural() {
        assert_eq!(plural(1_usize, "entry", "entries"), "entry");
        assert_eq!(plural(0_u64, "entry", "entries"), "entries");
        assert_eq!(plural(2_u64, "entry", "entries"), "entries");
    }
}

/// Command tests against an in-memory database, with mock network
/// implementations (no test reaches the network).
#[cfg(all(test, feature = "test-utils"))]
mod command_tests {
    use std::sync::Mutex;

    use metro_core::db::migrations::run_migrations;
    use metro_core::db::open_in_memory;
    use metro_core::fetch::{FeedMetadata, FetchResult, FetchStatus, FetchedEntry};
    use metro_core::folders::create_folder;
    use metro_core::models::FeedKind;

    use super::*;
    use crate::output::OutputFormat;

    const FEED_URL: &str = "https://atom.example.com/atom.xml";
    const HOMEPAGE: &str = "https://atom.example.com";

    fn test_conn() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    /// Discoverer resolving the homepage to [`FEED_URL`] and passing every
    /// other URL through, recording calls.
    #[derive(Default)]
    struct MockDiscoverer {
        calls: Mutex<Vec<String>>,
    }

    impl FeedDiscoverer for MockDiscoverer {
        fn discover(&self, url: &str) -> Result<String, MetroError> {
            self.calls.lock().unwrap().push(url.to_string());
            Ok(if url == HOMEPAGE {
                FEED_URL.to_string()
            } else {
                url.to_string()
            })
        }
    }

    /// Fetcher answering every request from a closure, recording URLs.
    struct MockFetcher<R> {
        respond: R,
        calls: Mutex<Vec<String>>,
    }

    impl<R> MockFetcher<R> {
        fn new(respond: R) -> Self {
            Self {
                respond,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl<R: Fn(&str) -> Result<FetchResult, MetroError> + Sync> FeedFetcher for MockFetcher<R> {
        fn fetch(
            &self,
            url: &str,
            _etag: Option<&str>,
            _last_modified: Option<&str>,
        ) -> Result<FetchResult, MetroError> {
            self.calls.lock().unwrap().push(url.to_string());
            (self.respond)(url)
        }
    }

    /// An Atom `200` result titled "Atom Blog" with two entries whose guids
    /// derive from the feed URL (entry guids are unique database-wide).
    fn atom_result(url: &str) -> Result<FetchResult, MetroError> {
        let entry = |n: &str| FetchedEntry {
            guid: format!("{url}#{n}"),
            url: Some(format!("{url}/{n}")),
            title: Some(format!("Entry {n}")),
            author: None,
            published: None,
            summary: None,
            content: None,
        };
        Ok(FetchResult {
            status: FetchStatus::Modified,
            feed: Some(FeedMetadata {
                kind: FeedKind::Atom,
                title: Some("Atom Blog".to_string()),
                site_url: Some("https://atom.example.com/".to_string()),
            }),
            entries: vec![entry("1"), entry("2")],
            etag: None,
            last_modified: None,
        })
    }

    fn add_args(url: &str) -> AddArgs {
        AddArgs {
            url: url.to_string(),
            folder: None,
            title: None,
            dry_run: false,
        }
    }

    /// Run `feeds add` with the mock network and return the rendered stdout.
    fn exec_add<R: Fn(&str) -> Result<FetchResult, MetroError> + Sync>(
        conn: &mut Connection,
        format: OutputFormat,
        args: &AddArgs,
        fetcher: &MockFetcher<R>,
    ) -> Result<String, MetroError> {
        let mut buf = Vec::new();
        add(
            args,
            conn,
            &mut Output::new(&mut buf, format),
            &MockDiscoverer::default(),
            fetcher,
        )?;
        Ok(String::from_utf8(buf).unwrap())
    }

    /// Run any non-network command and return the rendered stdout.
    fn exec(
        conn: &mut Connection,
        format: OutputFormat,
        command: FeedsCommand,
    ) -> Result<String, MetroError> {
        let mut buf = Vec::new();
        run(
            &FeedsArgs { command },
            conn,
            &mut Output::new(&mut buf, format),
        )?;
        Ok(String::from_utf8(buf).unwrap())
    }

    /// Subscribe to [`FEED_URL`] (or `url`) and return its ID.
    fn subscribe(conn: &mut Connection, url: &str, folder: Option<&str>) -> FeedId {
        let mut args = add_args(url);
        args.folder = folder.map(str::to_string);
        let text = exec_add(
            conn,
            OutputFormat::Json,
            &args,
            &MockFetcher::new(atom_result),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        FeedId(value["id"].as_u64().unwrap())
    }

    #[test]
    fn test_add_subscribes_into_unsorted_with_real_metadata() {
        let mut conn = test_conn();
        let fetcher = MockFetcher::new(atom_result);

        let text = exec_add(
            &mut conn,
            OutputFormat::Human,
            &add_args(FEED_URL),
            &fetcher,
        )
        .unwrap();

        assert_eq!(
            text,
            "Subscribed to f1 \"Atom Blog\" (atom, 2 new entries) in folder \"Unsorted\"\n"
        );
        let info = get_feed_info(&conn, FeedId(1)).unwrap();
        assert_eq!(info.kind, FeedKind::Atom);
        assert_eq!(info.entry_count, 2);
        assert_eq!(fetcher.calls(), 1);
    }

    #[test]
    fn test_add_with_autodiscovery_folder_and_title_json() {
        let mut conn = test_conn();
        create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        let args = AddArgs {
            url: HOMEPAGE.to_string(),
            folder: Some("tech".to_string()),
            title: Some("Mine".to_string()),
            dry_run: false,
        };

        let text = exec_add(
            &mut conn,
            OutputFormat::Json,
            &args,
            &MockFetcher::new(atom_result),
        )
        .unwrap();

        let value: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(value["status"], "subscribed");
        assert_eq!(value["url"], FEED_URL);
        assert_eq!(value["title"], "Mine");
        assert_eq!(value["kind"], "atom");
        assert_eq!(value["folder_name"], "Tech");
        assert_eq!(value["new_entries"], 2);
        assert_eq!(value["entry_count"], 2);
    }

    #[test]
    fn test_add_duplicate_reports_existing_feed() {
        let mut conn = test_conn();
        create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        let id = subscribe(&mut conn, FEED_URL, Some("Tech"));
        let fetcher = MockFetcher::new(atom_result);

        let text = exec_add(
            &mut conn,
            OutputFormat::Human,
            &add_args("https://ATOM.example.com/atom.xml#top"),
            &fetcher,
        )
        .unwrap();

        assert_eq!(
            text,
            format!("Already subscribed: {id} \"Atom Blog\" in folder \"Tech\" ({FEED_URL})\n")
        );
        assert_eq!(fetcher.calls(), 0);
        assert_eq!(list_feeds(&conn, None).unwrap().len(), 1);

        let json = exec_add(&mut conn, OutputFormat::Json, &add_args(FEED_URL), &fetcher).unwrap();
        let value: serde_json::Value = serde_json::from_str(json.trim_end()).unwrap();
        assert_eq!(value["status"], "duplicate");
        assert_eq!(value["id"], id.inner());
    }

    #[test]
    fn test_add_dry_run_shows_metadata_without_writing() {
        let mut conn = test_conn();
        let mut args = add_args(HOMEPAGE);
        args.dry_run = true;

        let text = exec_add(
            &mut conn,
            OutputFormat::Human,
            &args,
            &MockFetcher::new(atom_result),
        )
        .unwrap();
        assert_eq!(
            text,
            format!(
                "Dry run: would subscribe to \"Atom Blog\" (atom, 2 entries) at {FEED_URL} \
                 in folder \"Unsorted\"; nothing was saved\n"
            )
        );

        let json = exec_add(
            &mut conn,
            OutputFormat::Json,
            &args,
            &MockFetcher::new(atom_result),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(json.trim_end()).unwrap();
        assert_eq!(value["status"], "dry_run");
        assert_eq!(value["kind"], "atom");
        assert_eq!(value["site_url"], "https://atom.example.com/");
        assert_eq!(value["entry_count"], 2);
        assert_eq!(value["folder_name"], "Unsorted");

        assert!(list_feeds(&conn, None).unwrap().is_empty());
        assert!(list_folders(&conn, FolderKind::Feeds).unwrap().is_empty());
    }

    #[test]
    fn test_add_fetch_failures_keep_their_exit_code_and_write_nothing() {
        let mut conn = test_conn();
        let network = MockFetcher::new(|_: &str| Err(MetroError::Network("refused".to_string())));
        let parse = MockFetcher::new(|_: &str| Err(MetroError::Generic("not a feed".to_string())));

        let err = exec_add(
            &mut conn,
            OutputFormat::Human,
            &add_args(FEED_URL),
            &network,
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 4, "{err}");
        let err =
            exec_add(&mut conn, OutputFormat::Human, &add_args(FEED_URL), &parse).unwrap_err();
        assert_eq!(err.exit_code(), 1, "{err}");

        assert!(list_feeds(&conn, None).unwrap().is_empty());
    }

    #[test]
    fn test_add_rejects_bad_input_before_network() {
        let mut conn = test_conn();
        let fetcher = MockFetcher::new(atom_result);

        let bad_url = exec_add(
            &mut conn,
            OutputFormat::Human,
            &add_args("not a url"),
            &fetcher,
        );
        assert_eq!(bad_url.unwrap_err().exit_code(), 2);

        let mut args = add_args(FEED_URL);
        args.folder = Some("Ghost".to_string());
        let missing = exec_add(&mut conn, OutputFormat::Human, &args, &fetcher);
        assert_eq!(missing.unwrap_err().exit_code(), 3);

        let mut args = add_args(FEED_URL);
        args.title = Some("  ".to_string());
        let blank = exec_add(&mut conn, OutputFormat::Human, &args, &fetcher);
        assert_eq!(blank.unwrap_err().exit_code(), 2);

        assert_eq!(fetcher.calls(), 0);
    }

    #[test]
    fn test_list_grouped_flat_and_json() {
        let mut conn = test_conn();
        create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        let unsorted = subscribe(&mut conn, "https://a.example.com/feed.xml", None);
        let tech = subscribe(&mut conn, "https://b.example.com/feed.xml", Some("Tech"));

        let grouped = exec(
            &mut conn,
            OutputFormat::Tabular,
            FeedsCommand::List { folder: None },
        )
        .unwrap();
        assert_eq!(
            grouped,
            format!(
                "Tech\t{tech}\tAtom Blog\thttps://b.example.com/feed.xml\n\
                 Unsorted\t{unsorted}\tAtom Blog\thttps://a.example.com/feed.xml\n"
            )
        );

        let flat = exec(
            &mut conn,
            OutputFormat::Tabular,
            FeedsCommand::List {
                folder: Some("TECH".to_string()),
            },
        )
        .unwrap();
        assert_eq!(
            flat,
            format!("{tech}\tAtom Blog\thttps://b.example.com/feed.xml\n")
        );

        let json = exec(
            &mut conn,
            OutputFormat::Json,
            FeedsCommand::List { folder: None },
        )
        .unwrap();
        let values: Vec<serde_json::Value> = json
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["folder_name"], "Tech");
        assert_eq!(values[0]["id"], tech.inner());
        assert_eq!(values[1]["kind"], "atom");
    }

    #[test]
    fn test_list_empty_and_unknown_folder() {
        let mut conn = test_conn();
        let empty = exec(
            &mut conn,
            OutputFormat::Human,
            FeedsCommand::List { folder: None },
        )
        .unwrap();
        assert_eq!(empty, "");
        let err = exec(
            &mut conn,
            OutputFormat::Human,
            FeedsCommand::List {
                folder: Some("Ghost".to_string()),
            },
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn test_remove_feeds_all_or_nothing() {
        let mut conn = test_conn();
        let one = subscribe(&mut conn, "https://a.example.com/feed.xml", None);
        let two = subscribe(&mut conn, "https://b.example.com/feed.xml", None);

        let err = exec(
            &mut conn,
            OutputFormat::Human,
            FeedsCommand::Remove {
                ids: vec![one, FeedId(999)],
            },
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 3, "{err}");
        assert_eq!(list_feeds(&conn, None).unwrap().len(), 2);

        let text = exec(
            &mut conn,
            OutputFormat::Human,
            FeedsCommand::Remove {
                ids: vec![one, two, one],
            },
        )
        .unwrap();
        assert_eq!(
            text,
            format!(
                "Removed feed {one} \"Atom Blog\" and its 2 entries\n\
                 Removed feed {two} \"Atom Blog\" and its 2 entries\n"
            )
        );
        assert!(list_feeds(&conn, None).unwrap().is_empty());
    }

    #[test]
    fn test_move_feeds() {
        let mut conn = test_conn();
        create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        create_folder(&mut conn, "Later", FolderKind::Saved).unwrap();
        let id = subscribe(&mut conn, FEED_URL, None);

        let text = exec(
            &mut conn,
            OutputFormat::Human,
            FeedsCommand::Move {
                ids: vec![id],
                folder: "tech".to_string(),
            },
        )
        .unwrap();
        assert_eq!(
            text,
            format!("Moved feed {id} \"Atom Blog\" to folder \"Tech\"\n")
        );
        assert_eq!(get_feed_info(&conn, id).unwrap().folder_name, "Tech");

        // Saved folders are another namespace: not found as a feed folder.
        let err = exec(
            &mut conn,
            OutputFormat::Human,
            FeedsCommand::Move {
                ids: vec![id],
                folder: "Later".to_string(),
            },
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 3, "{err}");
    }

    #[test]
    fn test_rename_feed() {
        let mut conn = test_conn();
        let id = subscribe(&mut conn, FEED_URL, None);

        let text = exec(
            &mut conn,
            OutputFormat::Human,
            FeedsCommand::Rename {
                id,
                title: "  New Title ".to_string(),
            },
        )
        .unwrap();
        assert_eq!(text, format!("Renamed feed {id} to \"New Title\"\n"));

        let blank = exec(
            &mut conn,
            OutputFormat::Human,
            FeedsCommand::Rename {
                id,
                title: " ".to_string(),
            },
        );
        assert_eq!(blank.unwrap_err().exit_code(), 2);
        let missing = exec(
            &mut conn,
            OutputFormat::Human,
            FeedsCommand::Rename {
                id: FeedId(999),
                title: "X".to_string(),
            },
        );
        assert_eq!(missing.unwrap_err().exit_code(), 3);
    }

    #[test]
    fn test_info_text_and_json() {
        let mut conn = test_conn();
        let id = subscribe(&mut conn, FEED_URL, None);
        conn.execute(
            "UPDATE feeds SET last_fetch = 1767312000, error_state = 'HTTP 500' WHERE id = ?1",
            [id.inner()],
        )
        .unwrap();

        let text = exec(&mut conn, OutputFormat::Tabular, FeedsCommand::Info { id }).unwrap();
        assert_eq!(
            text,
            format!(
                "ID\t{id}\nTitle\tAtom Blog\nURL\t{FEED_URL}\nSite\thttps://atom.example.com/\n\
                 Kind\tatom\nFolder\tUnsorted\nLast fetch\t2026-01-02T00:00:00Z\n\
                 Error\tHTTP 500\nEntries\t2\n"
            )
        );

        let json = exec(&mut conn, OutputFormat::Json, FeedsCommand::Info { id }).unwrap();
        let info: FeedInfo = serde_json::from_str(json.trim_end()).unwrap();
        assert_eq!(info.id, id);
        assert_eq!(info.error_state.as_deref(), Some("HTTP 500"));

        let missing = exec(
            &mut conn,
            OutputFormat::Human,
            FeedsCommand::Info { id: FeedId(999) },
        );
        assert_eq!(missing.unwrap_err().exit_code(), 3);
    }

    #[test]
    fn test_info_never_fetched_feed() {
        let mut conn = test_conn();
        metro_core::feeds::add_feed(&mut conn, FEED_URL, None, None).unwrap();

        let text = exec(
            &mut conn,
            OutputFormat::Human,
            FeedsCommand::Info { id: FeedId(1) },
        )
        .unwrap();
        assert!(text.contains("Last fetch  never\n"), "{text}");
        assert!(text.contains("Error       none\n"), "{text}");
    }
}
