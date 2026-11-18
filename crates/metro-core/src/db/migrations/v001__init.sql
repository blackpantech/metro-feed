-- M1: Initial schema for folders, feeds, and entries.
-- This migration creates the core tables for feed management.

-- Folders table: organizational containers for feeds and saved items.
CREATE TABLE IF NOT EXISTS folders (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('feeds', 'saved'))
);

-- Feeds table: subscribed feeds with metadata and fetch state.
-- `last_fetch` is the last *successful* fetch (Unix seconds); `error_state`
-- and `last_error_at` (Unix seconds) describe the last failed fetch and are
-- both cleared by the next success (200 or 304).
CREATE TABLE IF NOT EXISTS feeds (
    id INTEGER PRIMARY KEY,
    url TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    site_url TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('rss', 'atom', 'json', 'youtube')),
    folder_id INTEGER NOT NULL,
    etag TEXT,
    last_modified TEXT,
    last_fetch INTEGER,
    error_state TEXT,
    last_error_at INTEGER,
    consecutive_failures INTEGER DEFAULT 0,
    next_retry INTEGER,
    FOREIGN KEY (folder_id) REFERENCES folders(id) ON DELETE CASCADE
);

-- Entries table: individual items from feeds.
-- `guid` holds the refresh dedup key (guid -> url -> content hash). It is
-- unique per feed, not globally: two feeds publishing the same guid each keep
-- their own entry (design D2).
CREATE TABLE IF NOT EXISTS entries (
    id INTEGER PRIMARY KEY,
    feed_id INTEGER NOT NULL,
    guid TEXT NOT NULL,
    url TEXT NOT NULL,
    title TEXT NOT NULL,
    author TEXT,
    published INTEGER,
    summary TEXT,
    content TEXT,
    is_read INTEGER NOT NULL DEFAULT 0,
    first_seen INTEGER DEFAULT 0,
    FOREIGN KEY (feed_id) REFERENCES feeds(id) ON DELETE CASCADE,
    UNIQUE (feed_id, guid)
);

-- Performance indexes.
CREATE INDEX IF NOT EXISTS idx_feeds_folder_id ON feeds(folder_id);
CREATE INDEX IF NOT EXISTS idx_entries_feed_id ON entries(feed_id);
-- Backs the per-feed URL dedup guard of the refresh writer.
CREATE INDEX IF NOT EXISTS idx_entries_feed_url ON entries(feed_id, url);
CREATE INDEX IF NOT EXISTS idx_entries_published ON entries(published);
CREATE INDEX IF NOT EXISTS idx_entries_is_read ON entries(is_read);
CREATE INDEX IF NOT EXISTS idx_feeds_next_retry ON feeds(next_retry);