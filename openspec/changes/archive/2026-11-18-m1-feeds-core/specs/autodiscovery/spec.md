# Spec Delta

## Purpose

Classifies the URL a user typed with a single request: a body that parses as a feed is a direct feed, otherwise the body is scanned as HTML for `<link rel="alternate">` tags pointing to RSS, Atom, or JSON Feed sources.

## ADDED Requirements

### Requirement: Parse-first classification with a single request
The system SHALL request the given URL once and first try to parse the response body as a feed, whatever the response `Content-Type`. When it parses, the URL SHALL be treated as a direct feed and the parsed response (entries, metadata, and validators) SHALL be returned for the caller to use as the subscription fetch without a second request. Otherwise the same body SHALL be scanned for a feed link.

#### Scenario: Direct feed is detected with one request
- **WHEN** discovery is run on `https://example.com/feed.xml`, which serves an RSS document
- **THEN** the URL is classified as a direct feed with its parsed content, and exactly one request is made

#### Scenario: Feed served as HTML or plain text is detected
- **WHEN** discovery is run on a URL serving a valid feed with `Content-Type: text/html` or `text/plain`
- **THEN** the URL is classified as a direct feed

### Requirement: Unclassifiable response errors
When the body is neither a feed nor a page advertising a supported feed link, a response served with a feed media type SHALL be a generic parse error (exit code 1) and any other response a "no feed found" not-found error (exit code 3).

#### Scenario: Broken feed served with a feed media type
- **WHEN** discovery is run on a URL serving an unparsable body with `Content-Type: application/rss+xml`
- **THEN** a generic parse error (exit code 1) is returned

#### Scenario: Page without feed link is not found
- **WHEN** discovery is run on a `text/html` page that is not a feed and advertises no feed link
- **THEN** a "no feed found" not-found error (exit code 3) is returned

### Requirement: Discover feed from homepage
When the body is not a feed, the system SHALL parse it as HTML for `<link>` tags whose `rel` token list contains `alternate` and whose `type` matches RSS (`application/rss+xml`), Atom (`application/atom+xml`), or JSON Feed (`application/feed+json`), with a non-empty `href`, and return the first such link in document order. `rel` and `type` SHALL be matched case-insensitively, ignoring media type parameters.

#### Scenario: RSS autodiscovery finds feed
- **WHEN** `discover_feed_url("https://blog.example.com")` is called on a page with `<link rel="alternate" type="application/rss+xml" href="https://blog.example.com/feed.xml">`
- **THEN** the RSS feed URL is returned

#### Scenario: Atom autodiscovery finds feed
- **WHEN** `discover_feed_url("https://blog.example.com")` is called on a page with Atom autodiscovery
- **THEN** the Atom feed URL is returned

#### Scenario: JSON Feed autodiscovery finds feed
- **WHEN** `discover_feed_url("https://blog.example.com")` is called on a page with JSON Feed autodiscovery
- **THEN** the JSON Feed URL is returned

#### Scenario: First feed link in document order wins
- **WHEN** a page advertises an Atom link followed by an RSS link
- **THEN** the Atom feed URL is returned

#### Scenario: No feed link returns not found
- **WHEN** `discover_feed_url("https://blog.example.com")` is called on an HTML page with no supported `<link rel="alternate">` feed tag
- **THEN** a "no feed found" not-found error (exit code 3) is returned

#### Scenario: Direct feed URL passes through
- **WHEN** `discover_feed_url("https://example.com/feed.xml")` is called on a URL whose body parses as a feed
- **THEN** the URL is returned unchanged

#### Scenario: Invalid homepage URL is a usage error
- **WHEN** `discover_feed_url` is called with an empty or non-`http(s)` URL
- **THEN** a usage error (exit code 2) is returned without any request

### Requirement: Discovered URL resolution
Relative `href` values SHALL be resolved against the final response URL (after redirects), and the returned URL SHALL be normalized like a stored feed URL.

#### Scenario: Relative link is resolved
- **WHEN** a page at `https://blog.example.com/` advertises `href="/feed.xml"`
- **THEN** `https://blog.example.com/feed.xml` is returned

### Requirement: Timeout handling
The system SHALL respect a 15-second timeout for autodiscovery fetches. Transport failures and non-success HTTP statuses SHALL be reported as network errors.

#### Scenario: Timeout returns network error
- **WHEN** the homepage fetch times out during discovery
- **THEN** a network error (exit code 4) is returned

#### Scenario: HTTP error status returns network error
- **WHEN** the homepage responds with a non-success status such as 404
- **THEN** a network error (exit code 4) is returned
