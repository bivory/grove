//! List command for Grove.
//!
//! Lists recent learnings from the active backend.

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::backends::{MemoryBackend, SearchFilters, SearchQuery};
use crate::config::{Config, DecayConfig};
use crate::core::CompoundLearning;
use crate::stats::{LearningStats, RejectedCandidateSummary, StatsCache};

/// Sort field for list output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortBy {
    /// Sort by creation timestamp (default).
    #[default]
    Created,
    /// Sort by last used/referenced date.
    LastUsed,
    /// Sort by hit rate (referenced/surfaced ratio).
    HitRate,
    /// Sort by number of times surfaced.
    Surfaced,
}

/// Sort direction for list output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortOrder {
    /// Descending order (default) - highest/newest first.
    #[default]
    Desc,
    /// Ascending order - lowest/oldest first.
    Asc,
}

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
    /// Sort field.
    pub sort_by: SortBy,
    /// Sort direction.
    pub sort_order: SortOrder,
    /// Show rejected candidates instead of accepted learnings.
    pub rejections: bool,
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
    /// Rejected candidates (when --rejections is used).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejections: Option<Vec<RejectionInfo>>,
    /// Error message if listing failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Information about a rejected learning candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectionInfo {
    /// Summary of the rejected candidate.
    pub summary: String,
    /// Tags associated with the rejected candidate.
    pub tags: Vec<String>,
    /// Why the candidate was rejected.
    pub reason: String,
    /// At which validation stage it was rejected.
    pub stage: String,
    /// When the candidate was rejected.
    pub rejected_at: String,
}

