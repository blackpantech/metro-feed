//! Subscribe-time first fetch (design decision D11).
//!
//! [`subscribe_feed`] is what `metro feeds add` calls after autodiscovery.
//! Unlike the network-free [`crate::feeds::add_feed`], it fetches the feed
//! **before** writing anything, so:
//!
//! - typos and dead URLs fail fast and leave the database untouched;
//! - the stored `kind`, `site_url`, and title are the real ones from the
//!   first response instead of placeholders;
//! - the initial entries, validators (`etag` / `last_modified`), and
//!   `last_fetch` are stored together with the feed row in **one**
//!   transaction, through the same writer path as a refresh
//!   (`writer::apply_fetch_success`). Storing validators without entries
//!   would make the next refresh receive a `304` and never insert them.
//!
//! # Flow
//!
//! 1. Normalize the URL and validate an explicit title (blank is a usage
//!    error).
//! 2. If the normalized URL is already subscribed, return
//!    [`SubscribeOutcome::Duplicate`] **without fetching**.
//! 3. If a folder was given, check it exists and is a feed folder (read-only)
//!    so a bad `--folder` fails before any network access.
//! 4. Fetch unconditionally. A fetch error is returned as is
//!    ([`MetroError::Network`] → exit 4, [`MetroError::Generic`] for a parse
//!    error → exit 1) and nothing is written.
//! 5. Compute the final metadata ([`FeedPreview`]): title = explicit title,
//!    else the feed's declared title, else the URL host; site URL = the
//!    feed's alternate link, else the URL origin; kind = the parsed kind.
//! 6. With `dry_run`, return [`SubscribeOutcome::DryRun`] without writing
//!    (not even the default `Unsorted` folder).
//! 7. Otherwise resolve the folder (default `Unsorted`), then in one
//!    `IMMEDIATE` transaction re-check for a concurrent duplicate, insert the
//!    feed row, and apply the fetch result exactly as a refresh would.

use rusqlite::{Connection, TransactionBehavior};

use crate::MetroError;
use crate::feeds::{
    NewFeed, ensure_feed_folder, find_feed_by_url, insert_feed, normalize_url, url_placeholders,
    validate_explicit_title,
};
use crate::fetch::FetchResult;
use crate::folders::ensure_default;
use crate::models::{FeedId, FeedKind, FolderId};
use crate::refresh::FeedFetcher;
use crate::refresh::writer::{apply_fetch_success, now_unix};

/// Metadata [`subscribe_feed`] stores (or, in dry-run mode, would store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedPreview {
    /// Normalized feed URL.
    pub url: String,
    /// Final display title.
    pub title: String,
    /// Site URL declared by the feed, or the URL origin.
    pub site_url: String,
    /// Feed format inferred from the parsed document.
    pub kind: FeedKind,
    /// Number of entries in the fetched document (before deduplication).
    pub entry_count: usize,
}

/// Result of [`subscribe_feed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscribeOutcome {
    /// The feed was fetched and subscribed.
    Subscribed {
        /// ID of the new feed.
        id: FeedId,
        /// The stored metadata.
        feed: FeedPreview,
        /// Entries inserted (after deduplication against existing entries).
        new_entries: usize,
    },
    /// The normalized URL was already subscribed; nothing was fetched or
    /// written.
    Duplicate {
        /// ID of the existing feed.
        id: FeedId,
    },
    /// Dry run: the feed was fetched and nothing was written.
    DryRun {
        /// The metadata that would be stored.
        feed: FeedPreview,
    },
}

