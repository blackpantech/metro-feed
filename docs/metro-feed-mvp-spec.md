# metro-feed: Product Decisions (MVP spec v0.2)

Status: first draft, decisions agreed after the Q&A round.

## 1. Vision

`metro-feed` (binary: `metro`) is a single, local-first, terminal-native app that unifies:

- **Following**: YouTube subscriptions and followed blogs/podcasts via RSS, Atom and JSON Feed.
- **Saving**: a read-it-later queue for articles and videos ("Pocket in the terminal").

One local database, one config file, one snappy TUI. No server, no account, no mandatory cloud.

## 2. Scope

### In the MVP
- Two-stage delivery: a complete **CLI first**, then the **TUI** on the same core.
- Subscribe to RSS/Atom/JSON Feed URLs, blog homepages (autodiscovery), and YouTube **channels** (URL, `@handle`, or raw `UC…` id).
- Refresh on startup, manually, and optionally on an interval.
- Save any URL, triage with folders, read/unread and archive.
- One-key push of feed entries and videos to the saved queue (without changing the entry's read state).
- Duplicate-save detection with a clear message.
- Reader mode in the terminal, offline reading from cache.
- Full-text search across saved items and entries.
- Import/export: OPML, NewPipe, Takeout, Pocket, full JSON.
- Vim-style and arrow-key navigation.
- Feed window (30 days default) with pruning; content cache TTL (90 days default); archived items are permanent.

### Post-MVP (V1.x)
- Pluggable sync backend(s).
- Advanced triage: bulk actions, filters, smart lists.
- Optional clipboard watcher (rejected for MVP).
- **Per-feed `hide_shorts` option** for YouTube feeds.
- **Inline image rendering in the reader** via terminal graphics protocols (Kitty, Sixel, iTerm2) where supported.
- YouTube **playlist** feeds (`playlist_id`): not in the MVP, may be revisited.
- Exposing Pocket tags in the UI/CLI (the MVP only stores them).

### Out of scope
- In-app video playback (videos always open in the browser).
- Mobile clients.
- Cloud/server mode of the app itself.
- Anything requiring Google API keys (channel RSS feeds need none).

## 3. Decision log

| # | Topic | Decision |
|---|---|---|
| 1 | Storage / "fully Rust" | Interpreted as "no system libraries needed". **`rusqlite` with bundled SQLite + FTS5**; **`rustls`** for TLS. |
| 2 | Platforms | Linux, macOS, Windows. CI covers all three. |
| 3 | Folders | Feed folders and saved folders are **separate namespaces**; **flat** (no nesting). |
| 4 | Read state coupling | A saved item and its source entry have **independent** read state. |
| 5 | Content prefetch | Article content is cached **at save time**. For feed entries it is cached **only when opened**. |
| 6 | Default folders | `Inbox` (saved) and `Unsorted` (feeds): auto-created, renamable, not deletable. |
| 7 | Unread entries vs pruning | Unread entries older than the window are **pruned too**. `prune --dry-run` previews, and a hint warns when unread items are about to expire. |
| 8 | YouTube | **Channel feeds only.** No playlists in the MVP. `hide_shorts` deferred to post-MVP; Shorts are shown by default. |
| 9 | Pocket import | Archived status maps to archived. Tags are stored as metadata (not exposed). Items go to a `Pocket import` folder unless `--folder` is given. |
| 10 | Reader images | **No images** in the MVP (rendered as `[image: alt]`). Terminal image rendering is a post-MVP idea. |
| 11 | Workspace topology | **4 crates with dispatcher**: `metro` (bin), `metro-cli` (lib), `metro-tui` (lib), `metro-core` (lib). No `default-members`. |
| 12 | Schema freeze point | v001 schema is revisable in place until v0.5 release at M4. From M5, changes are real v002+ migrations. |
| 13 | Config directory | Uses `directories` crate, config dir name: `metro` (matches binary, not project name). |
| 14 | Global flags | `--log` flag accepted from M0, inert until M1. Semantics pinned before M4. |

## 4. Principles and non-functional targets

- **Local-first**: one SQLite file, one config file, no telemetry.
- **One core, two frontends**: `metro-core` (library) holds all logic; `metro-cli` and `metro-tui` are thin layers. Anything the TUI can do goes through a core API the CLI can also call.
- **Snappy** (budgets to validate in M7):
  - TUI first paint under 100 ms with 10k entries in the DB.
  - The UI never blocks on network or disk. Refresh runs on worker threads and reports progress through channels.
  - Refreshing 100 feeds in under 10 s on a decent connection.
  - Release binary of roughly 10 MB or less.
- **Safe by default**: all remote text is stripped of terminal control sequences before display. Fetches have timeouts, a response size cap and a redirect limit.
- **Polite networking**: conditional GET (ETag / Last-Modified), per-host concurrency limits, exponential backoff on failing feeds, configurable User-Agent.
- **Concurrent access**: SQLite in WAL mode with a busy timeout, so `metro refresh` from cron can run next to an open TUI.

## 5. Data model

| Object | Key fields | Notes |
|---|---|---|
| **Folder** | name, kind (`feeds` or `saved`) | Flat. Unique per (kind, name). |
| **Feed** | url, title, site URL, kind (`rss`/`atom`/`json`/`youtube`), folder, etag, last_modified, last_fetch, error state | YouTube feeds store the resolved `channel_id`. |
| **Entry** | feed, guid, url, title, author, published, summary/content, read flag | Pruned after the feed window. |
| **SavedItem** | normalized URL, original URL, title, kind (`article`/`video`), folder (exactly one), read flag, archived flag, saved_at, source entry (optional), tags (metadata only) | Independent snapshot: pruning an entry never touches saved items. |
| **ContentCache** | item or entry ref, extracted text, fetched_at | TTL 90 days. Archived items are exempt (permanent). |

**IDs** are short stable integers with a type prefix: `f12` (feed), `e4031` (entry), `s88` (saved item). The prefix removes ambiguity in `metro read <e-id|s-id>` and is easy to type and script. A bare number is rejected with a hint.

## 6. Functional behavior

### Subscriptions
- `feeds add` accepts a feed URL, a blog homepage URL (autodiscovery through `<link rel="alternate">`), or a YouTube channel URL / `@handle` / `UC…` id.
- YouTube resolution fetches the channel page, extracts the `channel_id` and builds `https://www.youtube.com/feeds/videos.xml?channel_id=…`. If scraping fails (consent wall, layout change), the user can pass the raw `UC…` id.
- Accepted YouTube RSS limits: about the latest 15 videos, no duration, Shorts mixed in.
- Duplicate subscriptions are detected by normalized feed URL or `channel_id`, and the existing feed is reported.

### Refresh
- Triggers: TUI startup (configurable), manual (`R` / `metro refresh`), optional interval while the TUI is open (`refresh.interval = "30m"`).
- Worker pool (default 8, configurable). Entry dedup order: `guid`, then `url`, then content hash.
- A 301 updates the stored feed URL. Dead feeds get an error badge and are retried with backoff. They are never deleted automatically.
- Pruning runs after each refresh: entries older than `feeds.window_days` (30 default) are dropped, based on published date, or first-seen date if missing. Unread entries are not exempt; the user is warned beforehand.

### Saving
- `save <url>` fetches the title and caches content (unless `--no-fetch`), then stores the item in the given or default folder.
- URL normalization before duplicate detection: lowercase host, drop fragments and tracking params (`utm_*`, `fbclid`, …), strip trailing slashes, canonicalize YouTube (`youtu.be/ID`, `watch?v=ID`, `shorts/ID` all become `watch?v=ID`).
- **Duplicate**: no new item is created. The existing item is kept (or moved if `--folder` was given) and the user is told where it lives and its state, in both CLI and TUI.
- Saving a feed entry (`s` in the TUI, `metro save e123`) creates a SavedItem linked to the entry and **does not change the entry's read state**.
- An item belongs to exactly one folder and can be moved at any time.
- Archive is a flag: archived items leave the default saved list and their cached content becomes permanent.

### Reading
- Reader mode: fetch, extract main content (readability), convert to wrapped plain text with a numbered link list, cache it. Code blocks and simple tables are rendered, images become `[image: alt]`.
- Offline: serve from cache; with no cache and no network, show an explicit message.
- Videos are never rendered: `read` or `Enter` opens the default browser.
- Opening an unread entry marks it read (`--no-mark` or a config option disables this).

### Search
Full-text (FTS5) over saved items (title, URL, cached content) and entries (title, summary, cached content), scoped with `--in saved|entries|all` and `--folder`.

### Import / export

| Format | Import | Export |
|---|---|---|
| OPML (feeds and folders) | Yes | Yes |
| NewPipe subscriptions JSON | Yes | No |
| Google Takeout `subscriptions.csv` | Yes | No |
| Pocket export (CSV, legacy HTML) | Yes (migration tool) | No |
| Full JSON (feeds, folders, saved items, state) | Yes (restore) | Yes |

Imports are idempotent (duplicates skipped and reported), support `--dry-run`, and print a summary (added / skipped / failed).

### Folder name resolution
Because feed and saved folders are separate namespaces, `--folder <name>` is resolved by command context: feed commands (`feeds`, `view`, `refresh`, `list feeds`) use feed folders; save commands (`save`, `move`, `list saved|archived`) use saved folders. For commands that can span both (`mark`, `search`, `folders`), the name is looked up in both kinds, and `--kind feeds|saved` disambiguates when it matches both.

## 7. CLI reference

**Global flags**: `--db <path>`, `--config <path>`, `--json` (machine output), `--no-color`, `-q/--quiet`, `-v/--verbose`, `-V/--version`, `-h/--help`.

**Exit codes**: `0` ok, `1` generic error, `2` usage error, `3` not found, `4` network error, `5` partial success (some feeds failed).

**Output**: plain text, one record per line, tab-separated when piped. `--json` gives structured output (NDJSON for lists).

```
metro                                     TUI (prints help until the TUI milestone)

# Feeds
metro feeds add <url|@handle|UC-id> [--folder <n>] [--title <t>] [--dry-run]
metro feeds list [--folder <n>]           tree without the flag, flat list with it
metro feeds remove <f-id>... [--keep-entries]
metro feeds move <f-id>... --folder <n>
metro feeds rename <f-id> <title>
metro feeds info <f-id>                   URL, last fetch, errors, entry counts
metro refresh [<f-id>...] [--folder <n>] [--jobs <n>]

# Folders
metro folders list [--kind feeds|saved]
metro folders add|rename|remove <name> [--kind feeds|saved]
                                          remove refuses non-empty folders unless --move-to <n>

# Reading
metro view <f-id> [--all|--unread] [--limit <n>]
metro read <e-id|s-id> [--refresh] [--raw] [--width <n>] [--no-mark]
metro open <id>                           open in the default browser
metro mark <read|unread> <id>... | --feed <f-id> | --folder <n> | --all

# Saving
metro save <url|e-id> [--folder <n>] [--title <t>] [--no-fetch]
metro list [saved|unread|archived|feeds] [--folder <n>] [--limit <n>] [--sort date|title]
metro move <s-id>... --folder <n>
metro archive <s-id>...
metro unarchive <s-id>...
metro rm <s-id>...

# Search
metro search <query> [--in saved|entries|all] [--folder <n>] [--limit <n>]

# Data
metro import <file> [--format auto|opml|newpipe|takeout|pocket|json] [--folder <n>] [--dry-run]
metro export [feeds|saved|all] [--format opml|json] [-o <file>]
                                          feeds → OPML by default; saved/all → JSON

# Maintenance
metro prune [--dry-run]                   apply window and cache TTL now
metro config path|get <key>|set <key> <val>|edit
metro completions <bash|zsh|fish|powershell>
metro doctor                              DB integrity, config check, network sanity
```

Notes:
- `metro list feeds` is an alias of `metro feeds list`.
- `metro list unread` lists unread feed entries.
- Default folders: `metro save` without `--folder` uses `Inbox`; `metro feeds add` without `--folder` uses `Unsorted`.
- `metro import` of Pocket data defaults to the `Pocket import` saved folder.

## 8. TUI sketch (stage 2)

- **Layout**: sidebar (Unread / Saved / Archive / feed folders / saved folders), item list, preview or reader pane. The reader can go full-screen.
- **Keys** (vim + arrows): `j/k` or ↑/↓ move, `h/l` or ←/→ change pane, `g/G` top/bottom, `Space/b` page, `Enter` open (reader or browser), `o` browser, `s` save, `m` move to folder, `a` archive, `r` toggle read, `R` refresh, `/` search, `n/N` next/prev unread, `?` help, `q` quit.
- **Concurrency**: the UI thread only renders and handles input; workers report through channels.
- Duplicate saves, expiring unread items and offline state are surfaced as non-blocking notifications.

## 9. Proposed crates

| Purpose | Crate |
|---|---|
| CLI | `clap` (+ `clap_complete`) |
| TUI | `ratatui` + `crossterm` |
| HTTP | `ureq` + `rustls`, with a worker pool (no async runtime) |
| Storage, search | `rusqlite` (bundled SQLite, FTS5) |
| Feed parsing | `feed-rs` (RSS, Atom, JSON Feed) |
| HTML / extraction | `scraper`, `dom_smoothie` (readability), `html2text` |
| OPML / XML / CSV | `quick-xml`, `csv` |
| Serialization, config | `serde`, `serde_json`, `toml` |
| Time, paths, browser | `jiff`, `directories` (or `etcetera`), `open` |

## 10. Milestones

**M0, Foundations.** Cargo workspace (`metro`, `metro-cli`, `metro-tui`, `metro-core`), migration runner (hand-rolled, no external crate), config loading with `directories` crate, error types with exit codes, test utilities (`test-utils` feature), CI (fmt, clippy -D warnings, tests on Linux/macOS/Windows). Global flags `-v/--verbose` and `--log` accepted but inert. `doctor` command deferred to M1.
*Exit: `metro --version` and `metro --help` work; `metro` (no args) prints TUI stub; workspace builds and all tests pass on all three OSes; migration runner proven by tests against synthetic migrations; config loading tested (defaults < TOML < CLI precedence); database `open()` applies WAL, busy_timeout, foreign_keys, all tested.*

**M1, Feeds core.** Folders, `feeds add/list/remove/move/rename/info`, fetching and parsing, autodiscovery, threaded `refresh` with conditional GET and backoff, 30-day pruning, `view`.
*Exit: 50 feeds subscribed and refreshed in parallel; a second refresh is nearly free (304s).*

**M2, YouTube and subscription import/export.** `@handle` and channel resolution, OPML import/export, NewPipe and Takeout import, `--dry-run` and summaries.
*Exit: a real NewPipe or Takeout subscription list migrates in one command.*

**M3, Saving and reading.** `save` with title fetch, URL normalization and duplicate handling, saved folders, archive, move, rm, `list`, `mark`, content extraction and cache (TTL, archive exemption), `read` and `open`, offline behavior.
*Exit: save, read offline, archive, and survive a prune.*

**M4, Search and CLI 1.0.** Full-text search, Pocket import, full JSON export/import, `--json` everywhere, exit codes, completions, `prune`, `config`, man page and docs.
*Exit: CLI-only release (v0.5), usable daily and scriptable.*

**M5, TUI shell.** Layout, navigation, lists, background refresh with progress, mark read, save, move, archive, folder management, help overlay.
*Exit: a full triage session without leaving the TUI.*

**M6, TUI reader and search.** Reader pane (scroll, link list, open link), search UI, offline indicators, duplicate-save notifications, optional refresh interval, theming basics.
*Exit: feature parity with the CLI.*

**M7, Hardening and release (v1.0).** Performance pass against the budgets, large-DB tests (50k entries), parser fuzzing, packaging (cargo, binaries, Homebrew/AUR), README and demo.

## 11. Post-MVP backlog

1. Pluggable sync backend(s).
2. Bulk actions, filters, smart lists.
3. Optional clipboard watcher.
4. YouTube `hide_shorts` per-feed option.
5. Inline image rendering in the reader (Kitty / Sixel / iTerm2 protocols, with graceful fallback to `[image: alt]`).
6. Pocket tags exposed and searchable; tag-based filtering.
7. YouTube playlist feeds (revisit).

## 12. Open items to revisit

- Validate the performance budgets in M0/M1 with a synthetic 10k-entry DB.
- Confirm `ureq` + `rustls` handles all target feeds and sites. Fall back to another blocking client if needed.
- Check `dom_smoothie` extraction quality on a sample of real blogs early in M3.
- Decide the exact TUI theming and config keys during M5.
