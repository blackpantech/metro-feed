# Design

## Context

Current state: a single empty `metro-feed` package in `Cargo.toml` with no dependencies, no code, and no CI. The MVP spec (v0.1) defines M0–M7 milestones, but the repository has no workspace structure, no configuration loading, no database layer, and no CI infrastructure.

This design establishes the foundation for all subsequent milestones. The exploration conversation resolved key decisions: workspace topology B (4 crates with dispatcher binary), schema-by-need (v001 deferred to M1), hand-rolled migrations, config directory named `metro`, and `doctor` deferred to M1.

See proposal.md for motivation and business objectives.

## Goals / Non-Goals

**Goals:**
- Enable parallel development of CLI and TUI frontends over a shared core
- Prove the migration runner with synthetic tests (no real schema yet)
- Establish CI that catches formatting, lint, and test failures across all three target OSes
- Create a configuration system ready for M1's first-run behavior
- Align documentation (README + spec v0.2) with the decision set

**Non-Goals:**
- No feed management commands (M1)
- No actual database schema tables (v001 deferred to M1)
- No network layer or HTTP client (M1)
- No TUI implementation (stub only until M5)
- No `doctor` command (M1)

## Decisions

### Decision 1: Workspace Topology B (4 crates with dispatcher)

**Chosen:** `metro` (bin) + `metro-cli` (lib) + `metro-tui` (lib) + `metro-core` (lib)

**Rationale:** The dispatcher owns the "no args means TUI" product rule from day one. `metro-cli` and `metro-tui` are symmetric pure libraries that never depend on each other. M5 changes only `metro-tui`'s innards — the dispatch rule and its tests never move.

**Alternatives considered:**
- **Option A (3 crates, CLI owns bin):** Matches spec's literal wording but introduces "CLI crate depends on TUI crate" at M5, inverting the frontend relationship.
- **Option C (root package as bin):** Mixes manifest roles and requires the root package to depend on both frontends.

**Impact:** One extra 30-line crate, but the dependency DAG is stable forever.

### Decision 2: Hand-rolled migration runner

**Chosen:** ~50 lines using `PRAGMA user_version` + embedded SQL in transactions.

**Rationale:** Aligns with the tiny-dependency-tree principle. Migration logic is simple enough that a crate adds little value. Full control over error messages and rollback behavior.

**Alternatives considered:**
- **`rusqlite_migration` crate:** Tested runner, version tracking, but adds a dependency for ~50 lines of straightforward logic.

**Impact:** Implementation must handle edge cases (partial migrations, version skew) manually. Mitigated by comprehensive unit tests.

### Decision 3: Schema evolves with need, freeze at M4

**Chosen:** v001 authored in M1 alongside the features that need it. Revisable in place (delete dev DB) until v0.5 release at M4. From M5, changes are real v002+ migrations.

**Rationale:** Pre-release, nobody has production data. Schema churn is nearly free now, impossible later. Designing schema with the features that use it avoids speculative tables.

**Alternatives considered:**
- **Full §5 schema in v001 (M0):** More upfront work, but M1 lands on existing tables. Counter: fields left unpinned (error state shape, content_cache references) will get designed when M1/M3 land anyway.

**Impact:** M0 exit criteria revised to "runner proven by tests" instead of "DB created and migrated."

### Decision 4: Config directory named `metro`

**Chosen:** `directories` crate, config dir = `metro`.

**Rationale:** Users type `metro`, not `metro-feed`. Project name stays `metro-feed` (crate name, repo name), but the config dir matches the binary name.

**Alternatives considered:**
- **`metro-feed` config dir:** Matches project/crate name, but is what users never type.
- **`etcetera` crate:** Similar functionality, but `directories` is more battle-tested.

**Impact:** `directories` added to dependencies; `etcetera` remains listed as an alternative in spec §9 pending research.

### Decision 5: `--version` stays side-effect-free

**Chosen:** Config file created on first *real* command, not on `--version` or `--help`.

**Rationale:** `--version` should be a pure query. Creating files on `--version` surprises users and complicates scripting.

**Alternatives considered:**
- **Create config on any run:** Simpler bootstrap logic, but `--version` would have side effects.

**Impact:** M0 implements bootstrap code; users see it in M1 when first real command runs.

## Detailed Changes

