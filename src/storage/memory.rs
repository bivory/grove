//! In-memory session storage for testing.
//!
//! This module provides a thread-safe in-memory implementation of the
//! SessionStore trait, primarily for use in unit tests.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::core::SessionState;
use crate::error::Result;
use crate::storage::SessionStore;

/// In-memory session store for testing.
///
/// Thread-safe implementation using `RwLock<HashMap>`.
/// Sessions are stored in memory and lost when the store is dropped.
#[derive(Debug, Default)]
pub struct MemorySessionStore {
    /// Session storage.
    sessions: RwLock<HashMap<String, SessionState>>,
}

impl MemorySessionStore {
    /// Create a new empty in-memory store.
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Get the number of sessions in the store.
    pub fn len(&self) -> usize {
        self.sessions.read().unwrap().len()
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.sessions.read().unwrap().is_empty()
    }

    /// Clear all sessions from the store.
    pub fn clear(&self) {
        self.sessions.write().unwrap().clear();
    }
}

impl SessionStore for MemorySessionStore {
    fn get(&self, id: &str) -> Result<Option<SessionState>> {
        let sessions = self.sessions.read().unwrap();
        Ok(sessions.get(id).cloned())
    }

    fn put(&self, session: &SessionState) -> Result<()> {
        let mut sessions = self.sessions.write().unwrap();
        sessions.insert(session.id.clone(), session.clone());
        Ok(())
    }

    fn list(&self, limit: usize) -> Result<Vec<SessionState>> {
        let sessions = self.sessions.read().unwrap();
        let mut result: Vec<SessionState> = sessions.values().cloned().collect();

        // Sort by updated_at descending (most recent first)
        result.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        // Limit the results
        result.truncate(limit);

        Ok(result)
    }

    fn delete(&self, id: &str) -> Result<()> {
        let mut sessions = self.sessions.write().unwrap();
        sessions.remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::traits::tests::test_session_store_crud;

    #[test]
    fn test_memory_store_crud() {
        let store = MemorySessionStore::new();
        test_session_store_crud(&store);
    }

    #[test]
    fn test_new_store_is_empty() {
        let store = MemorySessionStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_len_and_is_empty() {
        let store = MemorySessionStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);

        let session = SessionState::new("s1", "/tmp", "/tmp/t.json");
        store.put(&session).unwrap();

        assert!(!store.is_empty());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_clear() {
        let store = MemorySessionStore::new();

        store
            .put(&SessionState::new("s1", "/tmp", "/tmp/t.json"))
            .unwrap();
        store
            .put(&SessionState::new("s2", "/tmp", "/tmp/t.json"))
            .unwrap();

        assert_eq!(store.len(), 2);

        store.clear();

        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_default_trait() {
        let store = MemorySessionStore::default();
        assert!(store.is_empty());
    }

    #[test]
    fn test_list_ordering() {
        use chrono::{Duration, Utc};

        let store = MemorySessionStore::new();

        // Create sessions with different timestamps
        let mut s1 = SessionState::new("s1", "/tmp", "/tmp/t.json");
        let mut s2 = SessionState::new("s2", "/tmp", "/tmp/t.json");
        let mut s3 = SessionState::new("s3", "/tmp", "/tmp/t.json");

        // Set different updated_at times
        s1.updated_at = Utc::now() - Duration::seconds(100);
        s2.updated_at = Utc::now() - Duration::seconds(50);
        s3.updated_at = Utc::now();

        // Put in random order
        store.put(&s2).unwrap();
        store.put(&s1).unwrap();
        store.put(&s3).unwrap();

        // List should return in reverse chronological order
        let result = store.list(10).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].id, "s3"); // Most recent
        assert_eq!(result[1].id, "s2");
        assert_eq!(result[2].id, "s1"); // Oldest
    }

    #[test]
    fn test_list_limit() {
        let store = MemorySessionStore::new();

        for i in 0..10 {
            store
                .put(&SessionState::new(format!("s{}", i), "/tmp", "/tmp/t.json"))
                .unwrap();
        }

        assert_eq!(store.len(), 10);

        let result = store.list(3).unwrap();
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_put_updates_existing() {
        let store = MemorySessionStore::new();

        let mut session = SessionState::new("s1", "/tmp/original", "/tmp/t.json");
        store.put(&session).unwrap();

        // Update the session
        session.cwd = "/tmp/updated".to_string();
        store.put(&session).unwrap();

        // Should still be only one session
        assert_eq!(store.len(), 1);

        // Should have the updated value
        let retrieved = store.get("s1").unwrap().unwrap();
        assert_eq!(retrieved.cwd, "/tmp/updated");
    }

    #[test]
    fn test_thread_safety() {
        use std::sync::Arc;
        use std::thread;

        let store = Arc::new(MemorySessionStore::new());
        let mut handles = vec![];

        // Spawn multiple threads that read and write
        for i in 0..10 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                let session = SessionState::new(format!("s{}", i), "/tmp", "/tmp/t.json");
                store_clone.put(&session).unwrap();
                store_clone.get(&format!("s{}", i)).unwrap();
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // All sessions should be stored
        assert_eq!(store.len(), 10);
    }
}
