# Spec Delta

## Purpose

Refreshes feed content in parallel using a configurable worker pool, deduplicates new entries, and reports progress through channels.

## ADDED Requirements

### Requirement: Refresh targets
The system SHALL refresh every subscribed feed, the feeds of one folder, or an explicit list of feeds. In an explicit list, a repeated ID SHALL be refreshed once and an unknown ID SHALL fail the whole call with a not-found error before anything is fetched. An unknown folder SHALL yield no feeds.

#### Scenario: Unknown feed ID aborts before fetching
- **WHEN** `refresh_feeds` targets `[f1, f99]` and f99 does not exist
- **THEN** a not-found error is returned and no feed is fetched

### Requirement: Worker pool with configurable concurrency
The system SHALL fetch feeds on a pool of worker threads whose size defaults to 8, is configurable, and is capped by the number of due feeds. A concurrency of 0 SHALL be a usage error.

#### Scenario: Default concurrency is 8
- **WHEN** `refresh_feeds` is called with no concurrency for 20 due feeds
- **THEN** 8 worker threads are used

#### Scenario: Custom concurrency is respected
- **WHEN** `refresh_feeds` is called with concurrency 4
- **THEN** at most 4 worker threads are used

#### Scenario: Pool is capped by due feeds
- **WHEN** `refresh_feeds` is called with concurrency 8 for 3 due feeds
- **THEN** 3 worker threads are used

#### Scenario: Zero concurrency is rejected
- **WHEN** `refresh_feeds` is called with concurrency 0
- **THEN** a usage error is returned and nothing is fetched

### Requirement: Parallel feed fetching
The system SHALL fetch multiple feeds concurrently, up to the worker pool size, while a single thread persists each result in its own transaction as it arrives.

#### Scenario: 50 feeds refresh in parallel
- **WHEN** `refresh_feeds` is called for 50 feeds with concurrency 8
- **THEN** feeds are fetched 8 at a time (or fewer as workers become available)

### Requirement: Entry deduplication
The system SHALL deduplicate entries in the following priority order: guid, then url, then content hash. The dedup key SHALL be the trimmed guid, else the trimmed url, else a stable content hash of the title, author, published date, summary, and content. An entry whose key, or whose non-empty url, already exists in the same feed SHALL be skipped. New entries SHALL be stored unread with first-seen set to the insertion time.

#### Scenario: Duplicate guid is skipped
- **WHEN** a refresh returns an entry with a guid already stored for the same feed
- **THEN** the entry is skipped

#### Scenario: Duplicate url is skipped
- **WHEN** a refresh returns an entry with a url matching an existing entry of the same feed (different guid)
- **THEN** the entry is skipped

#### Scenario: Entry without guid and url is deduplicated by content
- **WHEN** a refresh returns, twice, an entry with no guid and no url but the same content
- **THEN** only one entry is stored

### Requirement: Deduplication is scoped per feed
Every deduplication tier (guid, url, content hash) SHALL compare an entry only with entries of the same feed. Entries of different feeds SHALL never deduplicate each other.

#### Scenario: Same guid in two feeds is stored twice
- **WHEN** two feeds each publish an entry with the same guid
- **THEN** each feed stores its own entry

#### Scenario: Same url in two feeds is stored twice
- **WHEN** two feeds each publish an entry with the same url and different guids
- **THEN** each feed stores its own entry

### Requirement: Out-of-window entries are not inserted
The system SHALL NOT insert a fetched entry whose effective date (its `published` date, else the insertion time) is strictly before the pruning cutoff (`now - window_days` days, the same cutoff the post-refresh prune uses). A skipped entry SHALL NOT be counted in the new entry count. Entries without a published date and entries dated in the future SHALL always be inserted. The subscribe-time first fetch SHALL apply the same rule with the default window of 30 days.

#### Scenario: Old entry is not inserted
- **WHEN** a refresh returns an entry published 40 days ago with a 30-day window
- **THEN** the entry is not stored and is not counted as new

#### Scenario: Recent entry is inserted
- **WHEN** a refresh returns an entry published 2 days ago with a 30-day window
- **THEN** the entry is stored and counted as new

