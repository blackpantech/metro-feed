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
//!   types and handler live in their own module (`folders`, later `feeds`
//!   and `refresh`), each exposing `run(args, conn, output)`.
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

use crate::folders::FoldersArgs;
use crate::output::{IgnoreBrokenPipe, Output, OutputFormat, write_error};

mod database;
mod folders;
mod output;

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
    /// Manage feed and saved-item folders
    Folders(FoldersArgs),
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
    let mut err = std::io::stderr().lock();
    execute(&cli, &mut out, &mut err, stdout_is_terminal)
}

/// Execute parsed arguments, writing results to `out` and errors to `err`,
/// and return the exit code.
///
/// Separated from [`run`] so tests can capture both streams and choose the
/// terminal/pipe rendering.
fn execute(cli: &Cli, out: &mut dyn Write, err: &mut dyn Write, stdout_is_terminal: bool) -> i32 {
    let result = match &cli.command {
        // `metro <global flags>` without a command: show the top-level help.
        None => write!(out, "{}", Cli::command().render_help()).map_err(|e| write_error(&e)),
        Some(command) => {
            let format = OutputFormat::select(cli.json, stdout_is_terminal);
            dispatch(cli, command, &mut Output::new(out, format))
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
fn dispatch(cli: &Cli, command: &Commands, output: &mut Output<'_>) -> Result<(), MetroError> {
    match command {
        Commands::Folders(args) => {
            let mut conn = database::open(cli.db.as_deref())?;
            folders::run(args, &mut conn, output)
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
    fn test_no_subcommand_prints_help() {
        let cli = Cli::try_parse_from(["metro", "-v"]).unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(execute(&cli, &mut out, &mut err, false), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("folders"), "{out}");
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
        let code = execute(&cli, &mut out, &mut err, false);
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
