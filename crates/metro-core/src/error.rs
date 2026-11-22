//! Error types for Metro.

use thiserror::Error;

/// Metro error types with exit code mapping.
#[derive(Error, Debug)]
pub enum MetroError {
    #[error("OK")]
    Ok,
    #[error("Generic error: {0}")]
    Generic(String),
    #[error("Usage error: {0}")]
    Usage(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Network error: {0}")]
    Network(String),
    #[error("Partial success: {0}")]
    PartialSuccess(String),
}

impl MetroError {
    /// Get the exit code for this error.
    ///
    /// Exit codes:
    /// - 0: ok
    /// - 1: generic error
    /// - 2: usage error
    /// - 3: not found
    /// - 4: network error
    /// - 5: partial success
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            MetroError::Ok => 0,
            MetroError::Generic(_) => 1,
            MetroError::Usage(_) => 2,
            MetroError::NotFound(_) => 3,
            MetroError::Network(_) => 4,
            MetroError::PartialSuccess(_) => 5,
        }
    }
}

impl From<rusqlite::Error> for MetroError {
    fn from(err: rusqlite::Error) -> Self {
        MetroError::Generic(err.to_string())
    }
}

impl From<toml::de::Error> for MetroError {
    fn from(err: toml::de::Error) -> Self {
        MetroError::Usage(err.to_string())
    }
}

impl From<toml::ser::Error> for MetroError {
    fn from(err: toml::ser::Error) -> Self {
        MetroError::Generic(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_codes() {
        assert_eq!(MetroError::Ok.exit_code(), 0);
        assert_eq!(MetroError::Generic("test".to_string()).exit_code(), 1);
        assert_eq!(MetroError::Usage("test".to_string()).exit_code(), 2);
        assert_eq!(MetroError::NotFound("test".to_string()).exit_code(), 3);
        assert_eq!(MetroError::Network("test".to_string()).exit_code(), 4);
        assert_eq!(
            MetroError::PartialSuccess("test".to_string()).exit_code(),
            5
        );
    }
}
