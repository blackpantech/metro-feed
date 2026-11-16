//! Feed subscription CRUD API for Metro.
//!
//! This module implements the `feed-management` capability of the M1
//! milestone on top of the v001 database schema (`feeds`, `folders`, and
//! `entries` tables).
//!
//! # Semantics
//!
//! - **Duplicate detection by normalized URL**: [`add_feed`] normalizes the
//!   URL with [`normalize_url`] before storing or comparing it. A second
//!   subscription with a URL that normalizes to the same value is reported as
//!   a duplicate by returning the existing feed's ID ([`AddFeedOutcome`])
//!   instead of creating a second row.
//! - **Folder assignment**: feeds always live in a folder of kind
//!   [`FolderKind::Feeds`]. When no folder is supplied, [`add_feed`] assigns
//!   the idempotent default `Unsorted` folder via
//!   [`crate::folders::ensure_default`]. Adding or moving a feed into a
//!   `Saved` folder is rejected because the two folder namespaces are
//!   separate.
//! - **Cascade deletion**: [`remove_feed`] deletes the feed row; the schema's
//!   `ON DELETE CASCADE` on `entries.feed_id` removes its entries in the same
//!   statement. Connections opened through [`crate::db::open`] enforce
//!   foreign keys, so the cascade is guaranteed.
//! - **Provisional metadata**: the v001 schema requires `title`, `site_url`,
//!   and `kind` to be `NOT NULL`, but [`add_feed`] only receives a URL, an
//!   optional title, and an optional folder. Until the first refresh parses
//!   the feed content (which is authoritative for kind, title, and site URL),
//!   the API stores:
//!   - `title`: the supplied title, or the URL's lowercased host,
//!   - `site_url`: the URL's origin (`scheme://host[:port]`),
//!   - `kind`: [`FeedKind::Rss`] as a placeholder.
//!
//!   Callers that need the real metadata refresh the feed after adding it.
//!
//! # URL normalization
//!
//! [`normalize_url`] applies the normalization pinned by the spec:
//!
//! 1. the scheme and host are lowercased (paths and user info are
//!    case-sensitive and preserved),
//! 2. fragments (`#...`) are dropped,
//! 3. tracking query parameters are dropped (any `utm_*` key plus the
//!    well-known click IDs listed in [`TRACKING_PARAM_KEYS`]),
//! 4. remaining query parameters keep their original order and encoding,
//! 5. trailing slashes are stripped from the path (`https://example.com/`
//!    becomes `https://example.com`).
//!
//! Only absolute `http://` and `https://` URLs are accepted; anything else is
//! rejected with [`MetroError::Usage`] so the caller can report a clear
//! message. Default ports are intentionally left in place: they are harmless
//! for deduplication and removing them would diverge from the specified
//! normalization rules.
//!
//! # Concurrency
//!
//! Every mutating function runs inside an `IMMEDIATE` transaction, so the
//! duplicate lookup and the insert, or the folder validation and the update,
//! are atomic with respect to other processes. Combined with the 5-second
//! busy timeout configured by [`crate::db::open`], concurrent `metro`
//! invocations wait for each other instead of failing fast.
//!
//! # Usage notes for callers
//!
//! Callers must pass a migrated connection (see
//! [`crate::db::migrations::run_migrations`]); the functions in this module
//! assume the `feeds`, `folders`, and `entries` tables exist.

use std::str::FromStr;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::MetroError;
use crate::folders::ensure_default;
use crate::models::{Feed, FeedId, FeedKind, FolderId, FolderKind};

/// Well-known query parameter keys that identify tracking clicks rather than
/// feed content.
///
/// Any key that starts with `utm_` is treated as tracking as well. Keys are
/// compared case-insensitively. The list is deliberately conservative:
/// parameters that may influence the served feed (for example `format` or
/// `lang`) are preserved.
const TRACKING_PARAM_KEYS: &[&str] = &[
    "fbclid", "gclid", "dclid", "msclkid", "yclid", "igshid", "mc_cid", "mc_eid", "_ga", "_gl",
];

/// Result of [`add_feed`].
///
/// Reports both the feed ID and whether this call created the subscription.
/// A duplicate subscription is not an error: the existing feed's ID is
/// returned with `created == false`, which lets the CLI tell the user that the
/// feed was already subscribed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddFeedOutcome {
    /// The ID of the newly created or already existing feed.
    pub id: FeedId,
    /// `true` when this call inserted a new row, `false` for a duplicate.
    pub created: bool,
}

impl AddFeedOutcome {
    /// Returns `true` when [`add_feed`] created a new feed.
    #[must_use]
    pub fn is_new(&self) -> bool {
        self.created
    }
}

/// Detailed metadata for a single feed, as returned by [`get_feed_info`].
///
/// Unlike [`Feed`], this view joins the feed's folder name and aggregates the
/// number of stored entries, which is what `metro feeds info` displays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedInfo {
    /// Unique feed identifier.
    pub id: FeedId,
    /// Normalized feed URL.
    pub url: String,
    /// Human-readable feed title (may still be provisional until first fetch).
    pub title: String,
    /// The feed's home page URL.
    pub site_url: String,
    /// Feed format.
    pub kind: FeedKind,
    /// ID of the folder that contains the feed.
    pub folder_id: FolderId,
    /// Display name of the containing folder.
    pub folder_name: String,
    /// Unix timestamp of the last successful fetch, if any.
    pub last_fetch: Option<u64>,
    /// Error message from the last fetch, if it failed.
    pub error_state: Option<String>,
    /// Number of entries currently stored for the feed.
    pub entry_count: u64,
}

