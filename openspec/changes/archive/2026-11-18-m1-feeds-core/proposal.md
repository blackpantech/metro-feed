# Proposal: M1 — Feeds Core

## Why

M0 established the workspace, database connection, migration runner, config loading, and CLI scaffolding. M1 delivers the core feeds capability: subscribing to RSS/Atom/JSON Feed and blog homepage sources, fetching entries in parallel, managing folders, pruning stale entries, and exposing everything through a usable CLI. This is the first milestone that delivers observable user value — subscribers can add feeds, refresh them, and view entries from the terminal.

## What Changes

- **Data models**: Define `Folder`, `Feed`, and `Entry` structs in `metro-core` with typed ID wrappers (`FolderId`, `FeedId`, `EntryId`) supporting `f<N>`, `e<N>` display format.
- **Database schema**: Author `v001__init.sql` with `folders`, `feeds`, `entries` tables, foreign keys, and performance indexes.
- **Folder management API**: CRUD operations for feed folders in `metro-core`, including idempotent creation of the `Unsorted` default folder.
- **Feed management API**: CRUD operations for subscriptions (`add_feed`, `list_feeds`, `remove_feed`, `move_feed`, `rename_feed`, `get_feed_info`) with duplicate URL detection.
- **URL autodiscovery**: Fetch a blog homepage and extract `<link rel="alternate">` to discover feed URLs (RSS, Atom, JSON Feed).
- **Feed fetching**: HTTP fetcher using `ureq` with conditional GET (ETag/Last-Modified), 30s connect / 60s read timeouts, size cap, redirect limit, polite User-Agent.
- **Threaded refresh**: Worker pool (default 8) to fetch feeds in parallel, insert new entries with dedup (guid → url → content hash), report progress via channels.
- **Exponential backoff**: Track `error_state` and `next_retry` on feeds; skip feeds in backoff during refresh; exponential retry schedule (1m, 5m, 30m, 2h, 12h, 24h max).
- **Entry pruning**: Delete entries older than 30 days (configurable, hardcoded in M1) based on published date (or first-seen fallback); runs after each refresh.
- **CLI commands**: `folders` (list/add/rename/remove), `feeds` (add/list/remove/move/rename/info), `refresh`, `view` — all wired through `metro-cli`.
- **Subscribe-time fetch**: `feeds add` fetches the feed once at subscribe time to store its real kind, title, site URL, and initial entries; unreachable or unparsable feeds are rejected. Later refreshes keep kind and site URL current but never overwrite the title after the first successful fetch.
- **Integration tests**: End-to-end test of 50-feed refresh workflow verifying parallelism and 304 performance.

### New Capabilities

- `data-models`: Type-safe model definitions for Folder, Feed, Entry with ID wrappers
- `folder-management`: CRUD operations for feed folders (flat, separate namespace from saved folders)
- `feed-management`: CRUD operations for feed subscriptions with duplicate URL detection
- `feed-fetching`: HTTP-based feed fetching with conditional GET, parsing, and response validation
- `refresh`: Threaded worker pool for parallel feed fetching with progress reporting and entry dedup
- `backoff`: Exponential backoff for failing feeds with persisted retry state
- `pruning`: Age-based entry pruning with configurable window
- `autodiscovery`: Blog homepage to feed URL discovery via HTML `<link rel="alternate">` parsing
- `feeds-cli`: CLI command interface for folders, feeds, refresh, and view

### Modified Capabilities

- `database-migrations`: v001 migration replaces the M0 placeholder with real schema for folders, feeds, entries tables

## Ticket Analysis

**Business objective**: Enable terminal-based RSS/Atom/JSON Feed management — subscribe, refresh, view, and prune feed entries without leaving the terminal.

**Scope**:
- **In scope**: All 15 tickets (T-01 through T-15), covering data models, schema, CRUD APIs, fetching, threading, backoff, pruning, autodiscovery, CLI commands, integration tests, and spec updates.
- **Out of scope**: YouTube channel resolution (M2), saved items and reading (M3), search (M4), TUI (M5), `--dry-run` for pruning (deferred), `metro doctor` (deferred to M1 from M0), performance benchmarking (M7).

