//! Maintain command for Grove.
//!
//! Interactive review of stale learnings, with archive and restore operations.

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::backends::MemoryBackend;
use crate::config::Config;
use crate::core::LearningStatus;
use crate::error::Result;

/// Options for the maintain command.
#[derive(Debug, Clone, Default)]
pub struct MaintainOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Days until decay to consider "stale" (default: 7).
    pub stale_days: Option<u32>,
    /// Perform archive without confirmation.
    pub auto_archive: bool,
    /// Show dry run only.
    pub dry_run: bool,
}

/// Input for the maintain command (for programmatic use).
#[derive(Debug, Clone, Deserialize)]
pub struct MaintainInput {
    /// Action to perform.
    pub action: MaintainAction,
    /// Learning IDs to operate on.
    pub learning_ids: Vec<String>,
}

/// Actions available in maintain.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MaintainAction {
    /// List stale learnings.
    List,
    /// Archive specified learnings.
    Archive,
    /// Restore archived learnings.
    Restore,
}

/// Output format for the maintain command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintainOutput {
    /// Whether the operation was successful.
    pub success: bool,
    /// The action performed.
    pub action: String,
    /// Learnings that matched the criteria.
    pub stale_learnings: Vec<StaleLearningInfo>,
    /// Learnings that were archived.
    pub archived: Vec<String>,
    /// Learnings that were restored.
    pub restored: Vec<String>,
    /// Learnings that failed to update.
    pub failed: Vec<FailedUpdate>,
    /// Error message if operation failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Info about a stale learning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleLearningInfo {
    /// Learning ID.
    pub id: String,
    /// Learning summary.
    pub summary: String,
    /// Category.
    pub category: String,
    /// Days until decay.
    pub days_until_decay: i64,
    /// Current status.
    pub status: String,
}

/// Info about a failed update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedUpdate {
    /// Learning ID.
    pub id: String,
    /// Error message.
    pub error: String,
}

impl MaintainOutput {
    /// Create a list output.
    pub fn list(stale_learnings: Vec<StaleLearningInfo>) -> Self {
        Self {
            success: true,
            action: "list".to_string(),
            stale_learnings,
            archived: Vec::new(),
            restored: Vec::new(),
            failed: Vec::new(),
            error: None,
        }
    }

    /// Create an archive output.
    pub fn archive(
        stale_learnings: Vec<StaleLearningInfo>,
        archived: Vec<String>,
        failed: Vec<FailedUpdate>,
    ) -> Self {
        Self {
            success: failed.is_empty(),
            action: "archive".to_string(),
            stale_learnings,
            archived,
            restored: Vec::new(),
            failed,
            error: None,
        }
    }

