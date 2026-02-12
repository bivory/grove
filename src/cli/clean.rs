//! Clean command for Grove.
//!
//! Removes old session files and orphaned data.

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::warn;

use crate::config::sessions_dir;
use crate::core::SessionState;

/// Options for the clean command.
#[derive(Debug, Clone, Default)]
pub struct CleanOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Remove sessions older than this duration (e.g., "7d", "24h").
    pub before: Option<String>,
    /// Also remove orphaned temp files.
    pub orphans: bool,
    /// Dry run - show what would be deleted without deleting.
    pub dry_run: bool,
}

/// Output format for the clean command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanOutput {
    /// Whether the command was successful.
    pub success: bool,
    /// Number of sessions deleted.
    pub sessions_deleted: usize,
    /// Number of orphans deleted.
    pub orphans_deleted: usize,
    /// Total bytes freed.
    pub bytes_freed: u64,
    /// Session IDs that were deleted.
    pub deleted_ids: Vec<String>,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Error message if command failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl CleanOutput {
    /// Create a successful output.
    pub fn success(
        sessions_deleted: usize,
        orphans_deleted: usize,
        bytes_freed: u64,
        deleted_ids: Vec<String>,
        dry_run: bool,
    ) -> Self {
        Self {
            success: true,
            sessions_deleted,
            orphans_deleted,
            bytes_freed,
            deleted_ids,
            dry_run,
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            sessions_deleted: 0,
            orphans_deleted: 0,
            bytes_freed: 0,
            deleted_ids: Vec::new(),
            dry_run: false,
            error: Some(error.into()),
        }
    }
}

/// The clean command implementation.
pub struct CleanCommand {
    sessions_dir: PathBuf,
}

impl CleanCommand {
    /// Create a new clean command with the default sessions directory.
    pub fn new() -> Option<Self> {
        sessions_dir().map(|dir| Self { sessions_dir: dir })
    }

    /// Create a new clean command with a custom sessions directory.
    pub fn with_dir(sessions_dir: impl Into<PathBuf>) -> Self {
        Self {
            sessions_dir: sessions_dir.into(),
        }
    }

    /// Run the clean command.
    pub fn run(&self, options: &CleanOptions) -> CleanOutput {
        // Parse the duration
        let cutoff = match &options.before {
            Some(duration_str) => match Self::parse_duration(duration_str) {
                Ok(duration) => Utc::now() - duration,
                Err(e) => return CleanOutput::failure(e),
            },
            None => return CleanOutput::failure("--before is required"),
        };

        if !self.sessions_dir.exists() {
            return CleanOutput::success(0, 0, 0, Vec::new(), options.dry_run);
        }

        let mut sessions_deleted = 0;
        let mut orphans_deleted = 0;
        let mut bytes_freed = 0u64;
        let mut deleted_ids = Vec::new();

        // Read all files in sessions directory
        let entries = match fs::read_dir(&self.sessions_dir) {
            Ok(e) => e,
            Err(e) => {
                return CleanOutput::failure(format!("Failed to read sessions directory: {}", e))
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            let file_size = entry.metadata().map(|m| m.len()).unwrap_or(0);

            // Handle temp files (orphans)
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') && name.ends_with(".tmp") {
                    if options.orphans {
                        if !options.dry_run {
                            if let Err(e) = fs::remove_file(&path) {
                                warn!(
                                    path = %path.display(),
                                    error = %e,
                                    "Failed to delete orphan file (fail-open: continuing)"
                                );
                                continue;
                            }
                        }
                        orphans_deleted += 1;
                        bytes_freed += file_size;
                    }
                    continue;
                }
            }

            // Handle session files
            if path.extension().map(|e| e != "json").unwrap_or(true) {
                continue;
            }

            // Read and check session age
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let session: SessionState = match serde_json::from_str(&content) {
                Ok(s) => s,
                Err(_) => continue,
            };

            // Check if session is older than cutoff
            if session.updated_at < cutoff {
                if !options.dry_run {
                    if let Err(e) = fs::remove_file(&path) {
                        warn!(
                            path = %path.display(),
                            session_id = %session.id,
                            error = %e,
                            "Failed to delete session file (fail-open: continuing)"
                        );
                        continue;
                    }
                }

                deleted_ids.push(session.id.clone());
                sessions_deleted += 1;
                bytes_freed += file_size;
            }
        }

