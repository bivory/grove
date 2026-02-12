//! File-based session storage for Grove.
//!
//! Sessions are stored as JSON files in `~/.grove/sessions/`.
//! Atomic writes are achieved via temp file + rename pattern.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::config::sessions_dir;
use crate::core::SessionState;
use crate::error::{GroveError, Result};
use crate::storage::SessionStore;
use crate::util::sync_parent_dir;

/// File-based session storage.
///
/// Stores sessions as JSON files in a configurable directory.
/// Uses atomic writes via temp file + rename pattern.
#[derive(Debug, Clone)]
pub struct FileSessionStore {
    /// Directory where session files are stored.
    sessions_dir: PathBuf,
}

impl FileSessionStore {
    /// Create a new file session store with the default directory.
    ///
    /// Uses `~/.grove/sessions/` or `$GROVE_HOME/sessions/`.
    pub fn new() -> Result<Self> {
        let dir = sessions_dir().ok_or_else(|| {
            GroveError::config("Could not determine sessions directory (no home directory)")
        })?;
        Self::with_dir(dir)
    }

    /// Create a new file session store with a custom directory.
    pub fn with_dir(sessions_dir: impl Into<PathBuf>) -> Result<Self> {
        let sessions_dir = sessions_dir.into();

        // Create the directory if it doesn't exist
        if !sessions_dir.exists() {
            fs::create_dir_all(&sessions_dir).map_err(|e| GroveError::storage(&sessions_dir, e))?;
        }

        Ok(Self { sessions_dir })
    }

    /// Validate a session ID for safety.
    ///
    /// Session IDs must contain only safe characters to prevent path traversal attacks.
    /// Allowed characters: alphanumeric, dash, underscore.
    /// Disallowed: path separators (/\), parent directory (..), any other special chars.
    fn validate_session_id(id: &str) -> Result<()> {
        if id.is_empty() {
            return Err(GroveError::config("Session ID cannot be empty"));
        }

        // Check for path traversal patterns
        if id.contains("..") || id.contains('/') || id.contains('\\') {
            return Err(GroveError::config(format!(
                "Session ID contains invalid characters (path traversal attempt): {}",
                id
            )));
        }

        // Only allow alphanumeric, dash, underscore
        if !id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Err(GroveError::config(format!(
                "Session ID contains invalid characters (only alphanumeric, dash, underscore allowed): {}",
                id
            )));
        }

        Ok(())
    }

    /// Get the path for a session file.
    fn session_path(&self, id: &str) -> Result<PathBuf> {
        Self::validate_session_id(id)?;
        Ok(self.sessions_dir.join(format!("{}.json", id)))
    }

    /// Get the path for a temp file used during atomic writes.
    fn temp_path(&self, id: &str) -> Result<PathBuf> {
        Self::validate_session_id(id)?;
        Ok(self.sessions_dir.join(format!(".{}.json.tmp", id)))
    }

    /// Write a session atomically using temp file + rename.
    fn atomic_write(&self, session: &SessionState) -> Result<()> {
        let final_path = self.session_path(&session.id)?;
        let temp_path = self.temp_path(&session.id)?;

        // Serialize to JSON
        let json = serde_json::to_string_pretty(session)?;

        // Write to temp file
        {
            let mut file =
                fs::File::create(&temp_path).map_err(|e| GroveError::storage(&temp_path, e))?;
            file.write_all(json.as_bytes())
                .map_err(|e| GroveError::storage(&temp_path, e))?;
            file.sync_all()
                .map_err(|e| GroveError::storage(&temp_path, e))?;
        }

        // Rename temp file to final path (atomic on POSIX)
        fs::rename(&temp_path, &final_path).map_err(|e| GroveError::storage(&final_path, e))?;

        // Sync parent directory for durability (fail-open: write succeeded)
        let _ = sync_parent_dir(&final_path);

        Ok(())
    }
}

impl Default for FileSessionStore {
    fn default() -> Self {
        // Default::default() should not panic per Rust conventions.
        // Fall back to /tmp/grove/sessions if home directory unavailable.
        Self::new().unwrap_or_else(|_| {
            let fallback = std::path::PathBuf::from("/tmp/grove/sessions");
            Self::with_dir(fallback).expect("Failed to create fallback session store in /tmp")
        })
    }
}

impl SessionStore for FileSessionStore {
    fn get(&self, id: &str) -> Result<Option<SessionState>> {
        let path = self.session_path(id)?;

        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path).map_err(|e| GroveError::storage(&path, e))?;

        let session: SessionState = serde_json::from_str(&content)?;

