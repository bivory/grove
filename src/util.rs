//! Utility functions for Grove.
//!
//! This module provides common utilities used across Grove modules.

use std::fs;
use std::io;
use std::path::Path;

use crate::error::{GroveError, Result};

/// Maximum file size that can be read into memory (10 MB).
///
/// This limit prevents memory issues when reading very large learnings files
/// or stats logs. Under normal usage, these files should be well under this
/// limit.
pub const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

/// Read a file into a string with size limit protection.
///
/// Returns an error if the file exceeds `MAX_FILE_SIZE` to prevent memory
/// issues with unexpectedly large files.
///
/// # Arguments
///
/// * `path` - Path to the file to read
///
/// # Errors
///
/// Returns an error if:
/// * The file cannot be read (doesn't exist, permission denied, etc.)
/// * The file exceeds `MAX_FILE_SIZE`
pub fn read_to_string_limited(path: &Path) -> Result<String> {
    // Check file size before reading
    let metadata = fs::metadata(path).map_err(|e| GroveError::storage(path, e))?;

    let size = metadata.len();
    if size > MAX_FILE_SIZE {
        return Err(GroveError::storage(
            path,
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "File too large ({} bytes, max {} bytes). Consider archiving old entries.",
                    size, MAX_FILE_SIZE
                ),
            ),
        ));
    }

    fs::read_to_string(path).map_err(|e| GroveError::storage(path, e))
}

/// Read a file into a string with a custom size limit.
///
/// This variant allows specifying a custom limit for files that may need
/// different constraints.
///
/// # Arguments
///
/// * `path` - Path to the file to read
/// * `max_size` - Maximum allowed file size in bytes
///
/// # Errors
///
/// Returns an error if the file exceeds `max_size` or cannot be read.
pub fn read_to_string_with_limit(path: &Path, max_size: u64) -> Result<String> {
    let metadata = fs::metadata(path).map_err(|e| GroveError::storage(path, e))?;

    let size = metadata.len();
    if size > max_size {
        return Err(GroveError::storage(
            path,
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("File too large ({} bytes, max {} bytes)", size, max_size),
            ),
        ));
    }

    fs::read_to_string(path).map_err(|e| GroveError::storage(path, e))
}

/// Sync the parent directory of a file to ensure durability after atomic rename.
///
/// On POSIX systems, after renaming a file (atomic write pattern), the directory
/// entry may not be immediately persisted. This function syncs the parent directory
/// to ensure the rename is durable across system crashes.
///
/// On non-Unix systems, this is a no-op (returns Ok immediately).
///
/// # Arguments
///
/// * `path` - Path to the file whose parent directory should be synced
///
/// # Errors
///
/// Returns an error if the parent directory cannot be opened or synced.
/// Callers should typically ignore errors here (fail-open) since the write itself
/// succeeded - this is just for extra durability.
#[cfg(unix)]
pub fn sync_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        // Open the directory - on Unix, opening a directory gives us a fd we can fsync
        let dir = fs::File::open(parent).map_err(|e| {
            GroveError::backend(format!(
                "Failed to open parent dir {} for sync: {}",
                parent.display(),
                e
            ))
        })?;

        // sync_all() on the directory fd syncs the directory entries
        dir.sync_all().map_err(|e| {
            GroveError::backend(format!(
                "Failed to sync parent dir {}: {}",
                parent.display(),
                e
            ))
        })?;
    }
    Ok(())
}

/// Non-Unix stub for sync_parent_dir - always succeeds.
#[cfg(not(unix))]
pub fn sync_parent_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_read_to_string_limited_success() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.txt");
        fs::write(&path, "Hello, world!").unwrap();

        let content = read_to_string_limited(&path).unwrap();
        assert_eq!(content, "Hello, world!");
    }

    #[test]
    fn test_read_to_string_limited_nonexistent() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("nonexistent.txt");

        let result = read_to_string_limited(&path);
        assert!(result.is_err());
        // Should be a storage error with the path
        let err = result.unwrap_err().to_string();
        assert!(err.contains("storage error"));
        assert!(err.contains("nonexistent.txt"));
    }

    #[test]
    fn test_read_to_string_with_limit_exceeds() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("large.txt");

        // Write a file that exceeds a small limit
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(&[b'x'; 1000]).unwrap();

        let result = read_to_string_with_limit(&path, 500);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("too large"));
        assert!(err.contains("1000 bytes"));
        assert!(err.contains("max 500 bytes"));
    }

    #[test]
    fn test_read_to_string_with_limit_within_limit() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("small.txt");
        fs::write(&path, "small content").unwrap();

        let content = read_to_string_with_limit(&path, 1000).unwrap();
        assert_eq!(content, "small content");
    }

    #[test]
    fn test_read_to_string_limited_at_boundary() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("boundary.txt");

        // Create a file exactly at the limit
        let content = "x".repeat(100);
        fs::write(&path, &content).unwrap();

        // Should succeed at exactly the limit
        let result = read_to_string_with_limit(&path, 100);
        assert!(result.is_ok());

        // Should fail when one byte over the limit
        let result = read_to_string_with_limit(&path, 99);
        assert!(result.is_err());
    }
}
