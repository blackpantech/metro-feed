# Spec Delta

## Purpose

Applies exponential backoff to failing feeds, skipping them during refresh until the retry interval elapses.

## ADDED Requirements

### Requirement: Track consecutive failures
The system SHALL maintain a `consecutive_failures` counter and a `next_retry` timestamp on each feed. A failed fetch SHALL increment the counter; a successful fetch (200 or 304) SHALL reset the counter to 0 and clear `next_retry`.

#### Scenario: Counter increments on failure
- **WHEN** a feed fetch fails
- **THEN** `consecutive_failures` is incremented by 1

#### Scenario: Counter resets on success
- **WHEN** a feed succeeds after previous failures
- **THEN** `consecutive_failures` is reset to 0 and `next_retry` is NULL

### Requirement: Backoff schedule
The system SHALL set `next_retry` to the failure time plus a delay chosen by the incremented `consecutive_failures` value: 1 → 1 minute, 2 → 5 minutes, 3 → 30 minutes, 4 → 2 hours, 5 → 12 hours, 6 or more → 24 hours (maximum).

#### Scenario: First failure sets 1-minute backoff
- **WHEN** a feed fails once (`consecutive_failures = 1`)
- **THEN** `next_retry` is set to 1 minute after the failure time

#### Scenario: Third failure sets 30-minute backoff
- **WHEN** a feed fails for the third consecutive time
- **THEN** `next_retry` is set to 30 minutes after the failure time

#### Scenario: Sixth failure caps at 24 hours
- **WHEN** a feed has failed 6 or more times (`consecutive_failures >= 6`)
- **THEN** `next_retry` is set to 24 hours after the latest failure

### Requirement: Skip feeds in backoff
The system SHALL NOT fetch a feed whose `next_retry` is later than the current time, even when the feed is targeted explicitly. Skipped feeds SHALL be reported with their `next_retry` and SHALL NOT count as failures. A feed whose `next_retry` is NULL, past, or equal to the current time is due.

#### Scenario: Feed in backoff is skipped
- **WHEN** `refresh_feeds` runs and a feed's `next_retry` is in the future
- **THEN** the feed is not fetched and is reported as skipped with its `next_retry`

#### Scenario: Explicitly targeted feed in backoff is skipped
- **WHEN** `metro refresh f12` is run while f12's `next_retry` is in the future
- **THEN** f12 is not fetched and the command exits with code 0

#### Scenario: Feed past backoff is fetched
- **WHEN** `refresh_feeds` runs and a feed's `next_retry` is in the past
- **THEN** the feed is fetched normally

### Requirement: Never auto-delete failing feeds
The system SHALL never automatically delete a feed due to repeated failures.

#### Scenario: Feeds persist despite failures
- **WHEN** a feed fails 10 consecutive times
- **THEN** the feed remains in the database and can be manually removed