**Constraints**:
- No async runtime — blocking `ureq` with `std::thread` worker pool.
- SQLite only, WAL mode, concurrent-access safe.
- No external services or cloud.
- Must work on Linux, macOS, Windows (CI on all three).
- Schema editable in place until M4.

**Acceptance criteria summary**:
- 50 feeds subscribed and refreshed in parallel.
- Second refresh nearly free (304 Not Modified).
- `metro --help` / `metro --version` work; all M1 CLI commands operational.
- All tests pass on all three OSes in CI.

## Capabilities

### New Capabilities

- `data-models`: Type-safe structs for Folder, Feed, Entry with typed ID wrappers supporting `f<N>`, `e<N>` display format and serde serialization
- `folder-management`: Feed folder CRUD (create, list, rename, delete), idempotent `Unsorted` default, flat namespace, case-insensitive names
- `feed-management`: Feed subscription CRUD (add, list, remove, move, rename, info), duplicate URL detection by normalized URL
- `feed-fetching`: HTTP fetch with conditional GET (ETag/Last-Modified), `feed-rs` parsing, response size cap, timeout, redirect limit, polite User-Agent
- `refresh`: Threaded worker pool (default 8, configurable), parallel feed fetching, entry dedup (guid → url → content hash), progress reporting via channels
- `backoff`: Exponential backoff for failing feeds (1m, 5m, 30m, 2h, 12h, 24h max), persisted `consecutive_failures` and `next_retry`, feeds in backoff are skipped
- `pruning`: Delete entries older than `feeds.window_days` (30 default) based on published date or first-seen fallback, runs after each refresh, unread entries are not exempt
- `autodiscovery`: Fetch blog homepage HTML, parse `<link rel="alternate">` for RSS/Atom/JSON Feed links, return first valid feed URL
- `feeds-cli`: CLI commands: `folders list/add/rename/remove`, `feeds add/list/remove/move/rename/info`, `refresh [<f-id>...]`, `view <f-id>` — with `--folder`, `--kind`, `--jobs`, `--limit`, `--unread`, `--all` flags; `feeds add` fetches the feed at subscribe time and rejects unreachable or unparsable feeds

### Modified Capabilities

- `database-migrations`: v001__init.sql replaces M0 placeholder with real schema — `folders`, `feeds`, `entries` tables with FK constraints and performance indexes

## Impact

**Affected crates and files**:
- `crates/metro-core/src/lib.rs` — add `models`, `folders`, `feeds`, `fetch`, `refresh`, `prune`, `discovery` modules
- `crates/metro-core/src/models/` — data model files: `id.rs` (ID wrappers), `enums.rs` (FolderKind/FeedKind), `folder.rs`, `feed.rs`, `entry.rs`, `mod.rs` (re-exports)
- `crates/metro-core/src/db/migrations/v001__init.sql` — author real schema (replaces M0 placeholder)
- `crates/metro-cli/src/lib.rs` — expand `Commands` enum with `Folders`, `Feeds`, `Refresh`, `View` subcommands
- `crates/metro-cli/src/` — new files: `folders.rs`, `feeds.rs`, `refresh.rs`
- `Cargo.toml` (root + metro-core) — add workspace dependencies: `ureq`, `rustls`, `feed-rs`, `scraper`

**Dependencies added**:
- `ureq` + `rustls` for HTTP (blocking client)
- `feed-rs` for RSS/Atom/JSON Feed parsing
- `scraper` for HTML autodiscovery parsing

**Breaking changes**: None. M0 APIs are unchanged; this adds new modules and CLI commands.

**Regression risks**:
- New dependencies may introduce transitive conflicts — build immediately after adding.
- Schema change in v001 is deferred to M1; M0 database remains empty and compatible.
- CLI command additions are additive; no existing commands affected.
