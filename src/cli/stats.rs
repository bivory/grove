//! Stats command for Grove.
//!
//! Displays quality dashboard with insights.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{project_stats_log_path, stats_cache_path, Config};
use crate::core::{LearningCategory, WriteGateCriterion};
use crate::stats::{
    generate_insights, AggregateStats, Insight, InsightConfig, ReflectionStats, StatsCache,
    StatsCacheManager, WriteGateStats,
};

/// Options for the stats command.
#[derive(Debug, Clone, Default)]
pub struct StatsOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Show detailed stats.
    pub detailed: bool,
    /// Force rebuild the cache.
    pub rebuild: bool,
}

/// Output format for the stats command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsOutput {
    /// Whether stats were loaded successfully.
    pub success: bool,
    /// Aggregate statistics.
    pub aggregates: AggregateStatsInfo,
    /// Reflection statistics.
    pub reflections: ReflectionStatsInfo,
    /// Write gate statistics.
    pub write_gate: WriteGateStatsInfo,
    /// Generated insights.
    pub insights: Vec<InsightInfo>,
    /// Warnings (e.g., large learnings file).
    pub warnings: Vec<String>,
    /// Error message if stats failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Simplified aggregate stats for output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateStatsInfo {
    /// Total number of learnings.
    pub total_learnings: u32,
    /// Total archived learnings.
    pub total_archived: u32,
    /// Active learnings (total - archived).
    pub active_learnings: u32,
    /// Average hit rate.
    pub average_hit_rate: f64,
    /// Cross-pollination count.
    pub cross_pollination_count: usize,
}

impl From<&AggregateStats> for AggregateStatsInfo {
    fn from(stats: &AggregateStats) -> Self {
        Self {
            total_learnings: stats.total_learnings,
            total_archived: stats.total_archived,
            active_learnings: stats.total_learnings - stats.total_archived,
            average_hit_rate: stats.average_hit_rate,
            cross_pollination_count: stats.cross_pollination_count,
        }
    }
}

/// Simplified reflection stats for output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionStatsInfo {
    /// Number of completed reflections.
    pub completed: u32,
    /// Number of skipped reflections.
    pub skipped: u32,
    /// Skip rate.
    pub skip_rate: f64,
}

impl From<&ReflectionStats> for ReflectionStatsInfo {
    fn from(stats: &ReflectionStats) -> Self {
        let total = stats.completed + stats.skipped;
        let skip_rate = if total > 0 {
            stats.skipped as f64 / total as f64
        } else {
            0.0
        };
        Self {
            completed: stats.completed,
            skipped: stats.skipped,
            skip_rate,
        }
    }
}

/// Simplified write gate stats for output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteGateStatsInfo {
    /// Total candidates evaluated.
    pub total_evaluated: u32,
    /// Total accepted.
    pub total_accepted: u32,
    /// Total rejected.
    pub total_rejected: u32,
    /// Pass rate.
    pub pass_rate: f64,
}

impl From<&WriteGateStats> for WriteGateStatsInfo {
    fn from(stats: &WriteGateStats) -> Self {
        Self {
            total_evaluated: stats.total_evaluated,
            total_accepted: stats.total_accepted,
            total_rejected: stats.total_rejected,
            pass_rate: stats.pass_rate,
        }
    }
}

/// Simplified insight for output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightInfo {
    /// Insight kind.
    pub kind: String,
    /// Message.
    pub message: String,
    /// Suggestion.
    pub suggestion: String,
    /// Priority.
    pub priority: u8,
}

impl From<&Insight> for InsightInfo {
    fn from(insight: &Insight) -> Self {
        Self {
            kind: insight.kind.display_name().to_string(),
            message: insight.message.clone(),
            suggestion: insight.suggestion.clone(),
            priority: insight.priority,
        }
    }
}

impl StatsOutput {
    /// Create a successful output.
    pub fn success(cache: &StatsCache, insights: Vec<Insight>, warnings: Vec<String>) -> Self {
        Self {
            success: true,
            aggregates: AggregateStatsInfo::from(&cache.aggregates),
            reflections: ReflectionStatsInfo::from(&cache.reflections),
            write_gate: WriteGateStatsInfo::from(&cache.write_gate),
            insights: insights.iter().map(InsightInfo::from).collect(),
            warnings,
            error: None,
        }
    }

