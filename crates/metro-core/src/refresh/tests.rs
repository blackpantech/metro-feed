//! Unit tests for the refresh worker pool and writer (mock fetcher, in-memory
//! database).

use super::*;

use std::time::Duration;

use rusqlite::params;

use crate::feeds::{add_feed, move_feed, rename_feed};
use crate::folders::create_folder;
use crate::models::{FeedKind, FolderKind};
use crate::refresh::test_support::{
    MockFetcher, entry, entry_count, modified, not_modified, row_count, test_conn,
};

/// Subscribe `count` feeds with provisional metadata; returns their IDs.
fn add_feeds(conn: &mut Connection, count: usize) -> Vec<FeedId> {
    (0..count)
        .map(|index| {
            add_feed(
                conn,
                &format!("https://feed{index}.example.com/rss"),
                None,
                None,
            )
            .unwrap()
            .id
        })
        .collect()
}

/// Run a refresh of every feed with default options.
fn refresh_all<F: FeedFetcher>(conn: &mut Connection, fetcher: &F) -> RefreshReport {
    refresh_feeds(
        conn,
        fetcher,
        RefreshTarget::All,
        &RefreshOptions::default(),
    )
    .unwrap()
}

/// Feed columns written by the refresh writer.
#[derive(Debug)]
struct FeedState {
    title: String,
    site_url: String,
    kind: String,
    etag: Option<String>,
    last_modified: Option<String>,
    last_fetch: Option<i64>,
    error_state: Option<String>,
    consecutive_failures: Option<i64>,
    next_retry: Option<i64>,
}

fn feed_state(conn: &Connection, id: FeedId) -> FeedState {
    conn.query_row(
        "SELECT title, site_url, kind, etag, last_modified, last_fetch, error_state, \
                consecutive_failures, next_retry FROM feeds WHERE id = ?1",
        params![id.inner()],
        |row| {
            Ok(FeedState {
                title: row.get(0)?,
                site_url: row.get(1)?,
                kind: row.get(2)?,
                etag: row.get(3)?,
                last_modified: row.get(4)?,
                last_fetch: row.get(5)?,
                error_state: row.get(6)?,
                consecutive_failures: row.get(7)?,
                next_retry: row.get(8)?,
            })
        },
    )
    .unwrap()
}

fn set_column(conn: &Connection, id: FeedId, column: &str, value: Option<i64>) {
    conn.execute(
        &format!("UPDATE feeds SET {column} = ?1 WHERE id = ?2"),
        params![value, id.inner()],
    )
    .unwrap();
}

/// A fetcher answering every feed with an RSS document containing one entry
/// whose guid is unique per feed URL.
fn per_feed_entry_fetcher()
-> MockFetcher<impl Fn(&str, Option<&str>, Option<&str>) -> Result<FetchResult, MetroError> + Sync>
{
    MockFetcher::new(|url, _, _| {
        Ok(modified(
            FeedKind::Rss,
            Some("Feed"),
            None,
            vec![entry(&format!("{url}#1"), Some(&format!("{url}/1")))],
        ))
    })
}

// ---------------------------------------------------------------------------
// 8.1 Worker pool, parallelism, dedup, progress, last_fetch
// ---------------------------------------------------------------------------

#[test]
fn test_default_concurrency_is_eight() {
    let mut conn = test_conn();
    add_feeds(&mut conn, 12);
    let fetcher = per_feed_entry_fetcher();

    let report = refresh_all(&mut conn, &fetcher);

    assert_eq!(DEFAULT_CONCURRENCY, 8);
    assert_eq!(report.workers, 8);
    assert_eq!(report.results.len(), 12);
}

#[test]
fn test_custom_concurrency_bounds_parallel_fetches() {
    let mut conn = test_conn();
    add_feeds(&mut conn, 12);
    let fetcher = per_feed_entry_fetcher().with_delay(Duration::from_millis(40));

    let report = refresh_feeds(
        &mut conn,
        &fetcher,
        RefreshTarget::All,
        &RefreshOptions {
            concurrency: Some(4),
            progress: None,
        },
    )
    .unwrap();

    assert_eq!(report.workers, 4);
    assert!(fetcher.max_in_flight() <= 4, "pool exceeded its size");
    assert!(fetcher.max_in_flight() > 1, "fetches did not overlap");
    assert_eq!(fetcher.calls().len(), 12);
}

