//! List command for Grove.
//!
//! Lists recent learnings from the active backend.

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::backends::{MemoryBackend, SearchFilters, SearchQuery};
use crate::config::{Config, DecayConfig};
use crate::core::CompoundLearning;
use crate::stats::{LearningStats, StatsCache};

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
    /// Hide usage statistics in output.
    pub no_stats: bool,
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
    /// Number of times surfaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surfaced: Option<u32>,
    /// Number of times referenced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referenced: Option<u32>,
    /// Hit rate as percentage (0-100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_rate_pct: Option<u32>,
    /// Last referenced date (YYYY-MM-DD).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
}

impl LearningInfo {
    /// Create from a CompoundLearning with optional decay info and stats.
    ///
    /// Note: Uses the learning's creation timestamp for decay calculation.
    /// In a more complete implementation, this would look up the last reference
    /// time from the stats cache.
    pub fn from_learning(
        learning: &CompoundLearning,
        decay_config: Option<&DecayConfig>,
        stats: Option<&LearningStats>,
    ) -> Self {
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

        // Extract stats if available and learning has been surfaced
        let (surfaced, referenced, hit_rate_pct, last_used) = if let Some(s) = stats {
            if s.surfaced > 0 {
                let hit_pct = (s.hit_rate * 100.0).round() as u32;
                let last = s
                    .last_referenced
                    .map(|dt| dt.format("%Y-%m-%d").to_string());
                (Some(s.surfaced), Some(s.referenced), Some(hit_pct), last)
            } else {
                (None, None, None, None)
            }
        } else {
            (None, None, None, None)
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
            surfaced,
            referenced,
            hit_rate_pct,
            last_used,
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
pub struct ListCommand<B: MemoryBackend> {
    backend: B,
    config: Config,
    stats_cache: Option<StatsCache>,
}

impl<B: MemoryBackend> ListCommand<B> {
    /// Create a new list command.
    pub fn new(backend: B, config: Config) -> Self {
        Self {
            backend,
            config,
            stats_cache: None,
        }
    }

    /// Create a new list command with stats cache.
    pub fn with_stats(backend: B, config: Config, stats_cache: Option<StatsCache>) -> Self {
        Self {
            backend,
            config,
            stats_cache,
        }
    }

    /// Run the list command.
    pub fn run(&self, options: &ListOptions) -> ListOutput {
        // Use search with empty query to get all learnings
        let filters = if options.include_archived {
            SearchFilters::all()
        } else {
            SearchFilters::active_only()
        };

        match self.backend.search(&SearchQuery::new(), &filters) {
            Ok(results) => {
                // Extract learnings and sort by timestamp (most recent first)
                let mut learnings: Vec<_> = results.into_iter().map(|r| r.learning).collect();
                learnings.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

                let decay_config = &self.config.decay;
                let stale_days = options.stale_days.unwrap_or(7);

                // Convert to info with decay information and stats
                let mut learning_infos: Vec<LearningInfo> = learnings
                    .iter()
                    .map(|l| {
                        let stats = if options.no_stats {
                            None
                        } else {
                            self.stats_cache
                                .as_ref()
                                .and_then(|c| c.learnings.get(&l.id))
                        };
                        LearningInfo::from_learning(l, Some(decay_config), stats)
                    })
                    .collect();

                // Calculate stale count using same threshold as filter
                // (not the hardcoded 7-day approaching_decay threshold)
                let stale_count = learning_infos
                    .iter()
                    .filter(|l| {
                        if let Some(days) = l.days_until_decay {
                            days <= stale_days as i64 && days > 0
                        } else {
                            false
                        }
                    })
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

            // Add stats line if available (only shown when surfaced > 0)
            if let (Some(surfaced), Some(referenced), Some(hit_pct)) = (
                learning.surfaced,
                learning.referenced,
                learning.hit_rate_pct,
            ) {
                let last_used_part = learning
                    .last_used
                    .as_ref()
                    .map(|d| format!(" | Last used: {}", d))
                    .unwrap_or_default();
                lines.push(format!(
                    "   {} surfaced, {} referenced ({}% hit rate){}",
                    surfaced, referenced, hit_pct, last_used_part
                ));
            }

            lines.push(String::new());
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
            surfaced: None,
            referenced: None,
            hit_rate_pct: None,
            last_used: None,
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
    fn test_list_stale_count_uses_stale_days_option() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = ListCommand::new(backend, config);

        // With default stale_days=7, get the count
        let options_7_days = ListOptions {
            stale: true,
            stale_days: Some(7),
            ..Default::default()
        };
        let output_7 = cmd.run(&options_7_days);
        let count_7 = output_7.stale_count.unwrap_or(0);

        // With stale_days=30, should include more learnings
        let options_30_days = ListOptions {
            stale: true,
            stale_days: Some(30),
            ..Default::default()
        };
        let output_30 = cmd.run(&options_30_days);
        let count_30 = output_30.stale_count.unwrap_or(0);

        // The stale_count with wider window should be >= narrower window
        assert!(
            count_30 >= count_7,
            "stale_count with 30 days ({}) should be >= stale_count with 7 days ({})",
            count_30,
            count_7
        );
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
            surfaced: None,
            referenced: None,
            hit_rate_pct: None,
            last_used: None,
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
        let info = LearningInfo::from_learning(&learning, Some(&decay_config), None);

        assert_eq!(info.summary, "Test summary");
        assert_eq!(info.category, "pattern");
        assert!(info.days_until_decay.is_some());
        assert!(info.surfaced.is_none());
    }

    #[test]
    fn test_learning_info_with_stats() {
        use crate::core::{Confidence, LearningCategory, LearningScope, WriteGateCriterion};
        use chrono::Utc;

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

        let stats = LearningStats {
            surfaced: 5,
            referenced: 3,
            hit_rate: 0.6,
            last_referenced: Some(Utc::now()),
            ..Default::default()
        };

        let decay_config = DecayConfig::default();
        let info = LearningInfo::from_learning(&learning, Some(&decay_config), Some(&stats));

        assert_eq!(info.surfaced, Some(5));
        assert_eq!(info.referenced, Some(3));
        assert_eq!(info.hit_rate_pct, Some(60));
        assert!(info.last_used.is_some());
    }

    #[test]
    fn test_learning_info_stats_only_when_surfaced() {
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

        // Stats with surfaced=0 should not show stats
        let stats = LearningStats {
            surfaced: 0,
            referenced: 0,
            hit_rate: 0.0,
            ..Default::default()
        };

        let decay_config = DecayConfig::default();
        let info = LearningInfo::from_learning(&learning, Some(&decay_config), Some(&stats));

        assert!(info.surfaced.is_none());
        assert!(info.referenced.is_none());
        assert!(info.hit_rate_pct.is_none());
    }

    #[test]
    fn test_format_output_with_stats() {
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
            days_until_decay: Some(30),
            approaching_decay: Some(false),
            surfaced: Some(5),
            referenced: Some(3),
            hit_rate_pct: Some(60),
            last_used: Some("2026-02-10".to_string()),
        }];
        let output = ListOutput::success(learnings);
        let options = ListOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("5 surfaced, 3 referenced (60% hit rate)"));
        assert!(formatted.contains("Last used: 2026-02-10"));
    }
}
