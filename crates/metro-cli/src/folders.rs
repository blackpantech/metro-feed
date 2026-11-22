//! `metro folders …` commands.
//!
//! ```text
//! metro folders list                     [--kind feeds|saved]
//! metro folders add    <name>            [--kind feeds|saved]
//! metro folders rename <old> <new>       [--kind feeds|saved]
//! metro folders remove <name>            [--kind feeds|saved]
//! ```
//!
//! Feed folders and saved folders are separate namespaces, so every
//! subcommand takes `--kind` (default `feeds`, per the `feeds-cli` spec) to
//! pick the namespace the name is resolved in.
//!
//! This module only parses arguments and renders results; all rules (name
//! trimming, case-insensitive lookup, duplicate rejection, refusing to remove
//! a non-empty folder, protecting the default `Unsorted` feed folder) are
//! enforced by `metro_core::store::folders`, and errors are
//! returned untouched so their [`MetroError`] variant decides the exit code:
//!
//! | Situation                          | Core error              | Exit |
//! |------------------------------------|-------------------------|------|
//! | empty name                         | `Usage`                 | 2    |
//! | unknown folder (rename/remove)     | `NotFound`              | 3    |
//! | duplicate name (add/rename)        | `Generic`               | 1    |
//! | folder still contains feeds        | `Generic`               | 1    |
//! | default `Unsorted` (rename/remove) | `Generic`               | 1    |
//!
//! # Output
//!
//! - `list`: one folder name per line (human/tabular), or one
//!   [`Folder`] JSON object per line with `--json` (`{"id":"fo3",
//!   "name":"Tech","kind":"feeds"}`; IDs are prefixed strings, see
//!   `metro_core::models::id`). Folder IDs are omitted from text output on
//!   purpose: folders are addressed by name.
//! - `add` / `rename` / `remove`: a status line, or with `--json` the
//!   created / renamed / removed folder as one JSON object.

use clap::{Args, Subcommand, ValueEnum};
use metro_core::MetroError;
use metro_core::models::{Folder, FolderKind};
use metro_core::store::folders::{
    create_new_folder, delete_folder, find_folder, list_folders, rename_folder,
};
use rusqlite::Connection;

use crate::output::Output;

/// Arguments of `metro folders`.
#[derive(Debug, Args)]
pub struct FoldersArgs {
    /// Folder operation to run.
    #[command(subcommand)]
    pub command: FoldersCommand,
}

/// `metro folders` subcommands.
#[derive(Debug, Subcommand)]
pub enum FoldersCommand {
    /// List folders, sorted by name
    List {
        #[command(flatten)]
        kind: KindOption,
    },
    /// Create a folder (fails if it already exists)
    Add {
        /// Folder name (case-insensitive)
        name: String,
        #[command(flatten)]
        kind: KindOption,
    },
    /// Rename a folder (the default Unsorted folder cannot be renamed)
    Rename {
        /// Current folder name (case-insensitive)
        old_name: String,
        /// New folder name
        new_name: String,
        #[command(flatten)]
        kind: KindOption,
    },
    /// Remove an empty folder (refuses folders that still contain feeds and
    /// the default Unsorted folder)
    Remove {
        /// Folder name (case-insensitive)
        name: String,
        #[command(flatten)]
        kind: KindOption,
    },
}

/// The `--kind` option shared by every `folders` subcommand.
#[derive(Debug, Clone, Copy, Args)]
pub struct KindOption {
    /// Folder namespace to operate on
    #[arg(long, value_enum, default_value_t = KindArg::Feeds)]
    pub kind: KindArg,
}

/// CLI spelling of [`FolderKind`].
///
/// A separate enum keeps `clap` out of `metro-core`; the variants map 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum KindArg {
    /// Feed folders (hold subscriptions)
    Feeds,
    /// Saved-item folders (hold saved articles and videos)
    Saved,
}

impl From<KindArg> for FolderKind {
    fn from(kind: KindArg) -> Self {
        match kind {
            KindArg::Feeds => FolderKind::Feeds,
            KindArg::Saved => FolderKind::Saved,
        }
    }
}

/// Text columns of one folder in `folders list`.
fn folder_columns(folder: &Folder) -> Vec<String> {
    vec![folder.name().to_string()]
}

