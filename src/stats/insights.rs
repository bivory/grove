//! Insights engine for Grove stats.
//!
//! This module generates actionable insights from the stats cache.
//! Insights are generated on-the-fly when `grove stats` runs.
//!
//! Stage 1 insights:
//! - DecayWarning: N learnings approaching decay threshold
//! - HighCrossPollination: Cross-pollination count growing
//!
//! Stage 2 insights (deferred): LowHitCategory, HighValueRare, SkipMiss,
//! WriteGateTooStrict, WriteGateTooLoose, RubberStamping, StaleTopLearning

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::config::DecayConfig;
use crate::stats::decay::get_decay_warnings;
use crate::stats::StatsCache;

/// An insight generated from stats analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct Insight {
    /// The type of insight.
    pub kind: InsightKind,
    /// Human-readable message describing the insight.
    pub message: String,
    /// Suggested action.
    pub suggestion: String,
    /// Priority level (lower = more important).
    pub priority: u8,
}

impl Insight {
    /// Create a new insight.
    pub fn new(
        kind: InsightKind,
        message: impl Into<String>,
        suggestion: impl Into<String>,
        priority: u8,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            suggestion: suggestion.into(),
            priority,
        }
    }
}

/// Types of insights that can be generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InsightKind {
    /// Learnings approaching decay threshold.
    DecayWarning,
    /// Cross-pollination count is notable.
    HighCrossPollination,
    // Stage 2 insights (not implemented yet)
    // LowHitCategory,
    // HighValueRare,
    // SkipMiss,
    // WriteGateTooStrict,
    // WriteGateTooLoose,
    // RubberStamping,
    // StaleTopLearning,
}

impl InsightKind {
    /// Get the display name for this insight kind.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::DecayWarning => "Decay Warning",
            Self::HighCrossPollination => "High Cross-Pollination",
        }
    }
}

/// Configuration for insight thresholds.
#[derive(Debug, Clone)]
pub struct InsightConfig {
    /// Minimum cross-pollination count to trigger insight.
    pub min_cross_pollination: usize,
    /// Number of days before decay to trigger warning.
    pub decay_warning_days: u32,
}

impl Default for InsightConfig {
    fn default() -> Self {
        Self {
            min_cross_pollination: 3,
            decay_warning_days: 7,
        }
    }
}

/// Generate a decay warning insight if learnings are approaching the threshold.
///
/// Returns an insight when there are learnings within `warning_days` of
/// the decay threshold.
pub fn generate_decay_warning(
    cache: &StatsCache,
    learning_timestamps: &HashMap<String, DateTime<Utc>>,
    decay_config: &DecayConfig,
    warning_days: u32,
    now: DateTime<Utc>,
) -> Option<Insight> {
    let warnings = get_decay_warnings(cache, learning_timestamps, decay_config, warning_days, now);

    if warnings.is_empty() {
        return None;
    }

    let count = warnings.len();
    let threshold = decay_config.passive_duration_days;

    let message = if count == 1 {
        format!("1 learning approaching {}-day archive threshold", threshold)
    } else {
        format!(
            "{} learnings approaching {}-day archive threshold",
            count, threshold
        )
    };

    Some(Insight::new(
        InsightKind::DecayWarning,
        message,
        "Run `grove maintain` to review stale learnings",
        2,
    ))
}

/// Generate a high cross-pollination insight if learnings are being referenced
/// across multiple tickets.
///
/// Returns an insight when cross-pollination count exceeds the threshold.
pub fn generate_cross_pollination_insight(cache: &StatsCache, min_count: usize) -> Option<Insight> {
    let count = cache.cross_pollination.len();

    if count < min_count {
        return None;
    }

    let message = if count == 1 {
        "1 learning referenced outside originating ticket".to_string()
    } else {
        format!(
            "{} learnings referenced outside originating ticket — compound effect active",
            count
        )
    };

    Some(Insight::new(
        InsightKind::HighCrossPollination,
        message,
        "Your learnings are proving valuable across contexts",
        3,
    ))
}

