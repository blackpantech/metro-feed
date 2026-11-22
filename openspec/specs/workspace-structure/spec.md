# Workspace Structure Specification

## Purpose

Enables the project to build as a 4-crate workspace with a dispatcher binary, supporting parallel development of CLI and TUI frontends over a shared core library.

## Requirements

### Requirement: Workspace builds as a unit
The root `Cargo.toml` SHALL define a workspace with `resolver = "3"` and members including all four crates (`metro`, `metro-cli`, `metro-tui`, `metro-core`). The workspace SHALL NOT set `default-members`, so `cargo build` without arguments builds all crates.

#### Scenario: Full workspace build succeeds
- **WHEN** `cargo build --workspace` is run from the repository root
- **THEN** all four crates compile without errors on Linux, macOS, and Windows

#### Scenario: Individual crate build works
- **WHEN** `cargo build -p metro-core` is run from the repository root
- **THEN** only the `metro-core` crate and its dependencies are built

### Requirement: Dispatcher routes by argv
The `metro` binary SHALL route empty `argv` (no arguments) to `metro_tui::run()`. Any non-empty argv SHALL route to `metro_cli::run()`.

#### Scenario: No arguments invokes TUI stub
- **WHEN** `metro` is run with no arguments
- **THEN** the TUI stub message is printed ("TUI arrives in M5, use `metro --help`")

#### Scenario: Help flag invokes clap
- **WHEN** `metro --help` is run
- **THEN** clap help output is displayed, not the TUI stub

#### Scenario: Version flag invokes clap
- **WHEN** `metro --version` is run
- **THEN** clap displays the version from the workspace manifest

### Requirement: test-utils feature passthrough
All four crates SHALL expose a `test-utils` feature. The workspace root SHALL support `cargo test --workspace --features test-utils` without unresolved feature errors.

#### Scenario: Root-level test with test-utils
- **WHEN** `cargo test --workspace --features test-utils` is run
- **THEN** all crates' tests execute, including those gated behind `#[cfg(feature = "test-utils")]`

#### Scenario: Individual crate test with test-utils
- **WHEN** `cargo test -p metro-core --features test-utils` is run
- **THEN** `metro-core` tests gated behind `test-utils` execute

### Requirement: Workspace dependencies are centralized
Shared dependencies (clap, rusqlite, serde, toml, thiserror, jiff, directories) SHALL be declared in `[workspace.dependencies]` with pinned versions. Crates SHALL reference workspace dependencies via `workspace = true` syntax.

#### Scenario: Dependency version update propagates
- **WHEN** a version in `[workspace.dependencies]` is changed
- **THEN** `cargo build --workspace` succeeds with the new version for all referencing crates

### Requirement: Exit codes are defined
The workspace SHALL define exit codes: 0 (ok), 1 (generic error), 2 (usage error), 3 (not found), 4 (network error), 5 (partial success). The mapping SHALL live in `metro-core` and be usable by both frontends.

#### Scenario: CLI returns exit code 0 on success
- **WHEN** `metro --version` runs successfully
- **THEN** the process exits with code 0

#### Scenario: CLI returns exit code 2 on usage error
- **WHEN** `metro` is run with an unrecognized flag
- **THEN** the process exits with code 2