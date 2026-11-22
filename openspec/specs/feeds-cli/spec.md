# Feeds CLI Specification

## Purpose

Exposes folder and feed management operations through CLI commands with structured output, folder filtering, and proper exit codes.

## Requirements

### Requirement: Global data and output flags
The system SHALL accept the global flags `--json` (machine-readable output) and `--db <path>` (database file to use instead of the default `metro.db` in the platform data directory) on every command. The database SHALL be opened (and migrated) only by commands that need it, so `--help`, `--version`, and usage errors never create files.

#### Scenario: Database override
- **WHEN** `metro --db /tmp/test.db folders list` is run
- **THEN** the command reads `/tmp/test.db` instead of the default database

#### Scenario: Help creates no database
- **WHEN** `metro feeds --help` is run
- **THEN** no database file is created

### Requirement: Errors and exit codes
Command failures SHALL be printed to stderr as `metro: <error>` and the process SHALL exit with the code of the error kind: 1 generic, 2 usage, 3 not found, 4 network, 5 partial success. Argument parsing errors (unknown flags, malformed IDs such as a bare `12` instead of `f12` or a folder ID `fo3` where a feed ID is expected, conflicting flags) SHALL exit with code 2.

#### Scenario: Malformed feed ID is a usage error
- **WHEN** `metro feeds info 12` is run
- **THEN** the command exits with code 2 and hints at the `f<number>` form

#### Scenario: Folder ID is not a feed ID
- **WHEN** `metro feeds info fo3` is run
- **THEN** the command exits with code 2 and hints at the `f<number>` form

### Requirement: `folders list` command
The system SHALL expose `metro folders list [--kind feeds|saved]` to list folders of one namespace sorted by name (case-insensitive). The `--kind` flag defaults to `feeds`. Text output SHALL show one folder name per line.

#### Scenario: List feed folders
- **WHEN** `metro folders list` is run
- **THEN** feed folders are listed

#### Scenario: List saved folders
- **WHEN** `metro folders list --kind saved` is run
- **THEN** saved folders are listed

### Requirement: `folders add` command
The system SHALL expose `metro folders add <name> [--kind feeds|saved]` to create a new folder. Unlike the idempotent core creation used for the default folder, this command SHALL fail when a folder of the same kind already uses the name (case-insensitive).

#### Scenario: Create a folder
- **WHEN** `metro folders add Tech --kind feeds` is run
- **THEN** a new feed folder named "Tech" is created

#### Scenario: Duplicate folder name fails
- **WHEN** `metro folders add tech --kind feeds` is run where "Tech" already exists
- **THEN** the command fails with exit code 1 and nothing is created

#### Scenario: Empty folder name is a usage error
- **WHEN** `metro folders add "  "` is run
- **THEN** the command fails with exit code 2

### Requirement: `folders rename` command
The system SHALL expose `metro folders rename <old-name> <new-name> [--kind feeds|saved]` to rename a folder, resolving `<old-name>` case-insensitively. The default `Unsorted` feed folder SHALL NOT be renamed.

#### Scenario: Rename a folder
- **WHEN** `metro folders rename Tech Technology --kind feeds` is run
- **THEN** the folder is renamed

#### Scenario: Rename unknown folder fails
- **WHEN** `metro folders rename Missing Other` is run
- **THEN** the command fails with exit code 3

#### Scenario: Rename to an existing name fails
- **WHEN** `metro folders rename Tech News` is run where "News" already exists
- **THEN** the command fails with exit code 1

#### Scenario: Renaming the default folder fails
- **WHEN** `metro folders rename Unsorted Other` is run
- **THEN** the command fails with exit code 1, an error naming `Unsorted` as the default folder, and the folder keeps its name

### Requirement: `folders remove` command
The system SHALL expose `metro folders remove <name> [--kind feeds|saved]` to delete an empty folder. Non-empty folders and the default `Unsorted` feed folder SHALL be rejected.

#### Scenario: Remove empty folder succeeds
- **WHEN** `metro folders remove EmptyFolder --kind feeds` is run on an empty folder
- **THEN** the folder is deleted

#### Scenario: Remove non-empty folder fails
- **WHEN** `metro folders remove Tech --kind feeds` is run on a folder with feeds
- **THEN** the command fails with exit code 1 and an error indicating the folder is not empty

#### Scenario: Remove unknown folder fails
- **WHEN** `metro folders remove Missing` is run
- **THEN** the command fails with exit code 3

#### Scenario: Removing the default folder fails
- **WHEN** `metro folders remove Unsorted` is run, even when the folder is empty
- **THEN** the command fails with exit code 1, an error naming `Unsorted` as the default folder, and the folder still exists

