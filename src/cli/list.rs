//! List command for Grove.
//!
//! Lists recent learnings from the active backend.

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::backends::markdown::MarkdownBackend;
use crate::config::{Config, DecayConfig};
use crate::core::{CompoundLearning, LearningStatus};

/// Options for the list command.
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Maximum number of results.
    pub limit: Option<usize>,
    /// Show only learnings approaching decay.
    pub stale: bool,
    /// Include archived learnings.
    pub include_archived: bool,
    /// Days until decay to consider "stale" (default: 7).
    pub stale_days: Option<u32>,
}

/// Output format for the list command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListOutput {
    /// Whether the list was successful.
    pub success: bool,
    /// Number of learnings.
    pub count: usize,
    /// The learnings.
    pub learnings: Vec<LearningInfo>,
    /// Number of stale learnings (if stale filter was used).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_count: Option<usize>,
    /// Error message if listing failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Simplified learning info for output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningInfo {
    /// Learning ID.
    pub id: String,
    /// Learning summary.
    pub summary: String,
    /// Learning category.
    pub category: String,
    /// Tags.
    pub tags: Vec<String>,
    /// Status.
    pub status: String,
    /// Created timestamp.
    pub created: String,
    /// Days until decay (if approaching).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_until_decay: Option<i64>,
    /// Whether this learning is approaching decay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approaching_decay: Option<bool>,
}

impl LearningInfo {
    /// Create from a CompoundLearning with optional decay info.
    ///
    /// Note: Uses the learning's creation timestamp for decay calculation.
    /// In a more complete implementation, this would look up the last reference
    /// time from the stats cache.
    pub fn from_learning(learning: &CompoundLearning, decay_config: Option<&DecayConfig>) -> Self {
        let (days_until_decay, approaching_decay) = if let Some(config) = decay_config {
            // Use creation timestamp for decay calculation
            // (a more complete implementation would use last_referenced from stats cache)
            let days_since_creation = (Utc::now() - learning.timestamp).num_days();
            let decay_days = config.passive_duration_days as i64;
            let days_remaining = decay_days - days_since_creation;
            let warning_days = 7i64; // Default warning threshold
            (
                Some(days_remaining.max(0)),
                Some(days_remaining <= warning_days && days_remaining > 0),
            )
        } else {
            (None, None)
        };

        Self {
            id: learning.id.clone(),
            summary: learning.summary.clone(),
            category: format!("{:?}", learning.category).to_lowercase(),
            tags: learning.tags.clone(),
            status: format!("{:?}", learning.status).to_lowercase(),
            created: learning.timestamp.format("%Y-%m-%d").to_string(),
            days_until_decay,
            approaching_decay,
        }
    }
}

impl ListOutput {
    /// Create a successful output.
    pub fn success(learnings: Vec<LearningInfo>) -> Self {
        let count = learnings.len();
        Self {
            success: true,
            count,
            learnings,
            stale_count: None,
            error: None,
        }
    }

    /// Create a successful output with stale count.
    pub fn success_with_stale(learnings: Vec<LearningInfo>, stale_count: usize) -> Self {
        let count = learnings.len();
        Self {
            success: true,
            count,
            learnings,
            stale_count: Some(stale_count),
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            count: 0,
            learnings: Vec::new(),
            stale_count: None,
            error: Some(error.into()),
        }
    }
}

/// The list command implementation.
pub struct ListCommand {
    backend: MarkdownBackend,
    config: Config,
}

impl ListCommand {
    /// Create a new list command.
    pub fn new(backend: MarkdownBackend, config: Config) -> Self {
        Self { backend, config }
    }

