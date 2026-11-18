//! Parser tests over the shared feed fixtures in `<repo>/test-fixtures/`
//! (task 14.2 of the `m1-feeds-core` change, feed-fetching spec "Parse
//! response with feed-rs").
//!
//! Each fixture is loaded through [`metro_core::test_utils::load_fixture`] and
//! parsed with the public [`metro_core::http::client::parse_feed`], exactly as the
//! fetcher does after a `200` response. No network is involved.
//!
//! This target declares `required-features = ["test-utils"]` in
//! `crates/metro-core/Cargo.toml`, so it only builds with
//! `--features test-utils`.
//!
//! The expected values mirror the fixture files; the comment at the top of
//! each fixture lists what it covers. Timestamps are Unix seconds and were
//! cross-checked with `date -u -d <rfc3339> +%s`.

use metro_core::MetroError;
use metro_core::models::FeedKind;
use metro_core::parse::{FeedMetadata, FetchedEntry, parse_feed};
use metro_core::test_utils::load_fixture;

/// Load and parse a fixture, resolving relative links against `base_uri`.
fn parse_fixture(name: &str, base_uri: &str) -> (FeedMetadata, Vec<FetchedEntry>) {
    let bytes = load_fixture(name).unwrap();
    parse_feed(&bytes, Some(base_uri)).unwrap()
}

#[test]
fn rss2_fixture_parses() {
    let (feed, entries) = parse_fixture("rss2.xml", "https://rss.example.com/feed.xml");

    assert_eq!(feed.kind, FeedKind::Rss);
    assert_eq!(feed.title.as_deref(), Some("Metro Fixture RSS"));
    assert_eq!(feed.site_url.as_deref(), Some("https://rss.example.com/"));
    assert_eq!(entries.len(), 3);

    let first = &entries[0];
    assert_eq!(first.guid, "rss-1");
    assert_eq!(first.title.as_deref(), Some("First RSS item"));
    assert_eq!(
        first.url.as_deref(),
        Some("https://rss.example.com/posts/1")
    );
    // "email (Name)" author convention yields the bare name.
    assert_eq!(first.author.as_deref(), Some("Jane Doe"));
    // Thu, 01 Jan 2026 08:30:00 GMT
    assert_eq!(first.published, Some(1_767_256_200));
    assert_eq!(first.summary.as_deref(), Some("Summary of the first item"));
    assert_eq!(
        first.content.as_deref(),
        Some("<p>Full content of the first item</p>")
    );

    let second = &entries[1];
    assert_eq!(second.guid, "https://rss.example.com/posts/2");
    // CDATA title.
    assert_eq!(second.title.as_deref(), Some("Second RSS item"));
    // Relative <link> resolved against the base URI.
    assert_eq!(
        second.url.as_deref(),
        Some("https://rss.example.com/posts/2")
    );
    assert_eq!(second.author, None);
    // Fri, 02 Jan 2026 12:00:00 +0100 == 11:00:00Z
    assert_eq!(second.published, Some(1_767_351_600));

    let third = &entries[2];
    assert_eq!(third.guid, "rss-3");
    // Entity-escaped title is decoded.
    assert_eq!(third.title.as_deref(), Some("Third & undated"));
    // No pubDate: the entry has no date (pruning falls back to first_seen).
    assert_eq!(third.published, None);
}

#[test]
fn atom_fixture_parses() {
    let (feed, entries) = parse_fixture("atom.xml", "https://atom.example.com/atom.xml");

    assert_eq!(feed.kind, FeedKind::Atom);
    assert_eq!(feed.title.as_deref(), Some("Metro Fixture Atom"));
    // The alternate link wins over rel="self".
    assert_eq!(feed.site_url.as_deref(), Some("https://atom.example.com/"));
    assert_eq!(entries.len(), 2);

    let first = &entries[0];
    assert_eq!(first.guid, "urn:uuid:atom-entry-1");
    assert_eq!(first.title.as_deref(), Some("First Atom entry"));
    assert_eq!(
        first.url.as_deref(),
        Some("https://atom.example.com/entries/1")
    );
    assert_eq!(first.author.as_deref(), Some("John Roe"));
    // <published> 2026-02-01T10:00:00Z takes precedence over <updated>.
    assert_eq!(first.published, Some(1_769_940_000));
    assert_eq!(first.summary.as_deref(), Some("Summary of the first entry"));
    assert_eq!(
        first.content.as_deref(),
        Some("<p>Full content of the first entry</p>")
    );

    let second = &entries[1];
    assert_eq!(second.guid, "urn:uuid:atom-entry-2");
    assert_eq!(second.title.as_deref(), Some("Second Atom entry"));
    // Relative href resolved against the base URI.
    assert_eq!(
        second.url.as_deref(),
        Some("https://atom.example.com/entries/2")
    );
    assert_eq!(second.author, None);
    // Only <updated> 2026-02-03T00:00:00+02:00 (== 2026-02-02T22:00:00Z).
    assert_eq!(second.published, Some(1_770_069_600));
    assert_eq!(second.content, None);
}

#[test]
fn json_feed_fixture_parses() {
    let (feed, entries) = parse_fixture("feed.json", "https://json.example.com/feed.json");

    assert_eq!(feed.kind, FeedKind::Json);
    assert_eq!(feed.title.as_deref(), Some("Metro Fixture JSON"));
    assert_eq!(feed.site_url.as_deref(), Some("https://json.example.com/"));
    assert_eq!(entries.len(), 2);

    let first = &entries[0];
    assert_eq!(first.guid, "json-1");
    assert_eq!(first.title.as_deref(), Some("First JSON item"));
    assert_eq!(
        first.url.as_deref(),
        Some("https://json.example.com/posts/1")
    );
    assert_eq!(first.author.as_deref(), Some("Ada Lovelace"));
    // date_published 2026-03-01T09:15:00Z
    assert_eq!(first.published, Some(1_772_356_500));
    assert_eq!(first.summary.as_deref(), Some("Summary of the first item"));
    assert_eq!(
        first.content.as_deref(),
        Some("<p>Full content of the first item</p>")
    );

    let second = &entries[1];
    assert_eq!(second.guid, "json-2");
    // Untitled JSON Feed item.
    assert_eq!(second.title, None);
    assert_eq!(
        second.url.as_deref(),
        Some("https://json.example.com/posts/2")
    );
    // Only date_modified 2026-03-02T00:00:00Z: the updated-date fallback.
    assert_eq!(second.published, Some(1_772_409_600));
    assert_eq!(second.content.as_deref(), Some("An untitled note"));
}

#[test]
fn non_feed_fixture_is_a_parse_error() {
    let bytes = load_fixture("not-a-feed.html").unwrap();
    let error = parse_feed(&bytes, None).unwrap_err();
    // Unparsable bodies map to the generic error (exit code 1), not Network.
    assert!(matches!(error, MetroError::Generic(_)), "got {error:?}");
}