### Requirement: `feeds add` command
The system SHALL expose `metro feeds add <url> [--folder <name>] [--title <t>] [--dry-run]` to subscribe to a feed through `subscribe_url`: autodiscovery is used when the URL's body is not a feed, and the resolved feed SHALL be fetched at add time so its real kind, title, site URL, and initial entries are stored immediately. A feed that cannot be fetched or parsed SHALL be rejected and nothing written. `--folder` SHALL be resolved in the feed folder namespace before any network access.

#### Scenario: Add a feed by URL
- **WHEN** `metro feeds add https://example.com/feed.xml` is run
- **THEN** the feed is added to the `Unsorted` folder and a status line reports its ID, title, kind, new entry count, and folder

#### Scenario: Add with folder specification
- **WHEN** `metro feeds add https://example.com/feed.xml --folder Tech` is run
- **THEN** the feed is added to the "Tech" folder

#### Scenario: Add with unknown folder fails before fetching
- **WHEN** `metro feeds add https://example.com/feed.xml --folder Missing` is run
- **THEN** the command exits with code 3 and no request is made

#### Scenario: Add with autodiscovery
- **WHEN** `metro feeds add https://blog.example.com` is run
- **THEN** autodiscovery finds the feed URL and adds it

#### Scenario: Homepage without feed is not found
- **WHEN** `metro feeds add https://example.com` is run on a page that advertises no feed
- **THEN** nothing is saved and the command exits with code 3

#### Scenario: Real metadata stored at add time
- **WHEN** `metro feeds add https://example.com/atom.xml` is run for a reachable Atom feed
- **THEN** the feed is stored with kind `atom` and the feed's own title, and its entries are available without a refresh

#### Scenario: Unreachable or invalid feed is rejected
- **WHEN** `metro feeds add <url>` is run and the feed fetch fails
- **THEN** no feed is saved and the command exits with code 4 for network errors or 1 for parse errors

#### Scenario: Invalid URL or blank title is a usage error
- **WHEN** `metro feeds add ftp://example.com` or `metro feeds add <url> --title " "` is run
- **THEN** the command exits with code 2 and no request is made

#### Scenario: Add duplicate feed reports existing
- **WHEN** `metro feeds add https://example.com/feed.xml` is run for an already-subscribed feed
- **THEN** the existing feed and its folder are reported, nothing changes, and the command exits with code 0

#### Scenario: Dry-run shows without saving
- **WHEN** `metro feeds add https://example.com/feed.xml --dry-run` is run
- **THEN** the feed is fetched and the detected metadata and target folder are shown, but no changes are made to the database

#### Scenario: Add result as JSON
- **WHEN** `metro feeds add <url> --json` is run
- **THEN** one JSON object is printed whose `status` is `subscribed` (with the stored feed and `new_entries`), `duplicate` (with the existing feed), or `dry_run` (with the detected metadata and `folder_name`)

### Requirement: `feeds add` detects feeds by content
`metro feeds add <url>` SHALL request the URL once and parse its body as a feed whatever its `Content-Type`; for a direct feed that first response SHALL be the add-time fetch, so no second request is made.

#### Scenario: Feed served with a non-feed Content-Type is added
- **WHEN** `metro feeds add <url>` is run for a URL serving a valid RSS document as `text/html` or `text/plain`
- **THEN** the feed is subscribed with exactly one request and the command exits with code 0

### Requirement: `feeds list` command
The system SHALL expose `metro feeds list [--folder <name>]`. Without `--folder` it SHALL list every feed, one per line, ordered by folder then title with the folder name as the first column (`folder  id  title  url`). With `--folder` it SHALL list only that folder's feeds flat (`id  title  url`).

#### Scenario: List all feeds grouped
- **WHEN** `metro feeds list` is run
- **THEN** feeds are listed ordered by folder, each line starting with its folder name

#### Scenario: List feeds in a folder flat
- **WHEN** `metro feeds list --folder Tech` is run
- **THEN** feeds in the "Tech" folder are listed without a folder column

#### Scenario: List unknown folder fails
- **WHEN** `metro feeds list --folder Missing` is run
- **THEN** the command exits with code 3

### Requirement: `feeds remove` command
The system SHALL expose `metro feeds remove <f-id>...` to delete one or more feeds and their entries, all-or-nothing, reporting each removed feed with its deleted entry count.

#### Scenario: Remove a feed
- **WHEN** `metro feeds remove f12` is run
- **THEN** feed f12 and its entries are deleted

