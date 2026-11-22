# Tasks

## 1. Add HTTP and feed parsing dependencies

- [x] 1.1 Add `ureq`, `rustls`, `feed-rs`, and `scraper` to workspace dependencies in `Cargo.toml` and verify `cargo build --workspace` succeeds
- [x] 1.2 Update `crates/metro-core/Cargo.toml` to reference new dependencies via `workspace = true` and verify `cargo build -p metro-core` succeeds

## 2. Define data models

- [x] 2.1 Create `crates/metro-core/src/models/` with `id.rs`, `enums.rs`, `folder.rs`, `feed.rs`, `entry.rs` containing `Folder`, `Feed`, `Entry` structs, `FolderKind`/`FeedKind` enums, and typed ID wrappers (`FolderId`, `FeedId`, `EntryId`, `SavedItemId`) — verify tests pass for ID display format (`f12`, `e4031`) and serialization round-trip
- [x] 2.2 Export models module in `crates/metro-core/src/lib.rs` and verify `cargo build` succeeds

## 3. Author v001 database schema

- [x] 3.1 Write `crates/metro-core/src/db/migrations/v001__init.sql` with `folders`, `feeds`, `entries` tables, FK constraints (CASCADE on entries), `first_seen INTEGER DEFAULT 0` fallback column on entries, and performance indexes — verify migration runner creates all tables on fresh DB
- [x] 3.2 Add migration tests (fresh DB, version advance, idempotent re-run, FK enforcement) using `test-utils` in-memory DB — verify `cargo test --workspace --features test-utils` passes
- [x] 3.3 Update `openspec/specs/database-migrations/spec.md` delta with MODIFIED v001 requirements

## 4. Implement folder CRUD API

- [x] 4.1 Create `crates/metro-core/src/folders.rs` with `create_folder`, `list_folders`, `rename_folder`, `delete_folder`, `ensure_default` functions — verify unit tests pass with in-memory DB
- [x] 4.2 Export module in `lib.rs` — verify `cargo build` succeeds

## 5. Implement feed CRUD API

- [x] 5.1 Create `crates/metro-core/src/feeds.rs` with `add_feed`, `list_feeds`, `remove_feed`, `move_feed`, `rename_feed`, `get_feed_info` functions and URL normalization for dedup — verify unit tests pass
- [x] 5.2 Export module in `lib.rs` — verify `cargo build` succeeds

## 6. Implement URL autodiscovery

- [x] 6.1 Create `crates/metro-core/src/discovery.rs` with `discover_feed_url` using `scraper` for `<link rel="alternate">` parsing — verify unit tests with sample HTML documents
- [x] 6.2 Export module in `lib.rs` — verify `cargo build` succeeds

## 7. Implement feed fetching with conditional GET

- [x] 7.1 Create `crates/metro-core/src/fetch.rs` with `fetch_feed(url, etag, last_modified)` — 30s connect / 60s read timeout, `ureq` + `feed-rs` parsing, 304 handling, User-Agent, size cap, redirect limit — verify unit tests (304 passthrough, parse errors, timeout)
- [x] 7.2 Export module in `lib.rs` — verify `cargo build` succeeds

## 8. Implement threaded refresh worker pool

- [x] 8.1 Create `crates/metro-core/src/refresh.rs` with worker pool (default 8), parallel fetching, entry dedup (guid → url → content hash via UNIQUE constraint), progress channel, single-writer DB serialization, `last_fetch` written only on success (200/304) — verify unit tests with mock fetcher
- [x] 8.2 Persist fetched feed metadata in the refresh writer: `kind`/`site_url` on every 200, `title` only when `last_fetch IS NULL`; 304 leaves metadata unchanged — verify unit tests (kind update, first-fetch title fill, custom title preserved)
- [x] 8.3 Implement `subscribe_feed` (dedup check → fetch → single transaction: insert with final title + metadata, apply entries/validators/`last_fetch`; fetch failure writes nothing; dry-run returns metadata only) and update the provisional-metadata docs in `feeds.rs` — verify unit tests with mock fetcher
- [x] 8.4 Integrate backoff check (skip feeds in backoff) into refresh loop — verify backoff skip behavior
- [x] 8.5 Export module in `lib.rs` — verify `cargo build` succeeds

## 9. Implement exponential backoff

- [x] 9.1 Add `consecutive_failures` and `next_retry` columns to `feeds` table schema and update migration SQL — verify migration tests
- [x] 9.2 Implement backoff schedule (1m, 5m, 30m, 2h, 12h, 24h max) in `feeds.rs` — verify calculation tests
- [x] 9.3 Update `openspec/changes/m1-feeds-core/specs/database-migrations/spec.md` delta with v001 column additions

