//! Configuration loading for Metro.

use crate::MetroError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Metro configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Verbose output level
    #[serde(default)]
    pub verbose: u8,

    /// Log configuration
    #[serde(default)]
    pub log: Option<String>,
}

impl Config {
    /// Get the config directory path using the `directories` crate.
    ///
    /// Config dir name: `metro` (matches the binary name, not project name).
    #[must_use]
    pub fn config_dir() -> Option<PathBuf> {
        directories::BaseDirs::new().map(|d| d.config_dir().join("metro"))
    }

    /// Get the path to the config file.
    #[must_use]
    pub fn config_path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join("config.toml"))
    }

    /// Load configuration with precedence: defaults < TOML file < CLI flags.
    ///
    /// If `bootstrap` is true and no config file exists, creates one with defaults.
    /// Does NOT create config file if `skip_bootstrap` is true (e.g., for --version).
    ///
    /// # Errors
    ///
    /// Returns an error if the config file cannot be read, parsed, or written.
    pub fn load(
        cli_verbose: Option<u8>,
        cli_log: Option<String>,
        bootstrap: bool,
    ) -> Result<Self, MetroError> {
        // Start with defaults
        let mut config = Config::default();

        // Try to load from file
        if let Some(path) = Self::config_path() {
            if path.exists() {
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| MetroError::Generic(format!("Failed to read config: {e}")))?;
                let file_config: Config = toml::from_str(&content)?;
                config = file_config;
            } else if bootstrap {
                // Create config directory and file with defaults
                if let Some(dir) = Self::config_dir() {
                    std::fs::create_dir_all(&dir).map_err(|e| {
                        MetroError::Generic(format!("Failed to create config dir: {e}"))
                    })?;
                    let default_content = toml::to_string(&Config::default()).map_err(|e| {
                        MetroError::Generic(format!("Failed to serialize config: {e}"))
                    })?;
                    std::fs::write(&path, default_content)
                        .map_err(|e| MetroError::Generic(format!("Failed to write config: {e}")))?;
                }
            }
        }

        // Apply CLI overrides
        if let Some(v) = cli_verbose {
            config.verbose = v;
        }
        if let Some(log) = cli_log {
            config.log = Some(log);
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_dir_name() {
        // Verify config dir uses "metro" not "metro-feed"
        let dir = Config::config_dir();
        assert!(dir.is_some());
        let dir_path = dir.unwrap();
        assert!(dir_path.ends_with("metro"));
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.verbose, 0);
        assert!(config.log.is_none());
    }

    #[test]
    fn test_cli_override() {
        let config = Config::load(Some(2), Some("debug".to_string()), false).unwrap();
        assert_eq!(config.verbose, 2);
        assert_eq!(config.log, Some("debug".to_string()));
    }
}
