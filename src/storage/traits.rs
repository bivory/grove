//! Session storage traits for Grove.
//!
//! This module defines the `SessionStore` trait for session persistence.

use std::sync::Arc;

use crate::core::SessionState;
use crate::error::Result;

/// Trait for session storage backends.
///
/// Implementations provide persistent storage for session state,
/// supporting CRUD operations and listing recent sessions.
pub trait SessionStore: Send + Sync {
    /// Retrieve a session by ID.
    ///
    /// Returns `Ok(None)` if the session doesn't exist.
    fn get(&self, id: &str) -> Result<Option<SessionState>>;

    /// Save a session.
    ///
    /// Creates a new session or updates an existing one.
    fn put(&self, session: &SessionState) -> Result<()>;

    /// List recent sessions.
    ///
    /// Returns up to `limit` sessions, ordered by most recently updated.
    fn list(&self, limit: usize) -> Result<Vec<SessionState>>;

    /// Delete a session.
    ///
    /// Returns `Ok(())` even if the session doesn't exist.
    fn delete(&self, id: &str) -> Result<()>;

    /// Check if a session exists.
    fn exists(&self, id: &str) -> Result<bool> {
        Ok(self.get(id)?.is_some())
    }
}

/// Blanket implementation of SessionStore for Arc-wrapped stores.
///
/// This allows using `Arc<T>` where `T: SessionStore` is expected,
/// which is useful for sharing stores between tests and commands.
impl<T: SessionStore + ?Sized> SessionStore for Arc<T> {
    fn get(&self, id: &str) -> Result<Option<SessionState>> {
        (**self).get(id)
    }

    fn put(&self, session: &SessionState) -> Result<()> {
        (**self).put(session)
    }

    fn list(&self, limit: usize) -> Result<Vec<SessionState>> {
        (**self).list(limit)
    }

    fn delete(&self, id: &str) -> Result<()> {
        (**self).delete(id)
    }
}

/// Test utilities for SessionStore implementations.
#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::core::SessionState;

    /// Test helper to verify SessionStore implementations.
    pub fn test_session_store_crud<S: SessionStore>(store: &S) {
        // Create a session
        let session = SessionState::new("test-session", "/tmp/project", "/tmp/transcript.json");

        // Initially should not exist
        assert!(!store.exists(&session.id).unwrap());
        assert!(store.get(&session.id).unwrap().is_none());

        // Put the session
        store.put(&session).unwrap();

        // Now should exist
        assert!(store.exists(&session.id).unwrap());

        // Get should return the session
        let retrieved = store.get(&session.id).unwrap().unwrap();
        assert_eq!(retrieved.id, session.id);
        assert_eq!(retrieved.cwd, session.cwd);

        // List should include the session
        let sessions = store.list(10).unwrap();
        assert!(!sessions.is_empty());
        assert!(sessions.iter().any(|s| s.id == session.id));

        // Delete the session
        store.delete(&session.id).unwrap();

        // Should no longer exist
        assert!(!store.exists(&session.id).unwrap());
        assert!(store.get(&session.id).unwrap().is_none());

        // Delete again should succeed
        store.delete(&session.id).unwrap();
    }
}