## 10. Implement entry pruning

- [x] 10.1 Create `crates/metro-core/src/prune.rs` with `prune_entries(window_days) -> count` using `published` date with `first_seen` fallback — verify unit tests with synthetic dates
- [x] 10.2 Integrate pruning into refresh completion (run after entries inserted) — verify refresh → prune workflow
- [x] 10.3 Export module in `lib.rs` — verify `cargo build` succeeds

## 11. Implement CLI: folders commands

- [x] 11.1 Create `crates/metro-cli/src/folders.rs` with subcommands `list`, `add`, `rename`, `remove` — `--kind feeds|saved` flag, default `feeds` — verify clap parsing tests and command output
- [x] 11.2 Wire `Folders` variant into `Commands` enum in `lib.rs` — verify `metro folders --help` works

## 12. Implement CLI: feeds commands

- [x] 12.1 Create `crates/metro-cli/src/feeds.rs` with subcommands `add`, `list`, `remove`, `move`, `rename`, `info` — `--folder`, `--title`, `--dry-run` flags; `add` runs autodiscovery then `subscribe_feed`, fetch failures exit 4 (network) / 1 (parse), `--dry-run` fetches and shows detected metadata without writing — verify clap parsing and command output
- [x] 12.2 Wire `Feeds` variant into `Commands` enum in `lib.rs` — verify `metro feeds --help` works

## 13. Implement CLI: refresh and view commands

- [x] 13.1 Create `crates/metro-cli/src/refresh.rs` with `Refresh` and `View` subcommands — `--folder`, `--jobs`, `--all`, `--unread`, `--limit` flags — verify clap parsing and command output
- [x] 13.2 Wire `Refresh` and `View` variants into `Commands` enum in `lib.rs` — verify `metro refresh --help` and `metro view --help` work
- [x] 13.3 Wire exit codes: 0 on success, 5 on partial feed failure — verify exit code behavior

## 14. Add integration tests for refresh workflow

- [x] 14.1 Create `crates/metro-core/tests/refresh_integration.rs` with 50-feed parallel refresh test, 304 check on second refresh, and pruning integration — verify under 10s completion
- [x] 14.2 Add test fixtures for RSS, Atom, and JSON Feed parsing tests — verify fixtures load correctly
- [x] 14.3 Mark network-dependent tests with `#[ignore]` or `METRO_TEST_NETWORK=1` gate — verify test selection

## 15. Update OpenSpec specs for M1

- [x] 15.1 Review all 10 spec files against implemented behavior and update requirements/scenarios as needed — verify `openspec validate` passes
- [x] 15.2 Verify all 10 spec delta files exist under `openspec/changes/m1-feeds-core/specs/` and reference the correct capabilities

## 16. Integrated verification

- [x] 16.1 Run `cargo fmt` and verify no formatting changes needed
- [x] 16.2 Run `cargo clippy --workspace --all-targets -- -D warnings` and fix all warnings
- [x] 16.3 Run `cargo test --workspace` and verify all unit tests pass
- [x] 16.4 Run `cargo test --workspace --release --features test-utils` and verify all tests including integration and migration tests pass
- [x] 16.5 Run `cargo clippy -- -W clippy::pedantic` and fix pedantic warnings
- [x] 16.6 Verify `metro --help` displays all M1 commands, `metro --version` works, and `metro` (no args) still shows TUI stub
- [x] 16.7 Verify `metro feeds add --help`, `metro folders list --help`, `metro refresh --help`, `metro view --help` all work
- [x] 16.8 Verify `--json` flag produces valid NDJSON on list commands

## 17. Address M1 review findings

Findings from the post-implementation review, with the decisions taken for each. Every `###` subsection is one planned commit (`feat: adding|fixing <name>`); finish each with the `validations-suite` skill plus `cargo clippy --workspace --all-targets --features test-utils -- -D warnings`. Subsections are independent unless a dependency is stated.

### 17.1 `feat: fixing per-feed entry deduplication`

- **Objective**: two feeds that publish the same guid or URL must each keep their entry. Dedup is scoped per feed.
- **Files**: `crates/metro-core/src/db/migrations/v001__init.sql`, `crates/metro-core/src/refresh/writer.rs`, `crates/metro-core/src/refresh/dedup.rs` (docs), `refresh/tests.rs`, `db/migrations/mod.rs` tests.

