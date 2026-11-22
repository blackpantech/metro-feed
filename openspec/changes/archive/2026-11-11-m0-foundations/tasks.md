# Tasks

## 1. Workspace Structure

- [x] 1.1 Replace root `Cargo.toml` with workspace manifest (`resolver = "3"`, members, workspace.dependencies) and verify `cargo metadata --format-version 1` lists all four crates
- [x] 1.2 Create `crates/metro/Cargo.toml` (bin, depends on metro-cli + metro-tui) and verify `cargo build -p metro` succeeds
- [x] 1.3 Create `crates/metro/src/main.rs` (dispatcher: argv.is_empty() → tui, else → cli) and verify `metro` prints TUI stub, `metro --help` prints clap help
- [x] 1.4 Create `crates/metro-cli/Cargo.toml` (lib, depends on metro-core, clap, clap_complete) and verify `cargo build -p metro-cli` succeeds
- [x] 1.5 Create `crates/metro-cli/src/lib.rs` (clap Command with --version, --help, -v, --log global flags) and verify `metro_cli::run(&["--version"])` exits 0
- [x] 1.6 Create `crates/metro-tui/Cargo.toml` (lib, zero dependencies) and verify `cargo build -p metro-tui` succeeds with no deps beyond std
- [x] 1.7 Create `crates/metro-tui/src/lib.rs` (stub: `pub fn run()` prints "TUI arrives in M5") and verify `metro_tui::run()` output contains "M5"
- [x] 1.8 Add `test-utils` feature to all four crates (passthrough to metro-core) and verify `cargo test --workspace --features test-utils` resolves without "failed to select feature" errors

## 2. Core Library - Error Types

- [x] 2.1 Create `crates/metro-core/Cargo.toml` (rusqlite bundled+jiff, serde, toml, thiserror, directories, jiff) and verify `cargo build -p metro-core` succeeds
- [x] 2.2 Create `crates/metro-core/src/error.rs` (`MetroError` enum with variants for exit codes 0-5, `exit_code()` method) and verify `MetroError::NotFound.exit_code()` returns 3
- [x] 2.3 Create `crates/metro-core/src/lib.rs` (re-export error, config, db modules) and verify `cargo doc -p metro-core --no-deps` generates docs without warnings

## 3. Core Library - Configuration

- [x] 3.1 Create `crates/metro-core/src/config.rs` (Config struct, load() with defaults < TOML < flag precedence) and verify unit test: flag overrides file, file overrides default
- [x] 3.2 Implement config path resolution using `directories` crate (dir name: `metro`) and verify on Linux: `$XDG_CONFIG_HOME/metro` or `~/.config/metro`
- [x] 3.3 Implement bootstrap() to create `config.toml` with defaults on first run and verify file is created when load() called with no existing file
- [x] 3.4 Ensure `--version` and `--help` do NOT trigger bootstrap and verify no config file created after `metro --version`
- [x] 3.5 Add unit tests for TOML parsing (valid/invalid) and verify invalid TOML returns parse error with line/column

## 4. Core Library - Database Layer

- [x] 4.1 Create `crates/metro-core/src/db/mod.rs` (open() function: WAL mode, busy_timeout, foreign_keys ON) and verify `PRAGMA journal_mode` returns `wal` on opened connection
- [x] 4.2 Implement busy timeout (5 seconds default) and verify concurrent access test: second connection waits instead of SQLITE_BUSY
- [x] 4.3 Implement foreign key enforcement and verify FK violation INSERT returns error
- [x] 4.4 Create `crates/metro-core/src/db/migrations/mod.rs` (run_migrations() using PRAGMA user_version) and verify fresh DB has user_version = 0
- [x] 4.5 Implement transactional migrations (rollback on failure) and verify failed migration leaves user_version unchanged
- [x] 4.6 Create `crates/metro-core/src/db/migrations/v001__init.sql` (empty or placeholder comment: "Actual schema in M1") and verify migration runner skips empty SQL gracefully

## 5. Core Library - Test Utilities

- [x] 5.1 Create `crates/metro-core/src/test_utils.rs` (temp_db_factory(), fixture_loader() behind `#[cfg(feature = "test-utils")]`) and verify `cargo test -p metro-core --features test-utils` includes temp_db tests
- [x] 5.2 Add integration test: open() → run_migrations() → verify user_version advanced and verify test passes with temp DB

## 6. CLI Integration

- [x] 6.1 Wire metro-cli to use metro-core's exit_code() mapping and verify `metro nonexistent-command` exits with code 2
- [x] 6.2 Implement global flag parsing (-v, --log as inert flags) and verify `metro -v --version` succeeds without "unrecognized flag" error
- [x] 6.3 Add integration test: dispatcher routes correctly and verify `cargo test -p metro` passes

## 7. CI Workflow

- [x] 7.1 Create `.github/workflows/ci.yml` (matrix: ubuntu-latest, macos-latest, windows-latest) and verify workflow file passes `actionlint`
- [x] 7.2 Add job: `fmt --check` and verify `cargo fmt --check --workspace` runs in CI
- [x] 7.3 Add job: `clippy --workspace --all-targets -- -D warnings` and verify CI fails on warnings
- [x] 7.4 Add job: `test --workspace` and verify CI runs all tests
- [x] 7.5 Add job: `test --workspace --features test-utils` and verify test-utils tests run in CI
- [x] 7.6 Trigger CI on push/PR and verify all three OSes pass green

## 8. Documentation Alignment

- [x] 8.1 Update `README.md`: roadmap → M0-M7, `metro add` → `metro save`, "Pure Rust" → "no system libraries (bundled SQLite)" and verify README links are valid
- [x] 8.2 Update `docs/metro-feed-mvp-spec.md` to v0.2: M0 exit criteria (no doctor), Decision log (topology B, freeze at M4, directories crate, --log flag) and verify spec sections are internally consistent
- [x] 8.3 Add M0 exit criteria to spec §10 and verify exit criteria match tasks completed

## 9. Integrated Verification

- [x] 9.1 Run `cargo fmt --check --workspace` and verify no formatting changes needed
- [x] 9.2 Run `cargo clippy --workspace --all-targets -- -D warnings` and verify no warnings (fix or expect)
- [x] 9.3 Run `cargo test --workspace` and verify all tests pass on Linux
- [x] 9.4 Run `cargo test --workspace --features test-utils` and verify test-utils tests pass
- [x] 9.5 Run `cargo build --release` and verify binary size (note for M7 budget, not enforce now)
- [x] 9.6 Verify `metro --version` outputs version from workspace manifest
- [x] 9.7 Verify `metro --help` shows clap-generated help with global flags
- [x] 9.8 Verify `metro` (no args) prints TUI stub message
- [x] 9.9 Cross-verify: README M0 exit criteria match what was implemented
- [x] 9.10 Cross-verify: spec v0.2 decision log matches implementation (topology B, freeze point, directories, --log)

## Workflow follow-up

- Archive the change after the project's review requirements are satisfied.
- Verify the archived result with `openspec validate --change m0-foundations`.