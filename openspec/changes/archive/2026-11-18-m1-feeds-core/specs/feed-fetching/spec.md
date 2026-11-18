# Spec Delta

## Purpose

Fetches feed content from remote URLs using HTTP with conditional GET support, parses responses, and handles 304 Not Modified efficiently.

## ADDED Requirements

### Requirement: HTTP fetch with conditional GET
The system SHALL fetch a feed URL using `ureq`, sending `If-None-Match` when a cached ETag is available and `If-Modified-Since` when a cached Last-Modified value is available. Only absolute `http(s)` URLs SHALL be accepted; anything else is a usage error.

#### Scenario: Full fetch with no cached values
- **WHEN** `fetch_feed(url, None, None)` is called
- **THEN** a full GET request is made and the parsed feed is returned with the ETag and Last-Modified from the response

#### Scenario: 304 Not Modified returns empty entries
- **WHEN** `fetch_feed(url, Some(etag), Some(last_modified))` is called and the server returns 304
- **THEN** a not-modified result with no feed metadata and an empty entry list is returned, and the cached ETag/Last-Modified are preserved unless the 304 response repeats a validator, which then takes precedence

#### Scenario: New content returns updated entries
- **WHEN** `fetch_feed(url, Some(etag), Some(last_modified))` is called and the server returns 200 with new content
- **THEN** the entries are returned and the validators are replaced by those of the response (absent when the server no longer sends them)

#### Scenario: Invalid URL is a usage error
- **WHEN** `fetch_feed` is called with an empty, malformed, or non-`http(s)` URL
- **THEN** a usage error (exit code 2) is returned without any request

### Requirement: Fetch timeouts and limits
The system SHALL use a 30-second connect timeout and a 60-second read timeout (applied to both the response headers and the body). Response bodies SHALL be capped at 10 MiB. At most 5 redirects SHALL be followed. Transport failures, timeouts, too many redirects, oversized bodies, and non-success HTTP statuses other than 304 SHALL be network errors (exit code 4).

#### Scenario: Connection timeout returns error
- **WHEN** a feed URL does not respond within 30 seconds
- **THEN** the fetch returns a network error

#### Scenario: Read timeout returns error
- **WHEN** a feed response takes more than 60 seconds to download
- **THEN** the fetch returns a network error

#### Scenario: Redirect limit enforced
- **WHEN** a feed URL redirects more than 5 times
- **THEN** the fetch returns a network error

#### Scenario: Oversized body rejected
- **WHEN** a feed response body exceeds the size cap
- **THEN** the fetch returns a network error

#### Scenario: HTTP error status returns network error
- **WHEN** the server responds with a status such as 404 or 500
- **THEN** the fetch returns a network error

### Requirement: Parse response with feed-rs
The system SHALL parse the fetched body using `feed-rs` (RSS 0.9x/1.0/2.0, Atom, JSON Feed) and return feed metadata (kind inferred from the parsed format, title, site URL) and normalized entries. An unparsable body SHALL be a generic error (exit code 1).

#### Scenario: RSS feed parses successfully
- **WHEN** an RSS 2.0 response is parsed
- **THEN** the kind is `rss` and entries are returned with guid, url, title, author, published date, and summary

#### Scenario: Atom feed parses successfully
- **WHEN** an Atom response is parsed
- **THEN** the kind is `atom` and entries are returned with guid, url, title, author, published date, and summary

#### Scenario: JSON Feed parses successfully
- **WHEN** a JSON Feed response is parsed
- **THEN** the kind is `json` and entries are returned with the same normalized fields

#### Scenario: Invalid body returns parse error
- **WHEN** a body that is not a supported feed is parsed
- **THEN** a generic parse error (exit code 1) is returned

### Requirement: Entry field normalization
Each parsed entry SHALL carry: guid, url (the alternate link, else the first link, resolved against the final URL), title, author (first named author), published (Unix seconds of the published date, else the updated date), summary, and content. Blank text values SHALL be treated as absent.

#### Scenario: Atom entry without published uses updated
- **WHEN** an Atom entry carries only `<updated>`
- **THEN** its published value is the updated date

### Requirement: User-Agent header
The system SHALL send a polite User-Agent header: `metro-feed/0.1 (terminal; local-first)`.

#### Scenario: User-Agent is set
- **WHEN** a feed is fetched
- **THEN** the request includes the `User-Agent: metro-feed/0.1 (terminal; local-first)` header

### Requirement: Error state on failure
Fetching SHALL have no database access. When a feed's fetch fails, the refresh pipeline SHALL record on the feed the error message (`error_state`) and the failure time (`last_error_at`, Unix seconds), and SHALL clear both on the next successful fetch (200 or 304). A failure SHALL NOT change `last_fetch`.

#### Scenario: Network error updates feed state
- **WHEN** a refresh fetch of a feed returns a network or parse error
- **THEN** the feed's `error_state` is set to the error message and its `last_error_at` to the failure time

#### Scenario: Success clears error state
- **WHEN** a feed with an `error_state` is fetched successfully (200 or 304)
- **THEN** its `error_state` and `last_error_at` are cleared

#### Scenario: Feed that never failed has no failure time
- **WHEN** a feed has never failed to fetch
- **THEN** its `last_error_at` is empty