- [x] 17.1.1 Replace the schema-wide `UNIQUE(guid)` on `entries` with `UNIQUE(feed_id, guid)` in v001 (editable in place until M4) and add an index on `entries(feed_id, url)` — verify migration tests (fresh DB, constraint present, FK cascade still works)
- [x] 17.1.2 Scope the URL `NOT EXISTS` guard in `INSERT_ENTRY_SQL` to the same `feed_id`, and update the dedup docs (guid → url → content hash, all per feed) — verify new tests: same guid in two feeds stores two entries; same URL in two feeds stores two entries; a duplicate within one feed is still skipped

### 17.2 `feat: fixing out-of-window entry churn`

- **Objective**: entries already outside the prune window are never inserted, so `new_entries` and `pruned` only count real changes. Pruning keeps removing entries that age out.
- **Files**: `crates/metro-core/src/refresh/{writer.rs,mod.rs,subscribe.rs,tests.rs}`, `crates/metro-core/src/prune.rs` (shared cutoff helper), `crates/metro-core/tests/refresh_integration.rs`.
- **Depends on**: 17.1 (same insert statement).

- [x] 17.2.1 Expose the prune cutoff calculation from `prune.rs` (one source of truth) and have the refresh writer skip entries whose effective date (`published`, else now) is before the cutoff. `subscribe_feed` applies the same rule using the default window — verify unit tests: an old entry is not inserted and not counted; a recent entry is inserted; an undated entry is inserted
- [x] 17.2.2 Update the refresh/prune integration test, which expects old entries to be inserted and then pruned (`pruned == 50`), and the churn-documenting unit test from section 10, so they assert the new behaviour (old entries never appear, `pruned` counts only aged-out entries) — verify `cargo test -p metro-core --features test-utils --test refresh_integration`

### 17.3 `feat: fixing direct feed URL detection`

- **Objective**: `feeds add <url>` makes one request for a direct feed URL and works whatever the `Content-Type`. Parse the body as a feed first; if that fails, run `<link rel="alternate">` discovery on it as HTML.
- **Files**: `crates/metro-core/src/discovery.rs`, `crates/metro-core/src/refresh/subscribe.rs` (`subscribe_url`), `crates/metro-cli/src/feeds.rs` tests.

- [x] 17.3.1 Rework `subscribe_url` / discovery so the first response body is tried with `parse_feed`. On success it is used directly as the subscription fetch (no second GET). Otherwise the body is scanned for alternate links, and the discovered URL is fetched through `subscribe_feed`. Keep `FeedDiscoverer`/`FeedFetcher` injection for tests — verify tests: a feed served as `text/html` and as `text/plain` is subscribed; a direct feed causes exactly one request; an HTML page with a feed link is discovered; an HTML page without one still exits 3; dedup is checked before any network call

### 17.4 `feat: adding feed failure timestamp`

- **Objective**: a failed fetch records when it failed, as the feed-fetching spec requires.
- **Files**: `v001__init.sql`, `crates/metro-core/src/refresh/writer.rs`, `crates/metro-core/src/models/feed.rs`, `crates/metro-core/src/feeds.rs` (`FeedInfo`/`get_feed_info`), `crates/metro-cli/src/feeds.rs` (`info` output), tests.

- [x] 17.4.1 Add `last_error_at INTEGER` (nullable) to `feeds` in v001. `store_refresh_failure` sets it to the failure time; a successful fetch (200/304) clears it together with `error_state` — verify writer tests and the migration column test
- [x] 17.4.2 Surface it in `Feed`/`FeedInfo` and `metro feeds info` (RFC 3339 UTC, `never` when NULL) and in the JSON output — verify CLI output tests (text and `--json`)

### 17.5 `feat: fixing default folder protection`

- **Objective**: the default `Unsorted` folder can be neither deleted nor renamed.
- **Files**: `crates/metro-core/src/folders.rs`, `crates/metro-cli/src/folders.rs` tests.

- [x] 17.5.1 `delete_folder` and `rename_folder` refuse the default folder (a case-insensitive name match on `Unsorted` of kind `feeds`, or however `ensure_default` identifies it), using the same `MetroError` variant and exit code as refusing a non-empty folder deletion, with a clear message. Renaming another folder *to* `Unsorted` stays governed by the existing conflict rule — verify core tests (delete refused, rename refused, other folders unaffected) and CLI exit-code tests for `metro folders remove Unsorted` / `metro folders rename Unsorted X`

### 17.6 `feat: fixing typed ID prefixes in output`

- **Objective**: IDs look the same everywhere, and JSON values can be piped straight back into commands. Folder IDs get their own prefix.
- **Files**: `crates/metro-core/src/models/id.rs` (+ models docs), `crates/metro-cli/src/{output.rs,folders.rs,feeds.rs,refresh.rs}` tests, `crates/metro-core/tests/*` if they assert JSON.