#[test]
fn test_pool_is_capped_by_number_of_feeds() {
    let mut conn = test_conn();
    add_feeds(&mut conn, 3);
    let fetcher = per_feed_entry_fetcher();

    let report = refresh_all(&mut conn, &fetcher);

    assert_eq!(report.workers, 3);
}

#[test]
fn test_zero_concurrency_is_usage_error() {
    let mut conn = test_conn();
    add_feeds(&mut conn, 1);
    let fetcher = per_feed_entry_fetcher();

    let error = refresh_feeds(
        &mut conn,
        &fetcher,
        RefreshTarget::All,
        &RefreshOptions {
            concurrency: Some(0),
            progress: None,
        },
    )
    .unwrap_err();

    assert!(matches!(error, MetroError::Usage(_)));
    assert!(fetcher.calls().is_empty());
}

#[test]
fn test_no_feeds_is_a_successful_noop() {
    let mut conn = test_conn();
    let fetcher = per_feed_entry_fetcher();

    let report = refresh_all(&mut conn, &fetcher);

    assert_eq!(report.workers, 0);
    assert!(report.results.is_empty());
    assert!(report.exit_status().is_ok());
}

#[test]
fn test_fifty_feeds_are_all_fetched_and_stored() {
    let mut conn = test_conn();
    let ids = add_feeds(&mut conn, 50);
    let fetcher = per_feed_entry_fetcher();

    let report = refresh_all(&mut conn, &fetcher);

    assert_eq!(report.workers, 8);
    assert_eq!(fetcher.calls().len(), 50);
    assert_eq!(report.succeeded(), 50);
    assert_eq!(report.new_entries(), 50);
    assert_eq!(row_count(&conn, "entries"), 50);
    // Results are sorted by feed ID.
    let result_ids: Vec<FeedId> = report.results.iter().map(|r| r.feed_id).collect();
    assert_eq!(result_ids, ids);
}

#[test]
fn test_cached_validators_are_sent() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    conn.execute(
        "UPDATE feeds SET etag = '\"v1\"', last_modified = 'Thu, 01 Jan 2026 00:00:00 GMT' \
         WHERE id = ?1",
        params![id.inner()],
    )
    .unwrap();
    let fetcher = MockFetcher::new(|_, _, _| Ok(not_modified()));

    refresh_all(&mut conn, &fetcher);

    let calls = fetcher.calls();
    assert_eq!(calls[0].url, "https://feed0.example.com/rss");
    assert_eq!(calls[0].etag.as_deref(), Some("\"v1\""));
    assert_eq!(
        calls[0].last_modified.as_deref(),
        Some("Thu, 01 Jan 2026 00:00:00 GMT")
    );
}

#[test]
fn test_duplicate_guid_is_skipped() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    let fetcher = MockFetcher::new(|_, _, _| {
        Ok(modified(
            FeedKind::Rss,
            None,
            None,
            vec![
                entry("guid-1", Some("https://example.com/1")),
                entry("guid-2", Some("https://example.com/2")),
            ],
        ))
    });

    let first = refresh_all(&mut conn, &fetcher);
    let second = refresh_all(&mut conn, &fetcher);

    assert_eq!(first.new_entries(), 2);
    assert_eq!(second.new_entries(), 0);
    assert!(matches!(
        second.results[0].outcome,
        FeedOutcome::Updated { new_entries: 0 }
    ));
    assert_eq!(entry_count(&conn, id), 2);
}

#[test]
fn test_duplicate_url_with_different_guid_is_skipped() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    let fetcher = MockFetcher::new(|_, _, _| {
        Ok(modified(
            FeedKind::Rss,
            None,
            None,
            vec![
                entry("guid-1", Some("https://example.com/post")),
                entry("guid-2", Some("https://example.com/post")),
            ],
        ))
    });

    let report = refresh_all(&mut conn, &fetcher);

    assert_eq!(report.new_entries(), 1);
    assert_eq!(entry_count(&conn, id), 1);
}