/// Parsed components of an absolute `http(s)` URL.
///
/// Borrows slices of the input string so normalization can rebuild the URL
/// without allocations per component. IPv6 hosts are stored without their
/// surrounding brackets.
struct ParsedUrl<'a> {
    /// Original scheme spelling (`http` or `https`, any casing).
    scheme: &'a str,
    /// User info (everything before the last `@`), preserved verbatim.
    userinfo: Option<&'a str>,
    /// Host without brackets, original casing.
    host: &'a str,
    /// Explicit port, if present.
    port: Option<&'a str>,
    /// Path portion including its leading slash, or an empty string.
    path: &'a str,
    /// Query portion without `?`, or `None` when there is no query.
    query: Option<&'a str>,
}

impl<'a> ParsedUrl<'a> {
    /// Parse an absolute `http(s)` URL.
    ///
    /// Fragments are ignored. Returns [`MetroError::Usage`] for empty input,
    /// unsupported schemes, missing hosts, and malformed authorities.
    fn parse(raw: &'a str) -> Result<Self, MetroError> {
        let input = raw.trim();
        if input.is_empty() {
            return Err(MetroError::Usage("feed URL must not be empty".to_string()));
        }

        let (scheme, rest) = input.split_once("://").ok_or_else(|| {
            MetroError::Usage(format!(
                "invalid feed URL {input:?}: expected an absolute http:// or https:// URL"
            ))
        })?;
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return Err(MetroError::Usage(format!(
                "unsupported URL scheme {scheme:?}: only http and https feed URLs are supported"
            )));
        }

        // A fragment never affects the resource that is fetched.
        let rest = rest.split('#').next().unwrap_or_default();

        let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if authority.is_empty() {
            return Err(MetroError::Usage(format!(
                "invalid feed URL {input:?}: the URL has no host"
            )));
        }

        let remainder = &rest[authority_end..];
        let (path, query) = match remainder.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (remainder, None),
        };

        let (userinfo, host_port) = match authority.rsplit_once('@') {
            Some((userinfo, host_port)) => (Some(userinfo), host_port),
            None => (None, authority),
        };
        let bracketed = host_port.starts_with('[');
        let (host, port) = split_host_port(host_port)?;
        if host.is_empty() {
            return Err(MetroError::Usage(format!(
                "invalid feed URL {input:?}: the URL has no host"
            )));
        }
        if host.contains(':') && !bracketed {
            return Err(MetroError::Usage(format!(
                "invalid feed URL {input:?}: IPv6 hosts must be enclosed in brackets"
            )));
        }

        Ok(Self {
            scheme,
            userinfo,
            host,
            port,
            path,
            query,
        })
    }

    /// Rebuild the origin (`scheme://host[:port]`) with a lowercased host.
    ///
    /// User info is omitted on purpose: it is not part of a site URL.
    fn origin(&self) -> String {
        let mut origin = format!(
            "{}://{}",
            self.scheme.to_ascii_lowercase(),
            self.host_for_url()
        );
        if let Some(port) = self.port {
            origin.push(':');
            origin.push_str(port);
        }
        origin
    }

    /// The host with ASCII casing folded to lowercase.
    fn host_lowercase(&self) -> String {
        self.host.to_ascii_lowercase()
    }

    /// The lowercased host formatted for inclusion in a URL.
    ///
    /// IPv6 literals are wrapped back in brackets; because
    /// [`ParsedUrl::parse`] stores them without brackets, their presence is
    /// detected by the embedded colons.
    fn host_for_url(&self) -> String {
        let host = self.host_lowercase();
        if self.host.contains(':') {
            format!("[{host}]")
        } else {
            host
        }
    }
}

/// Split an authority's `host[:port]` portion into host and port.
///
/// Supports bracketed IPv6 literals. Rejects empty or non-numeric ports and
/// unbracketed IPv6 literals so malformed URLs are reported early.
fn split_host_port(host_port: &str) -> Result<(&str, Option<&str>), MetroError> {
    if let Some(stripped) = host_port.strip_prefix('[') {
        let Some((host, rest)) = stripped.split_once(']') else {
            return Err(MetroError::Usage(format!(
                "invalid IPv6 host in URL: {host_port:?} is missing a closing bracket"
            )));
        };
        let port = match rest {
            "" => None,
            other => match other.strip_prefix(':') {
                Some(port) if is_valid_port(port) => Some(port),
                _ => {
                    return Err(MetroError::Usage(format!(
                        "invalid port in URL authority {host_port:?}"
                    )));
                }
            },
        };
        return Ok((host, port));
    }

    match host_port.rsplit_once(':') {
        Some((host, port)) if is_valid_port(port) => Ok((host, Some(port))),
        Some(_) => Err(MetroError::Usage(format!(
            "invalid port in URL authority {host_port:?}"
        ))),
        None => Ok((host_port, None)),
    }
}

