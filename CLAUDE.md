# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Workspace

- **Rust workspace**, edition **2024**, resolver 3. No `default-members`; `cargo build` without args builds all four crates.
- Crates: `metro` (dispatcher bin), `metro-cli` (lib), `metro-tui` (lib), `metro-core` (lib — all product logic).
- `metro` binary routes: no args → `metro_tui::run()`, any argv → `metro_cli::run()`.
- `metro-core` is an optional dep of `metro` and `metro-tui`. `metro`, `metro-cli`, and `metro-tui` all expose a `test-utils` feature that pipes through to `metro-core/test-utils`.

## Commands

```sh
# Formatting
cargo fmt --check

# Linting (CI hard fails on both)
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --features test-utils -- -D warnings   # lints test-utils-gated code too

# Tests (CI runs both)
cargo test --workspace
cargo test --workspace --features test-utils        # includes #[cfg(feature = "test-utils")] gates

# Subsets
cargo test -p metro-core                                     # one crate
cargo test -p metro-core --features test-utils folders::     # one module (DB-backed tests need the feature)
cargo test -p metro-core --features test-utils -- feeds::tests::<test_name> --exact   # one test
cargo test -p metro-core --features test-utils --test refresh_integration   # one integration target
cargo test --workspace --features test-utils -- --ignored   # network-dependent tests (none yet)

# Apply-change finishing sequence (from openspec/config.yaml):
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --workspace --release --features test-utils
cargo clippy -- -W clippy::pedantic
```

## Architecture

All product logic lives in `metro-core`; `metro-cli` and `metro-tui` are thin frontends over it ("two frontends, one core", zero duplicated logic). `metro-cli` implements `folders`, `feeds`, `refresh`, and `view` (one module per command family plus shared `output.rs`), with global `--json` (NDJSON for lists) and `--db <path>` flags; it only maps flags to core calls and renders results. `metro-tui` is a stub that prints `"TUI arrives in M5, use \`metro --help\`"`.

### metro-core modules (M1)

- **`config.rs`** — config loading. Config directory name is `metro` (not `metro-feed`), resolved via the `directories` crate → `$XDG_CONFIG_HOME/metro` on Linux, `~/Library/Application Support/metro` on macOS, `%APPDATA%/metro` on Windows.
- **`error.rs`** — single `MetroError` enum; the variant determines the process exit code (`Usage` for bad input/URLs, `NotFound`, `Network` for transport/HTTP failures, `Generic` for unparsable bodies, `PartialSuccess`). Pick variants accordingly — they are user-visible contracts.
Modules are grouped by concern, with one-way dependencies `feed_sync/` → `http/` / `parse.rs` / `store/` → `db/` / `url.rs` / `models/`:

- **`db/`** — plumbing only: `open()` / `open_in_memory()` plus the migration runner. `rusqlite` with `bundled` SQLite + `time` feature. Connections always enable WAL, 5s busy timeout, and FK enforcement. `db/rows.rs` holds the shared row-id/timestamp decoders (`feed_id_from_row_id`, `folder_id_from_row_id`, `timestamp_from_row`).
  - Migration runner is hand-rolled (no external crate): the `user_version` pragma tracks the version; migrations are embedded SQL strings executed in a transaction.
  - `v001__init.sql` defines `folders`, `feeds`, `entries` (feeds → entries cascade on delete). Callers must pass an already-migrated connection.
  - Schema freeze point: v001 is editable in place until v0.5 (M4). From M5, changes create real v002+ migrations with upgrade paths.
- **`models/`** — plain data types (`Folder`, `Feed`, `Entry`), enums (`FolderKind`: feed folders vs saved-item folders; `FeedKind`), and typed ID newtypes (`FolderId`, `FeedId`, `EntryId`, `SavedItemId`) that display/parse with a prefix (`fo3` folder, `f12` feed, `e4031` entry, `s3` saved item; the prefix must match exactly, so `fo3` is never a feed ID). In JSON they serialize as the same prefixed strings (`"id":"f12"`), so IDs from `--json` output can be passed back as CLI arguments. Use the typed IDs, never raw `u64`, across APIs.
- **`store/`** — persistence plus the product rules on it (not plain CRUD; deliberately not named "repositories", since there is no storage abstraction behind it).
  - **`folders.rs` / `feeds.rs`** — free functions taking `&Connection` (reads) or `&mut Connection` (writes). Every mutation runs in an `IMMEDIATE` transaction so check-then-write sequences are atomic across processes. Folder names are case-insensitive (ASCII only) and creation is idempotent; folder deletion refuses non-empty folders because the cascade would otherwise destroy feeds and entries. `ensure_default` provides the `Unsorted` feed folder, which can be neither deleted nor renamed. `get_feed_info` exposes `last_error_at` alongside `error_state`. Feed dedup relies on `url::normalize_url`.
  - **`prune.rs`** — `prune_entries` deletes entries older than the window (`published`, else `first_seen`); `window_cutoff` / `is_outside_window` are the single source of the cutoff, shared with the insert-time skip.
  - **`entries.rs`** — `list_feed_entries` with an `EntryFilter` (read state + limit) backs `metro view`: newest first, 20 by default.
- **`http/`** — network transport, **no DB access**.
  - **`client.rs`** — `Fetcher` / `fetch_feed` do HTTP via `ureq` with conditional GET (ETag / Last-Modified), timeouts, body cap, redirect limit, then hand the body to `parse::parse_feed`. Recording `error_state` / `last_error_at` on the feed row is `feed_sync`'s job.
  - **`discovery.rs`** — `HttpDiscoverer` (behind the `FeedDiscoverer` trait) makes **one** request and classifies the body whatever its `Content-Type`: parsed as a feed first (`Discovery::Feed`, reused as the subscription fetch), else scanned as HTML for `<link rel="alternate">` (`Discovery::Link`).