#### Scenario: Undated entry is inserted
- **WHEN** a refresh returns an entry without a published date
- **THEN** the entry is stored and counted as new

#### Scenario: Subscribing skips entries outside the default window
- **WHEN** a feed whose document holds entries published 40 days ago and 2 days ago is subscribed
- **THEN** only the recent entry is stored

### Requirement: Persist fetched feed metadata
The system SHALL update a feed's `kind` and `site_url` from the parsed feed on every 200 response (keeping the stored `site_url` when the feed declares none), and store the response's validators. A 304 response SHALL leave `kind`, `site_url`, and `title` unchanged.

#### Scenario: Kind and site URL follow the feed
- **WHEN** a feed stored with kind `rss` is refreshed and the server returns an Atom document
- **THEN** the feed's `kind` is updated to `atom` and its `site_url` to the document's alternate link

### Requirement: Feed title is set once
The system SHALL update a feed's `title` from the parsed feed only on its first successful fetch (`last_fetch` is NULL) and only when the feed declares a non-blank title. Afterwards a refresh SHALL never overwrite the stored title.

#### Scenario: Title filled on first successful fetch
- **WHEN** a feed with `last_fetch` NULL is refreshed and the parsed feed declares a title
- **THEN** the feed's `title` is set to the parsed title

#### Scenario: Custom title is preserved
- **WHEN** a feed that was already fetched successfully has been renamed and is refreshed with a 200 response
- **THEN** the feed's `title` keeps the user's value

### Requirement: last_fetch records successful fetches only
The system SHALL set `last_fetch` to the current time on every successful fetch (200 or 304). A failed fetch SHALL NOT change `last_fetch`.

#### Scenario: Not modified updates last_fetch only
- **WHEN** a refresh receives 304 Not Modified
- **THEN** `kind`, `site_url`, and `title` are unchanged and `last_fetch` is updated

#### Scenario: Failure does not touch last_fetch
- **WHEN** a refresh of a feed fails
- **THEN** `last_fetch` keeps its previous value

### Requirement: Progress reporting
When a progress channel is supplied, the system SHALL send one start event (number of feeds to fetch, number skipped for backoff, number of workers) before fetching, then one event per fetched feed in completion order carrying the feed ID, completed count, total, and status (updated with its new entry count, not modified, or failed). A dropped receiver SHALL NOT fail the refresh.

#### Scenario: Progress updates are sent
- **WHEN** `refresh_feeds` is running with a progress channel for 50 due feeds
- **THEN** a start event with total 50 is sent, followed by events counting up to 50/50

#### Scenario: Progress includes new entry counts
- **WHEN** a feed is fetched with new entries
- **THEN** the progress event includes the count of new entries

### Requirement: Failure isolation
The system SHALL continue fetching remaining feeds when one feed fails, including when the fetcher panics. A database error SHALL stop the refresh and be returned as an error.

#### Scenario: Partial failure continues
- **WHEN** 3 of 50 feeds fail during refresh
- **THEN** the remaining 47 feeds are still fetched

#### Scenario: Failed feeds are reported
- **WHEN** a feed fails to fetch
- **THEN** the failure is reported with the feed ID and the error (keeping its kind), and the message is stored as the feed's error state

### Requirement: Exit code on partial success
The refresh report SHALL map to an exit status: success (exit code 0) when no fetched feed failed, including when no feed was fetched because none was due; partial success (exit code 5) when at least one fetched feed failed and at least one succeeded; generic error (exit code 1) when every fetched feed failed. Feeds skipped for backoff SHALL NOT count as fetched.

#### Scenario: All feeds succeed returns 0
- **WHEN** all feeds refresh successfully
- **THEN** exit code is 0

#### Scenario: Only skipped feeds returns 0
- **WHEN** every targeted feed is in backoff
- **THEN** nothing is fetched and exit code is 0

#### Scenario: Some feeds fail returns 5
- **WHEN** at least one feed fails and at least one succeeds
- **THEN** exit code is 5

#### Scenario: All feeds fail returns 1
- **WHEN** all fetched feeds fail to refresh
- **THEN** exit code is 1
