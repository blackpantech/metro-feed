# Feed Management Specification

## Purpose

Manages feed subscriptions with CRUD operations and duplicate URL detection for RSS, Atom, and JSON Feed sources.

## Requirements

### Requirement: Add feed
The system SHALL add a new feed subscription with a URL, optional title, and optional folder. The URL SHALL be normalized before it is compared and stored. If a feed with the same normalized URL already exists, the system SHALL return the existing feed's ID with a duplicate indication and SHALL NOT modify the existing subscription. The folder, when supplied, SHALL exist and be a feed folder. An explicit title SHALL be trimmed and MUST NOT be blank.

#### Scenario: Add a new feed succeeds
- **WHEN** `add_feed("https://example.com/feed.xml", None, unsorted_folder_id)` is called
- **THEN** a new feed is created and its ID is returned

#### Scenario: Add duplicate feed returns existing
- **WHEN** `add_feed("https://example.com/feed.xml", None, unsorted_folder_id)` is called twice
- **THEN** the second call returns the same feed ID as the first, marked as not created

#### Scenario: Add feed with explicit title
- **WHEN** `add_feed("https://example.com/feed.xml", Some("My Blog"), folder_id)` is called
- **THEN** the feed's title is set to "My Blog"

#### Scenario: Add feed to default folder
- **WHEN** `add_feed(url, None, None)` is called
- **THEN** the feed is assigned to the `Unsorted` feed folder, which can be neither renamed nor deleted (see folder-management)

#### Scenario: Add feed into a saved folder is rejected
- **WHEN** `add_feed` is called with the ID of a `Saved` folder
- **THEN** the operation fails with a usage error and nothing is written

#### Scenario: Add feed into unknown folder fails
- **WHEN** `add_feed` is called with a folder ID that does not exist
- **THEN** the operation fails with a not-found error

#### Scenario: Blank explicit title is rejected
- **WHEN** `add_feed(url, Some("  "), None)` is called
- **THEN** the operation fails with a usage error

### Requirement: Provisional metadata
The network-free `add_feed` SHALL store provisional metadata until the feed is first fetched: kind `rss`, the supplied title or else the URL's lowercased host, and the URL's origin as `site_url`. `last_fetch` SHALL be NULL.

#### Scenario: Provisional metadata on add
- **WHEN** `add_feed("https://example.com/feed.xml", None, None)` is called
- **THEN** the feed is stored with kind `rss`, title `example.com`, site_url `https://example.com`, and `last_fetch` NULL

### Requirement: Subscribe with initial fetch
The system SHALL provide `subscribe_feed`, which fetches a feed before subscribing to it. On success, a single transaction SHALL insert the feed into the given folder (default `Unsorted`) with the parsed `kind`, the feed's site URL (else the URL origin) and its final title (explicit title, else the parsed feed title, else the host placeholder), then store the fetched entries, `etag`, `last_modified`, and `last_fetch` exactly as a refresh would.

#### Scenario: Subscribe stores real metadata and entries
- **WHEN** `subscribe_feed` is called for a reachable Atom feed titled "Atom Blog"
- **THEN** the feed is stored with kind `atom`, title "Atom Blog", the feed's site URL, its entries, its validators, and a non-NULL `last_fetch`

#### Scenario: Explicit title wins
- **WHEN** `subscribe_feed` is called with title "My Blog" for a feed titled "Atom Blog"
- **THEN** the feed's title is "My Blog"

### Requirement: Subscribe writes nothing unless the feed is new and fetchable
`subscribe_feed` SHALL validate the URL, the explicit title, and the target folder before any network access. It SHALL return an already-subscribed URL (after normalization) as a duplicate without fetching. When the fetch fails, it SHALL return the error (network → exit code 4, unparsable body → exit code 1) and write nothing.

#### Scenario: Fetch failure writes nothing
- **WHEN** `subscribe_feed` is called and the fetch fails with a network or parse error
- **THEN** the error is returned and no feed, folder, or entry rows are written

#### Scenario: Duplicate is returned without fetching
- **WHEN** `subscribe_feed` is called for a URL that is already subscribed
- **THEN** the existing feed is returned as a duplicate and no request is made

#### Scenario: Invalid folder fails before network access
- **WHEN** `subscribe_feed` is called with a folder that does not exist or is a saved folder
- **THEN** a not-found or usage error is returned and no request is made

### Requirement: Subscribe from user input with autodiscovery
The system SHALL provide `subscribe_url`, which accepts a feed URL or a homepage URL. It SHALL validate its inputs and short-circuit an already-subscribed input URL as a duplicate before network access, then request the input URL once through autodiscovery. When the body is a page advertising a feed, `subscribe_feed` SHALL be called with the discovered URL (which re-checks for a duplicate). Discovery errors SHALL be returned unchanged and nothing SHALL be written.

#### Scenario: Homepage resolves to its feed
- **WHEN** `subscribe_url` is called with a homepage advertising `https://blog.example.com/feed.xml`
- **THEN** that feed is fetched and subscribed

#### Scenario: Homepage advertising a subscribed feed is a duplicate
- **WHEN** `subscribe_url` is called with a homepage whose advertised feed is already subscribed
- **THEN** the existing feed is returned as a duplicate and nothing is written

#### Scenario: Homepage without feed fails
- **WHEN** `subscribe_url` is called with a page that advertises no feed
- **THEN** a not-found error (exit code 3) is returned and nothing is written

