# metro-feed — Agent Instructions

## Workspace

- **Rust workspace**, edition **2024**, resolver 3. No `default-members`; `cargo build` without args builds all four crates.
- Crates: `metro` (dispatcher bin), `metro-cli` (lib), `metro-tui` (lib), `metro-core` (lib — shared logic).
- `metro` binary routes: no args → `metro_tui::run()`, any argv → `metro_cli::run()`.
- `metro-core` is optional dep of `metro` and `metro-tui`. All three expose a `test-utils` feature that pipes through to `metro-core/test-utils`.

## Commands

```sh
# Formatting
cargo fmt --check

# Linting (CI hard fails)
cargo clippy --workspace --all-targets -- -D warnings

# Tests (CI runs both)
cargo test --workspace
cargo test --workspace --features test-utils        # includes #[cfg(feature = "test-utils")] gates

# Apply-change finishing sequence (from openspec/config.yaml):
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --workspace --release --features test-utils
cargo clippy -- -W clippy::pedantic
```

## Architecture notes

- **metro-core** owns config (`config.rs`), error types + exit code mapping (`error.rs`), DB connect + migration runner (`db/`).
- Config directory name is `metro` (not `metro-feed`), resolved via the `directories` crate → `$XDG_CONFIG_HOME/metro` on Linux, `~/Library/Application Support/metro` on macOS, `%APPDATA%/metro` on Windows.
- DB uses `rusqlite` with `bundled` SQLite + `time` feature. Connections always enable WAL, 5s busy timeout, and FK enforcement.
- Migration runner is hand-rolled (no external crate): `user_version` pragma tracks version, migrations are embedded SQL strings executed in a transaction. `v001__init.sql` exists but is deferred to M1 — no real schema tables yet.
- Schema freeze point: v001 is editable in place until v0.5 (M4). From M5, changes create real v002+ migrations with upgrade paths.
- `metro-tui` is currently a stub that prints `"TUI arrives in M5, use \`metro --help\`"`.

## Test conventions

- `test-utils` feature gates the temp DB factory, in-memory DB helper, and fixture loader in `metro-core`.
- Tests in `db/mod.rs` and `db/migrations/mod.rs` use `feature = "test-utils"` — they are skipped in a plain `cargo test --workspace` unless the feature is enabled.

## OpenSpec workflow

This project uses the **spec-driven** OpenSpec schema. Change artifacts live in `openspec/changes/`, main specs in `openspec/specs/`.

OpenCode commands are wired via `.opencode/commands/` and `.opencode/skills/`:
- `/opsx-propose` — create a change + all planning artifacts in one step
- `/opsx-apply` — implement tasks from a change
- `/opsx-explore` — investigate/design in explore mode
- `/opsx-archive` — archive a completed change
- `/opsx-sync` — sync delta specs to main specs
- `/opsx-update` — revise change planning artifacts

## Gotchas

- `metro --help` and `metro --version` route through clap **even when called with no subcommand** (i.e. `metro --help` invokes CLI, `metro` alone invokes TUI stub).
- CLI global flags `-v/--verbose` and `--log` are accepted from M0 but inert until M1.
- Exit codes: `0` ok, `1` generic error, `2` usage error, `3` not found, `4` network error, `5` partial success. Defined in `metro-core/src/error.rs`.
- CI runs on every push (all branches) and PRs to `main` / `develop`, across Linux + macOS + Windows. No local pre-commit hooks.