- [x] 17.6.1 Change `FolderId` display/parse to `fo<N>` (e.g. `fo3`). Parsing matches the whole prefix exactly, so `fo3` is a folder and `f3` a feed; `FeedId` must reject `fo3` rather than misreading it, and a wrong-prefix hint names the expected prefix — verify id tests (display, parse, rejection of `f3` as a folder and of `fo3` as a feed)
- [x] 17.6.2 Serialize all typed IDs (`FeedId` `f12`, `FolderId` `fo3`, `EntryId` `e4031`, `SavedItemId` `s3`) as prefixed JSON strings, deserializing the same form — verify serde round-trip tests and the CLI `--json` tests (`folders list`, `feeds list`, `feeds add`, `view`, `refresh`), and that an `id` copied from JSON is accepted as a CLI argument

### 17.7 `feat: adding test-utils clippy to CI`

- **Objective**: CI also lints code gated on `test-utils` (DB-backed test modules, integration tests).
- **Files**: `.github/workflows/ci.yml`.

- [x] 17.7.1 Add a `cargo clippy --workspace --all-targets --features test-utils -- -D warnings` step next to the existing clippy step — verify the command passes locally

### 17.8 `feat: fixing pedantic lints in test code`

- **Objective**: `cargo clippy --workspace --all-targets --features test-utils -- -W clippy::pedantic` is clean (about 57 findings in `#[cfg(test)]` code of metro-core and metro-cli).
- **Files**: test modules in `crates/metro-core/src/**` and `crates/metro-cli/src/**`, `crates/metro-core/tests/*`.
- **Depends on**: best done after 17.1–17.6 so their new tests are covered too.

- [x] 17.8.1 Fix the pedantic findings in test code with real fixes (no blanket `allow`). Only use a targeted `#[expect(clippy::..., reason = "...")]` where a lint is genuinely inapplicable. Do not weaken assertions — verify the pedantic command above reports 0 warnings and all tests still pass

### 17.9 `feat: fixing documentation and spec drift`

- **Objective**: project docs and specs match the implementation and the decisions above. This is mostly OpenSpec and docs, so only the tracked files are committed.
- **Files**: `CLAUDE.md`, `AGENTS.md`, `openspec/specs/database-migrations/spec.md`, `openspec/changes/m1-feeds-core/{design.md,specs/**}`.
- **Depends on**: 17.1–17.6 (specs describe their final behaviour).

- [x] 17.9.1 Network test convention is `#[ignore]` only (run with `cargo test -- --ignored`). Remove `METRO_TEST_NETWORK=1` from `CLAUDE.md` and `AGENTS.md`. Replace the stale "planned / does not exist yet" lines for `test-fixtures/` and `crates/metro-core/tests/`, and document `test_utils::load_fixture` and the `required-features` integration-test targets
- [x] 17.9.2 Fix the main spec `openspec/specs/database-migrations/spec.md`, which says rusqlite uses the `jiff` feature; it uses `time`
- [x] 17.9.3 Record decisions D12–D17 in `design.md`: per-feed dedup (17.1), skip out-of-window entries at insert (17.2), parse-first discovery (17.3), `last_error_at` (17.4), protected default folder (17.5), `fo<N>` folder prefix plus prefixed JSON IDs (17.6). Also confirm the `view` default (20 most recent; `--all` = everything; `--unread` capped at 20; `--limit N` overrides) as the intended behaviour
- [x] 17.9.4 Update the delta specs to match: refresh (per-feed dedup, out-of-window skip), pruning (only aged-out entries), autodiscovery (parse-first, single request), feed-fetching (failure timestamp), feed-management/folder-management (default folder protection), data-models/feeds-cli (ID prefixes, JSON ID strings, `view` default). Also update database-migrations for the v001 changes — verify `openspec validate m1-feeds-core --strict`
- [x] 17.9.5 Add to `design.md` Non-goals, as deferred to milestone M3.5 "CLI completeness" (`docs/metro-feed-mvp-spec.md` §10), the items: `folders remove --move-to`, `feeds remove --keep-entries`, moving the feed on a duplicate `feeds add --folder`, the tree view for `feeds list`, and the global `--config`, `--no-color`, `-q/--quiet` flags

### 17.10 Integrated re-verification (no commit)

- [x] 17.10.1 Re-run the section 16 checks (validations-suite, `--help` and `--version` checks, NDJSON validity on `folders list`, `feeds list`, `view`) against the updated code, including a manual check that the JSON `id` values (`f…`, `fo…`, `e…`) round-trip as CLI arguments

## Workflow follow-up

- Archive the change after the project's review requirements are satisfied.
- Verify the archived result by reviewing synced main specs.