        Ok(Some(session))
    }

    fn put(&self, session: &SessionState) -> Result<()> {
        self.atomic_write(session)
    }

    fn list(&self, limit: usize) -> Result<Vec<SessionState>> {
        if !self.sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions: Vec<(SessionState, std::time::SystemTime)> = Vec::new();

        let entries = fs::read_dir(&self.sessions_dir)
            .map_err(|e| GroveError::storage(&self.sessions_dir, e))?;

        for entry in entries {
            let entry = entry.map_err(|e| GroveError::storage(&self.sessions_dir, e))?;
            let path = entry.path();

            // Skip non-JSON files and temp files
            if path.extension().map(|e| e != "json").unwrap_or(true) {
                continue;
            }
            if path
                .file_name()
                .map(|n| n.to_string_lossy().starts_with('.'))
                .unwrap_or(true)
            {
                continue;
            }

            // Read and parse the session
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(session) = serde_json::from_str::<SessionState>(&content) {
                    // Get modification time for sorting
                    let mtime = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    sessions.push((session, mtime));
                }
            }
        }

        // Sort by modification time (most recent first)
        sessions.sort_by(|a, b| b.1.cmp(&a.1));

        // Take up to limit
        let sessions: Vec<SessionState> =
            sessions.into_iter().take(limit).map(|(s, _)| s).collect();

        Ok(sessions)
    }

    fn delete(&self, id: &str) -> Result<()> {
        let path = self.session_path(id)?;

        if path.exists() {
            fs::remove_file(&path).map_err(|e| GroveError::storage(&path, e))?;
        }

        // Also clean up any temp file
        let temp_path = self.temp_path(id)?;
        if temp_path.exists() {
            let _ = fs::remove_file(&temp_path);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::traits::tests::test_session_store_crud;
    use tempfile::TempDir;

    fn create_test_store() -> (FileSessionStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = FileSessionStore::with_dir(dir.path()).unwrap();
        (store, dir)
    }

    #[test]
    fn test_file_session_store_crud() {
        let (store, _dir) = create_test_store();
        test_session_store_crud(&store);
    }

    #[test]
    fn test_with_dir_creates_directory() {
        let dir = TempDir::new().unwrap();
        let sessions_path = dir.path().join("sessions");

        assert!(!sessions_path.exists());

        let _store = FileSessionStore::with_dir(&sessions_path).unwrap();

        assert!(sessions_path.exists());
        assert!(sessions_path.is_dir());
    }

    #[test]
    fn test_session_path() {
        let (store, _dir) = create_test_store();

        let path = store.session_path("test-session").unwrap();
        assert!(path.ends_with("test-session.json"));
    }

    #[test]
    fn test_default_does_not_panic() {
        // Default::default() should not panic per Rust conventions
        // In a normal environment, this will use ~/.grove/sessions
        // If home is unavailable, it falls back to /tmp/grove/sessions
        let store = FileSessionStore::default();

        // Verify the store is functional by checking its dir exists
        assert!(
            store.sessions_dir.exists() || store.sessions_dir.to_string_lossy().contains("/tmp"),
            "Default store should have a valid sessions directory"
        );
    }

    #[test]
    fn test_get_nonexistent() {
        let (store, _dir) = create_test_store();

        let result = store.get("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_put_and_get() {
        let (store, _dir) = create_test_store();

        let session = SessionState::new("test-1", "/tmp", "/tmp/t.json");
        store.put(&session).unwrap();

        let retrieved = store.get("test-1").unwrap().unwrap();
        assert_eq!(retrieved.id, "test-1");
        assert_eq!(retrieved.cwd, "/tmp");
    }

    #[test]
    fn test_put_updates_existing() {
        let (store, _dir) = create_test_store();

        let mut session = SessionState::new("test-1", "/tmp", "/tmp/t.json");
        store.put(&session).unwrap();

        session.cwd = "/updated".to_string();
        store.put(&session).unwrap();

        let retrieved = store.get("test-1").unwrap().unwrap();
        assert_eq!(retrieved.cwd, "/updated");
    }

    #[test]
    fn test_list_empty() {
        let (store, _dir) = create_test_store();

        let sessions = store.list(10).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_list_multiple() {
        let (store, _dir) = create_test_store();

        let session1 = SessionState::new("test-1", "/tmp", "/tmp/t.json");
        let session2 = SessionState::new("test-2", "/tmp", "/tmp/t.json");
        let session3 = SessionState::new("test-3", "/tmp", "/tmp/t.json");

        store.put(&session1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.put(&session2).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.put(&session3).unwrap();

        let sessions = store.list(10).unwrap();
        assert_eq!(sessions.len(), 3);

        // Most recent should be first
        assert_eq!(sessions[0].id, "test-3");
    }

    #[test]
    fn test_list_with_limit() {
        let (store, _dir) = create_test_store();

        for i in 0..5 {
            let session = SessionState::new(format!("test-{}", i), "/tmp", "/tmp/t.json");
            store.put(&session).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let sessions = store.list(2).unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn test_delete() {
        let (store, _dir) = create_test_store();

        let session = SessionState::new("test-1", "/tmp", "/tmp/t.json");
        store.put(&session).unwrap();

        assert!(store.exists("test-1").unwrap());

        store.delete("test-1").unwrap();

        assert!(!store.exists("test-1").unwrap());
    }

    #[test]
    fn test_delete_nonexistent() {
        let (store, _dir) = create_test_store();

        // Should not error
        store.delete("nonexistent").unwrap();
    }

    #[test]
    fn test_atomic_write_creates_valid_json() {
        let (store, _dir) = create_test_store();

        let session = SessionState::new("test-atomic", "/tmp", "/tmp/t.json");
        store.put(&session).unwrap();

        let path = store.session_path("test-atomic").unwrap();
        let content = fs::read_to_string(&path).unwrap();

        // Verify it's valid JSON
        let parsed: SessionState = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.id, "test-atomic");
    }

    #[test]
    fn test_temp_file_cleaned_up() {
        let (store, _dir) = create_test_store();

        let session = SessionState::new("test-temp", "/tmp", "/tmp/t.json");
        store.put(&session).unwrap();

        let temp_path = store.temp_path("test-temp").unwrap();
        assert!(!temp_path.exists());
    }

    #[test]
    fn test_list_ignores_temp_files() {
        let (store, dir) = create_test_store();

        // Create a normal session
        let session = SessionState::new("normal", "/tmp", "/tmp/t.json");
        store.put(&session).unwrap();

        // Create a fake temp file
        let temp_path = dir.path().join(".temp.json.tmp");
        fs::write(&temp_path, "{}").unwrap();

        let sessions = store.list(10).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "normal");
    }

    #[test]
    fn test_list_ignores_invalid_json() {
        let (store, dir) = create_test_store();

        // Create a normal session
        let session = SessionState::new("valid", "/tmp", "/tmp/t.json");
        store.put(&session).unwrap();

        // Create an invalid JSON file
        let invalid_path = dir.path().join("invalid.json");
        fs::write(&invalid_path, "not valid json").unwrap();

        let sessions = store.list(10).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "valid");
    }

    #[test]
    fn test_exists() {
        let (store, _dir) = create_test_store();

        assert!(!store.exists("test").unwrap());

        let session = SessionState::new("test", "/tmp", "/tmp/t.json");
        store.put(&session).unwrap();

        assert!(store.exists("test").unwrap());
    }

    // ==========================================================================
    // Security tests for path traversal prevention
    // ==========================================================================

    #[test]
    fn test_validate_session_id_valid() {
        // Valid session IDs
        assert!(FileSessionStore::validate_session_id("abc123").is_ok());
        assert!(FileSessionStore::validate_session_id("test-session").is_ok());
        assert!(FileSessionStore::validate_session_id("test_session").is_ok());
        assert!(FileSessionStore::validate_session_id("Test-Session_123").is_ok());
        assert!(FileSessionStore::validate_session_id("01JKV4AGXRWZ1C7PT7HPXJNN51").is_ok());
    }

    #[test]
    fn test_validate_session_id_empty() {
        let result = FileSessionStore::validate_session_id("");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn test_validate_session_id_path_traversal_dotdot() {
        let result = FileSessionStore::validate_session_id("../etc/passwd");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path traversal"));
    }

    #[test]
    fn test_validate_session_id_path_traversal_forward_slash() {
        let result = FileSessionStore::validate_session_id("foo/bar");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path traversal"));
    }

    #[test]
    fn test_validate_session_id_path_traversal_backslash() {
        let result = FileSessionStore::validate_session_id("foo\\bar");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path traversal"));
    }

    #[test]
    fn test_validate_session_id_special_chars() {
        // Various special characters that should be rejected
        let invalid_ids = [
            "test.session",
            "test session",
            "test:session",
            "test;session",
            "test<session",
            "test>session",
            "test|session",
            "test\"session",
            "test'session",
            "test`session",
            "test$session",
            "test&session",
            "test*session",
            "test?session",
            "test!session",
            "test@session",
            "test#session",
            "test%session",
            "test^session",
            "test(session",
            "test)session",
            "test[session",
            "test]session",
            "test{session",
            "test}session",
            "test=session",
            "test+session",
        ];

        for id in invalid_ids {
            let result = FileSessionStore::validate_session_id(id);
            assert!(
                result.is_err(),
                "Should reject ID with special chars: {}",
                id
            );
        }
    }

    #[test]
    fn test_get_rejects_path_traversal() {
        let (store, _dir) = create_test_store();

        let result = store.get("../../../etc/passwd");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path traversal"));
    }

    #[test]
    fn test_put_rejects_path_traversal() {
        let (store, _dir) = create_test_store();

        let mut session = SessionState::new("valid", "/tmp", "/tmp/t.json");
        session.id = "../../../etc/passwd".to_string();

        let result = store.put(&session);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path traversal"));
    }

    #[test]
    fn test_delete_rejects_path_traversal() {
        let (store, _dir) = create_test_store();

        let result = store.delete("../../../etc/passwd");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path traversal"));
    }

    #[test]
    fn test_complex_path_traversal_attacks() {
        let (store, _dir) = create_test_store();

        // Various sophisticated path traversal attempts
        let attacks = [
            "....//....//etc/passwd",
            "..%2f..%2fetc/passwd",
            "..%252f..%252fetc/passwd",
            "%2e%2e%2f%2e%2e%2f",
            "..\\..\\..\\etc\\passwd",
            "foo/../../../etc/passwd",
            "foo/../../bar",
        ];

        for attack in attacks {
            let result = store.get(attack);
            assert!(result.is_err(), "Should reject attack: {}", attack);
        }
    }
}
