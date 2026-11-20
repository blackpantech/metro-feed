//! Retry backoff schedule for failing feeds.
//!
//! A feed whose fetch fails is not retried on every refresh: the refresh
//! writer stores a `next_retry` deadline computed by [`next_retry_at`] and
//! the refresh pipeline skips the feed until that deadline has passed
//! (`backoff` spec). The delay depends only on the
//! feed's `consecutive_failures` counter **after** it was incremented for
//! the current failure, and grows along [`BACKOFF_SCHEDULE`]:
//!
//! | `consecutive_failures` | delay before the next attempt |
//! |---|---|
//! | 0 | none (the feed is healthy) |
//! | 1 | 1 minute |
//! | 2 | 5 minutes |
//! | 3 | 30 minutes |
//! | 4 | 2 hours |
//! | 5 | 12 hours |
//! | 6 or more | 24 hours (cap) |
//!
//! The delay is relative to the failure time, so a stale deadline left by a
//! long offline period resolves naturally on the next run (design risk
//! table). A failing feed is never deleted automatically; only the counter
//! and the deadline change, and a success resets both. These functions are
//! pure (no clock, no database) so the schedule is unit-testable in
//! isolation.

use std::time::Duration;

/// Retry delays applied after the 1st, 2nd, … consecutive failure.
///
/// Index `n - 1` holds the delay after the `n`-th consecutive failure; any
/// failure beyond the last element reuses the last element, which is the
/// 24-hour cap of the `backoff` spec. See the module documentation for the
/// full table.
pub const BACKOFF_SCHEDULE: [Duration; 6] = [
    Duration::from_mins(1),
    Duration::from_mins(5),
    Duration::from_mins(30),
    Duration::from_hours(2),
    Duration::from_hours(12),
    Duration::from_hours(24),
];

/// Delay before a feed with `consecutive_failures` failures may be retried.
///
/// Returns `None` for `0` (a healthy feed is never delayed). Otherwise
/// returns the matching [`BACKOFF_SCHEDULE`] step, capped at its last element
/// (24 hours) for six or more failures.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use metro_core::feed_sync::backoff::backoff_delay;
///
/// assert_eq!(backoff_delay(0), None);
/// assert_eq!(backoff_delay(1), Some(Duration::from_secs(60)));
/// assert_eq!(backoff_delay(42), Some(Duration::from_hours(24)));
/// ```
#[must_use]
pub fn backoff_delay(consecutive_failures: u32) -> Option<Duration> {
    let step = usize::try_from(consecutive_failures.checked_sub(1)?).unwrap_or(usize::MAX);
    BACKOFF_SCHEDULE
        .get(step)
        .or_else(|| BACKOFF_SCHEDULE.last())
        .copied()
}

/// Unix timestamp (seconds) at which a failing feed may be fetched again.
///
/// `consecutive_failures` is the counter **including** the failure that
/// happened at `failed_at` (Unix seconds). The result is
/// `failed_at + backoff_delay(consecutive_failures)`, saturating at
/// [`i64::MAX`] instead of overflowing. Returns `None` when no delay applies
/// (`consecutive_failures == 0`), which the refresh writer stores as
/// `next_retry = NULL` ("due on the next refresh").
///
/// # Examples
///
/// ```
/// use metro_core::feed_sync::backoff::next_retry_at;
///
/// assert_eq!(next_retry_at(1, 1_000), Some(1_060));
/// assert_eq!(next_retry_at(0, 1_000), None);
/// ```
#[must_use]
pub fn next_retry_at(consecutive_failures: u32, failed_at: i64) -> Option<i64> {
    let delay = backoff_delay(consecutive_failures)?;
    let delay_seconds = i64::try_from(delay.as_secs()).unwrap_or(i64::MAX);
    Some(failed_at.saturating_add(delay_seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;

    #[test]
    fn test_backoff_delay_zero_failures_has_no_delay() {
        assert_eq!(backoff_delay(0), None);
        assert_eq!(next_retry_at(0, 1_000), None);
    }

    #[test]
    fn test_backoff_delay_follows_schedule() {
        let expected = [
            (1, MINUTE),
            (2, 5 * MINUTE),
            (3, 30 * MINUTE),
            (4, 2 * HOUR),
            (5, 12 * HOUR),
            (6, 24 * HOUR),
        ];
        for (failures, seconds) in expected {
            assert_eq!(
                backoff_delay(failures).map(|delay| delay.as_secs()),
                Some(u64::try_from(seconds).unwrap()),
                "failures = {failures}"
            );
        }
    }

    #[test]
    fn test_backoff_delay_caps_at_24_hours() {
        let cap = Some(Duration::from_hours(24));
        for failures in [6, 7, 10, 100, u32::MAX] {
            assert_eq!(backoff_delay(failures), cap, "failures = {failures}");
        }
    }

    #[test]
    fn test_backoff_delay_is_monotonic() {
        let delays: Vec<Duration> = (1..=8).filter_map(backoff_delay).collect();
        assert_eq!(delays.len(), 8);
        assert!(delays.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn test_next_retry_first_failure_is_one_minute_later() {
        assert_eq!(
            next_retry_at(1, 1_700_000_000),
            Some(1_700_000_000 + MINUTE)
        );
    }

    #[test]
    fn test_next_retry_sixth_failure_is_24_hours_later() {
        assert_eq!(
            next_retry_at(6, 1_700_000_000),
            Some(1_700_000_000 + 24 * HOUR)
        );
        assert_eq!(
            next_retry_at(50, 1_700_000_000),
            Some(1_700_000_000 + 24 * HOUR)
        );
    }

    #[test]
    fn test_next_retry_saturates_instead_of_overflowing() {
        assert_eq!(next_retry_at(6, i64::MAX - 10), Some(i64::MAX));
    }
}
