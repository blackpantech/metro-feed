//! Metro CLI library.
//!
//! A thin frontend over `metro-core` ("two frontends, one core"): this crate
//! parses arguments with `clap`, opens the database, calls core APIs and
//! renders their results. It contains no product rules.
//!
//! # Structure
//!
//! - [`Cli`] / [`Commands`]: the argument model. Global flags live on
//!   [`Cli`]; each command family is one [`Commands`] variant whose argument
//!   types and handler live in their own module (`feeds`, `folders`,
//!   `refresh` for `refresh` and `view`), each exposing a handler taking
//!   `(args, conn, output)`.
//! - stderr carries errors and, for `refresh` only, a progress line drawn
//!   when stderr is a terminal and `--json` is off; stdout only carries
//!   results.
//! - `database`: opens the migrated database (`--db` override or the
//!   platform default), delegating to `metro_core::db`.
//! - `output`: the human / tab-separated / NDJSON renderings every command
//!   shares, selected once from `--json` and whether stdout is a terminal.
//!
//! # Exit codes
//!
//! [`run`] returns the process exit code instead of exiting, so the
//! dispatcher binary owns `std::process::exit`. The mapping is not
//! duplicated here:
//!
//! - argument errors are reported by `clap`, whose exit code is 2 for usage
//!   errors (matching `MetroError::Usage`) and 0 for `--help` / `--version`;
//! - command failures are `MetroError`s, printed to stderr as
//!   `metro: <error>` and mapped with `MetroError::exit_code`.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use clap::{ArgAction, CommandFactory, Parser, Subcommand};
use metro_core::MetroError;

use crate::feeds::FeedsArgs;
use crate::folders::FoldersArgs;
use crate::output::{IgnoreBrokenPipe, Output, OutputFormat, write_error};
use crate::refresh::{RefreshArgs, ViewArgs, run_refresh, run_view};

mod database;
mod feeds;
mod folders;
mod output;
mod refresh;

/// Metro feed manager
#[derive(Debug, Parser)]
#[command(name = "metro")]
#[command(about = "Metro feed manager", long_about = None)]
#[command(version)]
pub struct Cli {
    /// Verbose output (inert until wired up)
    #[arg(short = 'v', long = "verbose", global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Log configuration (inert until wired up)
    #[arg(long = "log", global = true)]
    pub log: Option<String>,

    /// Machine-readable output: one JSON object per line (NDJSON for lists)
    #[arg(long = "json", global = true)]
    pub json: bool,

    /// Database file to use instead of the default `<data dir>/metro/metro.db`
    #[arg(long = "db", global = true, value_name = "PATH")]
    pub db: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Top-level commands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Manage feed subscriptions
    Feeds(FeedsArgs),
    /// Manage feed and saved-item folders
    Folders(FoldersArgs),
    /// Fetch every feed, or only the given feeds or folder, in parallel
    Refresh(RefreshArgs),
    /// List a feed's entries, newest first
    View(ViewArgs),
}

/// Run the CLI with the full argv (including the program name) and return
/// the process exit code.
///
/// Writes results to stdout and errors to stderr.
#[must_use]
pub fn run(args: &[String]) -> i32 {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            // `print` routes help/version to stdout and errors to stderr,
            // with colors when appropriate. A failure to print leaves nothing
            // more useful to report.
            let _ = err.print();
            return err.exit_code();
        }
    };

    let stdout = std::io::stdout();
    let stdout_is_terminal = stdout.is_terminal();
    let mut out = IgnoreBrokenPipe::new(stdout.lock());
    // stderr is not locked for the whole run: the refresh progress line is
    // written from a helper thread through its own handle, which would
    // deadlock on a lock held here. `Stderr` is unbuffered, so the two
    // handles never interleave partial lines.
    let mut err = std::io::stderr();
    let mut progress = std::io::stderr();
    let progress: Option<&mut (dyn Write + Send)> = if progress.is_terminal() {
        Some(&mut progress)
    } else {
        None
    };
    execute(&cli, &mut out, &mut err, stdout_is_terminal, progress)
}