| File | Action | Affected component | Change to make | Justification | Risks |
|------|--------|-------------------|----------------|---------------|-------|
| `Cargo.toml` | Replace | Workspace root | Convert to `[workspace]` with `resolver = "3"`, members, workspace.dependencies | Enables multi-crate structure | Existing Cargo.lock becomes invalid |
| `crates/metro/Cargo.toml` | Create | Dispatcher bin | 30-line bin, argv.is_empty() → tui, else → cli | Owns dispatch rule forever | None |
| `crates/metro/src/main.rs` | Create | Dispatcher bin | `if args.is_empty() { metro_tui::run() } else { metro_cli::run() }` | Implements Decision 1 | M0 TUI stub just prints message |
| `crates/metro-cli/Cargo.toml` | Create | CLI library | Depends on metro-core, clap, clap_complete | CLI skeleton | None |
| `crates/metro-cli/src/lib.rs` | Create | CLI library | clap Command with --version, --help, -v, --log | Global flags from M0 | Inert flags until M1 |
| `crates/metro-tui/Cargo.toml` | Create | TUI library | Zero dependencies (stub) | Reserve crate slot | None |
| `crates/metro-tui/src/lib.rs` | Create | TUI library | `pub fn run() { println!("TUI arrives in M5") }` | M0 stub | None |
| `crates/metro-core/Cargo.toml` | Create | Core library | rusqlite(bundled+jiff), serde, toml, thiserror, directories, jiff | Core dependencies | Bundled SQLite needs C compiler |
| `crates/metro-core/src/lib.rs` | Create | Core library | Re-export error, config, db modules | Public API | None |
| `crates/metro-core/src/error.rs` | Create | Core library | `MetroError` enum with exit_code() method | Exit codes 0-5 | Mapping must match spec |
| `crates/metro-core/src/config.rs` | Create | Core library | load() with defaults < TOML < flag precedence, bootstrap() | Config system | First-run timing must respect --version |
| `crates/metro-core/src/db/mod.rs` | Create | Core library | open() with WAL, busy_timeout, foreign_keys | Connection factory | WAL mode may not work on all FS |
| `crates/metro-core/src/db/migrations/mod.rs` | Create | Core library | run_migrations(conn) using user_version | Migration runner | Must handle partial migrations |
| `crates/metro-core/src/db/migrations/v001__init.sql` | Create | Core library | Empty or minimal placeholder (actual schema in M1) | First migration slot | None |
| `crates/metro-core/src/test_utils.rs` | Create | Core library | temp_db_factory(), fixture_loader() behind test-utils feature | Test infrastructure | Feature passthrough must work |
| `.github/workflows/ci.yml` | Create | CI | Matrix: linux/macos/windows; jobs: fmt, clippy -D, test (+test-utils) | Cross-platform CI | MSVC/clang on Windows runners |
| `README.md` | Update | Documentation | Roadmap → M0-M7, `metro add` → `metro save`, bundled-SQLite wording | Align with spec | None |
| `docs/metro-feed-mvp-spec.md` | Update | Documentation | v0.2 pass: M0 exit, freeze point, --log flag, directories in §9 | Decision log | None |

## Test Strategy

| Requirement / scenario | Component | Test type | Inputs / preconditions | Expected result | Likely location |
|------------------------|-----------|-----------|------------------------|-----------------|-----------------|
| Workspace builds as a unit | All crates | Integration | `cargo build --workspace` | Success on 3 OSes | CI workflow |
| Dispatcher routes by argv | metro bin | Unit | Empty vs non-empty args | TUI stub vs clap | `crates/metro/src/main.rs` |
| test-utils passthrough | All crates | Integration | `cargo test --workspace --features test-utils` | All tests run | CI workflow |
| Config path resolved | config.rs | Unit | XDG env vars set/unset | Correct path per platform | `crates/metro-core/src/config.rs` |
| Config precedence | config.rs | Unit | Defaults, file, CLI flag set | CLI wins, then file, then default | `crates/metro-core/src/config.rs` |
| First run creates config | config.rs | Integration | No config file exists | File created with defaults | `crates/metro-core/src/config.rs` |
| WAL mode enabled | db/mod.rs | Integration | Fresh DB opened | `PRAGMA journal_mode` = wal | `crates/metro-core/src/db/mod.rs` |
| Busy timeout set | db/mod.rs | Integration | Two concurrent connections | Second waits, no SQLITE_BUSY | `crates/metro-core/src/db/mod.rs` |
| Foreign keys enforced | db/mod.rs | Integration | FK violation INSERT | Error returned | `crates/metro-core/src/db/mod.rs` |
| Migration runner | migrations/mod.rs | Unit | Synthetic migrations in temp DB | user_version advances, rollback on failure | `crates/metro-core/src/db/migrations/mod.rs` |
| Exit codes | error.rs | Unit | Each MetroError variant | Correct exit code 0-5 | `crates/metro-core/src/error.rs` |

**Test categories covered:**
- Unit: error types, config parsing, migration runner logic
- Integration: database connections, workspace builds, feature passthrough
- Cross-platform: CI matrix ensures Linux/macOS/Windows compatibility
- Non-regression: fmt + clippy -D warnings on every commit

## Migration Plan

**Deployment:** This is the first implementation change — no prior code to migrate. The migration is:
1. Replace root `Cargo.toml` with workspace manifest
2. Create all four crate directories and manifests
3. Implement core library (error, config, db)
4. Implement dispatcher bin and CLI stub
5. Add CI workflow
6. Update README and spec doc

**Rollback:** Delete the `crates/` directory, restore original `Cargo.toml` from git history. No user data exists yet.

**Compatibility:** The workspace structure is permanent. Changing crate topology after M1 would break imports and relative paths.

## Open Questions

- **`directories` vs `etcetera`:** User is researching; M0 implements with `directories`, but the seam is swappable if research favors `etcetera`.
- **`--log` semantics:** Arity (file? level? both?) pinned when logging is designed, no later than M4. No M0 implementation impact — flag is inert.

## Operational Summary

**Chosen approach:** 4-crate workspace with dispatcher binary, hand-rolled migration runner, config system with `directories`, CI across 3 OSes. All decisions from exploration conversation are implemented.

**Mandatory constraints:**
- No system libraries (bundled SQLite only)
- `test-utils` feature works at workspace root
- `--version` remains side-effect-free
- Cross-platform CI (Linux, macOS, Windows)

**References:**
- Proposal: `openspec/changes/m0-foundations/proposal.md`
- Specs: `openspec/changes/m0-foundations/specs/{workspace-structure,configuration,database-migrations}/spec.md`

**Remaining assumptions:**
- `directories` crate will be final choice (vs `etcetera`)
- `--log` semantics deferred to M1/M4

**Residual risks:**
- Bundled SQLite build times on CI (mitigated: cached builds)
- WAL mode on network filesystems (mitigated: documented, user-configurable)
- Migration runner edge cases (mitigated: comprehensive unit tests)