/// Return `true` when `port` is a non-empty run of ASCII digits.
fn is_valid_port(port: &str) -> bool {
    !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
}

/// Return `true` when a query parameter pair is a tracking parameter.
///
/// The key is compared case-insensitively against [`TRACKING_PARAM_KEYS`] and
/// the `utm_` prefix.
fn is_tracking_param(pair: &str) -> bool {
    let key = pair.split_once('=').map_or(pair, |(key, _)| key).trim();
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_") || TRACKING_PARAM_KEYS.contains(&key.as_str())
}

/// Rebuild a query string without tracking parameters, preserving order.
///
/// Empty segments (`?`, `a=1&&b=2`) and parameters that become empty after
/// filtering are dropped. An empty result means the URL has no query.
fn normalized_query(query: &str) -> String {
    query
        .split('&')
        .filter(|pair| !pair.is_empty() && !is_tracking_param(pair))
        .collect::<Vec<_>>()
        .join("&")
}

/// Normalize a feed URL for storage and duplicate detection.
///
/// The normalization rules are documented at the module level. The returned
/// string is what [`add_feed`] stores, so comparing stored URLs to
/// [`normalize_url`] output is sufficient for deduplication.
///
/// # Errors
///
/// Returns [`MetroError::Usage`] when the input is empty, is not an absolute
/// `http(s)` URL, has no host, or has a malformed port.
#[must_use = "normalization errors must be handled before using the URL"]
pub fn normalize_url(raw: &str) -> Result<String, MetroError> {
    let parsed = ParsedUrl::parse(raw)?;

    let mut normalized = format!("{}://", parsed.scheme.to_ascii_lowercase());
    if let Some(userinfo) = parsed.userinfo {
        normalized.push_str(userinfo);
        normalized.push('@');
    }
    normalized.push_str(&parsed.host_for_url());
    if let Some(port) = parsed.port {
        normalized.push(':');
        normalized.push_str(port);
    }
    normalized.push_str(parsed.path.trim_end_matches('/'));

    let query = parsed.query.map(normalized_query).unwrap_or_default();
    if !query.is_empty() {
        normalized.push('?');
        normalized.push_str(&query);
    }

    Ok(normalized)
}

/// Convert a non-negative SQLite row id into a [`FeedId`].
fn feed_id_from_row_id(row_id: i64) -> Result<FeedId, MetroError> {
    u64::try_from(row_id)
        .map(FeedId::new)
        .map_err(|_| MetroError::Generic(format!("feed row id {row_id} is out of range")))
}

/// Convert a non-negative SQLite row id into a [`FolderId`].
fn folder_id_from_row_id(row_id: i64) -> Result<FolderId, MetroError> {
    u64::try_from(row_id)
        .map(FolderId::new)
        .map_err(|_| MetroError::Generic(format!("folder row id {row_id} is out of range")))
}

/// Convert an optional SQLite timestamp into an optional Unix timestamp.
///
/// Negative timestamps are rejected as database corruption because the model
/// represents timestamps as `u64`.
fn timestamp_from_row(value: Option<i64>) -> Result<Option<u64>, MetroError> {
    value
        .map(|timestamp| {
            u64::try_from(timestamp).map_err(|_| {
                MetroError::Generic(format!("negative timestamp {timestamp} stored in database"))
            })
        })
        .transpose()
}

/// Parse a stored feed-kind string, reporting corruption with context.
fn feed_kind_from_text(kind_text: &str) -> Result<FeedKind, MetroError> {
    FeedKind::from_str(kind_text).map_err(|_| {
        MetroError::Generic(format!(
            "invalid feed kind {kind_text:?} stored in database"
        ))
    })
}

/// Raw feed columns as stored in SQLite, before conversion to [`Feed`].
struct StoredFeed {
    id: i64,
    url: String,
    title: String,
    site_url: String,
    kind: String,
    folder_id: i64,
    etag: Option<String>,
    last_modified: Option<String>,
    last_fetch: Option<i64>,
    error_state: Option<String>,
}

impl StoredFeed {
    /// Read the ten feed columns from a query row, in selector order.
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            url: row.get(1)?,
            title: row.get(2)?,
            site_url: row.get(3)?,
            kind: row.get(4)?,
            folder_id: row.get(5)?,
            etag: row.get(6)?,
            last_modified: row.get(7)?,
            last_fetch: row.get(8)?,
            error_state: row.get(9)?,
        })
    }

    /// Validate stored values and build a [`Feed`].
    fn into_feed(self) -> Result<Feed, MetroError> {
        Ok(Feed {
            id: feed_id_from_row_id(self.id)?,
            url: self.url,
            title: self.title,
            site_url: self.site_url,
            kind: feed_kind_from_text(&self.kind)?,
            folder_id: folder_id_from_row_id(self.folder_id)?,
            etag: self.etag,
            last_modified: self.last_modified,
            last_fetch: timestamp_from_row(self.last_fetch)?,
            error_state: self.error_state,
        })
    }
}

