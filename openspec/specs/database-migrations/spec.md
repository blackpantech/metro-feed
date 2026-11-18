# Database Migrations Specification

## Purpose

Enables the application to maintain a SQLite database with a hand-rolled migration system, WAL mode, busy timeout, and foreign key enforcement.

## Requirements

### Requirement: Database uses bundled SQLite
The application SHALL use `rusqlite` with the `bundled` feature, compiling SQLite from source. No system SQLite library SHALL be required. The `time` feature SHALL be enabled for time handling.

#### Scenario: Build succeeds without system SQLite
- **WHEN** `cargo build` runs on a system without SQLite installed
- **THEN** the build succeeds, bundling SQLite into the binary

### Requirement: Connection uses WAL mode
Every database connection SHALL enable WAL (Write-Ahead Logging) mode via `PRAGMA journal_mode = WAL`.

#### Scenario: WAL mode is enabled
- **WHEN** a database connection is opened
- **THEN** `PRAGMA journal_mode` returns `wal`

### Requirement: Busy timeout is set
Every database connection SHALL set a busy timeout via `PRAGMA busy_timeout` to handle concurrent access.

#### Scenario: Concurrent access waits instead of failing
- **WHEN** two processes access the database simultaneously
- **THEN** the second process waits up to the timeout instead of receiving `SQLITE_BUSY`

### Requirement: Foreign keys are enforced
Every database connection SHALL enable foreign key enforcement via `PRAGMA foreign_keys = ON`.

#### Scenario: Foreign key violation is rejected
- **WHEN** an INSERT violates a foreign key constraint
- **THEN** the operation fails with a foreign key constraint error

### Requirement: Migration runner uses user_version
The migration system SHALL use `PRAGMA user_version` to track the current schema version. Migrations SHALL be embedded SQL strings executed in a transaction.

#### Scenario: Fresh database starts at version 0
- **WHEN** a new database file is opened
- **THEN** `PRAGMA user_version` returns 0

#### Scenario: Migration advances version
- **WHEN** a migration is successfully applied
- **THEN** `PRAGMA user_version` is incremented to the new version

### Requirement: Migrations run in a transaction
Each migration SHALL execute within a single transaction. If any statement fails, the entire migration SHALL roll back.

#### Scenario: Failed migration rolls back
- **WHEN** a migration's second statement fails
- **THEN** the first statement's changes are rolled back and `user_version` is unchanged

### Requirement: Migrations are ordered and idempotent
Migrations SHALL be numbered sequentially (v001, v002, ...). The runner SHALL apply migrations in order, skipping already-applied versions.

#### Scenario: Already-migrated database skips migrations
- **WHEN** `open()` is called on a database with `user_version = 1`
- **THEN** v001 is not re-applied

#### Scenario: Partial migration is detected
- **WHEN** a database has `user_version = 0` but some v001 tables exist
- **THEN** v001 runs from the start (idempotent SQL handles existing tables)

### Requirement: v001 is deferred to M1
No schema tables SHALL be created in M0. The migration runner SHALL be proven by tests against synthetic migrations, but the first real migration (v001) SHALL be authored in M1.

#### Scenario: M0 database is empty
- **WHEN** `open()` is called in M0
- **THEN** no tables exist except SQLite's internal `sqlite_sequence`

### Requirement: Schema freeze at v0.5 (M4)
The v001 schema SHALL be revisable in place (delete dev DB and re-migrate) until the v0.5 release at M4. From M5 onward, schema changes SHALL be real v002+ migrations with upgrade paths.

#### Scenario: Pre-M4 schema change edits v001
- **WHEN** a schema change is needed in M2
- **THEN** v001.sql is edited directly; dev DBs are deleted and re-migrated

#### Scenario: Post-M4 schema change creates v002
- **WHEN** a schema change is needed in M5
- **THEN** v002.sql is created; existing databases upgrade via migration

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