    /// Create an empty output (no stats).
    pub fn empty(warnings: Vec<String>) -> Self {
        Self {
            success: true,
            aggregates: AggregateStatsInfo {
                total_learnings: 0,
                total_archived: 0,
                active_learnings: 0,
                average_hit_rate: 0.0,
                cross_pollination_count: 0,
            },
            reflections: ReflectionStatsInfo {
                completed: 0,
                skipped: 0,
                skip_rate: 0.0,
            },
            write_gate: WriteGateStatsInfo {
                total_evaluated: 0,
                total_accepted: 0,
                total_rejected: 0,
                pass_rate: 0.0,
            },
            insights: Vec::new(),
            warnings,
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            aggregates: AggregateStatsInfo {
                total_learnings: 0,
                total_archived: 0,
                active_learnings: 0,
                average_hit_rate: 0.0,
                cross_pollination_count: 0,
            },
            reflections: ReflectionStatsInfo {
                completed: 0,
                skipped: 0,
                skip_rate: 0.0,
            },
            write_gate: WriteGateStatsInfo {
                total_evaluated: 0,
                total_accepted: 0,
                total_rejected: 0,
                pass_rate: 0.0,
            },
            insights: Vec::new(),
            warnings: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// The stats command implementation.
pub struct StatsCommand {
    #[allow(dead_code)]
    config: Config,
    project_path: std::path::PathBuf,
}

impl StatsCommand {
    /// Create a new stats command.
    pub fn new(config: Config, project_path: impl AsRef<Path>) -> Self {
        Self {
            config,
            project_path: project_path.as_ref().to_path_buf(),
        }
    }

    /// Run the stats command.
    pub fn run(&self, options: &StatsOptions) -> StatsOutput {
        let mut warnings = Vec::new();

        // Check learnings.md file size
        let learnings_path = self.project_path.join(".grove").join("learnings.md");
        if let Ok(metadata) = fs::metadata(&learnings_path) {
            let size_kb = metadata.len() / 1024;
            if size_kb > 500 {
                warnings.push(format!(
                    "learnings.md is {}KB (>500KB). Consider running 'grove maintain' to archive stale learnings.",
                    size_kb
                ));
            }
        }

        // Load or rebuild cache
        let log_path = project_stats_log_path(&self.project_path);
        let cache_path = match stats_cache_path() {
            Some(p) => p,
            None => return StatsOutput::empty(warnings),
        };
        let cache_manager = StatsCacheManager::new(&cache_path, &log_path);

        let cache = if options.rebuild {
            match cache_manager.force_rebuild() {
                Ok(c) => c,
                Err(e) => {
                    warnings.push(format!("Failed to rebuild stats cache: {}", e));
                    return StatsOutput::empty(warnings);
                }
            }
        } else {
            match cache_manager.load_or_rebuild() {
                Ok(c) => c,
                Err(e) => {
                    warnings.push(format!("Failed to load stats cache: {}", e));
                    return StatsOutput::empty(warnings);
                }
            }
        };

        // Generate insights
        // Build learning timestamps map from cache
        let learning_timestamps: HashMap<String, DateTime<Utc>> = cache
            .learnings
            .iter()
            .filter_map(|(id, stats)| stats.last_surfaced.map(|ts| (id.clone(), ts)))
            .collect();

        // Note: learning_categories, learning_criteria, and learning_context_files
        // would need to be loaded from the backend to enable full insights.
        // For now, pass empty maps.
        let learning_categories: HashMap<String, LearningCategory> = HashMap::new();
        let learning_criteria: HashMap<String, Vec<WriteGateCriterion>> = HashMap::new();
        let learning_context_files: HashMap<String, Vec<String>> = HashMap::new();

        let insight_config = InsightConfig::default();
        let decay_config = &self.config.decay;
        let now = Utc::now();
        let insights = generate_insights(
            &cache,
            &learning_timestamps,
            &learning_categories,
            &learning_criteria,
            &learning_context_files,
            decay_config,
            &insight_config,
            now,
        );

        StatsOutput::success(&cache, insights, warnings)
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &StatsOutput, options: &StatsOptions) -> String {
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
    fn format_human_readable(&self, output: &StatsOutput, options: &StatsOptions) -> String {
        if !output.success {
            return format!(
                "Stats failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            );
        }

        let mut lines = Vec::new();
        lines.push("=== Grove Quality Dashboard ===\n".to_string());

        // Warnings first
        for warning in &output.warnings {
            lines.push(format!("⚠ Warning: {}\n", warning));
        }

        // Learnings summary
        lines.push("📚 Learnings".to_string());
        lines.push(format!(
            "   Active: {} | Archived: {} | Total: {}",
            output.aggregates.active_learnings,
            output.aggregates.total_archived,
            output.aggregates.total_learnings
        ));
        lines.push(format!(
            "   Average hit rate: {:.1}%",
            output.aggregates.average_hit_rate * 100.0
        ));
        lines.push(format!(
            "   Cross-pollination events: {}\n",
            output.aggregates.cross_pollination_count
        ));

        // Reflections
        lines.push("🔄 Reflections".to_string());
        let total_reflections = output.reflections.completed + output.reflections.skipped;
        lines.push(format!(
            "   Completed: {} | Skipped: {} | Total: {}",
            output.reflections.completed, output.reflections.skipped, total_reflections
        ));
        lines.push(format!(
            "   Skip rate: {:.1}%\n",
            output.reflections.skip_rate * 100.0
        ));

        // Write gate
        if options.detailed {
            lines.push("🚪 Write Gate".to_string());
            lines.push(format!(
                "   Evaluated: {} | Accepted: {} | Rejected: {}",
                output.write_gate.total_evaluated,
                output.write_gate.total_accepted,
                output.write_gate.total_rejected
            ));
            lines.push(format!(
                "   Pass rate: {:.1}%\n",
                output.write_gate.pass_rate * 100.0
            ));
        }

        // Insights
        if !output.insights.is_empty() {
            lines.push("💡 Insights".to_string());
            for insight in &output.insights {
                lines.push(format!("   • {}: {}", insight.kind, insight.message));
                lines.push(format!("     → {}", insight.suggestion));
            }
            lines.push(String::new());
        }

        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::InsightKind;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> TempDir {
        let temp = TempDir::new().unwrap();
        let grove_dir = temp.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();
        temp
    }

    fn setup_with_stats() -> TempDir {
        let temp = setup();
        let log_path = temp.path().join(".grove").join("stats.log");

        // Write some sample stats events
        let events = r#"{"ts":"2026-01-01T00:00:00Z","v":"1.0","data":{"Reflection":{"session_id":"s1","candidates":2,"accepted":1,"categories":["Pattern"],"ticket_id":"T1","backend":"markdown"}}}
{"ts":"2026-01-02T00:00:00Z","v":"1.0","data":{"Skip":{"session_id":"s2","reason":"trivial","decider":"User","lines_changed":3,"ticket_id":"T2"}}}
{"ts":"2026-01-03T00:00:00Z","v":"1.0","data":{"Surfaced":{"learning_id":"cl_001","session_id":"s3"}}}
{"ts":"2026-01-04T00:00:00Z","v":"1.0","data":{"Referenced":{"learning_id":"cl_001","session_id":"s3","ticket_id":"T3"}}}
"#;
        fs::write(&log_path, events).unwrap();
        temp
    }

    #[test]
    fn test_stats_output_success() {
        let cache = StatsCache::default();
        let output = StatsOutput::success(&cache, vec![], vec![]);

        assert!(output.success);
        assert!(output.error.is_none());
    }

    #[test]
    fn test_stats_output_empty() {
        let output = StatsOutput::empty(vec!["warning".to_string()]);

        assert!(output.success);
        assert_eq!(output.aggregates.total_learnings, 0);
        assert_eq!(output.warnings.len(), 1);
    }

    #[test]
    fn test_stats_output_failure() {
        let output = StatsOutput::failure("test error");

        assert!(!output.success);
        assert_eq!(output.error, Some("test error".to_string()));
    }

    #[test]
    fn test_stats_basic() {
        let temp = setup_with_stats();
        let config = Config::default();

        let cmd = StatsCommand::new(config, temp.path());
        let options = StatsOptions::default();

        let output = cmd.run(&options);

        // The test verifies the command runs successfully.
        // Note: Stats may show 0 values because the cache is at user level
        // (~/.grove/stats-cache.json) while the log is at project level.
        assert!(output.success);
    }

    #[test]
    fn test_stats_empty_log() {
        let temp = setup();
        let config = Config::default();

        let cmd = StatsCommand::new(config, temp.path());
        let options = StatsOptions::default();

        let output = cmd.run(&options);

        assert!(output.success);
        assert_eq!(output.aggregates.total_learnings, 0);
    }

    #[test]
    fn test_stats_warns_on_large_learnings_file() {
        let temp = setup();

        // Create a large learnings.md file (>500KB)
        let learnings_path = temp.path().join(".grove").join("learnings.md");
        let large_content = "x".repeat(600 * 1024); // 600KB
        fs::write(&learnings_path, large_content).unwrap();

        let config = Config::default();
        let cmd = StatsCommand::new(config, temp.path());
        let options = StatsOptions::default();

        let output = cmd.run(&options);

        assert!(output.success);
        assert!(!output.warnings.is_empty());
        assert!(output.warnings[0].contains("500KB"));
    }

    #[test]
    fn test_stats_force_rebuild() {
        let temp = setup_with_stats();
        let config = Config::default();

        let cmd = StatsCommand::new(config, temp.path());
        let options = StatsOptions {
            rebuild: true,
            ..Default::default()
        };

        let output = cmd.run(&options);

        assert!(output.success);
    }

    #[test]
    fn test_format_output_json() {
        let temp = setup();
        let config = Config::default();
        let cmd = StatsCommand::new(config, temp.path());

        let output = StatsOutput::empty(vec![]);
        let options = StatsOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"total_learnings\": 0"));
    }

    #[test]
    fn test_format_output_quiet() {
        let temp = setup();
        let config = Config::default();
        let cmd = StatsCommand::new(config, temp.path());

        let output = StatsOutput::empty(vec![]);
        let options = StatsOptions {
            quiet: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_output_human_readable() {
        let temp = setup();
        let config = Config::default();
        let cmd = StatsCommand::new(config, temp.path());

        let output = StatsOutput::empty(vec![]);
        let options = StatsOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Grove Quality Dashboard"));
        assert!(formatted.contains("Learnings"));
        assert!(formatted.contains("Reflections"));
    }

    #[test]
    fn test_format_output_with_warnings() {
        let temp = setup();
        let config = Config::default();
        let cmd = StatsCommand::new(config, temp.path());

        let output = StatsOutput::empty(vec!["test warning".to_string()]);
        let options = StatsOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Warning"));
        assert!(formatted.contains("test warning"));
    }

    #[test]
    fn test_format_output_detailed() {
        let temp = setup();
        let config = Config::default();
        let cmd = StatsCommand::new(config, temp.path());

        let output = StatsOutput::empty(vec![]);
        let options = StatsOptions {
            detailed: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Write Gate"));
    }

    #[test]
    fn test_aggregate_stats_info_from() {
        use std::collections::HashMap;

        let stats = AggregateStats {
            total_learnings: 10,
            total_archived: 3,
            average_hit_rate: 0.75,
            cross_pollination_count: 5,
            by_category: HashMap::new(),
            by_scope: HashMap::new(),
        };

        let info = AggregateStatsInfo::from(&stats);
        assert_eq!(info.total_learnings, 10);
        assert_eq!(info.total_archived, 3);
        assert_eq!(info.active_learnings, 7);
        assert_eq!(info.cross_pollination_count, 5);
    }

    #[test]
    fn test_reflection_stats_info_from() {
        use std::collections::HashMap;

        let stats = ReflectionStats {
            completed: 8,
            skipped: 2,
            by_backend: HashMap::new(),
        };

        let info = ReflectionStatsInfo::from(&stats);
        assert_eq!(info.completed, 8);
        assert_eq!(info.skipped, 2);
        assert!((info.skip_rate - 0.2).abs() < 0.001);
    }

    #[test]
    fn test_write_gate_stats_info_from() {
        use std::collections::HashMap;

        let stats = WriteGateStats {
            total_evaluated: 20,
            total_accepted: 15,
            total_rejected: 5,
            pass_rate: 0.75,
            rejection_reasons: HashMap::new(),
            retrospective_misses: 0,
        };

        let info = WriteGateStatsInfo::from(&stats);
        assert_eq!(info.total_evaluated, 20);
        assert_eq!(info.total_accepted, 15);
        assert_eq!(info.total_rejected, 5);
        assert!((info.pass_rate - 0.75).abs() < 0.001);
    }

    #[test]
    fn test_insight_info_from() {
        let insight = Insight::new(
            InsightKind::DecayWarning,
            "Test message",
            "Test suggestion",
            1,
        );

        let info = InsightInfo::from(&insight);
        assert_eq!(info.kind, "Decay Warning");
        assert_eq!(info.message, "Test message");
        assert_eq!(info.suggestion, "Test suggestion");
        assert_eq!(info.priority, 1);
    }
}
