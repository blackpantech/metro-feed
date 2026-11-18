# Spec Delta

## Purpose

Removes feed entries older than a configurable window based on published date (or first-seen fallback), running after each refresh.

## ADDED Requirements

### Requirement: Age-based pruning
The system SHALL delete entries whose effective date is strictly before `now - window_days` days, where `window_days` is `feeds.window_days` (30 days default, hardcoded in M1). The effective date SHALL be the `published` date, or the entry's first-seen date when `published` is missing. An entry with neither date SHALL be kept. Pruning SHALL apply to the entries of all feeds and run in a single transaction.

#### Scenario: Old entries are pruned
- **WHEN** `prune_entries(30)` is called and entries are older than 30 days
- **THEN** those entries are deleted

#### Scenario: Recent entries are preserved
- **WHEN** `prune_entries(30)` is called and entries are newer than 30 days
- **THEN** those entries are not deleted

#### Scenario: Entry exactly at the window is kept
- **WHEN** `prune_entries(30)` is called and an entry is exactly 30 days old
- **THEN** the entry is not deleted

#### Scenario: Future-dated entries are kept
- **WHEN** an entry's published date is in the future
- **THEN** pruning never deletes it

#### Scenario: Pruning uses first-seen as fallback
- **WHEN** an entry has no published date
- **THEN** the first-seen date is used to determine if the entry exceeds the window

#### Scenario: Entry without any known date is kept
- **WHEN** an entry has no published date and no first-seen date
- **THEN** the entry is not deleted

### Requirement: Window validation
The system SHALL reject a window of 0 days as a usage error, both in `prune_entries` and in a refresh before any feed is fetched.

#### Scenario: Zero window is rejected
- **WHEN** `prune_entries(0)` is called
- **THEN** a usage error (exit code 2) is returned and nothing is deleted

#### Scenario: Zero window fails a refresh before fetching
- **WHEN** a refresh is started with a prune window of 0 days
- **THEN** a usage error is returned and no feed is fetched

### Requirement: Unread entries are not exempt
The system SHALL prune unread entries that exceed the window.

#### Scenario: Unread old entries are pruned
- **WHEN** `prune_entries(30)` is called with unread entries older than 30 days
- **THEN** those unread entries are deleted

### Requirement: Pruning returns count
The system SHALL return the number of entries pruned.

#### Scenario: Prune count is returned
- **WHEN** `prune_entries(30)` is called
- **THEN** the function returns the count of deleted entries

### Requirement: Pruning runs after refresh
The system SHALL execute pruning automatically after every completed refresh — after all fetched feeds have been persisted, including when some or all feeds failed or no feed was due — and report the pruned count in the refresh report. Pruning SHALL NOT run when the refresh aborts with an error (invalid options, unknown target, database error).

#### Scenario: Pruning runs post-refresh
- **WHEN** `refresh_feeds` completes
- **THEN** `prune_entries` is called before returning and the count is reported as `pruned`

#### Scenario: Pruning runs when every feed fails
- **WHEN** `refresh_feeds` completes with every fetched feed failing
- **THEN** old entries are still pruned

#### Scenario: Aborted refresh does not prune
- **WHEN** `refresh_feeds` is called with an unknown feed ID
- **THEN** a not-found error is returned and no entry is pruned

### Requirement: Pruned count reflects aged-out entries only
Because entries already outside the window are never inserted, the `pruned` count of a refresh SHALL only include entries that were stored while inside the window and have since aged out. A refresh SHALL compute the cutoff once and use it both to skip out-of-window entries and for its post-refresh prune.

#### Scenario: Old entries in a feed document do not churn
- **WHEN** a feed whose document always contains entries older than the window is refreshed twice
- **THEN** those entries are never stored, and both refreshes report 0 for them in `new_entries` and `pruned`

#### Scenario: Stored entry that ages out is pruned
- **WHEN** an entry stored inside the window becomes older than the window and a refresh runs
- **THEN** the entry is deleted and counted in `pruned`