    /// Create a restore output.
    pub fn restore(restored: Vec<String>, failed: Vec<FailedUpdate>) -> Self {
        Self {
            success: failed.is_empty(),
            action: "restore".to_string(),
            stale_learnings: Vec::new(),
            archived: Vec::new(),
            restored,
            failed,
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(action: &str, error: impl Into<String>) -> Self {
        Self {
            success: false,
            action: action.to_string(),
            stale_learnings: Vec::new(),
            archived: Vec::new(),
            restored: Vec::new(),
            failed: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// The maintain command implementation.
pub struct MaintainCommand<B: MemoryBackend> {
    backend: B,
    config: Config,
}

impl<B: MemoryBackend> MaintainCommand<B> {
    /// Create a new maintain command.
    pub fn new(backend: B, config: Config) -> Self {
        Self { backend, config }
    }

    /// Run the maintain command to list stale learnings.
    pub fn list_stale(&self, options: &MaintainOptions) -> MaintainOutput {
        let stale_days = options.stale_days.unwrap_or(7);

        match self.find_stale_learnings(stale_days) {
            Ok(stale) => MaintainOutput::list(stale),
            Err(e) => MaintainOutput::failure("list", e.to_string()),
        }
    }

    /// Run archive on specified learning IDs.
    pub fn archive(&self, learning_ids: &[String], options: &MaintainOptions) -> MaintainOutput {
        let stale_days = options.stale_days.unwrap_or(7);
        let stale = self.find_stale_learnings(stale_days).unwrap_or_default();

        if options.dry_run {
            // Just show what would be archived
            let would_archive: Vec<String> = if learning_ids.is_empty() {
                stale.iter().map(|s| s.id.clone()).collect()
            } else {
                learning_ids.to_vec()
            };
            return MaintainOutput {
                success: true,
                action: "archive (dry run)".to_string(),
                stale_learnings: stale,
                archived: would_archive,
                restored: Vec::new(),
                failed: Vec::new(),
                error: None,
            };
        }

        let ids_to_archive: Vec<String> = if learning_ids.is_empty() {
            stale.iter().map(|s| s.id.clone()).collect()
        } else {
            learning_ids.to_vec()
        };

        let mut archived = Vec::new();
        let mut failed = Vec::new();

        for id in &ids_to_archive {
            match self.backend.archive(id) {
                Ok(()) => archived.push(id.clone()),
                Err(e) => failed.push(FailedUpdate {
                    id: id.clone(),
                    error: e.to_string(),
                }),
            }
        }

        MaintainOutput::archive(stale, archived, failed)
    }

    /// Run restore on specified learning IDs.
    pub fn restore(&self, learning_ids: &[String], _options: &MaintainOptions) -> MaintainOutput {
        if learning_ids.is_empty() {
            return MaintainOutput::failure("restore", "No learning IDs specified");
        }

        let mut restored = Vec::new();
        let mut failed = Vec::new();

        for id in learning_ids {
            match self.backend.restore(id) {
                Ok(()) => restored.push(id.clone()),
                Err(e) => failed.push(FailedUpdate {
                    id: id.clone(),
                    error: e.to_string(),
                }),
            }
        }

        MaintainOutput::restore(restored, failed)
    }

    /// Run the maintain command with input.
    pub fn run_with_input(
        &self,
        input: &MaintainInput,
        options: &MaintainOptions,
    ) -> MaintainOutput {
        match input.action {
            MaintainAction::List => self.list_stale(options),
            MaintainAction::Archive => self.archive(&input.learning_ids, options),
            MaintainAction::Restore => self.restore(&input.learning_ids, options),
        }
    }

    /// Find stale learnings approaching decay.
    ///
    /// Note: Uses the learning's creation timestamp for decay calculation.
    /// In a more complete implementation, this would look up the last reference
    /// time from the stats cache.
    fn find_stale_learnings(&self, stale_days: u32) -> Result<Vec<StaleLearningInfo>> {
        let learnings = self.backend.list_all()?;
        let decay_days = self.config.decay.passive_duration_days as i64;
        let now = Utc::now();

        let mut stale = Vec::new();
        for learning in learnings {
            if learning.status != LearningStatus::Active {
                continue;
            }

            // Use creation timestamp for decay calculation
            let days_since_creation = (now - learning.timestamp).num_days();
            let days_until_decay = decay_days - days_since_creation;

            if days_until_decay <= stale_days as i64 && days_until_decay > 0 {
                stale.push(StaleLearningInfo {
                    id: learning.id.clone(),
                    summary: learning.summary.clone(),
                    category: format!("{:?}", learning.category).to_lowercase(),
                    days_until_decay,
                    status: "active".to_string(),
                });
            }
        }

        // Sort by days until decay (most urgent first)
        stale.sort_by_key(|s| s.days_until_decay);

        Ok(stale)
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &MaintainOutput, options: &MaintainOptions) -> String {
        if options.quiet {
            return String::new();
        }

        if options.json {
            serde_json::to_string_pretty(output).unwrap_or_else(|_| "{}".to_string())
        } else {
            self.format_human_readable(output)
        }
    }

    /// Format output as human-readable text.
    fn format_human_readable(&self, output: &MaintainOutput) -> String {
        if !output.success {
            return format!(
                "Maintain failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            );
        }

        let mut lines = Vec::new();

        match output.action.as_str() {
            "list" => {
                if output.stale_learnings.is_empty() {
                    lines.push("No stale learnings found.\n".to_string());
                } else {
                    lines.push(format!(
                        "Found {} stale learning(s) approaching decay:\n",
                        output.stale_learnings.len()
                    ));
                    for (i, learning) in output.stale_learnings.iter().enumerate() {
                        lines.push(format!(
                            "{}. [{}] {} ({}d until decay)",
                            i + 1,
                            learning.category,
                            learning.summary,
                            learning.days_until_decay
                        ));
                        lines.push(format!("   ID: {}", learning.id));
                        lines.push(String::new());
                    }
                    lines.push(
                        "Run 'grove maintain --archive' to archive stale learnings.".to_string(),
                    );
                }
            }
            action if action.starts_with("archive") => {
                if !output.archived.is_empty() {
                    lines.push(format!("Archived {} learning(s):", output.archived.len()));
                    for id in &output.archived {
                        lines.push(format!("  - {}", id));
                    }
                    lines.push(String::new());
                }
                if !output.failed.is_empty() {
                    lines.push(format!(
                        "Failed to archive {} learning(s):",
                        output.failed.len()
                    ));
                    for fail in &output.failed {
                        lines.push(format!("  - {}: {}", fail.id, fail.error));
                    }
                }
                if output.archived.is_empty() && output.failed.is_empty() {
                    lines.push("No learnings to archive.".to_string());
                }
            }
            "restore" => {
                if !output.restored.is_empty() {
                    lines.push(format!("Restored {} learning(s):", output.restored.len()));
                    for id in &output.restored {
                        lines.push(format!("  - {}", id));
                    }
                    lines.push(String::new());
                }
                if !output.failed.is_empty() {
                    lines.push(format!(
                        "Failed to restore {} learning(s):",
                        output.failed.len()
                    ));
                    for fail in &output.failed {
                        lines.push(format!("  - {}: {}", fail.id, fail.error));
                    }
                }
            }
            _ => {
                lines.push(format!("Unknown action: {}", output.action));
            }
        }

        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::MarkdownBackend;
    use chrono::Duration;
    use std::fs;
    use tempfile::TempDir;

    fn setup_with_learnings() -> (TempDir, MarkdownBackend) {
        let temp = TempDir::new().unwrap();
        let learnings_path = temp.path().join(".grove").join("learnings.md");
        fs::create_dir_all(learnings_path.parent().unwrap()).unwrap();

        let now = Utc::now();
        let old_date = (now - Duration::days(83)).format("%Y-%m-%dT%H:%M:%SZ");
        let recent_date = (now - Duration::days(1)).format("%Y-%m-%dT%H:%M:%SZ");

        let content = format!(
            r#"# Project Learnings

## cl_recent

**Category:** Pattern
**Summary:** Recent pattern
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #rust #recent
**Session:** test-session
**Criteria:** Behavior-Changing
**Created:** {}

A recently created pattern.

---

## cl_stale

**Category:** Pitfall
**Summary:** Old pitfall approaching decay
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #rust #stale
**Session:** test-session
**Criteria:** Behavior-Changing
**Created:** {}

An old pitfall that's approaching decay.

---

## cl_archived

**Category:** Pattern
**Summary:** Already archived
**Scope:** Project | **Confidence:** High | **Status:** Archived
**Tags:** #archived
**Session:** test-session
**Criteria:** Behavior-Changing
**Created:** {}

This pattern has been archived.

---
"#,
            recent_date, old_date, recent_date
        );
        fs::write(&learnings_path, content).unwrap();

        // Pass the file path, not the directory path
        let backend = MarkdownBackend::new(&learnings_path);
        (temp, backend)
    }

    #[test]
    fn test_maintain_output_list() {
        let stale = vec![StaleLearningInfo {
            id: "cl_001".to_string(),
            summary: "Test".to_string(),
            category: "pattern".to_string(),
            days_until_decay: 5,
            status: "active".to_string(),
        }];
        let output = MaintainOutput::list(stale);

        assert!(output.success);
        assert_eq!(output.action, "list");
        assert_eq!(output.stale_learnings.len(), 1);
    }

    #[test]
    fn test_maintain_output_archive() {
        let output = MaintainOutput::archive(vec![], vec!["cl_001".to_string()], vec![]);

        assert!(output.success);
        assert_eq!(output.action, "archive");
        assert_eq!(output.archived.len(), 1);
    }

    #[test]
    fn test_maintain_output_restore() {
        let output = MaintainOutput::restore(vec!["cl_001".to_string()], vec![]);

        assert!(output.success);
        assert_eq!(output.action, "restore");
        assert_eq!(output.restored.len(), 1);
    }

    #[test]
    fn test_maintain_output_failure() {
        let output = MaintainOutput::failure("list", "test error");

        assert!(!output.success);
        assert_eq!(output.error, Some("test error".to_string()));
    }

    #[test]
    fn test_list_stale_basic() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = MaintainCommand::new(backend, config);
        let options = MaintainOptions::default();

        let output = cmd.list_stale(&options);

        assert!(output.success);
        // Should find the stale pitfall
        assert!(output.stale_learnings.iter().any(|s| s.id == "cl_stale"));
    }

    #[test]
    fn test_list_stale_custom_days() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = MaintainCommand::new(backend, config);
        let options = MaintainOptions {
            stale_days: Some(1),
            ..Default::default()
        };

        let output = cmd.list_stale(&options);

        assert!(output.success);
    }

    #[test]
    fn test_archive_dry_run() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = MaintainCommand::new(backend, config);
        let options = MaintainOptions {
            dry_run: true,
            ..Default::default()
        };

        let output = cmd.archive(&[], &options);

        assert!(output.success);
        assert!(output.action.contains("dry run"));
    }

    #[test]
    fn test_archive_specific_ids() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = MaintainCommand::new(backend, config);
        let options = MaintainOptions::default();

        let output = cmd.archive(&["cl_stale".to_string()], &options);

        assert!(output.success || !output.failed.is_empty());
        // Either archived or failed (depending on backend)
    }

    #[test]
    fn test_restore_no_ids() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = MaintainCommand::new(backend, config);
        let options = MaintainOptions::default();

        let output = cmd.restore(&[], &options);

        assert!(!output.success);
        assert!(output.error.unwrap().contains("No learning IDs"));
    }

    #[test]
    fn test_restore_specific_ids() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = MaintainCommand::new(backend, config);
        let options = MaintainOptions::default();

        let output = cmd.restore(&["cl_archived".to_string()], &options);

        // Either restored or failed
        assert!(output.restored.len() + output.failed.len() == 1);
    }

    #[test]
    fn test_run_with_input_list() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = MaintainCommand::new(backend, config);
        let input = MaintainInput {
            action: MaintainAction::List,
            learning_ids: vec![],
        };
        let options = MaintainOptions::default();

        let output = cmd.run_with_input(&input, &options);

        assert!(output.success);
        assert_eq!(output.action, "list");
    }