#[test]
fn test_entries_without_url_are_not_deduplicated_by_url() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    let fetcher = MockFetcher::new(|_, _, _| {
        Ok(modified(
            FeedKind::Rss,
            None,
            None,
            vec![entry("guid-1", None), entry("guid-2", None)],
        ))
    });

    let report = refresh_all(&mut conn, &fetcher);

    assert_eq!(report.new_entries(), 2);
    assert_eq!(entry_count(&conn, id), 2);
}

#[test]
fn test_guidless_entries_fall_back_to_url_then_content_hash() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    let fetcher = MockFetcher::new(|_, _, _| {
        let mut hashed = entry("", None);
        hashed.title = Some("Only a title".to_string());
        Ok(modified(
            FeedKind::Rss,
            None,
            None,
            vec![entry("", Some("https://example.com/a")), hashed],
        ))
    });

    let first = refresh_all(&mut conn, &fetcher);
    let second = refresh_all(&mut conn, &fetcher);

    assert_eq!(first.new_entries(), 2);
    assert_eq!(second.new_entries(), 0);
    let keys: Vec<String> = conn
        .prepare("SELECT guid FROM entries ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(keys[0], "https://example.com/a");
    assert!(keys[1].starts_with("hash:"));
    assert_eq!(entry_count(&conn, id), 2);
}

#[test]
fn test_new_entries_are_unread_with_first_seen() {
    let mut conn = test_conn();
    add_feeds(&mut conn, 1);
    let before = now_unix();
    let fetcher = per_feed_entry_fetcher();

    refresh_all(&mut conn, &fetcher);

    let (is_read, first_seen, published): (i64, i64, Option<i64>) = conn
        .query_row(
            "SELECT is_read, first_seen, published FROM entries",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(is_read, 0);
    assert!(first_seen >= before);
    assert_eq!(published, Some(1_767_225_600));
}

#[test]
fn test_success_sets_last_fetch_and_validators() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    let before = now_unix();
    let fetcher = MockFetcher::new(|_, _, _| {
        let mut result = modified(FeedKind::Rss, None, None, Vec::new());
        result.etag = Some("\"v2\"".to_string());
        result.last_modified = Some("Sat, 03 Jan 2026 00:00:00 GMT".to_string());
        Ok(result)
    });

    refresh_all(&mut conn, &fetcher);

    let state = feed_state(&conn, id);
    assert!(state.last_fetch.unwrap() >= before);
    assert_eq!(state.etag.as_deref(), Some("\"v2\""));
    assert_eq!(
        state.last_modified.as_deref(),
        Some("Sat, 03 Jan 2026 00:00:00 GMT")
    );
}

#[test]
fn test_progress_events_are_sent() {
    let mut conn = test_conn();
    add_feeds(&mut conn, 5);
    let fetcher = per_feed_entry_fetcher();
    let (sender, receiver) = mpsc::channel();

    refresh_feeds(
        &mut conn,
        &fetcher,
        RefreshTarget::All,
        &RefreshOptions {
            concurrency: Some(2),
            progress: Some(&sender),
        },
    )
    .unwrap();
    drop(sender);
    let events: Vec<RefreshEvent> = receiver.iter().collect();

    assert_eq!(
        events[0],
        RefreshEvent::Started {
            total: 5,
            skipped: 0,
            workers: 2
        }
    );
    assert_eq!(events.len(), 6);
    for (index, event) in events[1..].iter().enumerate() {
        let RefreshEvent::FeedFinished {
            completed,
            total,
            status,
            ..
        } = *event
        else {
            panic!("unexpected event {event:?}");
        };
        assert_eq!(completed, index + 1);
        assert_eq!(total, 5);
        assert_eq!(status, FeedStatus::Updated { new_entries: 1 });
    }
}

#[test]
fn test_dropped_progress_receiver_does_not_fail_refresh() {
    let mut conn = test_conn();
    add_feeds(&mut conn, 3);
    let fetcher = per_feed_entry_fetcher();
    let (sender, receiver) = mpsc::channel();
    drop(receiver);

    let report = refresh_feeds(
        &mut conn,
        &fetcher,
        RefreshTarget::All,
        &RefreshOptions {
            concurrency: None,
            progress: Some(&sender),
        },
    )
    .unwrap();

    assert_eq!(report.succeeded(), 3);
}