/// Fetch a feed and subscribe to it in one transaction (design D11).
///
/// See the [module documentation](self) for the exact flow. `folder_id`
/// `None` means the default `Unsorted` folder. `title` overrides the feed's
/// declared title.
///
/// # Errors
///
/// - [`MetroError::Usage`] for an invalid URL, a blank explicit title, or a
///   folder that is not a feed folder.
/// - [`MetroError::NotFound`] when the given folder does not exist.
/// - [`MetroError::Network`] when the feed cannot be downloaded (including a
///   `304` answer to the unconditional request).
/// - [`MetroError::Generic`] when the body is not a feed, or a database
///   operation fails.
///
/// On any error, nothing is written.
pub fn subscribe_feed<F: FeedFetcher>(
    conn: &mut Connection,
    fetcher: &F,
    url: &str,
    title: Option<&str>,
    folder_id: Option<FolderId>,
    dry_run: bool,
) -> Result<SubscribeOutcome, MetroError> {
    let normalized = normalize_url(url)?;
    let explicit_title = validate_explicit_title(title)?;

    if let Some(id) = find_feed_by_url(conn, &normalized)? {
        return Ok(SubscribeOutcome::Duplicate { id });
    }
    if let Some(folder_id) = folder_id {
        ensure_feed_folder(conn, folder_id)?;
    }

    let result = fetcher.fetch(&normalized, None, None)?;
    let feed = preview(normalized, explicit_title, &result)?;

    if dry_run {
        return Ok(SubscribeOutcome::DryRun { feed });
    }

    // Default-folder creation runs in its own (idempotent) transaction, so
    // resolve it before taking the write lock, as `add_feed` does.
    let target_folder = match folder_id {
        Some(folder_id) => folder_id,
        None => ensure_default(conn)?,
    };

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(id) = find_feed_by_url(&tx, &feed.url)? {
        // Another process subscribed the same URL while we were fetching.
        tx.rollback()?;
        return Ok(SubscribeOutcome::Duplicate { id });
    }

    let id = insert_feed(
        &tx,
        &NewFeed {
            url: &feed.url,
            title: &feed.title,
            site_url: &feed.site_url,
            kind: feed.kind,
            folder_id: target_folder,
        },
    )?;
    // The final title is already stored, so no title candidate is offered.
    let new_entries = apply_fetch_success(&tx, id, &result, None, now_unix())?;
    tx.commit()?;

    Ok(SubscribeOutcome::Subscribed {
        id,
        feed,
        new_entries,
    })
}