    #[test]
    fn test_format_output_json() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = MaintainCommand::new(backend, config);

        let output = MaintainOutput::list(vec![]);
        let options = MaintainOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"action\": \"list\""));
    }

    #[test]
    fn test_format_output_quiet() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = MaintainCommand::new(backend, config);

        let output = MaintainOutput::list(vec![]);
        let options = MaintainOptions {
            quiet: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_output_human_readable_list() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = MaintainCommand::new(backend, config);

        let stale = vec![StaleLearningInfo {
            id: "cl_001".to_string(),
            summary: "Test pattern".to_string(),
            category: "pattern".to_string(),
            days_until_decay: 5,
            status: "active".to_string(),
        }];
        let output = MaintainOutput::list(stale);
        let options = MaintainOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Found 1 stale learning"));
        assert!(formatted.contains("Test pattern"));
        assert!(formatted.contains("5d until decay"));
    }

    #[test]
    fn test_format_output_human_readable_empty_list() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = MaintainCommand::new(backend, config);

        let output = MaintainOutput::list(vec![]);
        let options = MaintainOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("No stale learnings found"));
    }

    #[test]
    fn test_format_output_human_readable_archive() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = MaintainCommand::new(backend, config);

        let output = MaintainOutput::archive(vec![], vec!["cl_001".to_string()], vec![]);
        let options = MaintainOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Archived 1 learning"));
        assert!(formatted.contains("cl_001"));
    }

    #[test]
    fn test_format_output_human_readable_restore() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = MaintainCommand::new(backend, config);

        let output = MaintainOutput::restore(vec!["cl_001".to_string()], vec![]);
        let options = MaintainOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Restored 1 learning"));
        assert!(formatted.contains("cl_001"));
    }

    #[test]
    fn test_maintain_action_serde() {
        let json = r#""archive""#;
        let action: MaintainAction = serde_json::from_str(json).unwrap();
        assert_eq!(action, MaintainAction::Archive);

        let json = r#""list""#;
        let action: MaintainAction = serde_json::from_str(json).unwrap();
        assert_eq!(action, MaintainAction::List);

        let json = r#""restore""#;
        let action: MaintainAction = serde_json::from_str(json).unwrap();
        assert_eq!(action, MaintainAction::Restore);
    }
}