#[test]
fn test_failure_records_error_and_keeps_last_fetch() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    set_column(&conn, id, "last_fetch", Some(1_000));
    let fetcher =
        MockFetcher::new(|_, _, _| Err(MetroError::Network("connection refused".to_string())));

    let report = refresh_all(&mut conn, &fetcher);

    let state = feed_state(&conn, id);
    assert_eq!(
        state.last_fetch,
        Some(1_000),
        "failure must not touch last_fetch"
    );
    assert!(state.error_state.unwrap().contains("connection refused"));
    assert_eq!(state.consecutive_failures, Some(1));
    let failures: Vec<(FeedId, &MetroError)> = report.failures().collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].0, id);
    assert!(matches!(failures[0].1, MetroError::Network(_)));
}

#[test]
fn test_repeated_failures_increment_and_success_resets() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    let failing = MockFetcher::new(|_, _, _| Err(MetroError::Network("down".to_string())));

    refresh_all(&mut conn, &failing);
    refresh_all(&mut conn, &failing);
    assert_eq!(feed_state(&conn, id).consecutive_failures, Some(2));

    set_column(&conn, id, "next_retry", Some(now_unix() - 1));
    refresh_all(&mut conn, &per_feed_entry_fetcher());

    let state = feed_state(&conn, id);
    assert_eq!(state.consecutive_failures, Some(0));
    assert_eq!(state.next_retry, None);
    assert_eq!(state.error_state, None);
    assert!(state.last_fetch.is_some());
}

#[test]
fn test_partial_failure_continues_and_reports() {
    let mut conn = test_conn();
    let ids = add_feeds(&mut conn, 10);
    let fetcher = MockFetcher::new(|url, _, _| {
        if url.contains("feed3.") || url.contains("feed5.") || url.contains("feed7.") {
            Err(MetroError::Network(format!("{url} is down")))
        } else {
            Ok(modified(
                FeedKind::Rss,
                None,
                None,
                vec![entry(url, Some(&format!("{url}/1")))],
            ))
        }
    });

    let report = refresh_all(&mut conn, &fetcher);

    assert_eq!(fetcher.calls().len(), 10);
    assert_eq!(report.succeeded(), 7);
    let failed: Vec<FeedId> = report.failures().map(|(id, _)| id).collect();
    assert_eq!(failed, vec![ids[3], ids[5], ids[7]]);
    let error = report.exit_status().unwrap_err();
    assert!(matches!(error, MetroError::PartialSuccess(_)));
    assert_eq!(error.exit_code(), 5);
}

#[test]
fn test_exit_status_all_ok_and_all_failed() {
    let mut conn = test_conn();
    add_feeds(&mut conn, 2);

    let ok = refresh_all(&mut conn, &per_feed_entry_fetcher());
    assert!(ok.exit_status().is_ok());

    let failing = MockFetcher::new(|_, _, _| Err(MetroError::Network("down".to_string())));
    let failed = refresh_all(&mut conn, &failing);
    let error = failed.exit_status().unwrap_err();
    assert!(matches!(error, MetroError::Generic(_)));
    assert_eq!(error.exit_code(), 1);
}

#[test]
fn test_panicking_fetcher_is_isolated() {
    let mut conn = test_conn();
    add_feeds(&mut conn, 4);
    let fetcher = MockFetcher::new(|url, _, _| {
        assert!(!url.contains("feed1."), "boom");
        Ok(not_modified())
    });

    let report = refresh_all(&mut conn, &fetcher);

    assert_eq!(report.results.len(), 4);
    assert_eq!(report.succeeded(), 3);
    let (_, error) = report.failures().next().unwrap();
    assert!(error.to_string().contains("panicked"));
}

