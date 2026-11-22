# Spec Delta

## Purpose

Enables the application to load configuration from a TOML file with platform-appropriate defaults, support CLI flag overrides, and create the config file on first run.

## ADDED Requirements

### Requirement: Config directory uses XDG-style paths
The application SHALL use the `directories` crate to resolve the config directory. The config directory name SHALL be `metro` (not `metro-feed`). On Linux, the default config path SHALL be `$XDG_CONFIG_HOME/metro` or `~/.config/metro`. On macOS, `~/Library/Application Support/metro`. On Windows, `%APPDATA%/metro`.

#### Scenario: Config path resolved on Linux
- **WHEN** the application runs on Linux with `XDG_CONFIG_HOME` unset
- **THEN** the config directory resolves to `~/.config/metro`

#### Scenario: Config path respects XDG_CONFIG_HOME
- **WHEN** the application runs on Linux with `XDG_CONFIG_HOME=/custom/path`
- **THEN** the config directory resolves to `/custom/path/metro`

### Requirement: Config file is TOML format
The configuration file SHALL be named `config.toml` and SHALL be parseable by the `toml` crate with `serde` deserialization.

#### Scenario: Valid TOML parses successfully
- **WHEN** `config.toml` contains valid TOML syntax
- **THEN** the application loads the configuration without error

#### Scenario: Invalid TOML reports parse error
- **WHEN** `config.toml` contains invalid TOML syntax
- **THEN** the application reports a parse error with line/column information

### Requirement: Config precedence: defaults < file < CLI
Configuration values SHALL be resolved in this order: hardcoded defaults, then `config.toml` file values (overriding defaults), then CLI flags (overriding file values).

#### Scenario: Default value used when no config exists
- **WHEN** no `config.toml` file exists and no CLI flag is provided
- **THEN** the hardcoded default value is used

#### Scenario: File value overrides default
- **WHEN** `config.toml` sets a value and no CLI flag is provided
- **THEN** the file value is used

#### Scenario: CLI flag overrides file value
- **WHEN** both `config.toml` and a CLI flag set the same value
- **THEN** the CLI flag value is used

### Requirement: Config file created on first run
The application SHALL create `config.toml` with default values on the first run of a command that requires configuration. The `--version` and `--help` flags SHALL NOT trigger config file creation.

#### Scenario: First run creates config file
- **WHEN** a configuration-requiring command runs and no `config.toml` exists
- **THEN** `config.toml` is created with default values

#### Scenario: Version flag does not create config
- **WHEN** `metro --version` is run and no `config.toml` exists
- **THEN** no config file is created

### Requirement: Global flags are accepted from M0
The application SHALL accept `-v/--verbose` and `--log` as global flags from M0. These flags SHALL be inert (no-op) until M1.

#### Scenario: Verbose flag accepted without error
- **WHEN** `metro -v --version` is run
- **THEN** the command succeeds without "unrecognized flag" error

#### Scenario: Log flag accepted without error
- **WHEN** `metro --log debug --version` is run
- **THEN** the command succeeds without "unrecognized flag" error