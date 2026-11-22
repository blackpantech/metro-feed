# Design

## Context

M0 has established:
- 4-crate workspace: `metro` (bin), `metro-cli` (lib), `metro-tui` (lib), `metro-core` (lib).
- Database connection in `db/mod.rs` with WAL, busy_timeout (5s), foreign keys.
- Migration runner in `db/migrations/mod.rs` using `user_version` tracking. v001__init.sql is an empty placeholder.
- Config loading in `config.rs` with `directories` crate.
- Error types in `error.rs` with exit code mapping (0=ok, 1=generic, 2=usage, 3=not found, 4=network, 5=partial).
- CLI scaffolding in `metro-cli/src/lib.rs` with global `-v`/`--log` flags (inert) and a `Placeholder` subcommand.
- `test-utils` feature providing `temp_db_path()` and `open_in_memory()`.

**Constraint**: v001 schema is editable in place until M4. M1 is the first milestone to author v001.

**Dependency additions**: `ureq`, `rustls`, `feed-rs`, `scraper` to workspace dependencies and metro-core.

## Goals / Non-Goals

**Goals:**
- Define type-safe data models with prefixed ID display (`fo<N>`, `f<N>`, `e<N>`, `s<N>`), see D17.
- Author v001 SQL with `folders`, `feeds`, `entries` tables, FK constraints, and indexes.
- Implement folder and feed CRUD APIs in `metro-core`.
- Implement feed fetching with conditional GET and 304 handling.
- Implement threaded refresh worker pool (default 8 workers) with dedup and progress reporting.
- Implement exponential backoff for failing feeds.
- Implement 30-day entry pruning post-refresh.
- Implement URL autodiscovery via HTML `<link rel="alternate">` parsing.
- Wire all APIs through CLI commands with `--json` NDJSON output and proper exit codes.