#### Scenario: Remove with an unknown ID changes nothing
- **WHEN** `metro feeds remove f12 f99` is run and f99 does not exist
- **THEN** the command exits with code 3 and f12 is not removed

### Requirement: `feeds move` command
The system SHALL expose `metro feeds move <f-id>... --folder <name>` to reassign one or more feeds to a feed folder, all-or-nothing.

#### Scenario: Move feed to folder
- **WHEN** `metro feeds move f12 --folder Tech` is run
- **THEN** feed f12 is moved to the "Tech" folder

#### Scenario: Move with an unknown ID or folder changes nothing
- **WHEN** `metro feeds move f12 f99 --folder Tech` is run and f99 does not exist, or "Tech" does not exist
- **THEN** the command exits with code 3 and no feed is moved

### Requirement: `feeds rename` command
The system SHALL expose `metro feeds rename <f-id> <title>` to update a feed's title.

#### Scenario: Rename a feed
- **WHEN** `metro feeds rename f12 "New Feed Title"` is run
- **THEN** the feed's title is updated

#### Scenario: Blank title is a usage error
- **WHEN** `metro feeds rename f12 " "` is run
- **THEN** the command exits with code 2

### Requirement: `feeds info` command
The system SHALL expose `metro feeds info <f-id>` to display feed metadata as labelled fields: ID, title, URL, site, kind, folder, last fetch (RFC 3339 UTC, or `never`), error (or `none`), last error time (RFC 3339 UTC, or `never`), and entry count. With `--json`, timestamps SHALL be Unix seconds or `null`.

#### Scenario: Show feed info
- **WHEN** `metro feeds info f12` is run
- **THEN** the feed's URL, last fetch time, error state, last error time, and entry count are displayed

#### Scenario: Last error time of a failed feed
- **WHEN** `metro feeds info f12 --json` is run for a feed whose last fetch failed
- **THEN** the object's `error_state` holds the error message and `last_error_at` the failure time in Unix seconds

#### Scenario: Info of unknown feed fails
- **WHEN** `metro feeds info f99` is run and f99 does not exist
- **THEN** the command exits with code 3

### Requirement: `refresh` command
The system SHALL expose `metro refresh [<f-id>...] [--folder <name>] [--jobs|-j <n>]` to fetch all feeds, the listed feeds, or one feed folder's feeds. Feed IDs and `--folder` are mutually exclusive. After printing its report, the command SHALL exit with 0 when no fetched feed failed (including when nothing was due), 5 when some fetched feeds failed and at least one succeeded, and 1 when every fetched feed failed.

#### Scenario: Refresh all feeds
- **WHEN** `metro refresh` is run
- **THEN** all subscribed feeds that are not in backoff are fetched in parallel

#### Scenario: Refresh specific feeds
- **WHEN** `metro refresh f12 f13` is run
- **THEN** only feeds f12 and f13 are refreshed

#### Scenario: Refresh a folder
- **WHEN** `metro refresh --folder Tech` is run
- **THEN** only feeds in the "Tech" folder are refreshed

#### Scenario: Refresh with job limit
- **WHEN** `metro refresh --jobs 4` is run
- **THEN** at most 4 concurrent fetch workers are used

#### Scenario: Invalid refresh arguments are usage errors
- **WHEN** `metro refresh --jobs 0` or `metro refresh f12 --folder Tech` is run
- **THEN** the command exits with code 2 and nothing is fetched

#### Scenario: Unknown target fails before fetching
- **WHEN** `metro refresh f99` or `metro refresh --folder Missing` is run for a target that does not exist
- **THEN** the command exits with code 3 and nothing is fetched

#### Scenario: Exit code on partial success
- **WHEN** `metro refresh` is run and some feeds fail while others succeed
- **THEN** the report is printed and the process exits with code 5

#### Scenario: Exit code when every feed fails
- **WHEN** `metro refresh` is run and every fetched feed fails
- **THEN** the report is printed and the process exits with code 1

### Requirement: Refresh report
The `refresh` command SHALL print its report on stdout: one row per failed feed (`id  failed  title  error`) and per feed skipped for backoff (`id  skipped  title  retry time`), followed by a summary line with the fetched, ok, failed, and skipped feed counts and the new and pruned entry counts. Successful feeds are only counted.

#### Scenario: Refresh report lists problems and totals
- **WHEN** `metro refresh` is run and one of three feeds fails
- **THEN** one `failed` row is printed for that feed, followed by a summary line reporting 3 feeds, 2 ok, 1 failed