- **`parse.rs`** — `parse_feed` normalizes RSS / Atom / JSON Feed bytes via `feed-rs` into `FeedMetadata` + `FetchedEntry`. No I/O.
- **`url.rs`** — `normalize_url` (dedup key for feeds, also used by discovery) and `url_origin` (placeholder title / site URL). No I/O.
- **`feed_sync/`** — everything that writes fetched data into the DB. `refresh_feeds` (`mod.rs`): scoped `std::thread` worker pool (default 8) behind the `FeedFetcher` trait, single DB writer (`writer.rs`, one `IMMEDIATE` transaction per feed), backoff skip, progress channel, post-refresh prune. Retry schedule lives in `backoff.rs` (`next_retry_at`). Entry dedup (`dedup.rs`, guid → url → content hash) is scoped **per feed** (`UNIQUE(feed_id, guid)`). Entries already outside the prune window are never inserted. `subscribe.rs` has `subscribe_feed` / `subscribe_url` (fetch at add time, D11).

Module-level `//!` docs in each file document semantics and design decisions in detail; read them before modifying a module.

## Code cleanliness

- Keep a clean architecture. Follow SOLID principles.
- Document extensively the code to preserve knowledge and guide coding agents.
- Avoid using `crate::` or `super::` in production code. Import them explicitly at the top of files.
- Avoid using `.clone()`.
- No `.unwrap()` in production code. Catch all errors to avoid panics and crashes.

## Test conventions

- The `test-utils` feature gates the temp DB factory (`test_utils::temp_db_path()`), the in-memory DB helper (`db::open_in_memory()`), and the fixture helpers in `metro-core` (`test_utils::load_fixture("atom.xml")` reads a file from `test-fixtures/` as bytes).
- DB-backed test modules (`db`, `db/migrations`, `store/*`, `feed_sync`, …) are gated by `#[cfg(all(test, feature = "test-utils"))]`, so they silently run zero tests without `--features test-utils`. Pure modules (`models`, `error`, `config`, `http/client`, `parse`, `url`, `feed_sync/backoff`) use plain `#[cfg(test)]`. Sample feed strings shared by the parser and HTTP tests live in `parse::samples`.
- **Mock HTTP**: no mock-HTTP crate is a dependency. `http/client.rs` tests spin up a local `std::net::TcpListener` server serving canned responses. Follow that pattern (or discuss adding a dev-dependency such as `mockito`) rather than hitting the network.
- **Shared fixtures**: test feeds live in `test-fixtures/` (`rss2.xml`, `atom.xml`, `feed.json`, `not-a-feed.html`); load them with `test_utils::load_fixture` for parser tests without network.
- **Integration tests**: live in `crates/metro-core/tests/` (`refresh_integration.rs`, `feed_fixtures.rs`). Each is declared as a `[[test]]` target with `required-features = ["test-utils"]` in `crates/metro-core/Cargo.toml`, so Cargo skips it without the feature; add new ones the same way. Network-dependent tests are marked `#[ignore]` (the only convention) and run with `cargo test -- --ignored`, so CI stays non-flaky.
- **CI split**: `cargo test --workspace` runs unit tests only; `--features test-utils` additionally includes the gated tests and integration targets. Clippy runs both with and without `--features test-utils`.
- **Performance budget**: refresh of 50 feeds must complete in under 10s (spec §4); verify in integration tests.
- **Exit code testing**: verify CLI commands return the correct exit codes (0 = ok, 5 = partial success for feed failures).

## OpenSpec workflow

This project uses the **spec-driven** OpenSpec schema. Change artifacts live in `openspec/changes/`, main specs in `openspec/specs/`. In Claude Code the workflow is exposed as `/opsx:propose`, `/opsx:apply`, `/opsx:explore`, `/opsx:archive`, `/opsx:sync`, `/opsx:update` (`.claude/commands/opsx/`, `.claude/skills/openspec-*`); OpenCode equivalents live in `.opencode/`.

- `docs/metro-feed-mvp-spec.md` — product spec (milestones M0–M7, performance budgets referenced as "spec §N").
- `docs/tickets/T-*.md` — per-task tickets for the current milestone.
- `openspec/changes/m1-feeds-core/` — active change (`proposal.md`, `design.md` with decisions `D<N>`, `tasks.md`). Each numbered section in `tasks.md` is one planned commit; mark tasks `- [x]` as they are completed.
- `openspec/config.yaml` holds per-artifact authoring rules and `operations.apply.guidance`. When implementing tasks: work on one task section at a time and stop after each; annotate not-yet-used code with `#[cfg_attr(not(test), expect(dead_code, reason = "..."))]`; finish each commit with fmt → clippy `-D warnings` → `cargo test --workspace`, and each feature with the release `test-utils` run plus `clippy::pedantic`, fixing issues before running the next command.

Commit messages follow the `feat: adding <thing>` style seen in history.

## Gotchas

- `metro --help` and `metro --version` route through clap **even with no subcommand** (`metro --help` invokes the CLI; `metro` alone invokes the TUI stub).
- CLI global flags `-v/--verbose` and `--log` are accepted since M0 but still inert (not wired up yet); `--json` and `--db` are live.
- Exit codes: `0` ok, `1` generic error, `2` usage error, `3` not found, `4` network error, `5` partial success. Defined in `metro-core/src/error.rs`.
- CI runs on every push (all branches) and PRs to `main` / `develop`, across Linux + macOS + Windows. No local pre-commit hooks.
