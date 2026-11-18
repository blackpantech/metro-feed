# Folder Management Specification

## Purpose

Manages feed folders as a flat, case-insensitive namespace with CRUD operations and an idempotent, protected `Unsorted` default folder.

## Requirements

### Requirement: Create folder
The system SHALL create a folder with a given name and kind. The name SHALL be trimmed and MUST NOT be empty. If a folder with the same (kind, name) already exists, `create_folder` SHALL return the existing folder's ID instead of creating a duplicate. Feed folders and saved folders SHALL be separate namespaces.

#### Scenario: Create a new folder succeeds
- **WHEN** `create_folder("Tech", Feeds)` is called
- **THEN** a new folder is created and its ID is returned

#### Scenario: Create duplicate folder returns existing
- **WHEN** `create_folder("Tech", Feeds)` is called twice
- **THEN** the second call returns the same folder ID as the first

#### Scenario: Same name with different kind is separate
- **WHEN** `create_folder("Tech", Feeds)` and `create_folder("Tech", Saved)` are called
- **THEN** two distinct folders are created (separate namespaces)

#### Scenario: Empty name is rejected
- **WHEN** `create_folder("  ", Feeds)` is called
- **THEN** the operation fails with a usage error

### Requirement: Strict folder creation
The system SHALL provide `create_new_folder`, which follows the same name rules as `create_folder` but fails with a conflict error when a folder of the same kind already uses the name. It backs user-initiated creation (`metro folders add`).

#### Scenario: Strict creation of an existing name fails
- **WHEN** `create_new_folder("tech", Feeds)` is called where "Tech" already exists
- **THEN** the operation fails with a conflict error and nothing is created

### Requirement: Find folder by name
The system SHALL resolve a folder of a given kind by name, using the same trimming and case-insensitive comparison as creation. An unknown name SHALL be a not-found error.

#### Scenario: Find folder with different casing
- **WHEN** `find_folder("TECH", Feeds)` is called where "Tech" exists
- **THEN** the "Tech" folder is returned

#### Scenario: Find unknown folder fails
- **WHEN** `find_folder("Missing", Feeds)` is called
- **THEN** the operation fails with a not-found error

### Requirement: List folders
The system SHALL return all folders of a given kind, sorted by name case-insensitively.

#### Scenario: List all feed folders
- **WHEN** `list_folders(Feeds)` is called
- **THEN** all feed folders are returned sorted alphabetically by name

#### Scenario: List empty folder set returns empty
- **WHEN** `list_folders(Feeds)` is called with no folders
- **THEN** an empty list is returned

### Requirement: Rename folder
The system SHALL update a folder's name. The new name SHALL be trimmed, MUST NOT be empty, and MUST NOT conflict with another folder of the same kind. Renaming a folder to its own name (including a casing change) SHALL succeed.

#### Scenario: Rename a folder succeeds
- **WHEN** `rename_folder(id, "Technology")` is called
- **THEN** the folder's name is updated

#### Scenario: Rename to duplicate name fails
- **WHEN** `rename_folder(id, "Tech")` is called where another folder "Tech" already exists
- **THEN** the operation fails with a conflict error

#### Scenario: Casing-only rename succeeds
- **WHEN** `rename_folder(id, "TECH")` is called on the folder "Tech"
- **THEN** the folder's name becomes "TECH"

#### Scenario: Rename unknown folder fails
- **WHEN** `rename_folder(nonexistent_id, "Name")` is called
- **THEN** the operation fails with a not-found error

### Requirement: Delete folder
The system SHALL delete a folder. Deletion SHALL fail if the folder contains feeds, so that the schema cascade never removes subscriptions or entries.

#### Scenario: Delete empty folder succeeds
- **WHEN** `delete_folder(id)` is called on an empty folder
- **THEN** the folder is removed

#### Scenario: Delete non-empty folder fails
- **WHEN** `delete_folder(id)` is called on a folder with feeds
- **THEN** the operation fails with an error indicating the folder is not empty, and nothing is deleted

#### Scenario: Delete unknown folder fails
- **WHEN** `delete_folder(nonexistent_id)` is called
- **THEN** the operation fails with a not-found error

### Requirement: Default Unsorted folder
The system SHALL create a `Unsorted` folder of kind `Feeds` on first use, idempotently.

#### Scenario: Default folder created on first add
- **WHEN** a feed is added without a folder specified
- **THEN** the `Unsorted` feed folder is created (if it does not exist) and the feed is assigned to it

### Requirement: Default folder is protected
The system SHALL refuse to delete or rename the default `Unsorted` feed folder (matched case-insensitively) with a generic error (exit code 1, the same kind as refusing a non-empty folder deletion) whose message identifies it as the default folder, and SHALL leave it unchanged. A saved folder named `Unsorted` is not the default folder. Renaming another folder to `Unsorted` SHALL be governed only by the name-conflict rule.

#### Scenario: Deleting the default folder is refused
- **WHEN** `delete_folder` is called on the `Unsorted` feed folder, even when it is empty
- **THEN** the operation fails with an error naming it as the default folder, and the folder still exists

#### Scenario: Renaming the default folder is refused
- **WHEN** `rename_folder` is called on the `Unsorted` feed folder
- **THEN** the operation fails with an error naming it as the default folder, and its name is unchanged

#### Scenario: Other folders are unaffected
- **WHEN** `delete_folder` or `rename_folder` is called on an empty feed folder "Tech"
- **THEN** the operation succeeds

#### Scenario: Renaming another folder to Unsorted conflicts
- **WHEN** `rename_folder(tech_id, "unsorted")` is called while the `Unsorted` feed folder exists
- **THEN** the operation fails with a conflict error

### Requirement: Case-insensitive folder names
The system SHALL compare folder names case-insensitively (ASCII letters only) within a kind and SHALL store and display the casing given at creation (or the latest rename).

#### Scenario: Case-insensitive lookup
- **WHEN** `create_folder("Tech", Feeds)` is called and then `create_folder("tech", Feeds)` is called
- **THEN** both resolve to the same folder, listed as "Tech"