/// Run a `metro folders` subcommand against a migrated connection.
///
/// # Errors
///
/// Any [`MetroError`] from `metro_core::store::folders` (see the module table for
/// the exit codes), or [`MetroError::Generic`] if writing output fails.
pub fn run(
    args: &FoldersArgs,
    conn: &mut Connection,
    output: &mut Output<'_>,
) -> Result<(), MetroError> {
    match &args.command {
        FoldersCommand::List { kind } => {
            let folders = list_folders(conn, kind.kind.into())?;
            output.list(&folders, folder_columns)
        }
        FoldersCommand::Add { name, kind } => {
            let folder = create_new_folder(conn, name, kind.kind.into())?;
            let message = format!("Created {} folder \"{}\"", folder.kind(), folder.name());
            output.outcome(&folder, &message)
        }
        FoldersCommand::Rename {
            old_name,
            new_name,
            kind,
        } => {
            let kind = kind.kind.into();
            let old = find_folder(conn, old_name, kind)?;
            rename_folder(conn, old.id(), new_name)?;
            // Re-read so the result carries the name exactly as stored.
            let renamed = find_folder(conn, new_name, kind)?;
            let message = format!(
                "Renamed {kind} folder \"{}\" to \"{}\"",
                old.name(),
                renamed.name()
            );
            output.outcome(&renamed, &message)
        }
        FoldersCommand::Remove { name, kind } => {
            let folder = find_folder(conn, name, kind.kind.into())?;
            delete_folder(conn, folder.id())?;
            let message = format!("Removed {} folder \"{}\"", folder.kind(), folder.name());
            output.outcome(&folder, &message)
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::{Cli, Commands};

    /// Parse `metro folders …` and return the folders subcommand.
    fn parse(args: &[&str]) -> FoldersCommand {
        let command_line = ["metro", "folders"].iter().chain(args).copied();
        match Cli::try_parse_from(command_line).unwrap().command {
            Some(Commands::Folders(folders)) => folders.command,
            other => panic!("expected folders command, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_list_defaults_to_feeds() {
        assert!(matches!(
            parse(&["list"]),
            FoldersCommand::List {
                kind: KindOption {
                    kind: KindArg::Feeds
                }
            }
        ));
    }

    #[test]
    fn test_parse_list_saved() {
        assert!(matches!(
            parse(&["list", "--kind", "saved"]),
            FoldersCommand::List {
                kind: KindOption {
                    kind: KindArg::Saved
                }
            }
        ));
    }

    #[test]
    fn test_parse_add() {
        match parse(&["add", "Tech", "--kind", "feeds"]) {
            FoldersCommand::Add { name, kind } => {
                assert_eq!(name, "Tech");
                assert_eq!(kind.kind, KindArg::Feeds);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn test_parse_rename() {
        match parse(&["rename", "Tech", "Technology", "--kind", "saved"]) {
            FoldersCommand::Rename {
                old_name,
                new_name,
                kind,
            } => {
                assert_eq!(old_name, "Tech");
                assert_eq!(new_name, "Technology");
                assert_eq!(kind.kind, KindArg::Saved);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn test_parse_remove() {
        match parse(&["remove", "Old"]) {
            FoldersCommand::Remove { name, kind } => {
                assert_eq!(name, "Old");
                assert_eq!(kind.kind, KindArg::Feeds);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn test_parse_rejects_invalid_kind_and_missing_args() {
        for args in [
            &["folders", "list", "--kind", "videos"][..],
            &["folders", "add"][..],
            &["folders", "rename", "OnlyOne"][..],
            &["folders"][..],
        ] {
            let command_line = std::iter::once("metro").chain(args.iter().copied());
            let err = Cli::try_parse_from(command_line).unwrap_err();
            assert_eq!(err.exit_code(), 2, "args {args:?}");
        }
    }

    #[test]
    fn test_kind_arg_maps_to_folder_kind() {
        assert_eq!(FolderKind::from(KindArg::Feeds), FolderKind::Feeds);
        assert_eq!(FolderKind::from(KindArg::Saved), FolderKind::Saved);
    }
}

#[cfg(all(test, feature = "test-utils"))]
mod command_tests {
    use metro_core::db::migrations::run_migrations;
    use metro_core::db::open_in_memory;
    use metro_core::store::feeds::add_feed;
    use metro_core::store::folders::{create_folder, ensure_default};

    use super::*;
    use crate::output::OutputFormat;

    fn test_conn() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    fn kind(kind: KindArg) -> KindOption {
        KindOption { kind }
    }

    /// Run one command and return its rendered stdout.
    fn exec(
        conn: &mut Connection,
        format: OutputFormat,
        command: FoldersCommand,
    ) -> Result<String, MetroError> {
        let mut buf = Vec::new();
        run(
            &FoldersArgs { command },
            conn,
            &mut Output::new(&mut buf, format),
        )?;
        Ok(String::from_utf8(buf).unwrap())
    }

    fn list(conn: &mut Connection, format: OutputFormat, k: KindArg) -> String {
        exec(conn, format, FoldersCommand::List { kind: kind(k) }).unwrap()
    }

    fn add(conn: &mut Connection, name: &str, k: KindArg) -> Result<String, MetroError> {
        exec(
            conn,
            OutputFormat::Human,
            FoldersCommand::Add {
                name: name.to_string(),
                kind: kind(k),
            },
        )
    }

    #[test]
    fn test_list_empty_prints_nothing() {
        let mut conn = test_conn();
        assert_eq!(list(&mut conn, OutputFormat::Human, KindArg::Feeds), "");
    }

    #[test]
    fn test_add_then_list_sorted_and_filtered_by_kind() {
        let mut conn = test_conn();
        assert_eq!(
            add(&mut conn, "Tech", KindArg::Feeds).unwrap(),
            "Created feeds folder \"Tech\"\n"
        );
        add(&mut conn, "art", KindArg::Feeds).unwrap();
        add(&mut conn, "Reading", KindArg::Saved).unwrap();

        assert_eq!(
            list(&mut conn, OutputFormat::Tabular, KindArg::Feeds),
            "art\nTech\n"
        );
        assert_eq!(
            list(&mut conn, OutputFormat::Human, KindArg::Saved),
            "Reading\n"
        );
    }

    #[test]
    fn test_list_json_is_ndjson_folder_objects() {
        let mut conn = test_conn();
        add(&mut conn, "Tech", KindArg::Feeds).unwrap();
        add(&mut conn, "News", KindArg::Feeds).unwrap();

        let text = list(&mut conn, OutputFormat::Json, KindArg::Feeds);
        let folders: Vec<Folder> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let names: Vec<&str> = folders.iter().map(Folder::name).collect();
        assert_eq!(names, ["News", "Tech"]);
        assert!(folders.iter().all(|f| f.kind() == FolderKind::Feeds));
        // IDs are `fo`-prefixed strings, distinct from feed IDs.
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["id"], folders[0].id().to_string());
        assert!(first["id"].as_str().unwrap().starts_with("fo"), "{first}");
    }

    #[test]
    fn test_add_json_returns_created_folder() {
        let mut conn = test_conn();
        let text = exec(
            &mut conn,
            OutputFormat::Json,
            FoldersCommand::Add {
                name: " Tech ".to_string(),
                kind: kind(KindArg::Saved),
            },
        )
        .unwrap();
        let folder: Folder = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(folder.name(), "Tech");
        assert_eq!(folder.kind(), FolderKind::Saved);
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn test_add_duplicate_fails_case_insensitive() {
        let mut conn = test_conn();
        add(&mut conn, "Tech", KindArg::Feeds).unwrap();
        let err = add(&mut conn, "TECH", KindArg::Feeds).unwrap_err();
        assert_eq!(err.exit_code(), 1, "{err}");
        // Same name in the other namespace is fine.
        add(&mut conn, "Tech", KindArg::Saved).unwrap();
    }

    #[test]
    fn test_add_empty_name_is_usage_error() {
        let mut conn = test_conn();
        let err = add(&mut conn, "  ", KindArg::Feeds).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{err}");
    }

    #[test]
    fn test_rename_folder() {
        let mut conn = test_conn();
        add(&mut conn, "Tech", KindArg::Feeds).unwrap();
        let text = exec(
            &mut conn,
            OutputFormat::Human,
            FoldersCommand::Rename {
                old_name: "tech".to_string(),
                new_name: "Technology".to_string(),
                kind: kind(KindArg::Feeds),
            },
        )
        .unwrap();
        assert_eq!(text, "Renamed feeds folder \"Tech\" to \"Technology\"\n");
        assert_eq!(
            list(&mut conn, OutputFormat::Tabular, KindArg::Feeds),
            "Technology\n"
        );
    }

    #[test]
    fn test_rename_missing_or_conflicting() {
        let mut conn = test_conn();
        add(&mut conn, "Tech", KindArg::Feeds).unwrap();
        add(&mut conn, "News", KindArg::Feeds).unwrap();

        let rename = |old: &str, new: &str, k: KindArg| FoldersCommand::Rename {
            old_name: old.to_string(),
            new_name: new.to_string(),
            kind: kind(k),
        };

        let missing = exec(
            &mut conn,
            OutputFormat::Human,
            rename("Tech", "X", KindArg::Saved),
        )
        .unwrap_err();
        assert_eq!(missing.exit_code(), 3, "{missing}");

        let conflict = exec(
            &mut conn,
            OutputFormat::Human,
            rename("Tech", "news", KindArg::Feeds),
        )
        .unwrap_err();
        assert_eq!(conflict.exit_code(), 1, "{conflict}");
    }

    #[test]
    fn test_remove_empty_folder() {
        let mut conn = test_conn();
        add(&mut conn, "Empty", KindArg::Feeds).unwrap();
        let text = exec(
            &mut conn,
            OutputFormat::Json,
            FoldersCommand::Remove {
                name: "empty".to_string(),
                kind: kind(KindArg::Feeds),
            },
        )
        .unwrap();
        let removed: Folder = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(removed.name(), "Empty");
        assert_eq!(list(&mut conn, OutputFormat::Tabular, KindArg::Feeds), "");
    }

    #[test]
    fn test_remove_non_empty_folder_fails() {
        let mut conn = test_conn();
        let folder_id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        add_feed(
            &mut conn,
            "https://example.com/feed.xml",
            None,
            Some(folder_id),
        )
        .unwrap();

        let err = exec(
            &mut conn,
            OutputFormat::Human,
            FoldersCommand::Remove {
                name: "Tech".to_string(),
                kind: kind(KindArg::Feeds),
            },
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 1, "{err}");
        assert!(err.to_string().contains("contains"), "{err}");
        assert_eq!(
            list(&mut conn, OutputFormat::Tabular, KindArg::Feeds),
            "Tech\n"
        );
    }

    #[test]
    fn test_remove_default_folder_fails() {
        let mut conn = test_conn();
        ensure_default(&mut conn).unwrap();

        let err = exec(
            &mut conn,
            OutputFormat::Human,
            FoldersCommand::Remove {
                name: "unsorted".to_string(),
                kind: kind(KindArg::Feeds),
            },
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 1, "{err}");
        assert!(err.to_string().contains("default"), "{err}");
        assert_eq!(
            list(&mut conn, OutputFormat::Tabular, KindArg::Feeds),
            "Unsorted\n"
        );
    }

    #[test]
    fn test_rename_default_folder_fails() {
        let mut conn = test_conn();
        ensure_default(&mut conn).unwrap();

        let err = exec(
            &mut conn,
            OutputFormat::Human,
            FoldersCommand::Rename {
                old_name: "Unsorted".to_string(),
                new_name: "X".to_string(),
                kind: kind(KindArg::Feeds),
            },
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 1, "{err}");
        assert!(err.to_string().contains("default"), "{err}");
        assert_eq!(
            list(&mut conn, OutputFormat::Tabular, KindArg::Feeds),
            "Unsorted\n"
        );
    }

    #[test]
    fn test_saved_folder_named_unsorted_is_not_protected() {
        let mut conn = test_conn();
        add(&mut conn, "Unsorted", KindArg::Saved).unwrap();
        exec(
            &mut conn,
            OutputFormat::Human,
            FoldersCommand::Remove {
                name: "Unsorted".to_string(),
                kind: kind(KindArg::Saved),
            },
        )
        .unwrap();
        assert_eq!(list(&mut conn, OutputFormat::Tabular, KindArg::Saved), "");
    }

    #[test]
    fn test_remove_missing_folder_not_found() {
        let mut conn = test_conn();
        let err = exec(
            &mut conn,
            OutputFormat::Human,
            FoldersCommand::Remove {
                name: "Ghost".to_string(),
                kind: kind(KindArg::Feeds),
            },
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 3, "{err}");
    }
}