#[test]
fn test_target_specific_feeds_and_folder() {
    let mut conn = test_conn();
    let ids = add_feeds(&mut conn, 3);
    let folder = create_folder(&mut conn, "Tech", FolderKind::Feeds).unwrap();
    move_feed(&mut conn, ids[2], folder).unwrap();
    let fetcher = per_feed_entry_fetcher();

    let picked = refresh_feeds(
        &mut conn,
        &fetcher,
        RefreshTarget::Feeds(&[ids[1], ids[0], ids[1]]),
        &RefreshOptions::default(),
    )
    .unwrap();
    let picked_ids: Vec<FeedId> = picked.results.iter().map(|r| r.feed_id).collect();
    assert_eq!(picked_ids, vec![ids[0], ids[1]]);

    let in_folder = refresh_feeds(
        &mut conn,
        &fetcher,
        RefreshTarget::Folder(folder),
        &RefreshOptions::default(),
    )
    .unwrap();
    let folder_ids: Vec<FeedId> = in_folder.results.iter().map(|r| r.feed_id).collect();
    assert_eq!(folder_ids, vec![ids[2]]);
}

#[test]
fn test_unknown_target_feed_is_not_found() {
    let mut conn = test_conn();
    add_feeds(&mut conn, 1);
    let fetcher = per_feed_entry_fetcher();

    let error = refresh_feeds(
        &mut conn,
        &fetcher,
        RefreshTarget::Feeds(&[FeedId::new(999)]),
        &RefreshOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(error, MetroError::NotFound(_)));
    assert!(fetcher.calls().is_empty());
}

// ---------------------------------------------------------------------------
// 8.2 Feed metadata persistence
// ---------------------------------------------------------------------------

#[test]
fn test_kind_and_site_url_follow_the_feed() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    assert_eq!(feed_state(&conn, id).kind, "rss");
    let fetcher = MockFetcher::new(|_, _, _| {
        Ok(modified(
            FeedKind::Atom,
            None,
            Some("https://atom.example.com/"),
            Vec::new(),
        ))
    });

    refresh_all(&mut conn, &fetcher);

    let state = feed_state(&conn, id);
    assert_eq!(state.kind, "atom");
    assert_eq!(state.site_url, "https://atom.example.com/");
}

#[test]
fn test_kind_updates_on_every_200() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];

    refresh_all(
        &mut conn,
        &MockFetcher::new(|_, _, _| Ok(modified(FeedKind::Atom, None, None, Vec::new()))),
    );
    refresh_all(
        &mut conn,
        &MockFetcher::new(|_, _, _| Ok(modified(FeedKind::Json, None, None, Vec::new()))),
    );

    assert_eq!(feed_state(&conn, id).kind, "json");
}

#[test]
fn test_missing_site_url_keeps_stored_value() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    let fetcher = MockFetcher::new(|_, _, _| Ok(modified(FeedKind::Rss, None, None, Vec::new())));

    refresh_all(&mut conn, &fetcher);

    assert_eq!(feed_state(&conn, id).site_url, "https://feed0.example.com");
}

#[test]
fn test_title_filled_on_first_successful_fetch() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    assert_eq!(feed_state(&conn, id).title, "feed0.example.com");
    let fetcher = MockFetcher::new(|_, _, _| {
        Ok(modified(
            FeedKind::Rss,
            Some("Real Title"),
            None,
            Vec::new(),
        ))
    });

    refresh_all(&mut conn, &fetcher);

    assert_eq!(feed_state(&conn, id).title, "Real Title");
}

#[test]
fn test_blank_feed_title_is_not_applied() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    let fetcher =
        MockFetcher::new(|_, _, _| Ok(modified(FeedKind::Rss, Some("   "), None, Vec::new())));

    refresh_all(&mut conn, &fetcher);

    assert_eq!(feed_state(&conn, id).title, "feed0.example.com");
}

#[test]
fn test_custom_title_is_preserved_after_first_fetch() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    let fetcher = MockFetcher::new(|_, _, _| {
        Ok(modified(
            FeedKind::Rss,
            Some("Feed Title"),
            None,
            Vec::new(),
        ))
    });
    refresh_all(&mut conn, &fetcher);
    rename_feed(&mut conn, id, "My Name").unwrap();

    refresh_all(&mut conn, &fetcher);

    assert_eq!(feed_state(&conn, id).title, "My Name");
}

#[test]
fn test_failed_first_fetch_keeps_title_eligible() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    refresh_all(
        &mut conn,
        &MockFetcher::new(|_, _, _| Err(MetroError::Network("down".to_string()))),
    );
    assert_eq!(feed_state(&conn, id).last_fetch, None);

    refresh_all(
        &mut conn,
        &MockFetcher::new(|_, _, _| Ok(modified(FeedKind::Rss, Some("Later"), None, Vec::new()))),
    );

    assert_eq!(feed_state(&conn, id).title, "Later");
}

