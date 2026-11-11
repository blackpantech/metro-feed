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

## Planned usage

```sh
metro feeds add https://example.com/feed.xml         # follow a blog
metro feeds add @ThePrimeTimeagen                    # follow a YouTube channel
metro add https://example.com/article --folder tech  # add a link to the `tech` folder
metro list unread
metro view 42                                        # read in the terminal
metro refresh                                        # cron-friendly headless refresh
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

✅ **M0 complete.** Workspace foundation, database layer, configuration system, and CI infrastructure are implemented and tested. Ready for M1 development.