/// Compute the metadata to store from the fetch result.
///
/// # Errors
///
/// Returns [`MetroError::Network`] when the server answered `304` (or a
/// response without feed metadata) to a request that carried no validators,
/// which leaves nothing to subscribe to.
fn preview(
    normalized_url: String,
    explicit_title: Option<&str>,
    result: &FetchResult,
) -> Result<FeedPreview, MetroError> {
    let Some(metadata) = result.feed.as_ref().filter(|_| !result.is_not_modified()) else {
        return Err(MetroError::Network(format!(
            "failed to fetch {normalized_url}: server returned no feed content"
        )));
    };

    let placeholders = url_placeholders(&normalized_url)?;
    let declared_title = metadata
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty());
    let declared_site_url = metadata
        .site_url
        .as_deref()
        .map(str::trim)
        .filter(|site_url| !site_url.is_empty());

    let title = explicit_title
        .or(declared_title)
        .map_or(placeholders.title, str::to_owned);
    let site_url = declared_site_url.map_or(placeholders.site_url, str::to_owned);

    Ok(FeedPreview {
        url: normalized_url,
        title,
        site_url,
        kind: metadata.kind,
        entry_count: result.entries.len(),
    })
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;

    use rusqlite::params;

    use crate::folders::create_folder;
    use crate::models::FolderKind;
    use crate::refresh::test_support::{
        MockFetcher, atom_result, entry, modified, not_modified, row_count, test_conn,
    };

    const ATOM_URL: &str = "https://atom.example.com/atom.xml";

    #[test]
    fn test_subscribe_stores_real_metadata_and_entries() {
        let mut conn = test_conn();
        let fetcher = MockFetcher::new(|_, _, _| Ok(atom_result()));

        let outcome = subscribe_feed(&mut conn, &fetcher, ATOM_URL, None, None, false).unwrap();

        let SubscribeOutcome::Subscribed {
            id,
            feed,
            new_entries,
        } = outcome
        else {
            panic!("expected Subscribed, got {outcome:?}");
        };
        assert_eq!(feed.kind, FeedKind::Atom);
        assert_eq!(feed.title, "Atom Blog");
        assert_eq!(feed.site_url, "https://atom.example.com/");
        assert_eq!(new_entries, 2);

        let (kind, title, site_url, etag, last_modified, last_fetch): (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<i64>,
        ) = conn
            .query_row(
                "SELECT kind, title, site_url, etag, last_modified, last_fetch \
                 FROM feeds WHERE id = ?1",
                params![id.inner()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(kind, "atom");
        assert_eq!(title, "Atom Blog");
        assert_eq!(site_url, "https://atom.example.com/");
        assert_eq!(etag.as_deref(), Some("\"atom-v1\""));
        assert_eq!(
            last_modified.as_deref(),
            Some("Fri, 02 Jan 2026 00:00:00 GMT")
        );
        assert!(last_fetch.is_some());
        assert_eq!(row_count(&conn, "entries"), 2);

        // The fetch was unconditional and used the normalized URL.
        let calls = fetcher.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].url, ATOM_URL);
        assert_eq!(calls[0].etag, None);
        assert_eq!(calls[0].last_modified, None);
    }

    #[test]
    fn test_subscribe_explicit_title_wins() {
        let mut conn = test_conn();
        let fetcher = MockFetcher::new(|_, _, _| Ok(atom_result()));

        let outcome = subscribe_feed(
            &mut conn,
            &fetcher,
            ATOM_URL,
            Some(" My Blog "),
            None,
            false,
        )
        .unwrap();

        let SubscribeOutcome::Subscribed { id, feed, .. } = outcome else {
            panic!("expected Subscribed, got {outcome:?}");
        };
        assert_eq!(feed.title, "My Blog");
        let title: String = conn
            .query_row(
                "SELECT title FROM feeds WHERE id = ?1",
                params![id.inner()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, "My Blog");
    }

    #[test]
    fn test_subscribe_falls_back_to_placeholders() {
        let mut conn = test_conn();
        let fetcher = MockFetcher::new(|_, _, _| {
            Ok(modified(FeedKind::Rss, None, None, vec![entry("g1", None)]))
        });

        let outcome = subscribe_feed(
            &mut conn,
            &fetcher,
            "https://Blog.Example.com/feed.xml",
            None,
            None,
            false,
        )
        .unwrap();

        let SubscribeOutcome::Subscribed { feed, .. } = outcome else {
            panic!("expected Subscribed, got {outcome:?}");
        };
        assert_eq!(feed.url, "https://blog.example.com/feed.xml");
        assert_eq!(feed.title, "blog.example.com");
        assert_eq!(feed.site_url, "https://blog.example.com");
        assert_eq!(feed.kind, FeedKind::Rss);
    }

    #[test]
    fn test_subscribe_uses_given_folder() {
        let mut conn = test_conn();
        let folder = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
        let fetcher = MockFetcher::new(|_, _, _| Ok(atom_result()));

        let outcome =
            subscribe_feed(&mut conn, &fetcher, ATOM_URL, None, Some(folder), false).unwrap();

        let SubscribeOutcome::Subscribed { id, .. } = outcome else {
            panic!("expected Subscribed, got {outcome:?}");
        };
        let folder_id: i64 = conn
            .query_row(
                "SELECT folder_id FROM feeds WHERE id = ?1",
                params![id.inner()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(u64::try_from(folder_id).unwrap(), folder.inner());
    }

    #[test]
    fn test_subscribe_network_failure_writes_nothing() {
        let mut conn = test_conn();
        let fetcher =
            MockFetcher::new(|_, _, _| Err(MetroError::Network("connection refused".to_string())));

        let error = subscribe_feed(&mut conn, &fetcher, ATOM_URL, None, None, false).unwrap_err();

        assert!(matches!(error, MetroError::Network(_)));
        assert_eq!(row_count(&conn, "feeds"), 0);
        assert_eq!(row_count(&conn, "entries"), 0);
        // Not even the default folder is created.
        assert_eq!(row_count(&conn, "folders"), 0);
    }

    #[test]
    fn test_subscribe_parse_failure_writes_nothing() {
        let mut conn = test_conn();
        let fetcher = MockFetcher::new(|_, _, _| {
            Err(MetroError::Generic("failed to parse feed".to_string()))
        });

        let error = subscribe_feed(&mut conn, &fetcher, ATOM_URL, None, None, false).unwrap_err();

        assert!(matches!(error, MetroError::Generic(_)));
        assert_eq!(error.exit_code(), 1);
        assert_eq!(row_count(&conn, "feeds"), 0);
        assert_eq!(row_count(&conn, "folders"), 0);
    }

    #[test]
    fn test_subscribe_unexpected_not_modified_is_network_error() {
        let mut conn = test_conn();
        let fetcher = MockFetcher::new(|_, _, _| Ok(not_modified()));

        let error = subscribe_feed(&mut conn, &fetcher, ATOM_URL, None, None, false).unwrap_err();

        assert!(matches!(error, MetroError::Network(_)));
        assert_eq!(row_count(&conn, "feeds"), 0);
    }

    #[test]
    fn test_subscribe_duplicate_is_returned_without_fetching() {
        let mut conn = test_conn();
        let fetcher = MockFetcher::new(|_, _, _| Ok(atom_result()));
        let first = subscribe_feed(&mut conn, &fetcher, ATOM_URL, None, None, false).unwrap();
        let SubscribeOutcome::Subscribed { id, .. } = first else {
            panic!("expected Subscribed, got {first:?}");
        };

        let second = subscribe_feed(
            &mut conn,
            &fetcher,
            "https://ATOM.example.com/atom.xml?utm_source=x",
            None,
            None,
            false,
        )
        .unwrap();

        assert_eq!(second, SubscribeOutcome::Duplicate { id });
        assert_eq!(fetcher.calls().len(), 1, "duplicate must not be fetched");
        assert_eq!(row_count(&conn, "feeds"), 1);
    }

    #[test]
    fn test_subscribe_dry_run_writes_nothing() {
        let mut conn = test_conn();
        let fetcher = MockFetcher::new(|_, _, _| Ok(atom_result()));

        let outcome = subscribe_feed(&mut conn, &fetcher, ATOM_URL, None, None, true).unwrap();

        let SubscribeOutcome::DryRun { feed } = outcome else {
            panic!("expected DryRun, got {outcome:?}");
        };
        assert_eq!(feed.kind, FeedKind::Atom);
        assert_eq!(feed.title, "Atom Blog");
        assert_eq!(feed.entry_count, 2);
        assert_eq!(row_count(&conn, "feeds"), 0);
        assert_eq!(row_count(&conn, "entries"), 0);
        assert_eq!(row_count(&conn, "folders"), 0);
    }

    #[test]
    fn test_subscribe_unknown_folder_fails_before_fetching() {
        let mut conn = test_conn();
        let fetcher = MockFetcher::new(|_, _, _| Ok(atom_result()));

        let error = subscribe_feed(
            &mut conn,
            &fetcher,
            ATOM_URL,
            None,
            Some(FolderId::new(999)),
            false,
        )
        .unwrap_err();

        assert!(matches!(error, MetroError::NotFound(_)));
        assert!(fetcher.calls().is_empty());
    }

    #[test]
    fn test_subscribe_rejects_blank_title_and_bad_url() {
        let mut conn = test_conn();
        let fetcher = MockFetcher::new(|_, _, _| Ok(atom_result()));

        let blank = subscribe_feed(&mut conn, &fetcher, ATOM_URL, Some("  "), None, false);
        assert!(matches!(blank, Err(MetroError::Usage(_))));
        let bad_url = subscribe_feed(&mut conn, &fetcher, "ftp://x", None, None, false);
        assert!(matches!(bad_url, Err(MetroError::Usage(_))));
        assert!(fetcher.calls().is_empty());
    }
}
