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

    /// Get the path for a session file.
    fn session_path(&self, id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{}.json", id))
    }

    /// Get the path for a temp file used during atomic writes.
    fn temp_path(&self, id: &str) -> PathBuf {
        self.sessions_dir.join(format!(".{}.json.tmp", id))
    }

    /// Write a session atomically using temp file + rename.
    fn atomic_write(&self, session: &SessionState) -> Result<()> {
        let final_path = self.session_path(&session.id);
        let temp_path = self.temp_path(&session.id);

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

        Ok(())
    }
}

impl Default for FileSessionStore {
    fn default() -> Self {
        Self::new().expect("Failed to create default FileSessionStore")
    }
}

impl SessionStore for FileSessionStore {
    fn get(&self, id: &str) -> Result<Option<SessionState>> {
        let path = self.session_path(id);

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
        let path = self.session_path(id);

        if path.exists() {
            fs::remove_file(&path).map_err(|e| GroveError::storage(&path, e))?;
        }

        // Also clean up any temp file
        let temp_path = self.temp_path(id);
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

        let path = store.session_path("test-session");
        assert!(path.ends_with("test-session.json"));
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

        let path = store.session_path("test-atomic");
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

        let temp_path = store.temp_path("test-temp");
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
}