impl RejectionInfo {
    /// Create from a RejectedCandidateSummary.
    pub fn from_rejected(rejected: &RejectedCandidateSummary) -> Self {
        Self {
            summary: rejected.summary.clone(),
            tags: rejected.tags.clone(),
            reason: rejected.reason.clone(),
            stage: rejected.stage.clone(),
            rejected_at: rejected.rejected_at.format("%Y-%m-%d %H:%M").to_string(),
        }
    }
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
            rejections: None,
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
            rejections: None,
            error: None,
        }
    }

    /// Create a successful output with rejections.
    pub fn success_rejections(rejections: Vec<RejectionInfo>) -> Self {
        let count = rejections.len();
        Self {
            success: true,
            count,
            learnings: Vec::new(),
            stale_count: None,
            rejections: Some(rejections),
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
            rejections: None,
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
        // Handle rejections mode
        if options.rejections {
            return self.run_rejections(options);
        }

        // Use search with empty query to get all learnings
        let filters = if options.include_archived {
            SearchFilters::all()
        } else {
            SearchFilters::active_only()
        };

        match self.backend.search(&SearchQuery::new(), &filters) {
            Ok(results) => {
                // Extract learnings
                let mut learnings: Vec<_> = results.into_iter().map(|r| r.learning).collect();

                // Sort based on options
                self.sort_learnings(&mut learnings, options);

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

    /// Sort learnings based on the sort options.
    ///
    /// For stats-based sorts (last-used, hit-rate, surfaced), learnings without
    /// stats are sorted to the end, then by creation date (descending).
    fn sort_learnings(&self, learnings: &mut [CompoundLearning], options: &ListOptions) {
        use std::cmp::Ordering;

        let stats_cache = &self.stats_cache;

        learnings.sort_by(|a, b| {
            let cmp = match options.sort_by {
                SortBy::Created => {
                    // Sort by creation timestamp
                    b.timestamp.cmp(&a.timestamp)
                }
                SortBy::LastUsed => {
                    // Sort by last_referenced, learnings without stats go to end
                    let a_stats = stats_cache.as_ref().and_then(|c| c.learnings.get(&a.id));
                    let b_stats = stats_cache.as_ref().and_then(|c| c.learnings.get(&b.id));

                    match (
                        a_stats.and_then(|s| s.last_referenced),
                        b_stats.and_then(|s| s.last_referenced),
                    ) {
                        (Some(a_time), Some(b_time)) => b_time.cmp(&a_time),
                        (Some(_), None) => Ordering::Less, // a has stats, b doesn't -> a first
                        (None, Some(_)) => Ordering::Greater, // b has stats, a doesn't -> b first
                        (None, None) => b.timestamp.cmp(&a.timestamp), // Both no stats -> by created
                    }
                }
                SortBy::HitRate => {
                    // Sort by hit rate, learnings without stats go to end
                    let a_stats = stats_cache.as_ref().and_then(|c| c.learnings.get(&a.id));
                    let b_stats = stats_cache.as_ref().and_then(|c| c.learnings.get(&b.id));

                    match (
                        a_stats.filter(|s| s.surfaced > 0),
                        b_stats.filter(|s| s.surfaced > 0),
                    ) {
                        (Some(a_s), Some(b_s)) => b_s
                            .hit_rate
                            .partial_cmp(&a_s.hit_rate)
                            .unwrap_or(Ordering::Equal),
                        (Some(_), None) => Ordering::Less,
                        (None, Some(_)) => Ordering::Greater,
                        (None, None) => b.timestamp.cmp(&a.timestamp),
                    }
                }
                SortBy::Surfaced => {
                    // Sort by surfaced count, learnings without stats go to end
                    let a_stats = stats_cache.as_ref().and_then(|c| c.learnings.get(&a.id));
                    let b_stats = stats_cache.as_ref().and_then(|c| c.learnings.get(&b.id));

                    match (
                        a_stats.filter(|s| s.surfaced > 0),
                        b_stats.filter(|s| s.surfaced > 0),
                    ) {
                        (Some(a_s), Some(b_s)) => b_s.surfaced.cmp(&a_s.surfaced),
                        (Some(_), None) => Ordering::Less,
                        (None, Some(_)) => Ordering::Greater,
                        (None, None) => b.timestamp.cmp(&a.timestamp),
                    }
                }
            };

            // Apply sort order (reverse if ascending)
            match options.sort_order {
                SortOrder::Desc => cmp,
                SortOrder::Asc => cmp.reverse(),
            }
        });
    }

    /// Run the list command in rejections mode.
    fn run_rejections(&self, options: &ListOptions) -> ListOutput {
        let Some(cache) = &self.stats_cache else {
            return ListOutput::failure(
                "Stats cache not available. Run 'grove stats --rebuild' first.",
            );
        };

        if cache.recent_rejected.is_empty() {
            return ListOutput::success_rejections(Vec::new());
        }

        // Convert rejections to RejectionInfo, sorted by most recent first
        let mut rejections: Vec<RejectionInfo> = cache
            .recent_rejected
            .iter()
            .map(RejectionInfo::from_rejected)
            .collect();

        // Reverse to show most recent first (cache stores oldest first after trimming)
        rejections.reverse();

        // Apply sort order (default is desc = most recent first, asc = oldest first)
        if options.sort_order == SortOrder::Asc {
            rejections.reverse();
        }

        // Apply limit
        if let Some(limit) = options.limit {
            rejections.truncate(limit);
        }

        ListOutput::success_rejections(rejections)
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

        // Handle rejections mode
        if options.rejections {
            return self.format_rejections(output);
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

    /// Format rejections as human-readable text.
    fn format_rejections(&self, output: &ListOutput) -> String {
        let rejections = match &output.rejections {
            Some(r) => r,
            None => return "No rejections data available.\n".to_string(),
        };

        if rejections.is_empty() {
            return "No rejected candidates found.\n\nNote: Only the last 100 rejections are tracked.\n".to_string();
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "Found {} rejected candidate(s):\n",
            rejections.len()
        ));

        for (i, rejection) in rejections.iter().enumerate() {
            lines.push(format!(
                "{}. [{}] {}",
                i + 1,
                rejection.stage,
                rejection.summary
            ));
            lines.push(format!("   Reason: {}", rejection.reason));
            if !rejection.tags.is_empty() {
                lines.push(format!("   Tags: {}", rejection.tags.join(", ")));
            }
            lines.push(format!("   Rejected: {}", rejection.rejected_at));
            lines.push(String::new());
        }

        lines.push("Note: Only the last 100 rejections are tracked.".to_string());
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

    // Sorting tests

    #[test]
    fn test_sort_by_created_default() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = ListCommand::new(backend, config);
        let options = ListOptions::default(); // Default is SortBy::Created, SortOrder::Desc

        let output = cmd.run(&options);

        assert!(output.success);
        // Most recent should be first (default sort)
        if output.learnings.len() >= 2 {
            assert!(output.learnings[0].summary.contains("Recent"));
        }
    }

    #[test]
    fn test_sort_by_created_ascending() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = ListCommand::new(backend, config);
        let options = ListOptions {
            sort_by: SortBy::Created,
            sort_order: SortOrder::Asc,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        // Oldest should be first when ascending
        if output.learnings.len() >= 2 {
            assert!(output.learnings[0].summary.contains("Old"));
        }
    }

    #[test]
    fn test_sort_by_surfaced() {
        use std::collections::HashMap;

        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        // Create stats cache with different surfaced counts
        let mut learnings_stats = HashMap::new();
        learnings_stats.insert(
            "cl_20260101_001".to_string(),
            LearningStats {
                surfaced: 10,
                referenced: 5,
                hit_rate: 0.5,
                ..Default::default()
            },
        );
        learnings_stats.insert(
            "cl_20260101_002".to_string(),
            LearningStats {
                surfaced: 20,
                referenced: 15,
                hit_rate: 0.75,
                ..Default::default()
            },
        );

        let stats_cache = StatsCache {
            learnings: learnings_stats,
            ..Default::default()
        };

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            sort_by: SortBy::Surfaced,
            sort_order: SortOrder::Desc,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        // Higher surfaced count should be first
        if output.learnings.len() >= 2 {
            assert!(
                output.learnings[0].surfaced.unwrap_or(0)
                    >= output.learnings[1].surfaced.unwrap_or(0)
            );
        }
    }

    #[test]
    fn test_sort_by_hit_rate() {
        use std::collections::HashMap;

        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        // Create stats cache with different hit rates
        let mut learnings_stats = HashMap::new();
        learnings_stats.insert(
            "cl_20260101_001".to_string(),
            LearningStats {
                surfaced: 10,
                referenced: 8,
                hit_rate: 0.8,
                ..Default::default()
            },
        );
        learnings_stats.insert(
            "cl_20260101_002".to_string(),
            LearningStats {
                surfaced: 10,
                referenced: 3,
                hit_rate: 0.3,
                ..Default::default()
            },
        );

        let stats_cache = StatsCache {
            learnings: learnings_stats,
            ..Default::default()
        };

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            sort_by: SortBy::HitRate,
            sort_order: SortOrder::Desc,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        // Higher hit rate should be first
        if output.learnings.len() >= 2 {
            assert!(
                output.learnings[0].hit_rate_pct.unwrap_or(0)
                    >= output.learnings[1].hit_rate_pct.unwrap_or(0)
            );
        }
    }

    #[test]
    fn test_sort_by_last_used() {
        use std::collections::HashMap;

        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let now = Utc::now();

        // Create stats cache with different last_referenced times
        let mut learnings_stats = HashMap::new();
        learnings_stats.insert(
            "cl_20260101_001".to_string(),
            LearningStats {
                surfaced: 5,
                referenced: 3,
                hit_rate: 0.6,
                last_referenced: Some(now - Duration::days(1)), // Used yesterday
                ..Default::default()
            },
        );
        learnings_stats.insert(
            "cl_20260101_002".to_string(),
            LearningStats {
                surfaced: 5,
                referenced: 3,
                hit_rate: 0.6,
                last_referenced: Some(now - Duration::days(10)), // Used 10 days ago
                ..Default::default()
            },
        );

        let stats_cache = StatsCache {
            learnings: learnings_stats,
            ..Default::default()
        };

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            sort_by: SortBy::LastUsed,
            sort_order: SortOrder::Desc,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        // More recently used should be first
        if output.learnings.len() >= 2 {
            // First learning should have been used more recently
            assert!(output.learnings[0].last_used.is_some());
        }
    }

    #[test]
    fn test_sort_learnings_without_stats_go_to_end() {
        use std::collections::HashMap;

        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        // Create stats cache with stats for only one learning
        let mut learnings_stats = HashMap::new();
        learnings_stats.insert(
            "cl_20260101_001".to_string(),
            LearningStats {
                surfaced: 10,
                referenced: 5,
                hit_rate: 0.5,
                last_referenced: Some(Utc::now()),
                ..Default::default()
            },
        );
        // cl_20260101_002 has no stats

        let stats_cache = StatsCache {
            learnings: learnings_stats,
            ..Default::default()
        };

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            sort_by: SortBy::Surfaced, // Sort by stats field
            sort_order: SortOrder::Desc,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        if output.learnings.len() >= 2 {
            // Learning with stats should be first
            assert!(output.learnings[0].surfaced.is_some());
            // Learning without stats should be at end
            assert!(output.learnings.last().unwrap().surfaced.is_none());
        }
    }

    #[test]
    fn test_sort_order_enum_default() {
        assert_eq!(SortOrder::default(), SortOrder::Desc);
    }

    #[test]
    fn test_sort_by_enum_default() {
        assert_eq!(SortBy::default(), SortBy::Created);
    }

    // Rejections tests

    #[test]
    fn test_rejections_no_cache() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        // No stats cache
        let cmd = ListCommand::new(backend, config);
        let options = ListOptions {
            rejections: true,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(!output.success);
        assert!(output.error.is_some());
        assert!(output.error.unwrap().contains("Stats cache not available"));
    }

    #[test]
    fn test_rejections_empty() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        // Empty stats cache
        let stats_cache = StatsCache::default();

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            rejections: true,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.count, 0);
        assert!(output.rejections.is_some());
        assert!(output.rejections.unwrap().is_empty());
    }

    #[test]
    fn test_rejections_with_data() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        // Create stats cache with rejections
        let mut stats_cache = StatsCache::default();
        let now = Utc::now();
        stats_cache.track_rejected_candidate(
            "First rejection",
            vec!["tag1".to_string()],
            "too short",
            "schema",
            now,
        );
        stats_cache.track_rejected_candidate(
            "Second rejection",
            vec!["tag2".to_string(), "tag3".to_string()],
            "near duplicate",
            "duplicate",
            now,
        );

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            rejections: true,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.count, 2);
        assert!(output.rejections.is_some());
        let rejections = output.rejections.unwrap();
        assert_eq!(rejections.len(), 2);
        // Most recent first (descending order)
        assert_eq!(rejections[0].summary, "Second rejection");
        assert_eq!(rejections[0].reason, "near duplicate");
        assert_eq!(rejections[0].stage, "duplicate");
        assert_eq!(rejections[1].summary, "First rejection");
        assert_eq!(rejections[1].reason, "too short");
        assert_eq!(rejections[1].stage, "schema");
    }

    #[test]
    fn test_rejections_with_limit() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        // Create stats cache with multiple rejections
        let mut stats_cache = StatsCache::default();
        let now = Utc::now();
        for i in 0..10 {
            stats_cache.track_rejected_candidate(
                &format!("Rejection {}", i),
                vec![],
                "test reason",
                "schema",
                now,
            );
        }

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            rejections: true,
            limit: Some(3),
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.count, 3);
        assert!(output.rejections.is_some());
        assert_eq!(output.rejections.unwrap().len(), 3);
    }

    #[test]
    fn test_rejections_ascending_order() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        // Create stats cache with rejections
        let mut stats_cache = StatsCache::default();
        let now = Utc::now();
        stats_cache.track_rejected_candidate("First rejection", vec![], "test", "schema", now);
        stats_cache.track_rejected_candidate("Second rejection", vec![], "test", "schema", now);

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            rejections: true,
            sort_order: SortOrder::Asc,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        let rejections = output.rejections.unwrap();
        // Ascending order: oldest first
        assert_eq!(rejections[0].summary, "First rejection");
        assert_eq!(rejections[1].summary, "Second rejection");
    }

    #[test]
    fn test_rejections_json_output() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let mut stats_cache = StatsCache::default();
        stats_cache.track_rejected_candidate(
            "Test rejection",
            vec!["test".to_string()],
            "too short",
            "schema",
            Utc::now(),
        );

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            rejections: true,
            json: true,
            ..Default::default()
        };

        let output = cmd.run(&options);
        let formatted = cmd.format_output(&output, &options);

        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"rejections\""));
        assert!(formatted.contains("Test rejection"));
        assert!(formatted.contains("\"reason\""));
        assert!(formatted.contains("\"stage\""));
        assert!(formatted.contains("too short"));
        assert!(formatted.contains("schema"));
    }

    #[test]
    fn test_rejections_human_readable() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let mut stats_cache = StatsCache::default();
        stats_cache.track_rejected_candidate(
            "Test rejection summary",
            vec!["tag1".to_string()],
            "summary too short",
            "schema",
            Utc::now(),
        );

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            rejections: true,
            ..Default::default()
        };

        let output = cmd.run(&options);
        let formatted = cmd.format_output(&output, &options);

        assert!(formatted.contains("Found 1 rejected candidate(s)"));
        assert!(formatted.contains("Test rejection summary"));
        assert!(formatted.contains("Tags: tag1"));
        assert!(formatted.contains("Reason: summary too short"));
        assert!(formatted.contains("[schema]")); // Stage shown in brackets
        assert!(formatted.contains("Rejected:"));
    }

    #[test]
    fn test_rejection_info_from_rejected() {
        let rejected = RejectedCandidateSummary {
            summary: "Test summary".to_string(),
            tags: vec!["tag1".to_string(), "tag2".to_string()],
            reason: "test reason".to_string(),
            stage: "schema".to_string(),
            rejected_at: Utc::now(),
        };

        let info = RejectionInfo::from_rejected(&rejected);

        assert_eq!(info.summary, "Test summary");
        assert_eq!(info.tags, vec!["tag1", "tag2"]);
        assert_eq!(info.reason, "test reason");
        assert_eq!(info.stage, "schema");
        assert!(!info.rejected_at.is_empty());
    }

    #[test]
    fn test_rejections_quiet_mode() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let mut stats_cache = StatsCache::default();
        stats_cache.track_rejected_candidate(
            "Test rejection",
            vec![],
            "test",
            "schema",
            Utc::now(),
        );

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            rejections: true,
            quiet: true,
            ..Default::default()
        };

        let output = cmd.run(&options);
        let formatted = cmd.format_output(&output, &options);

        assert!(output.success);
        assert!(
            formatted.is_empty(),
            "quiet mode should return empty string"
        );
    }

    #[test]
    fn test_rejections_max_capacity() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let mut stats_cache = StatsCache::default();
        let now = Utc::now();

        // Add 100 rejections (the max stored by stats cache)
        for i in 0..100 {
            stats_cache.track_rejected_candidate(
                &format!("Rejection {}", i),
                vec![],
                "test",
                "schema",
                now,
            );
        }

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            rejections: true,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.count, 100);
        assert!(output.rejections.is_some());
        assert_eq!(output.rejections.unwrap().len(), 100);
    }

    #[test]
    fn test_rejections_output_has_empty_learnings() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let mut stats_cache = StatsCache::default();
        stats_cache.track_rejected_candidate(
            "Test rejection",
            vec![],
            "test",
            "schema",
            Utc::now(),
        );

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            rejections: true,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
        assert!(
            output.learnings.is_empty(),
            "learnings should be empty in rejections mode"
        );
        assert!(output.rejections.is_some());
    }

    #[test]
    fn test_rejections_error_message_contains_guidance() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        // No stats cache
        let cmd = ListCommand::new(backend, config);
        let options = ListOptions {
            rejections: true,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(!output.success);
        let error = output.error.unwrap();
        assert!(
            error.contains("grove stats --rebuild"),
            "error message should contain helpful guidance"
        );
    }

    #[test]
    fn test_rejections_with_empty_tags() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let mut stats_cache = StatsCache::default();
        stats_cache.track_rejected_candidate(
            "No tags rejection",
            vec![],
            "test",
            "schema",
            Utc::now(),
        );

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            rejections: true,
            ..Default::default()
        };

        let output = cmd.run(&options);
        let formatted = cmd.format_output(&output, &options);

        assert!(output.success);
        // Should not include "Tags:" line when tags are empty
        assert!(
            !formatted.contains("Tags:"),
            "should not show Tags line when tags are empty"
        );
    }

    #[test]
    fn test_rejections_ignores_include_archived() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let mut stats_cache = StatsCache::default();
        stats_cache.track_rejected_candidate(
            "Test rejection",
            vec![],
            "test",
            "schema",
            Utc::now(),
        );

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            rejections: true,
            include_archived: true, // Should be ignored in rejections mode
            ..Default::default()
        };

        let output = cmd.run(&options);

        // Should still work and return rejections, not learnings
        assert!(output.success);
        assert!(output.rejections.is_some());
        assert!(output.learnings.is_empty());
    }

    #[test]
    fn test_rejections_special_characters() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let mut stats_cache = StatsCache::default();
        stats_cache.track_rejected_candidate(
            "Summary with \"quotes\" and <brackets>",
            vec!["tag-with-dash".to_string()],
            "test reason",
            "schema",
            Utc::now(),
        );

        let cmd = ListCommand::with_stats(backend, config, Some(stats_cache));
        let options = ListOptions {
            rejections: true,
            ..Default::default()
        };

        let output = cmd.run(&options);
        let formatted = cmd.format_output(&output, &options);

        assert!(output.success);
        assert!(formatted.contains("\"quotes\""));
        assert!(formatted.contains("<brackets>"));
    }
}