/// Verify that a folder exists and is a feed folder.
///
/// Runs inside the caller's transaction so the check cannot race with a
/// concurrent folder deletion.
///
/// # Errors
///
/// - [`MetroError::NotFound`] if no folder has the given ID.
/// - [`MetroError::Usage`] if the folder belongs to the saved-items
///   namespace.
/// - [`MetroError::Generic`] if the stored folder kind is invalid.
fn ensure_feed_folder(tx: &Transaction<'_>, folder_id: FolderId) -> Result<(), MetroError> {
    let kind_text: Option<String> = tx
        .query_row(
            "SELECT kind FROM folders WHERE id = ?1",
            params![folder_id.inner()],
            |row| row.get(0),
        )
        .optional()?;

    let kind_text =
        kind_text.ok_or_else(|| MetroError::NotFound(format!("folder {folder_id} not found")))?;
    let kind = FolderKind::from_str(&kind_text).map_err(|_| {
        MetroError::Generic(format!(
            "invalid folder kind {kind_text:?} stored in database"
        ))
    })?;

    if kind != FolderKind::Feeds {
        return Err(MetroError::Usage(format!(
            "folder {folder_id} is a {kind_text} folder; feeds must belong to a feeds folder"
        )));
    }

    Ok(())
}

/// Add a feed subscription, or report the existing one when the URL is a
/// duplicate.
///
/// The URL is normalized with [`normalize_url`] before it is compared and
/// stored. A duplicate returns the existing feed's ID with
/// [`AddFeedOutcome::created`] set to `false`; it never modifies the existing
/// subscription.
///
/// When `folder_id` is `None` the feed is assigned to the idempotent
/// `Unsorted` feed folder. When a folder is supplied it must exist and be of
/// kind [`FolderKind::Feeds`].
///
/// `title` is trimmed. An explicitly empty title is rejected so the CLI does
/// not silently store a blank display name; callers that have no title pass
/// `None` and receive the URL host as provisional title.
///
/// # Errors
///
/// - [`MetroError::Usage`] for an invalid or empty URL, an explicitly empty
///   title, or a folder that is not a feed folder.
/// - [`MetroError::NotFound`] when a supplied folder does not exist.
/// - [`MetroError::Generic`] if the database operation fails.
pub fn add_feed(
    conn: &mut Connection,
    url: &str,
    title: Option<&str>,
    folder_id: Option<FolderId>,
) -> Result<AddFeedOutcome, MetroError> {
    let normalized = normalize_url(url)?;
    let parsed = ParsedUrl::parse(&normalized)?;

    let title = match title.map(str::trim) {
        Some("") => {
            return Err(MetroError::Usage(
                "feed title must not be empty".to_string(),
            ));
        }
        Some(title) => title.to_string(),
        None => parsed.host_lowercase(),
    };
    let site_url = parsed.origin();

    // Creating the default folder runs in its own transaction (folder
    // creation is idempotent), so resolve it before taking the write lock for
    // the subscription.
    let target_folder = match folder_id {
        Some(folder_id) => folder_id,
        None => ensure_default(conn)?,
    };

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM feeds WHERE url = ?1",
            params![normalized.as_str()],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(row_id) = existing_id {
        let id = feed_id_from_row_id(row_id)?;
        // No rows were written; release the write lock explicitly.
        tx.rollback()?;
        return Ok(AddFeedOutcome { id, created: false });
    }

    ensure_feed_folder(&tx, target_folder)?;

    tx.execute(
        "INSERT INTO feeds (url, title, site_url, kind, folder_id) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            normalized.as_str(),
            title.as_str(),
            site_url.as_str(),
            FeedKind::Rss.as_str(),
            target_folder.inner()
        ],
    )?;

    let id = feed_id_from_row_id(tx.last_insert_rowid())?;
    tx.commit()?;

    Ok(AddFeedOutcome { id, created: true })
}

/// SQL selector shared by [`list_feeds`] queries.
const FEED_COLUMNS: &str = "f.id, f.url, f.title, f.site_url, f.kind, f.folder_id, \
                            f.etag, f.last_modified, f.last_fetch, f.error_state";

/// SQL sort order shared by [`list_feeds`] queries.
///
/// Feeds are grouped by folder (case-insensitive folder name) and sorted by
/// title, with the stored casing and row id as deterministic tie-breakers.
const FEED_ORDER: &str = "ORDER BY lower(fo.name), fo.name, lower(f.title), f.title, f.id";

/// List feeds, optionally restricted to one folder.
///
/// The result is sorted by folder name and then by feed title, which
/// preserves the spec's "grouped by folder" ordering while keeping the return
/// type a flat `Vec<Feed>` for the CLI to group. Passing `Some(folder_id)`
/// returns only that folder's feeds (sorted by title); an unknown folder
/// yields an empty list rather than an error, matching the other read APIs.
///
/// # Errors
///
/// - [`MetroError::Generic`] if the database query fails or a stored row is
///   invalid (unknown feed kind, negative id, or negative timestamp).
pub fn list_feeds(conn: &Connection, folder_id: Option<FolderId>) -> Result<Vec<Feed>, MetroError> {
    if let Some(folder_id) = folder_id {
        let sql = format!(
            "SELECT {FEED_COLUMNS} FROM feeds AS f \
             JOIN folders AS fo ON fo.id = f.folder_id \
             WHERE f.folder_id = ?1 {FEED_ORDER}"
        );
        let folder_row_id = folder_id.inner();
        query_feeds(conn, &sql, &[&folder_row_id])
    } else {
        let sql = format!(
            "SELECT {FEED_COLUMNS} FROM feeds AS f \
             JOIN folders AS fo ON fo.id = f.folder_id {FEED_ORDER}"
        );
        query_feeds(conn, &sql, &[])
    }
}

