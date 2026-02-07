//! Unified error types for Grove with fail-open philosophy.
//!
//! All errors in Grove follow the fail-open principle: infrastructure errors
//! should never block work. When errors occur, we log warnings and return
//! safe defaults rather than propagating failures that would block developers.

use std::io;
use std::path::PathBuf;
use thiserror::Error;

/// The main error type for Grove operations.
#[derive(Error, Debug)]
pub enum GroveError {
    /// I/O errors from session file operations.
    #[error("storage error at {path}: {source}")]
    Storage {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// Memory backend errors (write, search, ping failures).
    #[error("backend error: {message}")]
    Backend { message: String },

    /// JSON or markdown parsing/serialization errors.
    #[error("serialization error: {message}")]
    Serde { message: String },

    /// State machine violations (invalid transitions).
    #[error("invalid state: {message}")]
    InvalidState { message: String },

    /// Session not found in storage.
    #[error("session not found: {session_id}")]
    SessionNotFound { session_id: String },

    /// Configuration loading errors.
    #[error("config error: {message}")]
    Config { message: String },

    /// Ticketing or backend discovery errors.
    #[error("discovery error: {message}")]
    Discovery { message: String },

    /// Reflection parsing or validation errors.
    #[error("reflection error: {message}")]
    Reflection { message: String },
}

/// A specialized Result type for Grove operations.
pub type Result<T> = std::result::Result<T, GroveError>;

impl GroveError {
    /// Create a storage error from an I/O error.
    pub fn storage(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Storage {
            path: path.into(),
            source,
        }
    }

    /// Create a backend error.
    pub fn backend(message: impl Into<String>) -> Self {
        Self::Backend {
            message: message.into(),
        }
    }

    /// Create a serialization error.
    pub fn serde(message: impl Into<String>) -> Self {
        Self::Serde {
            message: message.into(),
        }
    }

    /// Create an invalid state error.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::InvalidState {
            message: message.into(),
        }
    }

    /// Create a session not found error.
    pub fn session_not_found(session_id: impl Into<String>) -> Self {
        Self::SessionNotFound {
            session_id: session_id.into(),
        }
    }

    /// Create a config error.
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
        }
    }

    /// Create a discovery error.
    pub fn discovery(message: impl Into<String>) -> Self {
        Self::Discovery {
            message: message.into(),
        }
    }

    /// Create a reflection error.
    pub fn reflection(message: impl Into<String>) -> Self {
        Self::Reflection {
            message: message.into(),
        }
    }

    /// Check if this error should trigger fail-open behavior.
    ///
    /// All Grove errors are considered infrastructure errors that should
    /// not block work. This method returns true for all error types.
    pub fn is_fail_open(&self) -> bool {
        true
    }
}

impl From<io::Error> for GroveError {
    fn from(err: io::Error) -> Self {
        Self::Storage {
            path: PathBuf::new(),
            source: err,
        }
    }
}

impl From<serde_json::Error> for GroveError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serde {
            message: err.to_string(),
        }
    }
}

/// Trait for fail-open error handling.
///
/// This trait provides methods for handling errors according to Grove's
/// fail-open philosophy: log the error and return a safe default.
pub trait FailOpen<T> {
    /// Handle an error by logging a warning and returning the default value.
    fn fail_open_default(self, context: &str) -> T
    where
        T: Default;

    /// Handle an error by logging a warning and returning the provided fallback.
    fn fail_open_with(self, context: &str, fallback: T) -> T;
}

impl<T> FailOpen<T> for Result<T> {
    fn fail_open_default(self, context: &str) -> T
    where
        T: Default,
    {
        match self {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!("{}: {} (fail-open: using default)", context, err);
                T::default()
            }
        }
    }

    fn fail_open_with(self, context: &str, fallback: T) -> T {
        match self {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!("{}: {} (fail-open: using fallback)", context, err);
                fallback
            }
        }
    }
}

/// Exit codes for Grove CLI.
///
/// These exit codes are used by hook handlers to communicate decisions
/// to Claude Code.
pub mod exit_codes {
    /// Exit code indicating approval (allow action to proceed).
    pub const APPROVE: i32 = 0;

    /// Exit code indicating block (prevent action, require reflection).
    pub const BLOCK: i32 = 2;

    /// Exit code indicating crash (fail-open, treat as approve).
    pub const CRASH: i32 = 3;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_error_display() {
        let err = GroveError::storage(
            "/tmp/test.json",
            io::Error::new(io::ErrorKind::NotFound, "file not found"),
        );
        assert!(err.to_string().contains("storage error"));
        assert!(err.to_string().contains("/tmp/test.json"));
    }

    #[test]
    fn test_backend_error_display() {
        let err = GroveError::backend("connection failed");
        assert_eq!(err.to_string(), "backend error: connection failed");
    }

    #[test]
    fn test_serde_error_display() {
        let err = GroveError::serde("invalid JSON");
        assert_eq!(err.to_string(), "serialization error: invalid JSON");
    }

    #[test]
    fn test_invalid_state_error_display() {
        let err = GroveError::invalid_state("cannot transition from Idle to Reflected");
        assert!(err.to_string().contains("invalid state"));
    }

    #[test]
    fn test_session_not_found_error_display() {
        let err = GroveError::session_not_found("abc-123");
        assert_eq!(err.to_string(), "session not found: abc-123");
    }

    #[test]
    fn test_config_error_display() {
        let err = GroveError::config("invalid TOML");
        assert_eq!(err.to_string(), "config error: invalid TOML");
    }

    #[test]
    fn test_discovery_error_display() {
        let err = GroveError::discovery("no ticketing system found");
        assert_eq!(
            err.to_string(),
            "discovery error: no ticketing system found"
        );
    }

    #[test]
    fn test_reflection_error_display() {
        let err = GroveError::reflection("missing required field: summary");
        assert_eq!(
            err.to_string(),
            "reflection error: missing required field: summary"
        );
    }

    #[test]
    fn test_is_fail_open() {
        let errors = vec![
            GroveError::backend("test"),
            GroveError::serde("test"),
            GroveError::invalid_state("test"),
            GroveError::session_not_found("test"),
            GroveError::config("test"),
            GroveError::discovery("test"),
            GroveError::reflection("test"),
        ];

        for err in errors {
            assert!(err.is_fail_open(), "All errors should be fail-open");
        }
    }

    #[test]
    fn test_from_io_error() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "access denied");
        let grove_err: GroveError = io_err.into();
        assert!(matches!(grove_err, GroveError::Storage { .. }));
    }

    #[test]
    fn test_from_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let grove_err: GroveError = json_err.into();
        assert!(matches!(grove_err, GroveError::Serde { .. }));
    }

    #[test]
    fn test_fail_open_default() {
        let result: Result<Vec<String>> = Err(GroveError::backend("test"));
        let value = result.fail_open_default("test context");
        assert!(value.is_empty());
    }

    #[test]
    fn test_fail_open_with() {
        let result: Result<i32> = Err(GroveError::backend("test"));
        let value = result.fail_open_with("test context", 42);
        assert_eq!(value, 42);
    }

    #[test]
    fn test_fail_open_success() {
        let result: Result<i32> = Ok(100);
        let value = result.fail_open_default("test context");
        assert_eq!(value, 100);
    }

    #[test]
    fn test_exit_codes() {
        assert_eq!(exit_codes::APPROVE, 0);
        assert_eq!(exit_codes::BLOCK, 2);
        assert_eq!(exit_codes::CRASH, 3);
    }
}
