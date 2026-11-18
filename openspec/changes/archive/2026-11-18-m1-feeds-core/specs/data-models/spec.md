# Spec Delta

## Purpose

Provides type-safe in-memory representations of the core domain entities — Folder, Feed, and Entry — with stable typed ID wrappers for compile-time safety and human-friendly display.

## ADDED Requirements

### Requirement: Folder model
The system SHALL define a `Folder` struct with fields `id` (typed `FolderId`), `name` (case-insensitive text), and `kind` (enum: `Feeds` or `Saved`).

#### Scenario: Folder has required fields
- **WHEN** a `Folder` is constructed
- **THEN** it contains `id`, `name`, and `kind` fields

#### Scenario: Folder kind is typed enum
- **WHEN** a folder is created for feeds
- **THEN** `kind` is `FolderKind::Feeds`

#### Scenario: Folder kind distinguishes namespaces
- **WHEN** a folder is created for saved items
- **THEN** `kind` is `FolderKind::Saved`, distinct from `Feeds`

### Requirement: Feed model
The system SHALL define a `Feed` struct with fields `id` (typed `FeedId`), `url`, `title`, `site_url`, `kind` (enum: `Rss`, `Atom`, `Json`, `Youtube`), `folder_id` (typed `FolderId`), `etag`, `last_modified`, `last_fetch` (Unix timestamp of the last successful fetch), `error_state` (nullable string), and `last_error_at` (nullable Unix timestamp of the last failed fetch).

#### Scenario: Feed tracks all required metadata
- **WHEN** a feed is loaded from the database
- **THEN** it contains url, title, site_url, kind, folder_id, etag, last_modified, last_fetch, error_state, and last_error_at

#### Scenario: Feed kind distinguishes formats
- **WHEN** an RSS feed is parsed
- **THEN** `kind` is `FeedKind::Rss`

#### Scenario: Feed error state is nullable
- **WHEN** a feed has never failed
- **THEN** `error_state` and `last_error_at` are `None`

### Requirement: Entry model
The system SHALL define an `Entry` struct with fields `id` (typed `EntryId`), `feed_id` (typed `FeedId`), `guid`, `url`, `title`, `author`, `published` (Unix timestamp), `summary` (nullable), `content` (nullable), and `is_read` (boolean, default `false`).

#### Scenario: Entry tracks all required fields
- **WHEN** an entry is parsed from a feed
- **THEN** it contains feed_id, guid, url, title, author, published, summary, content, and is_read

#### Scenario: Entry read state defaults to false
- **WHEN** a new entry is inserted
- **THEN** `is_read` is `false`

### Requirement: Typed ID wrappers
The system SHALL define `FolderId`, `FeedId`, `EntryId`, and `SavedItemId` as distinct newtype wrappers around a numeric ID. Each type SHALL implement `Display` with its own prefix format (`FolderId` → `fo<N>`, `FeedId` → `f<N>`, `EntryId` → `e<N>`, `SavedItemId` → `s<N>`) and `FromStr` that accepts that format and rejects a bare number or a wrong prefix with a hint naming the expected prefix.

#### Scenario: FolderId displays with prefix
- **WHEN** `FolderId(3)` is formatted
- **THEN** the output is `fo3`

#### Scenario: FeedId displays with prefix
- **WHEN** `FeedId(12)` is formatted
- **THEN** the output is `f12`

#### Scenario: EntryId displays with prefix
- **WHEN** `EntryId(4031)` is formatted
- **THEN** the output is `e4031`

#### Scenario: SavedItemId displays with prefix
- **WHEN** `SavedItemId(88)` is formatted
- **THEN** the output is `s88`

#### Scenario: Bare number is rejected with hint
- **WHEN** `EntryId::from_str("123")` is called
- **THEN** parsing fails with a hint suggesting the `e` prefix

#### Scenario: Prefixed ID parses
- **WHEN** `FeedId::from_str("f12")` is called
- **THEN** the result is `FeedId(12)`

### Requirement: Exact ID prefix matching
ID parsing SHALL take the leading run of ASCII letters as the prefix and require it to match the expected prefix exactly (case-sensitive), so a folder ID is never accepted as a feed ID and a feed ID is never accepted as a folder ID.

#### Scenario: Folder ID is rejected as a feed ID
- **WHEN** `FeedId::from_str("fo3")` is called
- **THEN** parsing fails with a hint naming the `f<number>` form

#### Scenario: Feed ID is rejected as a folder ID
- **WHEN** `FolderId::from_str("f3")` is called
- **THEN** parsing fails with a hint naming the `fo<number>` form

#### Scenario: ID types are not interchangeable
- **WHEN** a `FolderId` is passed where a `FeedId` is expected
- **THEN** the code does not compile

### Requirement: Models serialize to JSON
The system SHALL derive `serde::{Serialize, Deserialize}` on all model structs. JSON field names SHALL use snake_case matching the spec §5 field list, kind enums SHALL serialize as lowercase strings (`feeds`, `saved`, `rss`, `atom`, `json`, `youtube`), and typed IDs SHALL serialize as their prefixed display string (e.g. `"f12"`) and deserialize only from that string form, so an ID copied from JSON output is accepted wherever the ID is parsed.

#### Scenario: Model round-trips through JSON
- **WHEN** a `Feed` is serialized to JSON and deserialized back
- **THEN** the reconstructed object matches the original

#### Scenario: Typed ID serializes as a prefixed string
- **WHEN** `FeedId(12)` and `FolderId(3)` are serialized to JSON
- **THEN** the outputs are `"f12"` and `"fo3"`

#### Scenario: Bare JSON number is rejected as an ID
- **WHEN** the JSON value `12` is deserialized as a `FeedId`
- **THEN** deserialization fails
