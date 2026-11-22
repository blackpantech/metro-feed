# metro-feed

🚇 A local-first RSS aggregator &amp; read-it-later queue for YouTube and blogs, straight from your terminal.

`metro-feed` (command: `metro`) is a terminal-native app that unifies the two things you do with online content: **following** what publishes regularly — YouTube channels, blogs, podcasts — and **saving** articles and videos you want to read or watch later. One place, one local database, no cloud required.

## Why

Your subscriptions live in YouTube's addictive algorithm feed, your saved articles live in a cloud bookmark service, and the blogs you follow live in an RSS reader you never open. `metro-feed` puts them back together where you actually work: your terminal.

## Design principles

| Principle        | Meaning                                                                                  |
| ---------------- | ---------------------------------------------------------------------------------------- |
| **Light**        | Small binary, tiny dependency tree, minimal resource usage                               |
| **Blazing fast** | Instant startup, non-blocking refreshes, snappy scrolling                                |
| **Compatible**   | No system libraries (bundled SQLite), single binary, Linux/macOS/Windows                 |
| **Local-first**  | Your data stays on your disk, exportable at any time — sync stays optional and pluggable |

## What it will do

- **Follow** — subscribe to RSS/Atom/JSON feeds; paste a YouTube channel URL or `@handle` and it is resolved to the official channel feed automatically
- **Import** — OPML, NewPipe subscriptions, YouTube Takeout, Pocket exports
- **Save** — push any link (or any feed entry) to a read-it-later queue, from the TUI or from the shell with `metro save <url>`
- **Organize** — folders/collections, unread/read states, archive, full-text search with filters
- **Read** — articles render in an in-app reader mode (plain text, images stripped); videos open in your default browser
- **Own your data** — everything exportable (OPML/JSON), retention and feed time window configurable

## Two frontends, one core

`metro-feed` ships in two stages over the same core library:

1. **A fully functional CLI** — every feature (feeds, saving, triage, reading) usable from the shell, scriptable, cron-friendly, great over SSH.
2. **A snappy TUI** — vim-style and arrow-key navigation, panes with folders/feeds/items, in-app reader — built on the proven core, with zero duplicated logic.

## Usage

### Available now (M1)

```sh
# Feeds
metro feeds add https://example.com/feed.xml              # follow a feed (RSS, Atom or JSON Feed)
metro feeds add https://example.com --folder tech         # blog homepage: feed found by autodiscovery
metro feeds add https://example.com/feed.xml --dry-run    # fetch and preview without saving
metro feeds list                                          # grouped by folder (flat with --folder)
metro feeds info f12                                      # URL, last fetch, error state, entry count
metro feeds rename f12 "My favourite blog"
metro feeds move f12 f13 --folder reading
metro feeds remove f12                                    # unsubscribes and deletes its entries

# Folders (feed folders by default, --kind saved for saved-item folders)
metro folders list
metro folders add tech
metro folders rename tech dev
metro folders remove dev                                  # refuses non-empty folders and Unsorted

# Refresh and read
metro refresh                                             # every feed, in parallel (cron-friendly)
metro refresh f12 f13 --jobs 4                            # only some feeds, 4 workers
metro refresh --folder tech                               # only one folder
metro view f12                                            # 20 most recent entries
metro view f12 --unread --limit 50                        # or --all for every entry

# Scripting
metro --json feeds list | jq -r .id                       # NDJSON output; IDs are reusable as arguments
metro --db /tmp/test.db feeds list                        # use another database file
```

IDs carry a type prefix (`fo3` folder, `f12` feed, `e4031` entry), so a folder ID is never mistaken for a feed ID. Exit codes are stable for scripts: `0` ok, `1` error, `2` usage, `3` not found, `4` network, `5` partial success (e.g. some feeds failed to refresh).

### Coming next

```sh
metro feeds add @ThePrimeTimeagen                    # follow a YouTube channel
metro import subscriptions.opml                      # OPML, NewPipe, Takeout, Pocket
metro save https://example.com/article --folder tech # save a link to read later
metro list unread
metro read e4031                                     # read an article in the terminal
metro search "rust async"
metro                                                # the TUI (later milestone)
```

## Roadmap

| Milestone               | Content                                                                   |
| ----------------------- | ------------------------------------------------------------------------- |
| M0 — Foundations        | Workspace structure, DB layer, config, CLI framework, CI                  |
| M1 — CLI: feeds         | Feed management, YouTube resolution, refresh, pruning, OPML import/export |
| M2 — CLI: read-it-later | Saving, folders, triage, in-terminal reading, Pocket/Takeout import       |
| M3 — Content cache      | Article caching, offline reading, image handling                          |
| M4 — Polish             | Search & filters, auto-refresh, performance passes                        |
| M5 — TUI                | Full terminal UI over the same core                                       |
| M6 — Sync               | Pluggable sync layer (Linkding, Linkwarden, Inoreader...)                 |
| M7 — Release            | Binary distribution, documentation, final polish                          |

## Status

✅ **M0 complete.** Workspace foundation, database layer, configuration system, and CI infrastructure.

✅ **M1 complete.** Feed subscriptions (RSS, Atom, JSON Feed, blog homepage autodiscovery), feed folders, parallel `refresh` with conditional GET and exponential backoff for failing feeds, 30-day entry pruning, and `view` — all usable from the CLI with `--json` output. Refreshing 50 feeds stays well under the 10 s budget, and a second refresh is nearly free thanks to `304 Not Modified`.

🚧 **Next:** YouTube channel resolution and subscription import/export (OPML, NewPipe, Takeout).