        CleanOutput::success(
            sessions_deleted,
            orphans_deleted,
            bytes_freed,
            deleted_ids,
            options.dry_run,
        )
    }

    /// Parse a duration string like "7d", "24h", "30m".
    fn parse_duration(s: &str) -> Result<Duration, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("Duration cannot be empty".to_string());
        }

        let (num_str, unit) = if let Some(stripped) = s.strip_suffix('d') {
            (stripped, 'd')
        } else if let Some(stripped) = s.strip_suffix('h') {
            (stripped, 'h')
        } else if let Some(stripped) = s.strip_suffix('m') {
            (stripped, 'm')
        } else if let Some(stripped) = s.strip_suffix('s') {
            (stripped, 's')
        } else {
            // Default to days if no unit
            (s, 'd')
        };

        let num: i64 = num_str
            .parse()
            .map_err(|_| format!("Invalid duration number: {}", num_str))?;

        if num <= 0 {
            return Err("Duration must be positive".to_string());
        }

        match unit {
            'd' => Ok(Duration::days(num)),
            'h' => Ok(Duration::hours(num)),
            'm' => Ok(Duration::minutes(num)),
            's' => Ok(Duration::seconds(num)),
            _ => Err(format!("Invalid duration unit: {}", unit)),
        }
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &CleanOutput, options: &CleanOptions) -> String {
        if options.quiet {
            return String::new();
        }

        if options.json {
            serde_json::to_string_pretty(output).unwrap_or_else(|_| "{}".to_string())
        } else {
            self.format_human_readable(output, options)
        }
    }

    /// Format output as human-readable text.
    fn format_human_readable(&self, output: &CleanOutput, _options: &CleanOptions) -> String {
        if !output.success {
            return format!(
                "Clean failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            );
        }

        let prefix = if output.dry_run { "[dry-run] " } else { "" };

        if output.sessions_deleted == 0 && output.orphans_deleted == 0 {
            return format!("{}No sessions to clean.\n", prefix);
        }

        let mut lines = Vec::new();

        if output.sessions_deleted > 0 {
            lines.push(format!(
                "{}Deleted {} session(s)",
                prefix, output.sessions_deleted
            ));
        }

        if output.orphans_deleted > 0 {
            lines.push(format!(
                "{}Deleted {} orphan(s)",
                prefix, output.orphans_deleted
            ));
        }

        let bytes_str = Self::format_bytes(output.bytes_freed);
        lines.push(format!("{}Freed {}", prefix, bytes_str));

        if !output.deleted_ids.is_empty() && output.deleted_ids.len() <= 10 {
            lines.push(String::new());
            lines.push("Deleted sessions:".to_string());
            for id in &output.deleted_ids {
                lines.push(format!("  {}", id));
            }
        } else if output.deleted_ids.len() > 10 {
            lines.push(format!("\n({} sessions deleted)", output.deleted_ids.len()));
        }

        lines.join("\n") + "\n"
    }

    /// Format bytes as human-readable string.
    fn format_bytes(bytes: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;

        if bytes >= GB {
            format!("{:.2} GB", bytes as f64 / GB as f64)
        } else if bytes >= MB {
            format!("{:.2} MB", bytes as f64 / MB as f64)
        } else if bytes >= KB {
            format!("{:.2} KB", bytes as f64 / KB as f64)
        } else {
            format!("{} bytes", bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_session(id: &str, age_hours: i64) -> SessionState {
        let mut session = SessionState::new(id, "/tmp", "/tmp/transcript.json");
        session.updated_at = Utc::now() - Duration::hours(age_hours);
        session.created_at = session.updated_at;
        session
    }

    fn write_session_to_dir(dir: &std::path::Path, session: &SessionState) {
        let path = dir.join(format!("{}.json", session.id));
        let content = serde_json::to_string_pretty(session).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn test_clean_output_success() {
        let output =
            CleanOutput::success(2, 1, 1024, vec!["s1".to_string(), "s2".to_string()], false);

        assert!(output.success);
        assert_eq!(output.sessions_deleted, 2);
        assert_eq!(output.orphans_deleted, 1);
        assert_eq!(output.bytes_freed, 1024);
        assert!(!output.dry_run);
        assert!(output.error.is_none());
    }

    #[test]
    fn test_clean_output_failure() {
        let output = CleanOutput::failure("test error");

        assert!(!output.success);
        assert_eq!(output.sessions_deleted, 0);
        assert_eq!(output.error, Some("test error".to_string()));
    }

    #[test]
    fn test_parse_duration_days() {
        let duration = CleanCommand::parse_duration("7d").unwrap();
        assert_eq!(duration.num_days(), 7);
    }

    #[test]
    fn test_parse_duration_hours() {
        let duration = CleanCommand::parse_duration("24h").unwrap();
        assert_eq!(duration.num_hours(), 24);
    }

    #[test]
    fn test_parse_duration_minutes() {
        let duration = CleanCommand::parse_duration("30m").unwrap();
        assert_eq!(duration.num_minutes(), 30);
    }

    #[test]
    fn test_parse_duration_default_days() {
        let duration = CleanCommand::parse_duration("7").unwrap();
        assert_eq!(duration.num_days(), 7);
    }

    #[test]
    fn test_parse_duration_invalid() {
        assert!(CleanCommand::parse_duration("abc").is_err());
        assert!(CleanCommand::parse_duration("").is_err());
        assert!(CleanCommand::parse_duration("-1d").is_err());
        assert!(CleanCommand::parse_duration("0d").is_err());
    }

    #[test]
    fn test_clean_requires_before() {
        let temp = TempDir::new().unwrap();
        let cmd = CleanCommand::with_dir(temp.path());
        let options = CleanOptions::default();

        let output = cmd.run(&options);

        assert!(!output.success);
        assert!(output.error.unwrap().contains("--before is required"));
    }

    #[test]
    fn test_clean_empty_directory() {
        let temp = TempDir::new().unwrap();
        let cmd = CleanCommand::with_dir(temp.path());
        let options = CleanOptions {
            before: Some("1d".to_string()),
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.sessions_deleted, 0);
    }

    #[test]
    fn test_clean_deletes_old_sessions() {
        let temp = TempDir::new().unwrap();

        // Create an old session (48 hours old)
        let old_session = create_test_session("old-session", 48);
        write_session_to_dir(temp.path(), &old_session);

        // Create a new session (1 hour old)
        let new_session = create_test_session("new-session", 1);
        write_session_to_dir(temp.path(), &new_session);

        let cmd = CleanCommand::with_dir(temp.path());
        let options = CleanOptions {
            before: Some("24h".to_string()),
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.sessions_deleted, 1);
        assert!(output.deleted_ids.contains(&"old-session".to_string()));
        assert!(!output.deleted_ids.contains(&"new-session".to_string()));

        // Verify files
        assert!(!temp.path().join("old-session.json").exists());
        assert!(temp.path().join("new-session.json").exists());
    }

    #[test]
    fn test_clean_dry_run() {
        let temp = TempDir::new().unwrap();

        let old_session = create_test_session("old-session", 48);
        write_session_to_dir(temp.path(), &old_session);

        let cmd = CleanCommand::with_dir(temp.path());
        let options = CleanOptions {
            before: Some("24h".to_string()),
            dry_run: true,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert!(output.dry_run);
        assert_eq!(output.sessions_deleted, 1);

        // File should still exist
        assert!(temp.path().join("old-session.json").exists());
    }

    #[test]
    fn test_clean_orphans() {
        let temp = TempDir::new().unwrap();

        // Create an orphan temp file
        fs::write(temp.path().join(".orphan.json.tmp"), "{}").unwrap();

        // Create a normal session
        let session = create_test_session("normal", 1);
        write_session_to_dir(temp.path(), &session);

        let cmd = CleanCommand::with_dir(temp.path());
        let options = CleanOptions {
            before: Some("24h".to_string()),
            orphans: true,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.orphans_deleted, 1);
        assert!(!temp.path().join(".orphan.json.tmp").exists());
    }

    #[test]
    fn test_clean_ignores_orphans_by_default() {
        let temp = TempDir::new().unwrap();

        fs::write(temp.path().join(".orphan.json.tmp"), "{}").unwrap();

        let cmd = CleanCommand::with_dir(temp.path());
        let options = CleanOptions {
            before: Some("24h".to_string()),
            orphans: false,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.orphans_deleted, 0);
        assert!(temp.path().join(".orphan.json.tmp").exists());
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(CleanCommand::format_bytes(500), "500 bytes");
        assert_eq!(CleanCommand::format_bytes(1024), "1.00 KB");
        assert_eq!(CleanCommand::format_bytes(1536), "1.50 KB");
        assert_eq!(CleanCommand::format_bytes(1048576), "1.00 MB");
        assert_eq!(CleanCommand::format_bytes(1073741824), "1.00 GB");
    }

    #[test]
    fn test_format_output_json() {
        let temp = TempDir::new().unwrap();
        let cmd = CleanCommand::with_dir(temp.path());

        let output = CleanOutput::success(1, 0, 1024, vec!["test".to_string()], false);
        let options = CleanOptions {
            json: true,
            before: Some("1d".to_string()),
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"sessions_deleted\": 1"));
    }

    #[test]
    fn test_format_output_quiet() {
        let temp = TempDir::new().unwrap();
        let cmd = CleanCommand::with_dir(temp.path());

        let output = CleanOutput::success(1, 0, 1024, vec!["test".to_string()], false);
        let options = CleanOptions {
            quiet: true,
            before: Some("1d".to_string()),
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_output_human_readable() {
        let temp = TempDir::new().unwrap();
        let cmd = CleanCommand::with_dir(temp.path());

        let output =
            CleanOutput::success(2, 1, 2048, vec!["s1".to_string(), "s2".to_string()], false);
        let options = CleanOptions {
            before: Some("1d".to_string()),
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Deleted 2 session(s)"));
        assert!(formatted.contains("Deleted 1 orphan(s)"));
        assert!(formatted.contains("2.00 KB"));
    }

    #[test]
    fn test_format_output_dry_run() {
        let temp = TempDir::new().unwrap();
        let cmd = CleanCommand::with_dir(temp.path());

        let output = CleanOutput::success(1, 0, 1024, vec!["test".to_string()], true);
        let options = CleanOptions {
            before: Some("1d".to_string()),
            dry_run: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("[dry-run]"));
    }

    #[test]
    fn test_format_output_nothing_to_clean() {
        let temp = TempDir::new().unwrap();
        let cmd = CleanCommand::with_dir(temp.path());

        let output = CleanOutput::success(0, 0, 0, vec![], false);
        let options = CleanOptions {
            before: Some("1d".to_string()),
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("No sessions to clean"));
    }

    #[test]
    fn test_clean_nonexistent_directory() {
        let cmd = CleanCommand::with_dir("/nonexistent/path/sessions");
        let options = CleanOptions {
            before: Some("1d".to_string()),
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.sessions_deleted, 0);
    }
}