/// Generate all Stage 1 insights.
///
/// Returns a list of insights sorted by priority.
pub fn generate_all(
    cache: &StatsCache,
    learning_timestamps: &HashMap<String, DateTime<Utc>>,
    decay_config: &DecayConfig,
    insight_config: &InsightConfig,
    now: DateTime<Utc>,
) -> Vec<Insight> {
    let mut insights = Vec::new();

    // Decay warning
    if let Some(insight) = generate_decay_warning(
        cache,
        learning_timestamps,
        decay_config,
        insight_config.decay_warning_days,
        now,
    ) {
        insights.push(insight);
    }

    // Cross-pollination
    if let Some(insight) =
        generate_cross_pollination_insight(cache, insight_config.min_cross_pollination)
    {
        insights.push(insight);
    }

    // Sort by priority (lower number = higher priority)
    insights.sort_by_key(|i| i.priority);

    insights
}

/// Check if any insights would be generated.
pub fn has_insights(
    cache: &StatsCache,
    learning_timestamps: &HashMap<String, DateTime<Utc>>,
    decay_config: &DecayConfig,
    insight_config: &InsightConfig,
    now: DateTime<Utc>,
) -> bool {
    !generate_all(
        cache,
        learning_timestamps,
        decay_config,
        insight_config,
        now,
    )
    .is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{CrossPollinationEdge, LearningStats};
    use chrono::Duration;

    fn default_decay_config() -> DecayConfig {
        DecayConfig::default()
    }

    fn default_insight_config() -> InsightConfig {
        InsightConfig::default()
    }

    fn make_learning_stats() -> LearningStats {
        LearningStats {
            surfaced: 0,
            referenced: 0,
            dismissed: 0,
            corrected: 0,
            hit_rate: 0.0,
            last_surfaced: None,
            last_referenced: None,
            origin_ticket: None,
            referencing_tickets: vec![],
            archived: false,
        }
    }

    // DecayWarning tests

    #[test]
    fn test_decay_warning_no_learnings() {
        let cache = StatsCache::new();
        let timestamps = HashMap::new();
        let config = default_decay_config();
        let now = Utc::now();

        let insight = generate_decay_warning(&cache, &timestamps, &config, 7, now);
        assert!(insight.is_none());
    }

    #[test]
    fn test_decay_warning_fresh_learnings() {
        let now = Utc::now();
        let mut cache = StatsCache::new();
        cache
            .learnings
            .insert("L001".to_string(), make_learning_stats());

        let mut timestamps = HashMap::new();
        timestamps.insert("L001".to_string(), now - Duration::days(10));

        let config = default_decay_config();

        let insight = generate_decay_warning(&cache, &timestamps, &config, 7, now);
        assert!(insight.is_none());
    }

    #[test]
    fn test_decay_warning_approaching_threshold() {
        let now = Utc::now();
        let mut cache = StatsCache::new();
        cache
            .learnings
            .insert("L001".to_string(), make_learning_stats());

        let mut timestamps = HashMap::new();
        // 85 days old, 7-day warning window for 90-day threshold
        timestamps.insert("L001".to_string(), now - Duration::days(85));

        let config = default_decay_config();

        let insight = generate_decay_warning(&cache, &timestamps, &config, 7, now);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert_eq!(insight.kind, InsightKind::DecayWarning);
        assert!(insight.message.contains("1 learning"));
        assert!(insight.message.contains("90-day"));
    }

    #[test]
    fn test_decay_warning_multiple_learnings() {
        let now = Utc::now();
        let mut cache = StatsCache::new();
        cache
            .learnings
            .insert("L001".to_string(), make_learning_stats());
        cache
            .learnings
            .insert("L002".to_string(), make_learning_stats());

        let mut timestamps = HashMap::new();
        timestamps.insert("L001".to_string(), now - Duration::days(85));
        timestamps.insert("L002".to_string(), now - Duration::days(86));

        let config = default_decay_config();

        let insight = generate_decay_warning(&cache, &timestamps, &config, 7, now);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert!(insight.message.contains("2 learnings"));
    }

    // CrossPollination tests

    #[test]
    fn test_cross_pollination_no_edges() {
        let cache = StatsCache::new();

        let insight = generate_cross_pollination_insight(&cache, 3);
        assert!(insight.is_none());
    }

    #[test]
    fn test_cross_pollination_below_threshold() {
        let mut cache = StatsCache::new();
        cache.cross_pollination.push(CrossPollinationEdge {
            learning_id: "L001".to_string(),
            origin_ticket: "T001".to_string(),
            referenced_in: vec!["T002".to_string()],
        });
        cache.cross_pollination.push(CrossPollinationEdge {
            learning_id: "L002".to_string(),
            origin_ticket: "T001".to_string(),
            referenced_in: vec!["T003".to_string()],
        });

        // Threshold is 3
        let insight = generate_cross_pollination_insight(&cache, 3);
        assert!(insight.is_none());
    }

    #[test]
    fn test_cross_pollination_at_threshold() {
        let mut cache = StatsCache::new();
        for i in 1..=3 {
            cache.cross_pollination.push(CrossPollinationEdge {
                learning_id: format!("L{:03}", i),
                origin_ticket: "T001".to_string(),
                referenced_in: vec![format!("T{:03}", i + 100)],
            });
        }

        let insight = generate_cross_pollination_insight(&cache, 3);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert_eq!(insight.kind, InsightKind::HighCrossPollination);
        assert!(insight.message.contains("3 learnings"));
    }

    #[test]
    fn test_cross_pollination_above_threshold() {
        let mut cache = StatsCache::new();
        for i in 1..=9 {
            cache.cross_pollination.push(CrossPollinationEdge {
                learning_id: format!("L{:03}", i),
                origin_ticket: "T001".to_string(),
                referenced_in: vec![format!("T{:03}", i + 100)],
            });
        }

        let insight = generate_cross_pollination_insight(&cache, 3);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert!(insight.message.contains("9 learnings"));
        assert!(insight.message.contains("compound effect"));
    }

    // generate_all tests

    #[test]
    fn test_generate_all_empty() {
        let cache = StatsCache::new();
        let timestamps = HashMap::new();
        let decay_config = default_decay_config();
        let insight_config = default_insight_config();
        let now = Utc::now();

        let insights = generate_all(&cache, &timestamps, &decay_config, &insight_config, now);

        assert!(insights.is_empty());
    }

    #[test]
    fn test_generate_all_multiple_insights() {
        let now = Utc::now();

        let mut cache = StatsCache::new();

        // Add learning approaching decay
        cache
            .learnings
            .insert("L001".to_string(), make_learning_stats());

        // Add cross-pollination edges
        for i in 1..=4 {
            cache.cross_pollination.push(CrossPollinationEdge {
                learning_id: format!("L{:03}", i),
                origin_ticket: "T001".to_string(),
                referenced_in: vec![format!("T{:03}", i + 100)],
            });
        }

        let mut timestamps = HashMap::new();
        timestamps.insert("L001".to_string(), now - Duration::days(85));

        let decay_config = default_decay_config();
        let insight_config = default_insight_config();

        let insights = generate_all(&cache, &timestamps, &decay_config, &insight_config, now);

        assert_eq!(insights.len(), 2);
        // Should be sorted by priority (decay warning first with priority 2)
        assert_eq!(insights[0].kind, InsightKind::DecayWarning);
        assert_eq!(insights[1].kind, InsightKind::HighCrossPollination);
    }

    #[test]
    fn test_has_insights() {
        let now = Utc::now();

        let mut cache = StatsCache::new();
        for i in 1..=3 {
            cache.cross_pollination.push(CrossPollinationEdge {
                learning_id: format!("L{:03}", i),
                origin_ticket: "T001".to_string(),
                referenced_in: vec![format!("T{:03}", i + 100)],
            });
        }

        let timestamps = HashMap::new();
        let decay_config = default_decay_config();
        let insight_config = default_insight_config();

        assert!(has_insights(
            &cache,
            &timestamps,
            &decay_config,
            &insight_config,
            now,
        ));
    }

    // InsightKind tests

    #[test]
    fn test_insight_kind_display_name() {
        assert_eq!(InsightKind::DecayWarning.display_name(), "Decay Warning");
        assert_eq!(
            InsightKind::HighCrossPollination.display_name(),
            "High Cross-Pollination"
        );
    }

    // Insight tests

    #[test]
    fn test_insight_new() {
        let insight = Insight::new(
            InsightKind::DecayWarning,
            "Test message",
            "Test suggestion",
            1,
        );

        assert_eq!(insight.kind, InsightKind::DecayWarning);
        assert_eq!(insight.message, "Test message");
        assert_eq!(insight.suggestion, "Test suggestion");
        assert_eq!(insight.priority, 1);
    }

    // InsightConfig tests

    #[test]
    fn test_insight_config_defaults() {
        let config = InsightConfig::default();
        assert_eq!(config.min_cross_pollination, 3);
        assert_eq!(config.decay_warning_days, 7);
    }
}