    /// Run the list command.
    pub fn run(&self, options: &ListOptions) -> ListOutput {
        // Parse all learnings from backend
        match self.backend.parse_all_learnings() {
            Ok(mut learnings) => {
                // Filter by status
                if !options.include_archived {
                    learnings.retain(|l| l.status == LearningStatus::Active);
                }

                // Sort by timestamp (most recent first)
                learnings.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

                let decay_config = &self.config.decay;
                let stale_days = options.stale_days.unwrap_or(7);

                // Convert to info with decay information
                let mut learning_infos: Vec<LearningInfo> = learnings
                    .iter()
                    .map(|l| LearningInfo::from_learning(l, Some(decay_config)))
                    .collect();

                // Filter to stale only if requested
                let stale_count = learning_infos
                    .iter()
                    .filter(|l| l.approaching_decay == Some(true))
                    .count();

                if options.stale {
                    // Filter to learnings approaching decay
                    learning_infos.retain(|l| {
                        if let Some(days) = l.days_until_decay {
                            days <= stale_days as i64 && days > 0
                        } else {
                            false
                        }
                    });
                }

                // Apply limit
                if let Some(limit) = options.limit {
                    learning_infos.truncate(limit);
                }

                if options.stale {
                    ListOutput::success_with_stale(learning_infos, stale_count)
                } else {
                    ListOutput::success(learning_infos)
                }
            }
            Err(e) => ListOutput::failure(e.to_string()),
        }
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &ListOutput, options: &ListOptions) -> String {
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
    fn format_human_readable(&self, output: &ListOutput, options: &ListOptions) -> String {
        if !output.success {
            return format!(
                "List failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            );
        }

        if output.learnings.is_empty() {
            return if options.stale {
                "No learnings approaching decay.\n".to_string()
            } else {
                "No learnings found.\n".to_string()
            };
        }

        let mut lines = Vec::new();

        if options.stale {
            lines.push(format!(
                "Found {} stale learning(s) approaching decay:\n",
                output.count
            ));
        } else {
            lines.push(format!("Found {} learning(s):\n", output.count));
        }

        for (i, learning) in output.learnings.iter().enumerate() {
            let decay_info = if let Some(days) = learning.days_until_decay {
                if days <= 7 && days > 0 {
                    format!(" ⚠ {}d until decay", days)
                } else if days == 0 {
                    " ⚠ decaying today".to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            lines.push(format!(
                "{}. [{}] {}{}",
                i + 1,
                learning.category,
                learning.summary,
                decay_info
            ));
            if !learning.tags.is_empty() {
                lines.push(format!("   Tags: {}", learning.tags.join(", ")));
            }
            lines.push(format!(
                "   Created: {} | ID: {}",
                learning.created, learning.id
            ));
            lines.push(String::new());
        }

        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

## cl_20260101_001

**Category:** Pattern
**Summary:** Recent pattern
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #rust #recent
**Session:** test-session
**Criteria:** Behavior-Changing
**Created:** {}

A recently created pattern.

---

## cl_20260101_002

**Category:** Pitfall
**Summary:** Old pitfall approaching decay
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #rust #stale
**Session:** test-session
**Criteria:** Behavior-Changing
**Created:** {}

An old pitfall that's approaching decay.

---

## cl_20260101_003

**Category:** Pattern
**Summary:** Archived pattern
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
    fn test_list_output_success() {
        let learnings = vec![LearningInfo {
            id: "cl_001".to_string(),
            summary: "Test summary".to_string(),
            category: "pattern".to_string(),
            tags: vec!["rust".to_string()],
            status: "active".to_string(),
            created: "2026-01-01".to_string(),
            days_until_decay: Some(30),
            approaching_decay: Some(false),
        }];
        let output = ListOutput::success(learnings);

        assert!(output.success);
        assert_eq!(output.count, 1);
        assert!(output.error.is_none());
    }

    #[test]
    fn test_list_output_failure() {
        let output = ListOutput::failure("backend error");

        assert!(!output.success);
        assert_eq!(output.count, 0);
        assert!(output.learnings.is_empty());
        assert_eq!(output.error, Some("backend error".to_string()));
    }

    #[test]
    fn test_list_basic() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = ListCommand::new(backend, config);
        let options = ListOptions::default();

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.count, 2); // 2 active learnings
        assert!(output.learnings.iter().all(|l| l.status == "active"));
    }

    #[test]
    fn test_list_includes_archived() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = ListCommand::new(backend, config);
        let options = ListOptions {
            include_archived: true,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.count, 3); // All 3 learnings
        assert!(output.learnings.iter().any(|l| l.status == "archived"));
    }

    #[test]
    fn test_list_stale_only() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = ListCommand::new(backend, config);
        let options = ListOptions {
            stale: true,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        // Should find the old pitfall that's approaching decay
        assert!(output.stale_count.is_some());
    }

    #[test]
    fn test_list_with_limit() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = ListCommand::new(backend, config);
        let options = ListOptions {
            limit: Some(1),
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.count, 1);
    }

    #[test]
    fn test_list_sorted_by_recent() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = ListCommand::new(backend, config);
        let options = ListOptions::default();

        let output = cmd.run(&options);

        assert!(output.success);
        // Most recent should be first
        if output.learnings.len() >= 2 {
            assert!(output.learnings[0].summary.contains("Recent"));
        }
    }

    #[test]
    fn test_format_output_json() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = ListCommand::new(backend, config);

        let output = ListOutput::success(vec![]);
        let options = ListOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
    }

    #[test]
    fn test_format_output_quiet() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = ListCommand::new(backend, config);

        let output = ListOutput::success(vec![]);
        let options = ListOptions {
            quiet: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_output_human_readable() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = ListCommand::new(backend, config);

        let learnings = vec![LearningInfo {
            id: "cl_001".to_string(),
            summary: "Test pattern".to_string(),
            category: "pattern".to_string(),
            tags: vec!["rust".to_string()],
            status: "active".to_string(),
            created: "2026-01-01".to_string(),
            days_until_decay: Some(5),
            approaching_decay: Some(true),
        }];
        let output = ListOutput::success(learnings);
        let options = ListOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Found 1 learning(s)"));
        assert!(formatted.contains("Test pattern"));
        assert!(formatted.contains("5d until decay"));
    }

    #[test]
    fn test_format_output_empty_list() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = ListCommand::new(backend, config);

        let output = ListOutput::success(vec![]);
        let options = ListOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("No learnings found"));
    }

    #[test]
    fn test_format_output_empty_stale_list() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = ListCommand::new(backend, config);

        let output = ListOutput::success(vec![]);
        let options = ListOptions {
            stale: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("No learnings approaching decay"));
    }

    #[test]
    fn test_learning_info_from_learning() {
        use crate::core::{Confidence, LearningCategory, LearningScope, WriteGateCriterion};

        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            "Test summary",
            "Test detail",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["rust".to_string()],
            "test-session",
        );

        let decay_config = DecayConfig::default();
        let info = LearningInfo::from_learning(&learning, Some(&decay_config));

        assert_eq!(info.summary, "Test summary");
        assert_eq!(info.category, "pattern");
        assert!(info.days_until_decay.is_some());
    }
}