#[test]
fn test_not_modified_updates_last_fetch_only() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    refresh_all(
        &mut conn,
        &MockFetcher::new(|_, _, _| {
            let mut result = modified(
                FeedKind::Atom,
                Some("Atom Title"),
                Some("https://site.example.com/"),
                Vec::new(),
            );
            result.etag = Some("\"v1\"".to_string());
            Ok(result)
        }),
    );
    set_column(&conn, id, "last_fetch", Some(1_000));

    let report = refresh_all(
        &mut conn,
        &MockFetcher::new(|_, etag, _| {
            let mut result = not_modified();
            // Like the real fetcher, a 304 preserves the cached validators.
            result.etag = etag.map(str::to_string);
            Ok(result)
        }),
    );

    assert!(matches!(
        report.results[0].outcome,
        FeedOutcome::NotModified
    ));
    let state = feed_state(&conn, id);
    assert_eq!(state.kind, "atom");
    assert_eq!(state.title, "Atom Title");
    assert_eq!(state.site_url, "https://site.example.com/");
    assert_eq!(state.etag.as_deref(), Some("\"v1\""));
    assert!(state.last_fetch.unwrap() > 1_000);
}

// ---------------------------------------------------------------------------
// 8.4 Backoff skip
// ---------------------------------------------------------------------------

#[test]
fn test_feed_in_backoff_is_skipped() {
    let mut conn = test_conn();
    let ids = add_feeds(&mut conn, 2);
    let deadline = now_unix() + 3_600;
    set_column(&conn, ids[0], "next_retry", Some(deadline));
    let fetcher = per_feed_entry_fetcher();
    let (sender, receiver) = mpsc::channel();

    let report = refresh_feeds(
        &mut conn,
        &fetcher,
        RefreshTarget::All,
        &RefreshOptions {
            concurrency: None,
            progress: Some(&sender),
        },
    )
    .unwrap();

    let calls = fetcher.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].url, "https://feed1.example.com/rss");
    assert_eq!(
        report.skipped,
        vec![SkippedFeed {
            feed_id: ids[0],
            next_retry: u64::try_from(deadline).unwrap(),
        }]
    );
    assert_eq!(report.results.len(), 1);
    assert_eq!(
        receiver.try_recv().unwrap(),
        RefreshEvent::Started {
            total: 1,
            skipped: 1,
            workers: 1
        }
    );
    // A skipped feed is neither a success nor a failure.
    assert!(report.exit_status().is_ok());
}

#[test]
fn test_explicitly_targeted_feed_in_backoff_is_skipped() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    set_column(&conn, id, "next_retry", Some(now_unix() + 3_600));
    let fetcher = per_feed_entry_fetcher();

    let report = refresh_feeds(
        &mut conn,
        &fetcher,
        RefreshTarget::Feeds(&[id]),
        &RefreshOptions::default(),
    )
    .unwrap();

    assert!(fetcher.calls().is_empty());
    assert_eq!(report.skipped.len(), 1);
}

#[test]
fn test_feed_past_backoff_is_fetched() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    set_column(&conn, id, "next_retry", Some(now_unix() - 60));
    let fetcher = per_feed_entry_fetcher();

    let report = refresh_all(&mut conn, &fetcher);

    assert_eq!(fetcher.calls().len(), 1);
    assert!(report.skipped.is_empty());
    assert_eq!(feed_state(&conn, id).next_retry, None);
}

#[test]
fn test_failing_feed_is_never_deleted() {
    let mut conn = test_conn();
    let id = add_feeds(&mut conn, 1)[0];
    let failing = MockFetcher::new(|_, _, _| Err(MetroError::Network("down".to_string())));

    for _ in 0..10 {
        set_column(&conn, id, "next_retry", None);
        refresh_all(&mut conn, &failing);
    }

    assert_eq!(row_count(&conn, "feeds"), 1);
    assert_eq!(feed_state(&conn, id).consecutive_failures, Some(10));
}
