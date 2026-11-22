# Proposal

## Why

Milestone M0 from the metro-feed MVP spec establishes the foundational workspace and infrastructure that all subsequent milestones depend on. Without this foundation, there is no coherent structure for the CLI, TUI, or core library to build upon. This change creates the multi-crate workspace, the database migration mechanism, configuration loading, and CI infrastructure that enables the team to deliver M1 (feeds core) and beyond.

## What Changes

- **New 4-crate workspace structure** under `crates/`:
  - `metro` — dispatcher binary (30 lines), routes empty argv to TUI stub, all else to CLI
  - `metro-cli` — library crate with clap skeleton, global flags (`--version`, `--help`, `-v`, `--log`), exit-code mapping
  - `metro-tui` — library stub with zero dependencies, prints "TUI arrives in M5" until M5
  - `metro-core` — library crate with error types, configuration loading, database connection + migration runner, `test-utils` feature
- **Root workspace `Cargo.toml`** replaces the current single-package manifest:
  - `resolver = "3"`, workspace-wide dependency versions, shared lints
  - `test-utils` feature passthrough on all four crates for root-level testing
- **CI workflow** (`.github/workflows/ci.yml`):
  - Matrix: Linux, macOS, Windows
  - Jobs: `fmt --check`, `clippy --workspace --all-targets -- -D warnings`, `test --workspace`, `test --workspace --features test-utils`
- **Configuration bootstrap**:
  - Uses `directories` crate for XDG-style paths (config dir: `metro`, not `metro-feed`)
  - First-run creates `config.toml` with defaults (not on `--version`, which stays side-effect-free)
  - TOML parsing with `serde` + `toml`
- **Database layer skeleton**:
  - `rusqlite` with `bundled` + `jiff` features (bundled SQLite with FTS5 enabled, no system libraries)
  - WAL mode, busy timeout, foreign keys ON
  - Hand-rolled migration runner using `PRAGMA user_version` (no migration crate dependency)
  - Migrations live in `metro-core/src/db/migrations/` as embedded SQL
  - v001 deferred to M1 (schema evolves with need; no speculative tables in M0)
- **Documentation alignment**:
  - `README.md` updated to reflect M0–M7 milestone structure from spec v0.1
  - `docs/metro-feed-mvp-spec.md` receives v0.2 pass:
    - M0 exit criteria revised (no `doctor` command in M0)
    - Decision log updated: workspace topology B, schema freeze at M4 (v0.5), `directories` crate added to §9
    - `--log` flag noted as accepted from M0, semantics pinned before M4
- **Global flags** `-v/--verbose` and `--log` accepted from M0 but inert; wiring deferred to M1

**BREAKING**: The root `Cargo.toml` changes from a single package to a workspace. The existing `metro-feed` package entry is replaced. This is acceptable as no code exists yet.

## Capabilities

### New Capabilities

- `workspace-structure`: The project builds as a 4-crate workspace with a dispatcher binary, enabling parallel development of CLI and TUI frontends over a shared core. This capability owns the workspace manifest structure, crate organization, and the `test-utils` feature passthrough contract.
- `configuration`: The application loads configuration from a TOML file with platform-appropriate defaults, supports CLI flag overrides, and creates the config file on first run. This capability owns config path resolution, default values, TOML parsing, and bootstrap behavior.
- `database-migrations`: The application maintains a SQLite database with a hand-rolled migration system, WAL mode, busy timeout, and foreign key enforcement. This capability owns the migration runner, schema versioning via `user_version`, and the `open()` connection factory.

### Modified Capabilities

*(None — no specs exist yet in the repository)*

## Impact

- **Affected code**: Entire repository structure changes. Current `Cargo.toml` and `Cargo.lock` are replaced. All future development occurs within the workspace crates.
- **Dependencies added**: `clap` (+ `clap_complete`), `rusqlite` (bundled + jiff), `serde`, `serde_json`, `toml`, `thiserror`, `jiff`, `directories`. All pinned in workspace `Cargo.toml`.
- **CI/CD**: New GitHub Actions workflow required. All three target OSes must pass before merging.
- **Breaking changes**: None for end users (no release yet). Internal break: the root package manifest is replaced by a workspace manifest.
- **Regression risks**: Minimal — no functional code exists to regress. The migration runner is untested in production but covered by unit tests against synthetic migrations.
- **Compatibility**: Workspace structure is fixed; changing crate topology after M1 would break relative paths and imports. The dispatcher's no-args → TUI rule is now permanent (encoded in `metro` bin from day one).

## Requirements and Uncertainties

### Confirmed Requirements (from exploration conversation)

1. Workspace topology B: 4 crates (`metro`, `metro-cli`, `metro-tui`, `metro-core`), no `default-members`.
2. Schema evolves with need; v001 authored in M1, not M0.
3. Schema freeze point: v001 is revisable in place until v0.5 (M4); from M5 onward, changes are real v002+ migrations.
4. Migration runner is hand-rolled (no `rusqlite_migration` dependency).
5. `doctor` command deferred entirely to M1.
6. Config directory name: `metro` (project name stays `metro-feed`).
7. First run creates config file with defaults via `directories` crate.
8. `-v/--verbose` and `--log` flags accepted from M0, inert until M1.
9. README and spec doc (v0.2) aligned to these decisions.

### Deductions

- The `test-utils` feature must expose a temp-DB factory and fixture loader in `metro-core`, with passthrough features on all four crates so `cargo test --workspace --features test-utils` resolves.
- M0 exit criteria must be revised from the spec's original "DB is created and migrated" to "migration runner proven by tests on synthetic migrations" (no real schema yet).

### Provisional Assumptions

- `directories` crate will be chosen over `etcetera` (user researching; both listed in spec §9 for now).
- `--log` flag semantics (arity, level vs. file) will be pinned when logging is designed, no later than M4.

### Missing Information

- None blocking. The `directories` vs `etcetera` decision can be made during implementation without blocking.

## Questions

*(None — all decisions were resolved during exploration)*

## Ticket Analysis

**Business Objective**: Enable the team to deliver M1 (feeds core) by establishing a proven, tested workspace foundation with configuration loading, database connectivity, and CI infrastructure.

**Scope**:
- In: workspace structure, config loading, database connection + migration runner, CI workflow, documentation alignment.
- Out: `doctor` command (M1), actual database schema (M1), network layer (M1), feed fetching/parsing (M1).

**Out-of-Scope Items**:
- No feed management commands.
- No YouTube resolution.
- No refresh logic.
- No TUI implementation (stub only).

**Constraints**:
- No system libraries (bundled SQLite only).
- Cross-platform CI (Linux, macOS, Windows).
- `test-utils` feature must work at workspace root.
- `--version` must remain side-effect-free (no config file creation).

**Acceptance Criteria Summary**:
- `metro --version` and `metro --help` work.
- `cargo build --workspace` green on all three OSes.
- CI passes (fmt, clippy -D warnings, tests with and without `test-utils`).
- Migration runner tested against synthetic migrations in temp DBs.
- Config loading tested (defaults < TOML < flag precedence).
- Database `open()` applies WAL, busy_timeout, foreign_keys, tested.
- Dispatcher routes no-args to TUI stub, everything else to clap.
- README and spec v0.2 reflect the decision set.