/// Run a feed selector and convert every row into a [`Feed`].
///
/// `sql` must select exactly the ten [`FEED_COLUMNS`]. Keeping the execution
/// and row conversion in one helper keeps the filtered and unfiltered
/// [`list_feeds`] queries in sync.
fn query_feeds(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Feed>, MetroError> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map(params, StoredFeed::from_row)?;

    let mut feeds = Vec::new();
    for row in rows {
        feeds.push(row?.into_feed()?);
    }
    Ok(feeds)
}

/// Remove a feed and, by cascade, all of its entries.
///
/// # Errors
///
/// - [`MetroError::NotFound`] if no feed exists with the given ID.
/// - [`MetroError::Generic`] if the database delete fails.
pub fn remove_feed(conn: &mut Connection, id: FeedId) -> Result<(), MetroError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let deleted = tx.execute("DELETE FROM feeds WHERE id = ?1", params![id.inner()])?;
    if deleted == 0 {
        return Err(MetroError::NotFound(format!("feed {id} not found")));
    }

    tx.commit()?;
    Ok(())
}

/// Move a feed to another feed folder.
///
/// The target folder must exist and be of kind [`FolderKind::Feeds`]; moving
/// a feed into the saved-items namespace is rejected.
///
/// # Errors
///
/// - [`MetroError::NotFound`] if the feed or the target folder does not exist.
/// - [`MetroError::Usage`] if the target folder is not a feed folder.
/// - [`MetroError::Generic`] if the database update fails or a stored row is
///   invalid.
pub fn move_feed(conn: &mut Connection, id: FeedId, folder_id: FolderId) -> Result<(), MetroError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM feeds WHERE id = ?1",
            params![id.inner()],
            |row| row.get(0),
        )
        .optional()?;
    if existing_id.is_none() {
        return Err(MetroError::NotFound(format!("feed {id} not found")));
    }

    ensure_feed_folder(&tx, folder_id)?;

    tx.execute(
        "UPDATE feeds SET folder_id = ?1 WHERE id = ?2",
        params![folder_id.inner(), id.inner()],
    )?;
    tx.commit()?;
    Ok(())
}

/// Rename a feed.
///
/// The title is trimmed and must not be empty. Renaming to the current title
/// succeeds as a no-op.
///
/// # Errors
///
/// - [`MetroError::Usage`] if the trimmed title is empty.
/// - [`MetroError::NotFound`] if no feed exists with the given ID.
/// - [`MetroError::Generic`] if the database update fails.
pub fn rename_feed(conn: &mut Connection, id: FeedId, title: &str) -> Result<(), MetroError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(MetroError::Usage(
            "feed title must not be empty".to_string(),
        ));
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM feeds WHERE id = ?1",
            params![id.inner()],
            |row| row.get(0),
        )
        .optional()?;
    if existing_id.is_none() {
        return Err(MetroError::NotFound(format!("feed {id} not found")));
    }

    tx.execute(
        "UPDATE feeds SET title = ?1 WHERE id = ?2",
        params![title, id.inner()],
    )?;
    tx.commit()?;
    Ok(())
}