#### Scenario: Refresh report as JSON
- **WHEN** `metro refresh --json` is run
- **THEN** one JSON object is printed with `fetched`, `succeeded`, `failed`, `skipped`, `new_entries`, `pruned`, `workers`, and a `feeds` array holding every fetched or skipped feed with its `id`, `title`, and `status` (`updated` with `new_entries`, `not_modified`, `failed` with `error` and `exit_code`, `skipped` with `next_retry`)

### Requirement: Refresh progress on stderr
The `refresh` command SHALL draw a single progress line (`Refreshing feeds: <done>/<total>`, rewritten in place) on stderr only when stderr is a terminal and `--json` is not set. stdout SHALL carry only the report. A failure to draw progress SHALL NOT fail the refresh.

#### Scenario: No progress when piped or in JSON mode
- **WHEN** `metro refresh 2>log` or `metro refresh --json` is run
- **THEN** no progress line is written

### Requirement: `view` command
The system SHALL expose `metro view <f-id> [--all|--unread] [--limit <n>]` to list one feed's entries. Without flags it SHALL list the 20 most recent entries; `--all` SHALL list every entry; `--unread` SHALL restrict to unread entries and keep the 20-entry default cap; `--limit <n>` SHALL list at most `n` entries and override both the default and `--all`. Listing SHALL NOT mark entries read.

#### Scenario: View recent entries
- **WHEN** `metro view f12` is run
- **THEN** at most the 20 most recent entries from feed f12 are listed, newest first

#### Scenario: View all entries
- **WHEN** `metro view f12 --all` is run
- **THEN** every entry from feed f12 is listed

#### Scenario: View unread entries only
- **WHEN** `metro view f12 --unread` is run
- **THEN** only the 20 most recent unread entries from feed f12 are listed

#### Scenario: View limited entries
- **WHEN** `metro view f12 --limit 10` is run
- **THEN** at most 10 entries are displayed

#### Scenario: Invalid view arguments are usage errors
- **WHEN** `metro view f12 --all --unread` or `metro view f12 --limit 0` is run
- **THEN** the command exits with code 2

#### Scenario: View unknown feed fails
- **WHEN** `metro view f99` is run and f99 does not exist
- **THEN** the command exits with code 3

### Requirement: View ordering and columns
`metro view` SHALL list entries newest first by published date (else first-seen time), with undated entries last and ties broken by most recently inserted. Text output SHALL show one entry per line: `id  published  read-state  title  url` (published as RFC 3339 UTC, or `-`).

#### Scenario: Entries without published date use first-seen
- **WHEN** a feed has an entry without a published date first seen today and an entry published yesterday
- **THEN** `metro view` lists the undated entry first

### Requirement: Machine-readable output
With `--json`, list commands (`folders list`, `feeds list`, `view`) SHALL print NDJSON (one JSON object per record per line), and single-result commands SHALL print one JSON object per line (one per affected feed for batch `remove`/`move`). Typed IDs SHALL appear as their prefixed strings (`"fo3"`, `"f12"`, `"e4031"`), and an `id` value copied from JSON output SHALL be accepted verbatim as a command argument. An empty list SHALL print nothing.

#### Scenario: JSON output format
- **WHEN** `metro feeds list --json` is run
- **THEN** the output is valid NDJSON, one feed object (with its `folder_name`) per line

#### Scenario: Folder JSON shape
- **WHEN** `metro folders list --json` is run
- **THEN** each line is an object like `{"id":"fo3","name":"Tech","kind":"feeds"}`

#### Scenario: Entries JSON
- **WHEN** `metro view f12 --json` is run
- **THEN** each line is one entry object with prefixed `id` (`"e…"`) and `feed_id` (`"f12"`) strings

#### Scenario: JSON IDs round-trip as arguments
- **WHEN** the `id` of a line of `metro feeds list --json` (e.g. `"f12"`) is passed to `metro feeds info`
- **THEN** the command accepts it and shows that feed

### Requirement: Output format
Without `--json`, the system SHALL output plain text with one record per line: columns aligned with spaces when stdout is a terminal, and tab-separated when stdout is piped. Control characters in displayed text (including terminal escape sequences from remote feed content) SHALL be replaced with spaces. A closed stdout pipe SHALL NOT be reported as an error.

#### Scenario: Human-readable output
- **WHEN** `metro feeds list` is run in a terminal (no --json)
- **THEN** output is plain text, one record per line, with aligned columns

#### Scenario: Piped output is tab-separated
- **WHEN** `metro feeds list | cat` is run
- **THEN** each line's fields are separated by tabs

#### Scenario: Control characters are stripped
- **WHEN** a feed title contains an escape sequence or a newline
- **THEN** the text output shows it on one line with the control characters replaced by spaces