**Non-Goals:**
- YouTube channel resolution (M2).
- Saved items, reading, and content extraction (M3).
- Full-text search (M4).
- TUI implementation (M5).
- `--dry-run` for pruning (deferred to M4).
- Performance benchmarking (M7).
- `metro doctor` command (deferred to M1 from M0).
- Deferred to milestone M3.5 "CLI completeness" (`docs/metro-feed-mvp-spec.md` §10), which completes the §6–§7 CLI surface before the M4 CLI 1.0 freeze:
  - `folders remove --move-to <name>` (move a non-empty folder's feeds, then delete it); M1 only refuses non-empty folders.
  - `feeds remove --keep-entries` (keep entries after unsubscribing; needs a v001 schema decision before the M4 freeze).
  - Moving the existing feed on a duplicate `feeds add --folder <name>`; M1 only reports the duplicate and changes nothing.
  - The tree view for `feeds list` without `--folder` (human output only); M1 prints a flat list with a folder column.
  - The global flags `--config <path>`, `--no-color` (also honoring `NO_COLOR`), and `-q/--quiet`.

## Decisions

### D1: Worker pool uses `std::thread` (no async runtime)
**Chosen**: `std::thread::spawn` with `std::sync::mpsc`/crossbeam channels for progress reporting.
**Why**: The MVP spec (Decision 1) specifies blocking HTTP client. Adding tokio/async would double the dependency surface. The refresh operation is infrequent (startup, manual) so thread-per-feed is acceptable.
**Alternatives**: tokio-threadpool (heavier, needs runtime setup), rayon (data-parallel, not I/O-bound).

### D2: Entry dedup uses database UNIQUE constraint + INSERT OR IGNORE
**Chosen**: Add `UNIQUE(guid)` on entries table, use `INSERT OR IGNORE` in refresh.
**Why**: Database-level dedup is the most reliable — handles concurrent refreshes and is simpler than in-memory dedup with rollback.
**Alternatives**: In-memory set dedup (loses concurrent safety), full content hash dedup (expensive for first comparison).
**Superseded in part by D12**: the constraint is `UNIQUE(feed_id, guid)`, not `UNIQUE(guid)`.

### D3: Pruning deletes entries via CASCADE
**Chosen**: `ON DELETE CASCADE` on `entries.feed_id` FK, pruning uses `DELETE FROM entries WHERE ...`.
**Why**: Simple and correct — when a feed is removed, all its entries go with it. No orphan entries possible.
**Alternatives**: Soft-delete flag (adds complexity, no benefit for M1).

### D4: Autodiscovery uses `scraper` crate (not a full HTML parser)
**Chosen**: `scraper` for CSS-selector-based `<link rel="alternate">` extraction.
**Why**: `scraper` is lightweight, designed for HTML extraction, and avoids the full DOM parser dependency. It handles malformed HTML gracefully.
**Alternatives**: `html5ever` (full parser, heavier), regex-based parsing (fragile).

### D5: Feed kind is inferred from parse result, not user input
**Chosen**: `feed-rs` parses the response and we map the result type to `FeedKind`.
**Why**: The feed format is determined by the content, not the URL. This avoids misclassification and handles feeds that claim one type but deliver another.
**Alternatives**: URL-based classification (error-prone), user-specified type (adds friction).

### D6: Backoff state stored as `consecutive_failures` + `next_retry` columns
**Chosen**: Two integer columns on `feeds` table. On failure, `next_retry` = `now + backoff_seconds(consecutive_failures)`. `last_fetch` is written **only on success** (200 or 304) and is never touched by a failure.
**Why**: Simpler than storing timestamps for each failure. A single integer comparison against `now()` suffices for skip decision. Keeping `last_fetch` success-only means `last_fetch IS NULL` reliably means "never fetched successfully", which D10 relies on.
**Alternatives**: Store last failure timestamp and calculate backoff on read (requires recalculating), store individual failure times (overkill).

### D7: Pruning uses `published` with first-seen fallback
**Chosen**: `entries.published` column; when NULL, fall back to `ROWID` as a proxy for insertion order, compared against a `first_seen` concept.
**Why**: Published date is the user-facing semantic. Some feeds omit it; we need a deterministic fallback.
**Alternatives**: Add explicit `first_seen` column (cleaner but more schema changes), always use published (breaks feeds without it).

### D8: CLI modules organized by command family in `metro-cli/src/`
**Chosen**: Separate files `folders.rs`, `feeds.rs`, `refresh.rs` in metro-cli, each implementing a `run(&Cli, &Command)` function.
**Why**: Follows the existing single-file pattern, scales well as commands grow, keeps each command family self-contained.
**Alternatives**: One file per sub-subcommand (too granular), all commands inline in lib.rs (unmaintainable).

### D9: ID type wrappers use `newtype` pattern
**Chosen**: `struct FolderId(u64)`, `struct FeedId(u64)`, etc. — Rust newtypes with `Display`/`FromStr` impls.
**Why**: Compile-time safety (cannot pass FolderId to FeedId parameter), zero runtime cost, clean API.
**Alternatives**: `typenum`-style at compile-time IDs (complex, no runtime value), plain `u64` with comments (error-prone).

### D10: Persisting fetched feed metadata
**Chosen**: On a 200 response, the refresh writer updates `kind` and `site_url` from the parsed feed every time. It updates `title` only when `last_fetch IS NULL` (first successful fetch) and the feed declares a non-blank title. A 304 leaves all metadata unchanged.
**Why**: The document decides kind and site URL (D5), and feeds can migrate between formats. Titles are user-editable (`--title`, `feeds rename`) and the schema has no "custom title" flag, so the title is set once and never overwritten afterwards. That requires no schema change.
**Alternatives**: Always overwrite the title (clobbers user titles), a `title_is_custom` column (schema change for little gain), comparing against the host-derived placeholder (brittle).

### D11: Subscribing fetches at add time
**Chosen**: A core `subscribe_feed(conn, fetcher, url, title, folder, dry_run)`:
1. Normalizes the URL; if it is already subscribed, returns the existing feed unchanged without fetching.
2. Fetches the feed.
3. On fetch failure, returns the error and writes nothing.
4. Otherwise, in one transaction, inserts the feed row with its final title (explicit `title`, else the feed's title, else the host placeholder) and the parsed `kind`/`site_url`, then applies the fetch result exactly as a refresh would (entries, `etag`, `last_modified`, `last_fetch`).
5. With `dry_run`, stops after the fetch and returns the metadata it would store.

`metro feeds add` calls it after autodiscovery. The core `add_feed` stays network-free and stores provisional metadata (placeholder kind `rss`, host title, origin site URL). It is the building block for later import paths, and D10's first-success rule fills in its title.
**Why**: Users see the real kind and title immediately, typos and dead URLs fail fast, and sharing the refresh writer path keeps validators and entries consistent. Storing validators without entries would make the next refresh get a 304 and never insert those entries.
**Alternatives**: Metadata-only fetch at add time (rename before first refresh gets overwritten; duplicate download), saving with placeholders on failure (silently accepts dead URLs).

### D12: Entry dedup is scoped per feed
**Chosen**: v001 declares `UNIQUE(feed_id, guid)` on `entries` (replacing D2's schema-wide `UNIQUE(guid)`) plus an index on `entries(feed_id, url)`. Every dedup tier (guid → url → content hash) compares only entries of the same feed: the dedup key goes through `INSERT OR IGNORE` on the composite constraint, and the URL guard of the insert statement (`NOT EXISTS` an entry with the same non-empty URL) is restricted to the same `feed_id`.
**Why**: Two feeds can legitimately publish the same guid or URL (mirrors, aggregators, a blog's main and per-tag feeds). A global constraint silently dropped the second feed's entry, so that feed looked empty. v001 is still editable in place (until M4), so the fix needs no migration.
**Alternatives**: Prefix the stored guid with the feed ID (keeps `UNIQUE(guid)`, but leaks an encoding into the column and complicates lookups), keep global dedup (loses entries).

### D13: Out-of-window entries are skipped at insert time
**Chosen**: `prune.rs` exposes the cutoff calculation (`window_cutoff`, `is_outside_window`) as the single source of truth. The refresh writer skips a fetched entry whose effective date (`published`, else now) is strictly before the cutoff: it is not inserted and not counted in `new_entries`. A refresh computes the cutoff once from its start time and reuses it for the post-refresh prune. `subscribe_feed` applies the same rule with the default window (30 days). Undated and future-dated entries are always inserted.
**Why**: Feeds that keep years of items in their documents previously had their old entries inserted and then deleted by the prune on every refresh. `new_entries` and `pruned` reported churn instead of real changes. With the skip, `pruned` only counts entries that were stored inside the window and later aged out.
**Alternatives**: Insert then prune (the previous behaviour, churns every refresh), exclude just-inserted rows from the prune (hides the churn in the counters but still writes and deletes rows).

### D14: Autodiscovery parses the body as a feed first
**Chosen**: `subscribe_url` requests the user's URL once through a `FeedDiscoverer`. The body is first parsed as a feed whatever its `Content-Type`. On success the URL is a direct feed and that parsed response **is** the subscription fetch (`Discovery::Feed`: entries, metadata, and validators), so no second request is made. Otherwise the same body is scanned as HTML for a `<link rel="alternate">` feed link (`Discovery::Link`), which is then fetched through `subscribe_feed`. When neither works, a response served with a feed media type is a broken feed (generic error, exit 1) and anything else is a page without a feed (not found, exit 3). Duplicate and folder checks still run before any request.
**Why**: Servers routinely serve feeds as `text/html`, `text/plain`, or `application/octet-stream`, so trusting `Content-Type` rejected valid feeds. It also downloaded a direct feed twice (once for discovery, once for the subscription fetch).
**Alternatives**: `Content-Type` decides (rejects mislabelled feeds), sniff the first bytes (`<rss`, `<feed`, `{`) (fragile heuristic; parsing is the authoritative test and the body is already in memory).

### D15: Failed fetches record `last_error_at`
**Chosen**: v001 adds a nullable `last_error_at INTEGER` (Unix seconds) to `feeds`. `store_refresh_failure` sets it to the failure time together with `error_state`, and a successful fetch (200 or 304) clears both. It is exposed on `Feed` and `FeedInfo`, in `metro feeds info` as "Last error" (RFC 3339 UTC, `never` when NULL), and in JSON as Unix seconds or `null`.
**Why**: The feed-fetching spec requires the error state to carry a timestamp. `last_fetch` is success-only (D6) and `next_retry` is derived from the failure time plus backoff, so neither tells when a feed last failed.
**Alternatives**: Derive the failure time from `next_retry` minus the backoff delay (indirect, breaks if the backoff schedule changes), embed the time in the `error_state` text (not queryable).

### D16: The default `Unsorted` folder is protected
**Chosen**: `delete_folder` and `rename_folder` refuse the default feed folder (`Unsorted` of kind `feeds`, as created by `ensure_default`, matched case-insensitively) with `MetroError::Generic` (exit 1), the same variant as refusing a non-empty folder deletion, and a message naming it as the default folder. Renaming another folder *to* `Unsorted` is only governed by the usual name-conflict rule. Product spec decision 6 is updated accordingly: default folders are neither renamable nor deletable (`Inbox` follows the same rule when it arrives in M3).
**Why**: `feeds add` without `--folder` always targets `Unsorted`. Renaming it would make `ensure_default` silently create a second `Unsorted`, and deleting it would only defer the same surprise. A fixed default is predictable for users and scripts.
**Alternatives**: Allow renaming and track the default by a flag column (schema change, and the "default" would then have an arbitrary name), allow deletion and let `ensure_default` recreate it on the next add (the earlier behaviour: the deletion silently does not stick).

### D17: Folder IDs use `fo<N>` and JSON carries prefixed ID strings
**Chosen**: `FolderId` displays and parses as `fo<N>` (e.g. `fo3`). Parsing takes the leading run of ASCII letters as the prefix and requires an exact, case-sensitive match, so `fo3` is a folder ID and is rejected as a feed ID (and `f3` as a folder ID) with a hint naming the expected `<prefix><number>` form. All typed IDs (`fo3`, `f12`, `e4031`, `s3`) serialize to JSON as the same prefixed **strings** and deserialize only from that form. The database keeps plain integer row IDs.
**Why**: With `FolderId` displayed as `f<N>`, a folder and a feed with the same row ID looked identical. JSON numbers could not be pasted back into commands that take `f12`/`e4031`. One external form everywhere (text, JSON, arguments) makes output scriptable.
**Alternatives**: Keep JSON numbers (consumers must re-add the prefix), `d<N>` or `g<N>` for folders (less mnemonic than `fo`), prefix-insensitive parsing (would let `fo3` be misread as feed 3 with a typo).

### `view` default (confirmed)
`metro view <f-id>` lists the 20 most recent entries by default. `--all` lists every entry, `--unread` restricts to unread entries and keeps the 20-entry cap, and `--limit <n>` overrides both the default cap and `--all`. This is the intended behaviour, not a placeholder: a feed can hold up to 30 days of entries, and the default keeps the terminal output short while `--all` and `--limit` cover scripting.

## Detailed Changes

| File | Action | Affected component | Change to make | Justification | Risks |
|---|---|---|---|---|---|
| `crates/metro-core/src/lib.rs` | Modify | Module exports | Add `pub mod models;`, `pub mod folders;`, `pub mod feeds;`, `pub mod fetch;`, `pub mod refresh;`, `pub mod prune;`, `pub mod discovery;`, `pub mod entries;` | Expose new modules for CLI and tests | New module references must compile |
| `crates/metro-core/src/models/` | Create | Data models | Define `id.rs` (ID wrappers), `enums.rs` (FolderKind, FeedKind), `folder.rs` (Folder), `feed.rs` (Feed), `entry.rs` (Entry), `mod.rs` (re-exports). Derive serde. | Provides type-safe domain types for all M1 APIs | Must match v001 schema column names |
| `crates/metro-core/src/db/migrations/v001__init.sql` | Modify | Schema | Add `folders`, `feeds`, `entries` CREATE TABLE statements, FK constraints, performance indexes. | First real schema replaces M0 placeholder | Must be idempotent-safe; schema editable until M4 |
| `crates/metro-core/src/folders.rs` | Create | Folder API | Implement `create_folder`, `list_folders`, `rename_folder`, `delete_folder`, `ensure_default` functions. | Core folder CRUD backend | FK constraint enforcement for non-empty folder deletion |
| `crates/metro-core/src/feeds.rs` | Create | Feed API | Implement `add_feed`, `list_feeds`, `remove_feed`, `move_feed`, `rename_feed`, `get_feed_info` functions with URL normalization. | Core feed CRUD backend | URL normalization edge cases |
| `crates/metro-core/src/fetch.rs` | Create | Feed fetching | Implement `fetch_feed(url, etag, last_modified)` → `(FeedParseResult, etag, last_modified)`. Uses `ureq` + `feed-rs`. | HTTP fetching with conditional GET | Timeout and error handling edge cases |
| `crates/metro-core/src/refresh/` | Create | Refresh pipeline | `mod.rs`: `refresh_feeds(feed_ids, concurrency)` with worker pool and channel-based progress; `writer.rs`: single-writer persistence, per-feed dedup (D12), out-of-window skip (D13), metadata (D10), `last_error_at` (D15); `dedup.rs`: dedup keys; `subscribe.rs`: `subscribe_feed` (D11) and parse-first `subscribe_url` (D14). | Parallel feed fetch orchestration and subscribe-time first fetch | Channel buffering, panic recovery in workers |
| `crates/metro-core/src/prune.rs` | Create | Entry pruning | Implement `prune_entries(window_days) -> count`. | Age-based cleanup | Date handling with jiff crate |
| `crates/metro-core/src/entries.rs` | Create | Entry read API | Implement `list_feed_entries(feed_id, EntryFilter)` for `metro view` (20 most recent by default). | Keeps `view` rules in the core for the TUI | Ordering of undated entries |
| `crates/metro-core/src/discovery.rs` | Create | Autodiscovery | Implement `discover_feed_url(homepage_url) -> feed_url` using `scraper`. | Blog homepage to feed URL | HTML varies across sites |
| `crates/metro-cli/src/lib.rs` | Modify | CLI commands | Expand `Commands` enum: `Folders(FoldersCmd)`, `Feeds(FeedsCmd)`, `Refresh(RefreshCmd)`, `View(ViewCmd)`. Wire to handler modules. | CLI command surface | Clap derive complexity grows |
| `crates/metro-cli/src/folders.rs` | Create | CLI folders | Implement `FoldersCmd` subcommand (list/add/rename/remove). | Folder CLI commands | None |
| `crates/metro-cli/src/feeds.rs` | Create | CLI feeds | Implement `FeedsCmd` subcommand (add/list/remove/move/rename/info); `add` runs autodiscovery then `subscribe_feed`. | Feed CLI commands | Autodiscovery and add-time fetch error handling |
| `crates/metro-cli/src/refresh.rs` | Create | CLI refresh/view | Implement `RefreshCmd` and `ViewCmd` subcommands. | Refresh and view CLI commands | Progress output formatting |
| `Cargo.toml` (root) | Modify | Dependencies | Add `ureq`, `rustls`, `feed-rs`, `scraper` to `[workspace.dependencies]`. Add `jiff` if not present. | Required by metro-core | Transitive dependency conflicts |
| `crates/metro-core/Cargo.toml` | Modify | Dependencies | Add `ureq`, `rustls`, `feed-rs`, `scraper` via `workspace = true`. | Uses new deps | Feature flag alignment with workspace |
| `crates/metro-cli/Cargo.toml` | Modify | Dependencies | Add `jiff` (for date formatting in CLI output) if not inherited. | CLI date display | None |

## Risks / Trade-offs

| Risk | Impact | Mitigation |
|---|---|---|
| `feed-rs` transitive deps conflict | Build failure | Build immediately after adding deps; pin versions explicitly |
| Worker pool panic crashes process | Data loss during refresh | Use `std::panic::catch_unwind` in each worker; log panic, skip feed |
| Database write contention during refresh | Slow or deadlock | Single-writer pattern: workers send parsed entries to a channel, single writer thread does DB inserts |
| Autodiscovery HTML parsing fails on some sites | User must provide manual feed URL | Log warning in verbose mode; error message suggests manual URL |
| Pruning deletes unread entries unexpectedly | User data loss | Warning message before pruning (deferred to M4); document in help text |
| Backoff persists across restarts | Feeds stuck in backoff after long offline period | Backoff is relative to the failure time; stale backoff resolves naturally on next run |
| URL normalization misses edge cases | Duplicate subscriptions | Start with basic normalization; add more cases as reported by users |
| Adding a feed requires network | Offline `feeds add` fails | Clear network error (exit 4); the network-free core `add_feed` remains available for offline/import paths |
| 30-day hardcoded pruning window | Not configurable in M1 | Acceptable for M1; config wiring planned for M4 |

## Migration Plan

1. Build current M0 codebase (ensure it compiles and tests pass).
2. Add workspace dependencies (T-01).
3. Author data models and v001 schema (T-02, T-03).
4. Implement backend APIs sequentially following dependency chain: folders → feeds → fetch → refresh → backoff → pruning → autodiscovery.
5. Wire CLI commands (T-11, T-12, T-13).
6. Add integration tests (T-14).
7. Update OpenSpec specs (T-15).

No data migration is needed — v001 is the first real schema, applied to empty databases only.

## Open Questions

1. **Pruning date fallback**: When `published` is NULL, should we use `ROWID` as a proxy for first-seen order, or should we add a `first_seen INTEGER` column to the entries table? Adding the column is cleaner but requires a schema decision now. **Recommendation**: Add `first_seen INTEGER DEFAULT 0` to entries table in v001 (zero = unset), used as fallback. This avoids ROWID dependency and is cheap.

2. **Autodiscovery timeout**: What timeout should autodiscovery use? The main fetch uses 30s connect + 60s read. Autodiscovery fetches a single HTML page, so a shorter timeout (15s) seems appropriate. **Assumption**: 15-second timeout for autodiscovery.

3. **Feed kind for autodiscovered URLs**: When autodiscovery finds a feed URL, should we try to fetch it first to determine the kind, or classify based on URL heuristics (`.xml` → RSS/Atom, `/api/feed` → JSON)? **Resolved**: Neither heuristic is used. The kind always comes from parsing the fetched document (D5), and `feeds add` fetches at add time (D11), so subscriptions created from the CLI store the real kind immediately.