/// Get detailed metadata for one feed.
///
/// Includes the containing folder's display name and the number of stored
/// entries, which `metro feeds info` reports.
///
/// # Errors
///
/// - [`MetroError::NotFound`] if no feed exists with the given ID.
/// - [`MetroError::Generic`] if the database query fails or a stored row is
///   invalid (unknown feed kind, negative id, or negative timestamp/count).
pub fn get_feed_info(conn: &Connection, id: FeedId) -> Result<FeedInfo, MetroError> {
    let row = conn
        .query_row(
            "SELECT f.id, f.url, f.title, f.site_url, f.kind, f.folder_id, fo.name, \
                    f.last_fetch, f.error_state, \
                    (SELECT COUNT(*) FROM entries AS e WHERE e.feed_id = f.id) \
             FROM feeds AS f JOIN folders AS fo ON fo.id = f.folder_id \
             WHERE f.id = ?1",
            params![id.inner()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            },
        )
        .optional()?;

    let Some((
        feed_row_id,
        url,
        title,
        site_url,
        kind_text,
        folder_row_id,
        folder_name,
        last_fetch,
        error_state,
        entry_count,
    )) = row
    else {
        return Err(MetroError::NotFound(format!("feed {id} not found")));
    };

    Ok(FeedInfo {
        id: feed_id_from_row_id(feed_row_id)?,
        url,
        title,
        site_url,
        kind: feed_kind_from_text(&kind_text)?,
        folder_id: folder_id_from_row_id(folder_row_id)?,
        folder_name,
        last_fetch: timestamp_from_row(last_fetch)?,
        error_state,
        entry_count: u64::try_from(entry_count).map_err(|_| {
            MetroError::Generic(format!(
                "negative entry count {entry_count} stored in database"
            ))
        })?,
    })
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use crate::folders::{create_folder, list_folders};

    /// Open a migrated in-memory database for feed tests.
    fn test_conn() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    /// Insert an entry row for a feed using direct SQL.
    fn insert_entry_row(conn: &Connection, feed_id: FeedId, guid: &str) {
        conn.execute(
            "INSERT INTO entries (feed_id, guid, url, title) VALUES (?1, ?2, ?3, ?4)",
            params![
                feed_id.inner(),
                guid,
                format!("https://example.com/entries/{guid}"),
                format!("Entry {guid}")
            ],
        )
        .unwrap();
    }

    /// Count rows in a table, used to verify deletes.
    fn row_count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[test]
    fn test_normalize_url_lowercases_scheme_and_host() {
        assert_eq!(
            normalize_url("HTTPS://Example.COM/Feed.xml").unwrap(),
            "https://example.com/Feed.xml"
        );
    }

    #[test]
    fn test_normalize_url_strips_fragment_and_trailing_slashes() {
        assert_eq!(
            normalize_url("https://example.com/#section").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            normalize_url("https://example.com/feed///").unwrap(),
            "https://example.com/feed"
        );
    }

    #[test]
    fn test_normalize_url_drops_tracking_params() {
        assert_eq!(
            normalize_url(
                "https://example.com/feed.xml?utm_source=rss&format=rss&fbclid=abc&utm_medium=email"
            )
            .unwrap(),
            "https://example.com/feed.xml?format=rss"
        );
    }

    #[test]
    fn test_normalize_url_preserves_remaining_query_order() {
        assert_eq!(
            normalize_url("https://example.com/feed?b=2&a=1&gclid=x").unwrap(),
            "https://example.com/feed?b=2&a=1"
        );
    }

    #[test]
    fn test_normalize_url_drops_empty_query() {
        assert_eq!(
            normalize_url("https://example.com/feed.xml?utm_source=rss").unwrap(),
            "https://example.com/feed.xml"
        );
        assert_eq!(
            normalize_url("https://example.com/feed.xml?").unwrap(),
            "https://example.com/feed.xml"
        );
    }

    #[test]
    fn test_normalize_url_preserves_port_and_userinfo() {
        assert_eq!(
            normalize_url("https://Example.com:8443/feed/").unwrap(),
            "https://example.com:8443/feed"
        );
        assert_eq!(
            normalize_url("https://User:Pass@Example.com/feed").unwrap(),
            "https://User:Pass@example.com/feed"
        );
    }

    #[test]
    fn test_normalize_url_bracketed_ipv6() {
        assert_eq!(
            normalize_url("http://[2001:DB8::1]:8080/feed/").unwrap(),
            "http://[2001:db8::1]:8080/feed"
        );
    }

    #[test]
    fn test_normalize_url_rejects_invalid_input() {
        assert!(matches!(normalize_url(""), Err(MetroError::Usage(_))));
        assert!(matches!(normalize_url("   "), Err(MetroError::Usage(_))));
        assert!(matches!(
            normalize_url("example.com/feed"),
            Err(MetroError::Usage(_))
        ));
        assert!(matches!(
            normalize_url("ftp://example.com/feed"),
            Err(MetroError::Usage(_))
        ));
        assert!(matches!(
            normalize_url("https://"),
            Err(MetroError::Usage(_))
        ));
        assert!(matches!(
            normalize_url("https://example.com:abc/feed"),
            Err(MetroError::Usage(_))
        ));
        assert!(matches!(
            normalize_url("https://2001:db8::1/feed"),
            Err(MetroError::Usage(_))
        ));
    }

    #[test]
    fn test_add_feed_returns_new_id_and_default_folder() {
        let mut conn = test_conn();

        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();
        assert!(outcome.created);
        assert!(outcome.is_new());

        let feeds = list_feeds(&conn, None).unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].id(), outcome.id);
        assert_eq!(feeds[0].url(), "https://example.com/feed.xml");
        assert_eq!(feeds[0].title(), "example.com");
        assert_eq!(feeds[0].site_url(), "https://example.com");
        assert_eq!(feeds[0].kind(), FeedKind::Rss);

        let folders = list_folders(&conn, FolderKind::Feeds).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name(), "Unsorted");
        assert_eq!(feeds[0].folder_id(), folders[0].id());
    }

    #[test]
    fn test_add_feed_with_title_and_explicit_folder() {
        let mut conn = test_conn();
        let folder_id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();

        let outcome = add_feed(
            &mut conn,
            "https://example.com/feed.xml",
            Some("  My Blog  "),
            Some(folder_id),
        )
        .unwrap();

        let feeds = list_feeds(&conn, Some(folder_id)).unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].id(), outcome.id);
        assert_eq!(feeds[0].title(), "My Blog");
        assert_eq!(feeds[0].folder_id(), folder_id);
    }

    #[test]
    fn test_add_feed_rejects_explicitly_empty_title() {
        let mut conn = test_conn();

        assert!(matches!(
            add_feed(&mut conn, "https://example.com/feed.xml", Some("   "), None),
            Err(MetroError::Usage(_))
        ));
        assert_eq!(row_count(&conn, "feeds"), 0);
    }

    #[test]
    fn test_add_feed_duplicate_returns_existing_id() {
        let mut conn = test_conn();

        let first = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();
        let second = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();

        assert_eq!(first.id, second.id);
        assert!(!second.created);
        assert!(!second.is_new());
        assert_eq!(row_count(&conn, "feeds"), 1);
    }

    #[test]
    fn test_add_feed_duplicate_detection_normalizes_url() {
        let mut conn = test_conn();

        let first = add_feed(
            &mut conn,
            "https://Example.com/feed.xml?utm_source=rss#top",
            None,
            None,
        )
        .unwrap();
        let second = add_feed(&mut conn, "https://example.com/feed.xml/", None, None).unwrap();

        assert_eq!(first.id, second.id);
        assert!(!second.created);
        assert_eq!(row_count(&conn, "feeds"), 1);
        assert_eq!(
            list_feeds(&conn, None).unwrap()[0].url(),
            "https://example.com/feed.xml"
        );
    }

    #[test]
    fn test_add_feed_to_nonexistent_folder_not_found() {
        let mut conn = test_conn();

        assert!(matches!(
            add_feed(
                &mut conn,
                "https://example.com/feed.xml",
                None,
                Some(FolderId::new(999))
            ),
            Err(MetroError::NotFound(_))
        ));
        assert_eq!(row_count(&conn, "feeds"), 0);
    }

    #[test]
    fn test_add_feed_to_saved_folder_rejected() {
        let mut conn = test_conn();
        let saved_id = create_folder(&mut conn, "Saved", FolderKind::Saved).unwrap();

        assert!(matches!(
            add_feed(
                &mut conn,
                "https://example.com/feed.xml",
                None,
                Some(saved_id)
            ),
            Err(MetroError::Usage(_))
        ));
        assert_eq!(row_count(&conn, "feeds"), 0);
    }

    #[test]
    fn test_add_feed_invalid_url_usage_error() {
        let mut conn = test_conn();

        assert!(matches!(
            add_feed(&mut conn, "not a url", None, None),
            Err(MetroError::Usage(_))
        ));
        assert_eq!(row_count(&conn, "feeds"), 0);
    }

    #[test]
    fn test_list_feeds_empty() {
        let conn = test_conn();
        assert!(list_feeds(&conn, None).unwrap().is_empty());
    }

    #[test]
    fn test_list_feeds_sorted_by_folder_then_title() {
        let mut conn = test_conn();
        let zeta_id = create_folder(&mut conn, "Zeta", FolderKind::Feeds).unwrap();
        let alpha_id = create_folder(&mut conn, "Alpha", FolderKind::Feeds).unwrap();

        add_feed(
            &mut conn,
            "https://example.com/zulu.xml",
            Some("Zulu"),
            Some(zeta_id),
        )
        .unwrap();
        add_feed(
            &mut conn,
            "https://example.com/alpha.xml",
            Some("alpha"),
            Some(alpha_id),
        )
        .unwrap();
        add_feed(
            &mut conn,
            "https://example.com/zulu2.xml",
            Some("Alpha"),
            Some(alpha_id),
        )
        .unwrap();

        let feeds = list_feeds(&conn, None).unwrap();
        let titles: Vec<&str> = feeds.iter().map(Feed::title).collect();
        assert_eq!(titles, vec!["Alpha", "alpha", "Zulu"]);
        assert_eq!(feeds[0].folder_id(), alpha_id);
        assert_eq!(feeds[2].folder_id(), zeta_id);
    }

    #[test]
    fn test_list_feeds_filters_by_folder() {
        let mut conn = test_conn();
        let first = create_folder(&mut conn, "First", FolderKind::Feeds).unwrap();
        let second = create_folder(&mut conn, "Second", FolderKind::Feeds).unwrap();

        add_feed(&mut conn, "https://example.com/one.xml", None, Some(first)).unwrap();
        add_feed(&mut conn, "https://example.com/two.xml", None, Some(second)).unwrap();

        let feeds = list_feeds(&conn, Some(first)).unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].folder_id(), first);

        // Unknown folder filters yield an empty list.
        assert!(
            list_feeds(&conn, Some(FolderId::new(999)))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_list_feeds_maps_optional_columns() {
        let mut conn = test_conn();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();
        conn.execute(
            "UPDATE feeds SET etag = ?1, last_modified = ?2, last_fetch = ?3, error_state = ?4 \
             WHERE id = ?5",
            params![
                "\"abc\"",
                "Wed, 21 Oct 2015 07:28:00 GMT",
                1_445_414_880_i64,
                "timeout",
                outcome.id.inner()
            ],
        )
        .unwrap();

        let feeds = list_feeds(&conn, None).unwrap();
        let feed = &feeds[0];
        assert_eq!(feed.etag(), Some("\"abc\""));
        assert_eq!(feed.last_modified(), Some("Wed, 21 Oct 2015 07:28:00 GMT"));
        assert_eq!(feed.last_fetch(), Some(1_445_414_880));
        assert_eq!(feed.error_state(), Some("timeout"));
    }

    #[test]
    fn test_remove_feed_deletes_feed_and_entries() {
        let mut conn = test_conn();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();
        insert_entry_row(&conn, outcome.id, "guid-1");
        insert_entry_row(&conn, outcome.id, "guid-2");

        remove_feed(&mut conn, outcome.id).unwrap();

        assert_eq!(row_count(&conn, "feeds"), 0);
        assert_eq!(row_count(&conn, "entries"), 0);
    }

    #[test]
    fn test_remove_feed_nonexistent_not_found() {
        let mut conn = test_conn();
        assert!(matches!(
            remove_feed(&mut conn, FeedId::new(999)),
            Err(MetroError::NotFound(_))
        ));
    }

    #[test]
    fn test_move_feed_to_another_folder() {
        let mut conn = test_conn();
        let target = create_folder(&mut conn, "Target", FolderKind::Feeds).unwrap();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();

        move_feed(&mut conn, outcome.id, target).unwrap();

        let feeds = list_feeds(&conn, Some(target)).unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].id(), outcome.id);
    }

    #[test]
    fn test_move_feed_missing_feed_or_folder_not_found() {
        let mut conn = test_conn();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();
        let target = create_folder(&mut conn, "Target", FolderKind::Feeds).unwrap();

        assert!(matches!(
            move_feed(&mut conn, FeedId::new(999), target),
            Err(MetroError::NotFound(_))
        ));
        assert!(matches!(
            move_feed(&mut conn, outcome.id, FolderId::new(999)),
            Err(MetroError::NotFound(_))
        ));
    }

    #[test]
    fn test_move_feed_to_saved_folder_rejected() {
        let mut conn = test_conn();
        let saved_id = create_folder(&mut conn, "Saved", FolderKind::Saved).unwrap();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();

        assert!(matches!(
            move_feed(&mut conn, outcome.id, saved_id),
            Err(MetroError::Usage(_))
        ));
    }

    #[test]
    fn test_rename_feed_updates_title() {
        let mut conn = test_conn();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();

        rename_feed(&mut conn, outcome.id, "  New Title  ").unwrap();

        assert_eq!(list_feeds(&conn, None).unwrap()[0].title(), "New Title");
    }

    #[test]
    fn test_rename_feed_nonexistent_not_found() {
        let mut conn = test_conn();
        assert!(matches!(
            rename_feed(&mut conn, FeedId::new(999), "Title"),
            Err(MetroError::NotFound(_))
        ));
    }

    #[test]
    fn test_rename_feed_empty_title_rejected() {
        let mut conn = test_conn();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();

        assert!(matches!(
            rename_feed(&mut conn, outcome.id, "   "),
            Err(MetroError::Usage(_))
        ));
        assert_eq!(list_feeds(&conn, None).unwrap()[0].title(), "example.com");
    }

    #[test]
    fn test_get_feed_info_returns_complete_metadata() {
        let mut conn = test_conn();
        let folder_id = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        let outcome = add_feed(
            &mut conn,
            "https://example.com/feed.xml",
            Some("My Blog"),
            Some(folder_id),
        )
        .unwrap();
        insert_entry_row(&conn, outcome.id, "guid-1");
        insert_entry_row(&conn, outcome.id, "guid-2");
        conn.execute(
            "UPDATE feeds SET last_fetch = ?1, error_state = ?2 WHERE id = ?3",
            params![1_445_414_880_i64, "timeout", outcome.id.inner()],
        )
        .unwrap();

        let info = get_feed_info(&conn, outcome.id).unwrap();
        assert_eq!(info.id, outcome.id);
        assert_eq!(info.url, "https://example.com/feed.xml");
        assert_eq!(info.title, "My Blog");
        assert_eq!(info.site_url, "https://example.com");
        assert_eq!(info.kind, FeedKind::Rss);
        assert_eq!(info.folder_id, folder_id);
        assert_eq!(info.folder_name, "Tech");
        assert_eq!(info.last_fetch, Some(1_445_414_880));
        assert_eq!(info.error_state.as_deref(), Some("timeout"));
        assert_eq!(info.entry_count, 2);
    }

    #[test]
    fn test_get_feed_info_defaults_for_unfetched_feed() {
        let mut conn = test_conn();
        let outcome = add_feed(&mut conn, "https://example.com/feed.xml", None, None).unwrap();

        let info = get_feed_info(&conn, outcome.id).unwrap();
        assert_eq!(info.folder_name, "Unsorted");
        assert_eq!(info.last_fetch, None);
        assert_eq!(info.error_state, None);
        assert_eq!(info.entry_count, 0);
    }

    #[test]
    fn test_get_feed_info_nonexistent_not_found() {
        let conn = test_conn();
        assert!(matches!(
            get_feed_info(&conn, FeedId::new(999)),
            Err(MetroError::NotFound(_))
        ));
    }
}