/// Execute parsed arguments, writing results to `out` and errors to `err`,
/// and return the exit code.
///
/// `progress` is where long-running commands may draw progress (stderr when
/// it is a terminal, else `None`); it is only used when `--json` is off.
///
/// Separated from [`run`] so tests can capture every stream and choose the
/// terminal/pipe rendering.
fn execute(
    cli: &Cli,
    out: &mut dyn Write,
    err: &mut dyn Write,
    stdout_is_terminal: bool,
    progress: Option<&mut (dyn Write + Send)>,
) -> i32 {
    let result = match &cli.command {
        // `metro <global flags>` without a command: show the top-level help.
        None => write!(out, "{}", Cli::command().render_help()).map_err(|e| write_error(&e)),
        Some(command) => {
            let format = OutputFormat::select(cli.json, stdout_is_terminal);
            dispatch(cli, command, &mut Output::new(out, format), progress)
        }
    }
    .and_then(|()| out.flush().map_err(|e| write_error(&e)));

    match result {
        Ok(()) => MetroError::Ok.exit_code(),
        Err(error) => {
            // Nothing more useful can be done if stderr itself is broken.
            let _ = writeln!(err, "metro: {error}");
            error.exit_code()
        }
    }
}

/// Route a parsed command to its handler.
///
/// Each arm opens the database only if the command needs it, then hands the
/// family's arguments, the connection and the shared output to the family
/// module's `run`.
fn dispatch(
    cli: &Cli,
    command: &Commands,
    output: &mut Output<'_>,
    progress: Option<&mut (dyn Write + Send)>,
) -> Result<(), MetroError> {
    match command {
        Commands::Feeds(args) => {
            let mut conn = database::open(cli.db.as_deref())?;
            feeds::run(args, &mut conn, output)
        }
        Commands::Folders(args) => {
            let mut conn = database::open(cli.db.as_deref())?;
            folders::run(args, &mut conn, output)
        }
        Commands::Refresh(args) => {
            let mut conn = database::open(cli.db.as_deref())?;
            // `--json` output is for machines: no progress noise at all.
            let progress = progress.filter(|_| !cli.json);
            run_refresh(args, &mut conn, output, progress)
        }
        Commands::View(args) => {
            let conn = database::open(cli.db.as_deref())?;
            run_view(args, &conn, output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_exits_successfully() {
        let err = Cli::try_parse_from(["metro", "--version"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
        assert_eq!(err.exit_code(), MetroError::Ok.exit_code());
    }

    #[test]
    fn test_verbose_flag_accepted() {
        let cli = Cli::try_parse_from(["metro", "-v", "folders", "list"]).unwrap();
        assert_eq!(cli.verbose, 1);
    }

    #[test]
    fn test_log_flag_accepted() {
        let cli = Cli::try_parse_from(["metro", "--log", "debug", "folders", "list"]).unwrap();
        assert_eq!(cli.log.as_deref(), Some("debug"));
    }

    #[test]
    fn test_global_flags_accepted_after_subcommand() {
        let cli =
            Cli::try_parse_from(["metro", "folders", "list", "--json", "--db", "x.db"]).unwrap();
        assert!(cli.json);
        assert_eq!(cli.db.as_deref(), Some(std::path::Path::new("x.db")));
    }

    #[test]
    fn test_usage_error_exit_code_matches_core_contract() {
        let err = Cli::try_parse_from(["metro", "nope"]).unwrap_err();
        assert_eq!(
            err.exit_code(),
            MetroError::Usage(String::new()).exit_code()
        );
    }

    #[test]
    fn test_folders_help_works() {
        let err = Cli::try_parse_from(["metro", "folders", "--help"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
        assert_eq!(err.exit_code(), 0);
        let help = err.to_string();
        for sub in ["list", "add", "rename", "remove"] {
            assert!(help.contains(sub), "missing {sub} in:\n{help}");
        }
    }

    #[test]
    fn test_feeds_help_works() {
        let err = Cli::try_parse_from(["metro", "feeds", "--help"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
        assert_eq!(err.exit_code(), 0);
        let help = err.to_string();
        for sub in ["add", "list", "remove", "move", "rename", "info"] {
            assert!(help.contains(sub), "missing {sub} in:\n{help}");
        }

        let err = Cli::try_parse_from(["metro", "feeds", "add", "--help"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
        let help = err.to_string();
        for flag in ["--folder", "--title", "--dry-run"] {
            assert!(help.contains(flag), "missing {flag} in:\n{help}");
        }
    }

    #[test]
    fn test_refresh_and_view_help_work() {
        for (command, flags) in [
            ("refresh", &["--folder", "--jobs", "FEED_ID"][..]),
            ("view", &["--all", "--unread", "--limit", "FEED_ID"][..]),
        ] {
            let err = Cli::try_parse_from(["metro", command, "--help"]).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
            assert_eq!(err.exit_code(), 0);
            let help = err.to_string();
            for flag in flags {
                assert!(help.contains(flag), "missing {flag} in:\n{help}");
            }
        }
    }

    #[test]
    fn test_no_subcommand_prints_help() {
        let cli = Cli::try_parse_from(["metro", "-v"]).unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(execute(&cli, &mut out, &mut err, false, None), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("folders"), "{out}");
        assert!(out.contains("feeds"), "{out}");
        assert!(out.contains("refresh"), "{out}");
        assert!(out.contains("view"), "{out}");
        assert!(err.is_empty());
    }

    #[test]
    fn test_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}

/// End-to-end tests: argv → exit code + captured stdout/stderr, against a
/// temporary on-disk database passed with `--db`.
#[cfg(all(test, feature = "test-utils"))]
mod end_to_end_tests {
    use metro_core::test_utils::{cleanup_temp_db, temp_db_path};

    use super::*;

    /// A temporary database removed (with its WAL side files) on drop.
    struct TempDb(PathBuf);

    impl Drop for TempDb {
        fn drop(&mut self) {
            cleanup_temp_db(&self.0);
            for suffix in ["-wal", "-shm"] {
                let mut side = self.0.as_os_str().to_owned();
                side.push(suffix);
                let _ = std::fs::remove_file(side);
            }
        }
    }

    struct Run {
        code: i32,
        out: String,
        err: String,
    }

    fn metro(db: &TempDb, args: &[&str]) -> Run {
        let db_arg = db.0.to_string_lossy();
        let argv = ["metro", "--db", db_arg.as_ref()]
            .into_iter()
            .chain(args.iter().copied());
        let cli = Cli::try_parse_from(argv).unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = execute(&cli, &mut out, &mut err, false, None);
        Run {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    #[test]
    fn test_folders_round_trip() {
        let db = TempDb(temp_db_path());

        let run = metro(&db, &["folders", "add", "Tech"]);
        assert_eq!(run.code, 0, "{}", run.err);
        assert_eq!(run.out, "Created feeds folder \"Tech\"\n");

        assert_eq!(metro(&db, &["folders", "list"]).out, "Tech\n");
        assert_eq!(metro(&db, &["folders", "list", "--kind", "saved"]).out, "");

        let run = metro(&db, &["folders", "rename", "Tech", "Technology"]);
        assert_eq!(run.code, 0, "{}", run.err);

        let run = metro(&db, &["--json", "folders", "list"]);
        let value: serde_json::Value = serde_json::from_str(run.out.trim_end()).unwrap();
        assert_eq!(value["name"], "Technology");
        assert_eq!(value["kind"], "feeds");

        let run = metro(&db, &["folders", "remove", "technology"]);
        assert_eq!(run.code, 0, "{}", run.err);
        assert_eq!(metro(&db, &["folders", "list"]).out, "");
    }

    /// A local HTTP server answering `connections` requests by path, so
    /// `feeds add` runs its real autodiscovery and fetch code without the
    /// internet. Routes are `(path, content type, body)` served with
    /// `200 OK`; unknown paths get a 404. Returns the base URL
    /// (`http://127.0.0.1:<port>`).
    fn serve(routes: Vec<(&'static str, &'static str, String)>, connections: usize) -> String {
        let routes = routes
            .into_iter()
            .map(|(path, content_type, body)| (path, "200 OK", content_type, body))
            .collect();
        serve_with_status(routes, connections)
    }

    /// [`serve`] with an explicit status line per route:
    /// `(path, status, content type, body)`.
    fn serve_with_status(
        routes: Vec<(&'static str, &'static str, &'static str, String)>,
        connections: usize,
    ) -> String {
        use std::io::{BufRead, BufReader};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().take(connections) {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(&stream);
                let mut request_line = String::new();
                let _ = reader.read_line(&mut request_line);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("");
                let route = routes.iter().find(|(p, _, _, _)| *p == path);
                let (status, content_type, body) = route.map_or(
                    ("404 Not Found", "text/plain", ""),
                    |(_, status, content_type, body)| (*status, *content_type, body.as_str()),
                );
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        base
    }

    /// A one-item RSS document; its links are relative, so they resolve
    /// against whichever local address served it.
    fn rss() -> String {
        "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel>\
         <title>Local Blog</title><link>/</link><description>d</description>\
         <item><title>One</title><link>/1</link><guid>g1</guid></item>\
         </channel></rss>"
            .to_string()
    }

    #[test]
    fn test_feeds_add_with_autodiscovery_end_to_end() {
        let db = TempDb(temp_db_path());
        // Two requests: discovery (`/`), then the feed fetch (`/feed.xml`).
        let base = serve_blog();

        let run = metro(&db, &["feeds", "add", &base]);
        assert_eq!(run.code, 0, "{}", run.err);
        assert_eq!(
            run.out,
            "Subscribed to f1 \"Local Blog\" (rss, 1 new entry) in folder \"Unsorted\"\n"
        );

        let run = metro(&db, &["--json", "feeds", "list"]);
        let value: serde_json::Value = serde_json::from_str(run.out.trim_end()).unwrap();
        assert_eq!(value["url"], format!("{base}/feed.xml"));
        assert_eq!(value["kind"], "rss");
        assert_eq!(value["folder_name"], "Unsorted");

        // Already subscribed: reported without any network access (the
        // server has stopped accepting connections by now).
        let run = metro(&db, &["feeds", "add", &format!("{base}/feed.xml")]);
        assert_eq!(run.code, 0, "{}", run.err);
        assert!(run.out.starts_with("Already subscribed: f1"), "{}", run.out);

        let run = metro(&db, &["feeds", "info", "f1"]);
        assert_eq!(run.code, 0, "{}", run.err);
        assert!(run.out.contains("Entries\t1\n"), "{}", run.out);

        let run = metro(&db, &["feeds", "rename", "f1", "Mine"]);
        assert_eq!(run.code, 0, "{}", run.err);
        assert_eq!(metro(&db, &["folders", "add", "Tech"]).code, 0);
        let run = metro(&db, &["feeds", "move", "f1", "--folder", "Tech"]);
        assert_eq!(run.code, 0, "{}", run.err);
        let run = metro(&db, &["feeds", "list", "--folder", "Tech"]);
        assert_eq!(run.out, format!("f1\tMine\t{base}/feed.xml\n"));

        let run = metro(&db, &["feeds", "remove", "f1"]);
        assert_eq!(run.code, 0, "{}", run.err);
        assert_eq!(metro(&db, &["feeds", "list"]).out, "");
    }

    /// Serve a blog homepage advertising its RSS feed, for two requests.
    fn serve_blog() -> String {
        // The port is only known once bound, so the page advertises the
        // feed with a relative href, resolved by autodiscovery.
        let html = "<html><head><link rel=\"alternate\" type=\"application/rss+xml\" \
                    href=\"/feed.xml\"></head><body></body></html>"
            .to_string();
        serve(
            vec![
                ("/", "text/html", html),
                ("/feed.xml", "application/rss+xml", rss()),
            ],
            2,
        )
    }

    #[test]
    fn test_feeds_add_dry_run_and_failures_end_to_end() {
        let db = TempDb(temp_db_path());
        let base = serve(
            vec![
                ("/feed.xml", "application/rss+xml", rss()),
                ("/broken.xml", "application/xml", "not a feed".to_string()),
            ],
            // dry run: 2, broken feed: 2, missing: 1 (discovery only)
            5,
        );

        let run = metro(
            &db,
            &["feeds", "add", &format!("{base}/feed.xml"), "--dry-run"],
        );
        assert_eq!(run.code, 0, "{}", run.err);
        assert!(
            run.out
                .starts_with("Dry run: would subscribe to \"Local Blog\""),
            "{}",
            run.out
        );
        assert_eq!(metro(&db, &["feeds", "list"]).out, "");
        assert_eq!(metro(&db, &["folders", "list"]).out, "");

        // Parse failure: exit 1, nothing saved.
        let run = metro(&db, &["feeds", "add", &format!("{base}/broken.xml")]);
        assert_eq!(run.code, 1, "{}", run.err);
        assert!(run.err.starts_with("metro: "), "{}", run.err);

        // HTTP 404 during discovery: network error, exit 4.
        let run = metro(&db, &["feeds", "add", &format!("{base}/missing.xml")]);
        assert_eq!(run.code, 4, "{}", run.err);
        assert_eq!(metro(&db, &["feeds", "list"]).out, "");
    }

    #[test]
    fn test_feeds_usage_and_not_found_exit_codes() {
        let db = TempDb(temp_db_path());
        assert_eq!(metro(&db, &["feeds", "info", "f9"]).code, 3);
        assert_eq!(metro(&db, &["feeds", "remove", "f9"]).code, 3);
        assert_eq!(
            metro(&db, &["feeds", "add", "ftp://example.com/feed"]).code,
            2
        );
        assert_eq!(
            metro(
                &db,
                &[
                    "feeds",
                    "add",
                    "http://127.0.0.1:9/feed.xml",
                    "--folder",
                    "Nope"
                ]
            )
            .code,
            3
        );
    }

    /// Subscribe to `urls` without network access (core `add_feed`), so a
    /// refresh can then hit feeds that would fail at `feeds add` time.
    fn add_feeds_offline(db: &TempDb, urls: &[String]) {
        let mut conn = metro_core::db::open_migrated(&db.0).unwrap();
        for url in urls {
            metro_core::feeds::add_feed(&mut conn, url, None, None).unwrap();
        }
    }

    #[test]
    fn test_refresh_and_view_exit_codes_end_to_end() {
        let db = TempDb(temp_db_path());
        // First refresh: 3 requests; second refresh: only the healthy feed
        // is due again (the failing ones are in backoff): 1 request.
        let base = serve_with_status(
            vec![
                ("/feed.xml", "200 OK", "application/rss+xml", rss()),
                (
                    "/error.xml",
                    "500 Internal Server Error",
                    "text/plain",
                    "boom".to_string(),
                ),
            ],
            4,
        );
        add_feeds_offline(
            &db,
            &[
                format!("{base}/feed.xml"),
                format!("{base}/error.xml"),
                format!("{base}/missing.xml"),
            ],
        );

        // Some feeds fail: report on stdout, error on stderr, exit 5.
        let run = metro(&db, &["refresh", "--jobs", "2"]);
        assert_eq!(run.code, 5, "{}", run.err);
        let lines: Vec<&str> = run.out.lines().collect();
        assert_eq!(lines.len(), 3, "{}", run.out);
        assert!(lines[0].starts_with("f2\tfailed\t"), "{}", run.out);
        assert!(lines[1].starts_with("f3\tfailed\t"), "{}", run.out);
        assert_eq!(
            lines[2],
            "Refreshed 3 feeds: 1 ok, 2 failed, 0 skipped; 1 new entry, 0 pruned"
        );
        assert_eq!(
            run.err,
            "metro: Partial success: 2 of 3 feeds failed to refresh\n"
        );

        // The healthy feed's entry is listed.
        let run = metro(&db, &["view", "f1"]);
        assert_eq!(run.code, 0, "{}", run.err);
        assert_eq!(run.out, format!("e1\t-\tunread\tOne\t{base}/1\n"),);
        let run = metro(&db, &["--json", "view", "f1", "--unread"]);
        let value: serde_json::Value = serde_json::from_str(run.out.trim_end()).unwrap();
        assert_eq!(value["guid"], "g1");
        assert_eq!(metro(&db, &["view", "f2"]).out, "");

        // Failed feeds are now in backoff: skipped, not failures, exit 0.
        let run = metro(&db, &["--json", "refresh"]);
        assert_eq!(run.code, 0, "{}", run.err);
        let value: serde_json::Value = serde_json::from_str(run.out.trim_end()).unwrap();
        assert_eq!(value["fetched"], 1);
        assert_eq!(value["skipped"], 2);
        assert!(run.err.is_empty(), "{}", run.err);

        // Usage and not-found errors.
        assert_eq!(metro(&db, &["view", "f9"]).code, 3);
        assert_eq!(metro(&db, &["view", "f1", "--limit", "0"]).code, 2);
        assert_eq!(metro(&db, &["refresh", "f9"]).code, 3);
        assert_eq!(metro(&db, &["refresh", "--folder", "Ghost"]).code, 3);
        assert_eq!(metro(&db, &["refresh", "--jobs", "0"]).code, 2);
    }

    #[test]
    fn test_refresh_all_feeds_failing_exits_1_end_to_end() {
        let db = TempDb(temp_db_path());
        let base = serve_with_status(
            vec![(
                "/error.xml",
                "500 Internal Server Error",
                "text/plain",
                "boom".to_string(),
            )],
            2,
        );
        add_feeds_offline(
            &db,
            &[format!("{base}/error.xml"), format!("{base}/missing.xml")],
        );

        let run = metro(&db, &["refresh"]);
        assert_eq!(run.code, 1, "{}", run.err);
        assert_eq!(
            run.err,
            "metro: Generic error: all 2 feeds failed to refresh\n"
        );
        assert!(
            run.out.ends_with(
                "Refreshed 2 feeds: 0 ok, 2 failed, 0 skipped; 0 new entries, 0 pruned\n"
            ),
            "{}",
            run.out
        );
    }

    #[test]
    fn test_refresh_without_feeds_exits_0_end_to_end() {
        let db = TempDb(temp_db_path());
        let run = metro(&db, &["refresh"]);
        assert_eq!(run.code, 0, "{}", run.err);
        assert!(run.out.starts_with("Refreshed 0 feeds"), "{}", run.out);
    }

    #[test]
    fn test_folders_errors_map_to_exit_codes_on_stderr() {
        let db = TempDb(temp_db_path());
        assert_eq!(metro(&db, &["folders", "add", "Tech"]).code, 0);

        let duplicate = metro(&db, &["folders", "add", "tech"]);
        assert_eq!(duplicate.code, 1);
        assert!(duplicate.out.is_empty());
        assert!(duplicate.err.starts_with("metro: "), "{}", duplicate.err);

        let missing = metro(&db, &["folders", "remove", "Ghost"]);
        assert_eq!(missing.code, 3);
        assert!(missing.err.contains("Ghost"), "{}", missing.err);

        let empty = metro(&db, &["folders", "add", " "]);
        assert_eq!(empty.code, 2);
    }
}
