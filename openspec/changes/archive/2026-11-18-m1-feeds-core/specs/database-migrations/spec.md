# Spec Delta

## MODIFIED Requirements

### Requirement: v001 is deferred to M1
No schema tables SHALL be created in M0. The migration runner SHALL be proven by tests against synthetic migrations, but the first real migration (v001) SHALL be authored in M1.

#### Scenario: M0 database is empty
- **WHEN** `open()` is called in M0
- **THEN** no tables exist except SQLite's internal `sqlite_sequence`

## ADDED Requirements

### Requirement: v001 schema for feeds core
The v001 migration SHALL create three tables: `folders`, `feeds`, `entries` — each with primary keys, foreign key constraints, and performance indexes as defined in the data model.

#### Scenario: v001 creates folders table
- **WHEN** a database at version 0 is migrated
- **THEN** a `folders` table is created with columns: id INTEGER PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('feeds', 'saved'))

#### Scenario: v001 creates feeds table
- **WHEN** a database at version 0 is migrated
- **THEN** a `feeds` table is created with columns: id INTEGER PRIMARY KEY, url TEXT NOT NULL UNIQUE, title TEXT NOT NULL, site_url TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('rss', 'atom', 'json', 'youtube')), folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE, etag TEXT, last_modified TEXT, last_fetch INTEGER, error_state TEXT, last_error_at INTEGER, consecutive_failures INTEGER DEFAULT 0, next_retry INTEGER

#### Scenario: v001 feeds table stores the last failure time
- **WHEN** a database at version 0 is migrated
- **THEN** the `feeds` table has a nullable `last_error_at INTEGER` column holding a Unix timestamp in seconds, `NULL` for a newly inserted feed

#### Scenario: v001 feeds table stores backoff state
- **WHEN** a database at version 0 is migrated
- **THEN** the `feeds` table has a `consecutive_failures INTEGER` column with default `0` and a nullable `next_retry INTEGER` column holding a Unix timestamp in seconds

#### Scenario: New feed starts without backoff
- **GIVEN** the v001 migration has been applied
- **WHEN** a feed is inserted without values for `consecutive_failures` and `next_retry`
- **THEN** `consecutive_failures` is `0` and `next_retry` is `NULL`

#### Scenario: v001 creates entries table
- **WHEN** a database at version 0 is migrated
- **THEN** an `entries` table is created with columns: id INTEGER PRIMARY KEY, feed_id INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE, guid TEXT NOT NULL, url TEXT NOT NULL, title TEXT NOT NULL, author TEXT, published INTEGER, summary TEXT, content TEXT, is_read INTEGER NOT NULL DEFAULT 0, first_seen INTEGER DEFAULT 0, UNIQUE(feed_id, guid)

#### Scenario: v001 creates performance indexes
- **WHEN** a database at version 0 is migrated
- **THEN** indexes exist for `feeds(folder_id)`, `entries(feed_id)`, `entries(feed_id, url)`, `entries(published)`, `entries(is_read)`, and `feeds(next_retry)`

#### Scenario: v001 sets user_version to 1
- **WHEN** the v001 migration is applied
- **THEN** `PRAGMA user_version` returns 1

#### Scenario: v001 enforces foreign key constraints
- **GIVEN** the v001 migration has been applied
- **WHEN** inserting a feed with a non-existent folder_id
- **THEN** the insert fails due to FK constraint

#### Scenario: v001 cascade deletes entries when feed is deleted
- **GIVEN** a folder, feed, and entry exist with proper FK relationships
- **WHEN** the feed is deleted
- **THEN** the entry is automatically deleted via ON DELETE CASCADE

#### Scenario: v001 prevents duplicate guid values within a feed
- **GIVEN** the v001 migration has been applied
- **WHEN** inserting an entry with a guid that already exists for the same feed
- **THEN** the insert fails due to UNIQUE constraint

#### Scenario: v001 allows the same guid in different feeds
- **GIVEN** the v001 migration has been applied and two feeds exist
- **WHEN** inserting entries with the same guid into each feed
- **THEN** both inserts succeed
