//! Entry identity keys for refresh deduplication.
//!
//! The refresh spec deduplicates entries in priority order **guid → url →
//! content hash**, and every tier is scoped **per feed**: two feeds that
//! publish the same guid or URL each keep their own entry. This module turns
//! a [`FetchedEntry`] into the single key stored in the `entries.guid`
//! column, whose `UNIQUE(feed_id, guid)` constraint (design decision D2) then
//! lets `INSERT OR IGNORE` skip duplicates within a feed atomically:
//!
//! 1. the entry's own guid, trimmed, when it is not blank;
//! 2. otherwise the entry's URL, trimmed, when it is not blank;
//! 3. otherwise `hash:<16 hex digits>`, a content hash over the title,
//!    author, published date, summary, and content.
//!
//! In practice tier 1 almost always applies, because `feed-rs` synthesizes a
//! stable id for entries that omit one. Tiers 2 and 3 are the safety net for
//! feeds (or future parsers) that yield blank ids.
//!
//! The URL tier is **also** enforced independently of the key by the writer
//! (an entry whose URL is already stored for the same feed is skipped even
//! when its guid is new); see `refresh::writer`.
//!
//! # Hash stability
//!
//! The key is persisted, so the hash must be identical across processes,
//! platforms, and Rust releases. `std::hash::DefaultHasher` makes no such
//! promise, so a hand-rolled 64-bit FNV-1a is used instead. It is not
//! cryptographic; collisions only cause a genuinely new, guid-less, URL-less
//! entry of the same feed to be skipped, which is an acceptable failure mode
//! for this tier.

use std::borrow::Cow;

use crate::parse::FetchedEntry;

/// Prefix marking keys produced by the content-hash tier.
const HASH_KEY_PREFIX: &str = "hash:";

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Compute the deduplication key stored in `entries.guid` for an entry.
///
/// Borrows from the entry for the guid and URL tiers so the common case does
/// not allocate. See the module documentation for the tier order.
pub(crate) fn dedup_key(entry: &FetchedEntry) -> Cow<'_, str> {
    if let Some(guid) = non_blank(Some(entry.guid.as_str())) {
        return Cow::Borrowed(guid);
    }
    if let Some(url) = non_blank(entry.url.as_deref()) {
        return Cow::Borrowed(url);
    }
    Cow::Owned(format!("{HASH_KEY_PREFIX}{:016x}", content_hash(entry)))
}

/// The entry URL to store and compare for URL-tier deduplication.
///
/// Returns the trimmed URL, or an empty string when the entry has none. The
/// `entries.url` column is `NOT NULL`, and the writer treats the empty string
/// as "no URL" (it never deduplicates on it).
pub(crate) fn entry_url(entry: &FetchedEntry) -> &str {
    non_blank(entry.url.as_deref()).unwrap_or_default()
}

/// Trim a value and map blank strings to `None`.
fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Hash the content fields of an entry with FNV-1a.
///
/// Each optional field contributes a presence tag and a terminator, so
/// `(title: "ab", summary: None)` and `(title: "a", summary: "b")` hash
/// differently.
fn content_hash(entry: &FetchedEntry) -> u64 {
    let published = entry.published.map(|seconds| seconds.to_string());
    let fields = [
        entry.title.as_deref(),
        entry.author.as_deref(),
        published.as_deref(),
        entry.summary.as_deref(),
        entry.content.as_deref(),
    ];

    fields
        .iter()
        .fold(FNV_OFFSET_BASIS, |hash, field| match field {
            Some(text) => {
                let hash = fnv1a(hash, &[1]);
                let hash = fnv1a(hash, text.as_bytes());
                fnv1a(hash, &[0])
            }
            None => fnv1a(hash, &[2, 0]),
        })
}

/// Fold bytes into an FNV-1a 64-bit hash state.
fn fnv1a(state: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(state, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(guid: &str, url: Option<&str>, title: Option<&str>) -> FetchedEntry {
        FetchedEntry {
            guid: guid.to_string(),
            url: url.map(str::to_string),
            title: title.map(str::to_string),
            author: None,
            published: None,
            summary: None,
            content: None,
        }
    }

    #[test]
    fn test_guid_is_preferred() {
        let entry = entry(" post-1 ", Some("https://example.com/1"), None);
        assert_eq!(dedup_key(&entry), "post-1");
    }

    #[test]
    fn test_url_is_used_when_guid_is_blank() {
        let entry = entry("  ", Some(" https://example.com/1 "), None);
        assert_eq!(dedup_key(&entry), "https://example.com/1");
    }

    #[test]
    fn test_content_hash_is_used_last() {
        let entry = entry("", None, Some("Title"));
        let key = dedup_key(&entry);
        assert!(key.starts_with(HASH_KEY_PREFIX));
        assert_eq!(key.len(), HASH_KEY_PREFIX.len() + 16);
    }

    #[test]
    fn test_content_hash_is_deterministic() {
        let first = entry("", None, Some("Title"));
        let second = entry("", None, Some("Title"));
        assert_eq!(dedup_key(&first), dedup_key(&second));
    }

    #[test]
    fn test_content_hash_is_stable_across_releases() {
        // Pinned value: changing the hash would re-insert every stored
        // guid-less, URL-less entry on the next refresh.
        let entry = entry("", None, Some("Title"));
        assert_eq!(dedup_key(&entry), "hash:56debb41cd07dc3e");
    }

    #[test]
    fn test_content_hash_distinguishes_field_boundaries() {
        let mut first = entry("", None, Some("ab"));
        first.summary = None;
        let mut second = entry("", None, Some("a"));
        second.summary = Some("b".to_string());
        assert_ne!(dedup_key(&first), dedup_key(&second));
    }

    #[test]
    fn test_entry_url_defaults_to_empty() {
        assert_eq!(entry_url(&entry("g", None, None)), "");
        assert_eq!(entry_url(&entry("g", Some("  "), None)), "");
        assert_eq!(
            entry_url(&entry("g", Some(" https://example.com/1 "), None)),
            "https://example.com/1"
        );
    }
}