### Requirement: Direct feed reuses the discovery response
When the body returned by `subscribe_url`'s single request parses as a feed, whatever its `Content-Type`, that response SHALL be used as the subscription fetch and stored under the normalized input URL exactly as `subscribe_feed` would store it, without a second request.

#### Scenario: Direct feed URL is subscribed with one request
- **WHEN** `subscribe_url` is called with a URL serving an RSS document as `text/html`
- **THEN** the feed and its entries are stored from that response, and exactly one request is made

### Requirement: Subscribe dry run
In dry-run mode, `subscribe_feed` and `subscribe_url` SHALL perform discovery and the fetch and return the metadata they would store (URL, title, site URL, kind, entry count), without writing to the database — not even the default `Unsorted` folder.

#### Scenario: Dry run writes nothing
- **WHEN** `subscribe_feed` is called in dry-run mode
- **THEN** the detected metadata is returned and the database is unchanged

### Requirement: List feeds
The system SHALL return feeds sorted by folder name (case-insensitive) then by title. When a folder filter is applied, only feeds in that folder are returned; an unknown folder yields an empty list.

#### Scenario: List all feeds
- **WHEN** `list_feeds(None)` is called
- **THEN** all feeds are returned sorted by folder name then title

#### Scenario: List feeds in a folder
- **WHEN** `list_feeds(Some(folder_id))` is called
- **THEN** only feeds in that folder are returned

### Requirement: Remove feed
The system SHALL delete one or more feeds and cascade-delete their entries in a single transaction, all-or-nothing. A repeated ID SHALL be removed once. The removed feeds SHALL be returned as they were just before deletion (including their entry count).

#### Scenario: Remove a feed deletes it and its entries
- **WHEN** `remove_feed(feed_id)` is called
- **THEN** the feed is deleted and all associated entries are cascade-deleted

#### Scenario: Remove non-existent feed fails
- **WHEN** `remove_feed(nonexistent_id)` is called
- **THEN** the operation fails with a not-found error

#### Scenario: Batch remove with an unknown ID changes nothing
- **WHEN** `remove_feeds([f1, f2, f99])` is called and f99 does not exist
- **THEN** the operation fails with a not-found error and f1 and f2 are not removed

### Requirement: Move feed
The system SHALL reassign one or more feeds to a feed folder in a single transaction, all-or-nothing. A repeated ID SHALL be moved once; moving a feed into its current folder SHALL succeed.

#### Scenario: Move feed to another folder
- **WHEN** `move_feed(feed_id, new_folder_id)` is called
- **THEN** the feed's folder_id is updated to new_folder_id

#### Scenario: Move to non-existent folder fails
- **WHEN** `move_feed(feed_id, invalid_folder_id)` is called
- **THEN** the operation fails with a not-found error

#### Scenario: Move into a saved folder is rejected
- **WHEN** `move_feed(feed_id, saved_folder_id)` is called
- **THEN** the operation fails with a usage error

#### Scenario: Batch move with an unknown ID changes nothing
- **WHEN** `move_feeds([f1, f99], folder_id)` is called and f99 does not exist
- **THEN** the operation fails with a not-found error and f1 is not moved

### Requirement: Rename feed
The system SHALL update a feed's title. The title SHALL be trimmed and MUST NOT be blank.

#### Scenario: Rename a feed
- **WHEN** `rename_feed(feed_id, "New Title")` is called
- **THEN** the feed's title is updated

#### Scenario: Blank title is rejected
- **WHEN** `rename_feed(feed_id, "  ")` is called
- **THEN** the operation fails with a usage error

#### Scenario: Rename non-existent feed fails
- **WHEN** `rename_feed(nonexistent_id, "Title")` is called
- **THEN** the operation fails with a not-found error

### Requirement: Get feed info
The system SHALL return feed metadata including ID, URL, title, site URL, kind, folder ID and name, last fetch time, error state, last failure time (`last_error_at`), and entry count.

#### Scenario: Get feed info returns complete metadata
- **WHEN** `get_feed_info(feed_id)` is called
- **THEN** the result includes url, title, site_url, kind, folder_name, last_fetch, error_state, last_error_at, and entry_count

#### Scenario: Get info of non-existent feed fails
- **WHEN** `get_feed_info(nonexistent_id)` is called
- **THEN** the operation fails with a not-found error

### Requirement: Duplicate detection by normalized URL
The system SHALL normalize feed URLs before storage and comparison: lowercase the scheme and host, drop fragments, drop tracking query parameters (any `utm_*` key plus well-known click IDs such as `fbclid`, compared case-insensitively) while keeping the remaining parameters in order, and strip trailing slashes from the path. Only absolute `http(s)` URLs with a host SHALL be accepted; anything else is a usage error.

#### Scenario: URLs with different cases are detected as duplicates
- **WHEN** `add_feed("https://Example.com/feed.xml", ...)` and `add_feed("https://example.com/feed.xml", ...)` are called
- **THEN** the second call is treated as a duplicate

#### Scenario: URLs with tracking params are normalized
- **WHEN** `add_feed("https://example.com/feed.xml?utm_source=rss", ...)` is called
- **THEN** the tracking parameter is stripped before dedup comparison

#### Scenario: Fragment and trailing slash are dropped
- **WHEN** `https://example.com/feed/#top` is normalized
- **THEN** the result is `https://example.com/feed`

#### Scenario: Non-http URL is rejected
- **WHEN** `add_feed("ftp://example.com/feed.xml", ...)` is called
- **THEN** the operation fails with a usage error
