//! Insights engine for Grove stats.
//!
//! This module generates actionable insights from the stats cache.
//! Insights are generated on-the-fly when `grove stats` runs.
//!
//! Implemented insights:
//! - DecayWarning: N learnings approaching decay threshold
//! - HighCrossPollination: Cross-pollination count growing
//! - StaleTopLearning: Top learning hasn't been referenced recently
//! - LowHitCategory: Category with hit rate < 0.3 over 10+ learnings
//! - HighValueRare: Category with hit rate > 0.7 but < 5 learnings
//! - RubberStamping: >90% of learnings claim same write gate criterion
//! - WriteGateTooStrict: Write gate pass rate < 0.5
//! - WriteGateTooLoose: Write gate pass rate > 0.95 but hit rate < 0.3
//! - SkipMiss: Skipped session on ticket later produced learnings

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::config::DecayConfig;
use crate::core::{LearningCategory, WriteGateCriterion};
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
    /// Top learning hasn't been referenced recently.
    StaleTopLearning,
    /// Category with low hit rate.
    LowHitCategory,
    /// Category with high hit rate but few learnings.
    HighValueRare,
    /// >90% of learnings claim the same write gate criterion.
    RubberStamping,
    /// Write gate pass rate < 0.5.
    WriteGateTooStrict,
    /// Write gate pass rate > 0.95 but hit rate < 0.3.
    WriteGateTooLoose,
    /// Skipped session on ticket later produced learnings.
    SkipMiss,
}

impl InsightKind {
    /// Get the display name for this insight kind.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::DecayWarning => "Decay Warning",
            Self::HighCrossPollination => "High Cross-Pollination",
            Self::StaleTopLearning => "Stale Top Learning",
            Self::LowHitCategory => "Low Hit Category",
            Self::HighValueRare => "High Value Rare",
            Self::RubberStamping => "Rubber Stamping",
            Self::WriteGateTooStrict => "Write Gate Too Strict",
            Self::WriteGateTooLoose => "Write Gate Too Loose",
            Self::SkipMiss => "Skip Miss",
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
    /// Days without reference before flagging top learning as stale.
    pub stale_top_learning_days: u32,
    /// Minimum learnings in a category to trigger low hit category insight.
    pub low_hit_category_min_learnings: usize,
    /// Hit rate threshold below which a category is considered low-performing.
    pub low_hit_category_threshold: f64,
    /// Maximum learnings in a category for high value rare insight.
    pub high_value_rare_max_learnings: usize,
    /// Hit rate threshold above which a category is considered high-value.
    pub high_value_rare_threshold: f64,
    /// Minimum learnings to evaluate for rubber stamping insight.
    pub rubber_stamping_min_learnings: usize,
    /// Threshold above which criterion usage is considered rubber stamping.
    pub rubber_stamping_threshold: f64,
    /// Minimum evaluations to trigger write gate strictness insight.
    pub write_gate_min_evaluations: u32,
    /// Pass rate threshold below which write gate is considered too strict.
    pub write_gate_too_strict_threshold: f64,
    /// Pass rate threshold above which write gate may be too loose.
    pub write_gate_too_loose_pass_rate: f64,
    /// Hit rate threshold below which write gate is considered too loose.
    pub write_gate_too_loose_hit_rate: f64,
    /// Minimum skip misses to trigger the insight.
    pub skip_miss_min_count: usize,
}

impl Default for InsightConfig {
    fn default() -> Self {
        Self {
            min_cross_pollination: 3,
            decay_warning_days: 7,
            stale_top_learning_days: 60,
            low_hit_category_min_learnings: 10,
            low_hit_category_threshold: 0.3,
            high_value_rare_max_learnings: 5,
            high_value_rare_threshold: 0.7,
            rubber_stamping_min_learnings: 10,
            rubber_stamping_threshold: 0.9,
            write_gate_min_evaluations: 10,
            write_gate_too_strict_threshold: 0.5,
            write_gate_too_loose_pass_rate: 0.95,
            write_gate_too_loose_hit_rate: 0.3,
            skip_miss_min_count: 1,
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

/// Generate an insight when the most-referenced learning hasn't been
/// referenced recently.
///
/// Returns an insight when the learning with the highest hit rate hasn't
/// been referenced in `stale_days` or more.
pub fn generate_stale_top_learning_insight(
    cache: &StatsCache,
    stale_days: u32,
    now: DateTime<Utc>,
) -> Option<Insight> {
    // Find the learning with the highest hit rate that isn't archived
    let top_learning = cache
        .learnings
        .iter()
        .filter(|(_, stats)| !stats.archived && stats.surfaced > 0 && !stats.hit_rate.is_nan())
        .max_by(|(_, a), (_, b)| a.hit_rate.total_cmp(&b.hit_rate));

    let (learning_id, stats) = top_learning?;

    // Check if it's been referenced at all
    let last_referenced = stats.last_referenced?;

    // Check if it's stale (not referenced in stale_days)
    let days_since = (now - last_referenced).num_days();
    if days_since < stale_days as i64 {
        return None;
    }

    // Only trigger if this learning has proven valuable (hit rate > 0.5)
    if stats.hit_rate < 0.5 {
        return None;
    }

    let message = format!(
        "Top learning '{}' hasn't been referenced in {} days (hit rate: {:.0}%)",
        learning_id,
        days_since,
        stats.hit_rate * 100.0
    );

    Some(Insight::new(
        InsightKind::StaleTopLearning,
        message,
        "Verify this learning is still accurate and relevant",
        2,
    ))
}

/// Generate an insight when a learning category has a low hit rate.
///
/// Returns an insight when a category has `min_learnings` or more learnings
/// and an average hit rate below `threshold`.
pub fn generate_low_hit_category_insight(
    cache: &StatsCache,
    learning_categories: &HashMap<String, LearningCategory>,
    min_learnings: usize,
    threshold: f64,
) -> Option<Insight> {
    // Aggregate stats by category
    let mut category_stats: HashMap<LearningCategory, (u32, f64)> = HashMap::new();

    for (learning_id, stats) in &cache.learnings {
        // Skip archived learnings
        if stats.archived {
            continue;
        }

        // Only consider learnings that have been surfaced
        if stats.surfaced == 0 {
            continue;
        }

        // Skip NaN hit rates (invalid data)
        if stats.hit_rate.is_nan() {
            continue;
        }

        // Get category for this learning
        if let Some(category) = learning_categories.get(learning_id) {
            let entry = category_stats.entry(*category).or_insert((0, 0.0));
            entry.0 += 1; // count
            entry.1 += stats.hit_rate; // sum of hit rates
        }
    }

    // Find categories with min_learnings+ and hit rate < threshold
    let mut low_hit_categories: Vec<(LearningCategory, u32, f64)> = Vec::new();

    for (category, (count, hit_rate_sum)) in category_stats {
        if count >= min_learnings as u32 {
            let avg_hit_rate = hit_rate_sum / count as f64;
            if avg_hit_rate < threshold {
                low_hit_categories.push((category, count, avg_hit_rate));
            }
        }
    }

    if low_hit_categories.is_empty() {
        return None;
    }

    // Sort by hit rate (lowest first)
    low_hit_categories.sort_by(|a, b| a.2.total_cmp(&b.2));

    // Take the worst performing category
    let (category, count, avg_hit_rate) = &low_hit_categories[0];

    let message = format!(
        "{:?} category has {:.0}% average hit rate across {} learnings",
        category,
        avg_hit_rate * 100.0,
        count
    );

    Some(Insight::new(
        InsightKind::LowHitCategory,
        message,
        "Consider being more specific when capturing learnings in this category",
        2,
    ))
}

/// Generate an insight when a learning category has high value but few learnings.
///
/// Returns an insight when a category has fewer than `max_learnings` learnings
/// but an average hit rate above `threshold`.
pub fn generate_high_value_rare_insight(
    cache: &StatsCache,
    learning_categories: &HashMap<String, LearningCategory>,
    max_learnings: usize,
    threshold: f64,
) -> Option<Insight> {
    // Aggregate stats by category
    let mut category_stats: HashMap<LearningCategory, (u32, f64)> = HashMap::new();

    for (learning_id, stats) in &cache.learnings {
        // Skip archived learnings
        if stats.archived {
            continue;
        }

        // Only consider learnings that have been surfaced
        if stats.surfaced == 0 {
            continue;
        }

        // Skip NaN hit rates (invalid data)
        if stats.hit_rate.is_nan() {
            continue;
        }

        // Get category for this learning
        if let Some(category) = learning_categories.get(learning_id) {
            let entry = category_stats.entry(*category).or_insert((0, 0.0));
            entry.0 += 1; // count
            entry.1 += stats.hit_rate; // sum of hit rates
        }
    }

    // Find categories with < max_learnings and hit rate > threshold
    let mut high_value_categories: Vec<(LearningCategory, u32, f64)> = Vec::new();

    for (category, (count, hit_rate_sum)) in category_stats {
        // Must have at least 1 learning but fewer than max
        if count > 0 && count < max_learnings as u32 {
            let avg_hit_rate = hit_rate_sum / count as f64;
            if avg_hit_rate > threshold {
                high_value_categories.push((category, count, avg_hit_rate));
            }
        }
    }

    if high_value_categories.is_empty() {
        return None;
    }

    // Sort by hit rate (highest first)
    high_value_categories.sort_by(|a, b| b.2.total_cmp(&a.2));

    // Take the best performing rare category
    let (category, count, avg_hit_rate) = &high_value_categories[0];

    let message = format!(
        "{:?} category has {:.0}% hit rate with only {} learning(s)",
        category,
        avg_hit_rate * 100.0,
        count
    );

    Some(Insight::new(
        InsightKind::HighValueRare,
        message,
        "Consider capturing more learnings in this high-value category",
        3,
    ))
}

/// Generate an insight when >90% of learnings claim the same write gate criterion.
///
/// Returns an insight when a single criterion is claimed by more than `threshold`
/// percent of learnings, suggesting the agent may not be properly evaluating
/// which criterion applies.
pub fn generate_rubber_stamping_insight(
    learning_criteria: &HashMap<String, Vec<WriteGateCriterion>>,
    min_learnings: usize,
    threshold: f64,
) -> Option<Insight> {
    // Need enough learnings to make a meaningful assessment
    if learning_criteria.len() < min_learnings {
        return None;
    }

    // Count occurrences of each criterion across all learnings
    let mut criterion_counts: HashMap<WriteGateCriterion, u32> = HashMap::new();
    let mut total_learnings = 0u32;

    for criteria in learning_criteria.values() {
        // Count each unique criterion per learning (don't double-count if same criterion listed twice)
        let unique_criteria: std::collections::HashSet<_> = criteria.iter().collect();
        for criterion in unique_criteria {
            *criterion_counts.entry(*criterion).or_insert(0) += 1;
        }
        total_learnings += 1;
    }

    if total_learnings == 0 {
        return None;
    }

    // Find the most-used criterion
    let (top_criterion, top_count) = criterion_counts.iter().max_by_key(|(_, count)| *count)?;

    let usage_rate = *top_count as f64 / total_learnings as f64;

    // Only trigger if usage exceeds threshold
    if usage_rate <= threshold {
        return None;
    }

    let message = format!(
        "{:.0}% of learnings claim '{}' criterion ({}/{})",
        usage_rate * 100.0,
        top_criterion.display_name(),
        top_count,
        total_learnings
    );

    Some(Insight::new(
        InsightKind::RubberStamping,
        message,
        "Ensure each learning's criterion is evaluated individually, not picked by default",
        1, // High priority - indicates potential non-evaluation
    ))
}

/// Generate an insight when the write gate pass rate is too low.
///
/// Returns an insight when the write gate pass rate is below `threshold`,
/// suggesting the write gate criteria may be too strict.
pub fn generate_write_gate_too_strict_insight(
    cache: &StatsCache,
    min_evaluations: u32,
    threshold: f64,
) -> Option<Insight> {
    // Need enough evaluations to make a meaningful assessment
    if cache.write_gate.total_evaluated < min_evaluations {
        return None;
    }

    // Check if pass rate is below threshold
    if cache.write_gate.pass_rate >= threshold {
        return None;
    }

    let message = format!(
        "Write gate pass rate is {:.0}% ({}/{} accepted)",
        cache.write_gate.pass_rate * 100.0,
        cache.write_gate.total_accepted,
        cache.write_gate.total_evaluated
    );

    Some(Insight::new(
        InsightKind::WriteGateTooStrict,
        message,
        "Consider relaxing write gate criteria or improving candidate quality",
        2, // Medium priority
    ))
}

/// Generate an insight when the write gate is too loose.
///
/// Returns an insight when the write gate pass rate is above `pass_rate_threshold`
/// but the average hit rate is below `hit_rate_threshold`, suggesting the write
/// gate criteria may be too loose (accepting too many low-quality learnings).
pub fn generate_write_gate_too_loose_insight(
    cache: &StatsCache,
    min_evaluations: u32,
    pass_rate_threshold: f64,
    hit_rate_threshold: f64,
) -> Option<Insight> {
    // Need enough evaluations to make a meaningful assessment
    if cache.write_gate.total_evaluated < min_evaluations {
        return None;
    }

    // Need some accepted learnings to calculate hit rate
    if cache.write_gate.total_accepted == 0 {
        return None;
    }

    // Check if pass rate is high (potentially too loose)
    if cache.write_gate.pass_rate <= pass_rate_threshold {
        return None;
    }

    // Calculate average hit rate across all surfaced learnings
    let surfaced_learnings: Vec<_> = cache
        .learnings
        .iter()
        .filter(|(_, stats)| !stats.archived && stats.surfaced > 0)
        .collect();

    // Need some surfaced learnings to assess quality
    if surfaced_learnings.is_empty() {
        return None;
    }

    let total_hit_rate: f64 = surfaced_learnings.iter().map(|(_, s)| s.hit_rate).sum();
    let avg_hit_rate = total_hit_rate / surfaced_learnings.len() as f64;

    // Only trigger if average hit rate is low (learnings aren't useful)
    if avg_hit_rate >= hit_rate_threshold {
        return None;
    }

    let message = format!(
        "Write gate pass rate is {:.0}% but average hit rate is only {:.0}%",
        cache.write_gate.pass_rate * 100.0,
        avg_hit_rate * 100.0
    );

    Some(Insight::new(
        InsightKind::WriteGateTooLoose,
        message,
        "Consider tightening write gate criteria to filter low-quality learnings",
        2, // Medium priority
    ))
}

/// Generate an insight when skipped sessions later produced learnings on the same ticket.
/// Reason why a skip miss was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipMissReason {
    /// Learning was created for a ticket that had a skipped session.
    TicketMatch,
    /// Learning's context files overlap with files from a skipped session.
    FileOverlap,
}

///
/// Returns an insight when a ticket had a skipped reflection but later produced
/// learnings, suggesting the skip may have been premature.
///
/// Detection methods:
/// 1. Ticket-based: Learning's origin ticket matches a previously skipped ticket
/// 2. File-based: Learning's context files overlap with files from skipped sessions
pub fn generate_skip_miss_insight(
    cache: &StatsCache,
    learning_context_files: &HashMap<String, Vec<String>>,
    min_count: usize,
) -> Option<Insight> {
    // Find learnings that match skipped sessions (by ticket or file overlap)
    let mut skip_misses: Vec<(&String, SkipMissReason)> = Vec::new();

    for (learning_id, stats) in &cache.learnings {
        // Skip archived learnings
        if stats.archived {
            continue;
        }

        // Check ticket-based correlation first
        if let Some(ref origin_ticket) = stats.origin_ticket {
            if cache.skipped_tickets.contains(origin_ticket) {
                skip_misses.push((learning_id, SkipMissReason::TicketMatch));
                continue; // Don't double-count
            }
        }

        // Check file-based correlation
        if let Some(files) = learning_context_files.get(learning_id) {
            if cache.has_skipped_file_overlap(files) {
                skip_misses.push((learning_id, SkipMissReason::FileOverlap));
            }
        }
    }

    if skip_misses.len() < min_count {
        return None;
    }

    // Count by reason type
    let ticket_matches = skip_misses
        .iter()
        .filter(|(_, r)| *r == SkipMissReason::TicketMatch)
        .count();
    let file_overlaps = skip_misses
        .iter()
        .filter(|(_, r)| *r == SkipMissReason::FileOverlap)
        .count();

    let message = if skip_misses.len() == 1 {
        let reason = if ticket_matches == 1 {
            "ticket match"
        } else {
            "file overlap"
        };
        format!(
            "1 learning was created for a session that was previously skipped ({})",
            reason
        )
    } else {
        let mut parts = Vec::new();
        if ticket_matches > 0 {
            parts.push(format!("{} by ticket", ticket_matches));
        }
        if file_overlaps > 0 {
            parts.push(format!("{} by file overlap", file_overlaps));
        }
        format!(
            "{} learnings were created for sessions that were previously skipped ({})",
            skip_misses.len(),
            parts.join(", ")
        )
    };

    Some(Insight::new(
        InsightKind::SkipMiss,
        message,
        "Review skip decisions — these sessions may have had valuable learnings",
        2, // Medium priority
    ))
}

/// Generate all insights.
///
/// Returns a list of insights sorted by priority.
#[allow(clippy::too_many_arguments)]
pub fn generate_all(
    cache: &StatsCache,
    learning_timestamps: &HashMap<String, DateTime<Utc>>,
    learning_categories: &HashMap<String, LearningCategory>,
    learning_criteria: &HashMap<String, Vec<WriteGateCriterion>>,
    learning_context_files: &HashMap<String, Vec<String>>,
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

    // Stale top learning
    if let Some(insight) =
        generate_stale_top_learning_insight(cache, insight_config.stale_top_learning_days, now)
    {
        insights.push(insight);
    }

    // Low hit category
    if let Some(insight) = generate_low_hit_category_insight(
        cache,
        learning_categories,
        insight_config.low_hit_category_min_learnings,
        insight_config.low_hit_category_threshold,
    ) {
        insights.push(insight);
    }

    // High value rare category
    if let Some(insight) = generate_high_value_rare_insight(
        cache,
        learning_categories,
        insight_config.high_value_rare_max_learnings,
        insight_config.high_value_rare_threshold,
    ) {
        insights.push(insight);
    }

    // Rubber stamping
    if let Some(insight) = generate_rubber_stamping_insight(
        learning_criteria,
        insight_config.rubber_stamping_min_learnings,
        insight_config.rubber_stamping_threshold,
    ) {
        insights.push(insight);
    }

    // Write gate too strict
    if let Some(insight) = generate_write_gate_too_strict_insight(
        cache,
        insight_config.write_gate_min_evaluations,
        insight_config.write_gate_too_strict_threshold,
    ) {
        insights.push(insight);
    }

    // Write gate too loose
    if let Some(insight) = generate_write_gate_too_loose_insight(
        cache,
        insight_config.write_gate_min_evaluations,
        insight_config.write_gate_too_loose_pass_rate,
        insight_config.write_gate_too_loose_hit_rate,
    ) {
        insights.push(insight);
    }

    // Skip miss
    if let Some(insight) = generate_skip_miss_insight(
        cache,
        learning_context_files,
        insight_config.skip_miss_min_count,
    ) {
        insights.push(insight);
    }

    // Sort by priority (lower number = higher priority)
    insights.sort_by_key(|i| i.priority);

    insights
}

/// Check if any insights would be generated.
#[allow(clippy::too_many_arguments)]
pub fn has_insights(
    cache: &StatsCache,
    learning_timestamps: &HashMap<String, DateTime<Utc>>,
    learning_categories: &HashMap<String, LearningCategory>,
    learning_criteria: &HashMap<String, Vec<WriteGateCriterion>>,
    learning_context_files: &HashMap<String, Vec<String>>,
    decay_config: &DecayConfig,
    insight_config: &InsightConfig,
    now: DateTime<Utc>,
) -> bool {
    !generate_all(
        cache,
        learning_timestamps,
        learning_categories,
        learning_criteria,
        learning_context_files,
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
        let categories = HashMap::new();
        let criteria = HashMap::new();
        let context_files = HashMap::new();
        let decay_config = default_decay_config();
        let insight_config = default_insight_config();
        let now = Utc::now();

        let insights = generate_all(
            &cache,
            &timestamps,
            &categories,
            &criteria,
            &context_files,
            &decay_config,
            &insight_config,
            now,
        );

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

        let categories = HashMap::new();
        let criteria = HashMap::new();
        let context_files = HashMap::new();
        let decay_config = default_decay_config();
        let insight_config = default_insight_config();

        let insights = generate_all(
            &cache,
            &timestamps,
            &categories,
            &criteria,
            &context_files,
            &decay_config,
            &insight_config,
            now,
        );

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
        let categories = HashMap::new();
        let criteria = HashMap::new();
        let context_files = HashMap::new();
        let decay_config = default_decay_config();
        let insight_config = default_insight_config();

        assert!(has_insights(
            &cache,
            &timestamps,
            &categories,
            &criteria,
            &context_files,
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
        assert_eq!(
            InsightKind::StaleTopLearning.display_name(),
            "Stale Top Learning"
        );
        assert_eq!(
            InsightKind::LowHitCategory.display_name(),
            "Low Hit Category"
        );
        assert_eq!(InsightKind::HighValueRare.display_name(), "High Value Rare");
        assert_eq!(
            InsightKind::RubberStamping.display_name(),
            "Rubber Stamping"
        );
        assert_eq!(
            InsightKind::WriteGateTooStrict.display_name(),
            "Write Gate Too Strict"
        );
        assert_eq!(
            InsightKind::WriteGateTooLoose.display_name(),
            "Write Gate Too Loose"
        );
        assert_eq!(InsightKind::SkipMiss.display_name(), "Skip Miss");
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
        assert_eq!(config.stale_top_learning_days, 60);
        assert_eq!(config.low_hit_category_min_learnings, 10);
        assert!((config.low_hit_category_threshold - 0.3).abs() < f64::EPSILON);
        assert_eq!(config.high_value_rare_max_learnings, 5);
        assert!((config.high_value_rare_threshold - 0.7).abs() < f64::EPSILON);
        assert_eq!(config.rubber_stamping_min_learnings, 10);
        assert!((config.rubber_stamping_threshold - 0.9).abs() < f64::EPSILON);
        assert_eq!(config.write_gate_min_evaluations, 10);
        assert!((config.write_gate_too_strict_threshold - 0.5).abs() < f64::EPSILON);
        assert!((config.write_gate_too_loose_pass_rate - 0.95).abs() < f64::EPSILON);
        assert!((config.write_gate_too_loose_hit_rate - 0.3).abs() < f64::EPSILON);
        assert_eq!(config.skip_miss_min_count, 1);
    }

    // StaleTopLearning tests

    #[test]
    fn test_stale_top_learning_no_learnings() {
        let cache = StatsCache::new();
        let now = Utc::now();

        let insight = generate_stale_top_learning_insight(&cache, 60, now);
        assert!(insight.is_none());
    }

    #[test]
    fn test_stale_top_learning_fresh_reference() {
        let now = Utc::now();
        let mut cache = StatsCache::new();

        let mut stats = make_learning_stats();
        stats.surfaced = 10;
        stats.referenced = 6;
        stats.hit_rate = 0.6;
        stats.last_referenced = Some(now - Duration::days(30)); // Only 30 days ago

        cache.learnings.insert("L001".to_string(), stats);

        let insight = generate_stale_top_learning_insight(&cache, 60, now);
        assert!(insight.is_none()); // Not stale yet
    }

    #[test]
    fn test_stale_top_learning_triggers_after_threshold() {
        let now = Utc::now();
        let mut cache = StatsCache::new();

        let mut stats = make_learning_stats();
        stats.surfaced = 10;
        stats.referenced = 7;
        stats.hit_rate = 0.7;
        stats.last_referenced = Some(now - Duration::days(70)); // 70 days ago

        cache.learnings.insert("L001".to_string(), stats);

        let insight = generate_stale_top_learning_insight(&cache, 60, now);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert_eq!(insight.kind, InsightKind::StaleTopLearning);
        assert!(insight.message.contains("L001"));
        assert!(insight.message.contains("70 days"));
        assert!(insight.message.contains("70%"));
    }

    #[test]
    fn test_stale_top_learning_ignores_low_hit_rate() {
        let now = Utc::now();
        let mut cache = StatsCache::new();

        let mut stats = make_learning_stats();
        stats.surfaced = 10;
        stats.referenced = 3;
        stats.hit_rate = 0.3; // Below 0.5 threshold
        stats.last_referenced = Some(now - Duration::days(70));

        cache.learnings.insert("L001".to_string(), stats);

        let insight = generate_stale_top_learning_insight(&cache, 60, now);
        assert!(insight.is_none()); // Low hit rate, not a "top" learning
    }

    #[test]
    fn test_stale_top_learning_ignores_archived() {
        let now = Utc::now();
        let mut cache = StatsCache::new();

        let mut stats = make_learning_stats();
        stats.surfaced = 10;
        stats.referenced = 7;
        stats.hit_rate = 0.7;
        stats.last_referenced = Some(now - Duration::days(70));
        stats.archived = true;

        cache.learnings.insert("L001".to_string(), stats);

        let insight = generate_stale_top_learning_insight(&cache, 60, now);
        assert!(insight.is_none()); // Archived learnings are ignored
    }

    #[test]
    fn test_stale_top_learning_picks_highest_hit_rate() {
        let now = Utc::now();
        let mut cache = StatsCache::new();

        // Lower hit rate learning that's stale
        let mut stats1 = make_learning_stats();
        stats1.surfaced = 10;
        stats1.referenced = 5;
        stats1.hit_rate = 0.5;
        stats1.last_referenced = Some(now - Duration::days(90));
        cache.learnings.insert("L001".to_string(), stats1);

        // Higher hit rate learning that's also stale
        let mut stats2 = make_learning_stats();
        stats2.surfaced = 10;
        stats2.referenced = 8;
        stats2.hit_rate = 0.8;
        stats2.last_referenced = Some(now - Duration::days(70));
        cache.learnings.insert("L002".to_string(), stats2);

        let insight = generate_stale_top_learning_insight(&cache, 60, now);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        // Should pick L002 because it has higher hit rate
        assert!(insight.message.contains("L002"));
        assert!(insight.message.contains("80%"));
    }

    #[test]
    fn test_stale_top_learning_with_nan_hit_rate() {
        let now = Utc::now();
        let mut cache = StatsCache::new();

        // Learning with NaN hit rate (edge case)
        let mut stats1 = make_learning_stats();
        stats1.surfaced = 10;
        stats1.referenced = 8;
        stats1.hit_rate = f64::NAN;
        stats1.last_referenced = Some(now - Duration::days(90));
        cache.learnings.insert("L001".to_string(), stats1);

        // Learning with valid high hit rate
        let mut stats2 = make_learning_stats();
        stats2.surfaced = 10;
        stats2.referenced = 8;
        stats2.hit_rate = 0.8;
        stats2.last_referenced = Some(now - Duration::days(70));
        cache.learnings.insert("L002".to_string(), stats2);

        // Learning with valid low hit rate
        let mut stats3 = make_learning_stats();
        stats3.surfaced = 10;
        stats3.referenced = 6;
        stats3.hit_rate = 0.6;
        stats3.last_referenced = Some(now - Duration::days(80));
        cache.learnings.insert("L003".to_string(), stats3);

        let insight = generate_stale_top_learning_insight(&cache, 60, now);

        // Should still find the highest valid hit rate learning
        assert!(insight.is_some());
        let insight = insight.unwrap();
        // Should pick L002 because it has the highest valid hit rate
        // NaN values are sorted to the end by total_cmp
        assert!(insight.message.contains("L002"));
        assert!(insight.message.contains("80%"));
    }

    #[test]
    fn test_stale_top_learning_never_referenced() {
        let now = Utc::now();
        let mut cache = StatsCache::new();

        let mut stats = make_learning_stats();
        stats.surfaced = 10;
        stats.referenced = 0;
        stats.hit_rate = 0.0;
        stats.last_referenced = None; // Never referenced

        cache.learnings.insert("L001".to_string(), stats);

        let insight = generate_stale_top_learning_insight(&cache, 60, now);
        assert!(insight.is_none()); // No last_referenced to check
    }

    // LowHitCategory tests

    #[test]
    fn test_low_hit_category_no_learnings() {
        let cache = StatsCache::new();
        let categories = HashMap::new();

        let insight = generate_low_hit_category_insight(&cache, &categories, 10, 0.3);
        assert!(insight.is_none());
    }

    #[test]
    fn test_low_hit_category_below_min_learnings() {
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 5 learnings (below threshold of 10)
        for i in 1..=5 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 2;
            stats.hit_rate = 0.2;
            cache.learnings.insert(format!("L{:03}", i), stats);
            categories.insert(format!("L{:03}", i), LearningCategory::Pattern);
        }

        let insight = generate_low_hit_category_insight(&cache, &categories, 10, 0.3);
        assert!(insight.is_none()); // Not enough learnings in category
    }

    #[test]
    fn test_low_hit_category_triggers_at_threshold() {
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 10 learnings with low hit rate (0.2)
        for i in 1..=10 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 2;
            stats.hit_rate = 0.2;
            cache.learnings.insert(format!("L{:03}", i), stats);
            categories.insert(format!("L{:03}", i), LearningCategory::Pattern);
        }

        let insight = generate_low_hit_category_insight(&cache, &categories, 10, 0.3);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert_eq!(insight.kind, InsightKind::LowHitCategory);
        assert!(insight.message.contains("Pattern"));
        assert!(insight.message.contains("20%"));
        assert!(insight.message.contains("10 learnings"));
    }

    #[test]
    fn test_low_hit_category_above_threshold_no_trigger() {
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 10 learnings with hit rate above threshold (0.4)
        for i in 1..=10 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 4;
            stats.hit_rate = 0.4;
            cache.learnings.insert(format!("L{:03}", i), stats);
            categories.insert(format!("L{:03}", i), LearningCategory::Pattern);
        }

        let insight = generate_low_hit_category_insight(&cache, &categories, 10, 0.3);
        assert!(insight.is_none()); // Hit rate above threshold
    }

    #[test]
    fn test_low_hit_category_ignores_archived() {
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 10 archived learnings with low hit rate
        for i in 1..=10 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 1;
            stats.hit_rate = 0.1;
            stats.archived = true;
            cache.learnings.insert(format!("L{:03}", i), stats);
            categories.insert(format!("L{:03}", i), LearningCategory::Pattern);
        }

        let insight = generate_low_hit_category_insight(&cache, &categories, 10, 0.3);
        assert!(insight.is_none()); // Archived learnings are ignored
    }

    #[test]
    fn test_low_hit_category_ignores_unsurfaced() {
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 10 learnings that have never been surfaced
        for i in 1..=10 {
            let stats = make_learning_stats(); // surfaced = 0 by default
            cache.learnings.insert(format!("L{:03}", i), stats);
            categories.insert(format!("L{:03}", i), LearningCategory::Pattern);
        }

        let insight = generate_low_hit_category_insight(&cache, &categories, 10, 0.3);
        assert!(insight.is_none()); // Unsurfaced learnings are ignored
    }

    #[test]
    fn test_low_hit_category_picks_worst_category() {
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 10 Pattern learnings with 0.2 hit rate
        for i in 1..=10 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 2;
            stats.hit_rate = 0.2;
            cache.learnings.insert(format!("P{:03}", i), stats);
            categories.insert(format!("P{:03}", i), LearningCategory::Pattern);
        }

        // Add 10 Pitfall learnings with 0.1 hit rate (worse)
        for i in 1..=10 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 1;
            stats.hit_rate = 0.1;
            cache.learnings.insert(format!("F{:03}", i), stats);
            categories.insert(format!("F{:03}", i), LearningCategory::Pitfall);
        }

        let insight = generate_low_hit_category_insight(&cache, &categories, 10, 0.3);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        // Should pick Pitfall because it has lower hit rate
        assert!(insight.message.contains("Pitfall"));
        assert!(insight.message.contains("10%"));
    }

    #[test]
    fn test_low_hit_category_no_categories_for_learnings() {
        let mut cache = StatsCache::new();
        let categories = HashMap::new(); // Empty - no categories

        // Add 10 learnings with low hit rate but no category mapping
        for i in 1..=10 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 2;
            stats.hit_rate = 0.2;
            cache.learnings.insert(format!("L{:03}", i), stats);
        }

        let insight = generate_low_hit_category_insight(&cache, &categories, 10, 0.3);
        assert!(insight.is_none()); // No category mappings available
    }

    #[test]
    fn test_low_hit_category_in_generate_all() {
        let now = Utc::now();
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 10 learnings with low hit rate
        for i in 1..=10 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 2;
            stats.hit_rate = 0.2;
            cache.learnings.insert(format!("L{:03}", i), stats);
            categories.insert(format!("L{:03}", i), LearningCategory::Convention);
        }

        let timestamps = HashMap::new();
        let criteria = HashMap::new();
        let context_files = HashMap::new();
        let decay_config = default_decay_config();
        let insight_config = default_insight_config();

        let insights = generate_all(
            &cache,
            &timestamps,
            &categories,
            &criteria,
            &context_files,
            &decay_config,
            &insight_config,
            now,
        );

        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].kind, InsightKind::LowHitCategory);
        assert!(insights[0].message.contains("Convention"));
    }

    // HighValueRare tests

    #[test]
    fn test_high_value_rare_no_learnings() {
        let cache = StatsCache::new();
        let categories = HashMap::new();

        let insight = generate_high_value_rare_insight(&cache, &categories, 5, 0.7);
        assert!(insight.is_none());
    }

    #[test]
    fn test_high_value_rare_triggers_with_few_high_hit_learnings() {
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 3 learnings with high hit rate (0.8)
        for i in 1..=3 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 8;
            stats.hit_rate = 0.8;
            cache.learnings.insert(format!("L{:03}", i), stats);
            categories.insert(format!("L{:03}", i), LearningCategory::Domain);
        }

        let insight = generate_high_value_rare_insight(&cache, &categories, 5, 0.7);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert_eq!(insight.kind, InsightKind::HighValueRare);
        assert!(insight.message.contains("Domain"));
        assert!(insight.message.contains("80%"));
        assert!(insight.message.contains("3 learning"));
    }

    #[test]
    fn test_high_value_rare_no_trigger_with_many_learnings() {
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 6 learnings with high hit rate (above max_learnings=5)
        for i in 1..=6 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 8;
            stats.hit_rate = 0.8;
            cache.learnings.insert(format!("L{:03}", i), stats);
            categories.insert(format!("L{:03}", i), LearningCategory::Domain);
        }

        let insight = generate_high_value_rare_insight(&cache, &categories, 5, 0.7);
        assert!(insight.is_none()); // Too many learnings
    }

    #[test]
    fn test_high_value_rare_no_trigger_with_low_hit_rate() {
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 3 learnings with low hit rate (below threshold)
        for i in 1..=3 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 5;
            stats.hit_rate = 0.5;
            cache.learnings.insert(format!("L{:03}", i), stats);
            categories.insert(format!("L{:03}", i), LearningCategory::Domain);
        }

        let insight = generate_high_value_rare_insight(&cache, &categories, 5, 0.7);
        assert!(insight.is_none()); // Hit rate below threshold
    }

    #[test]
    fn test_high_value_rare_ignores_archived() {
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 3 archived learnings with high hit rate
        for i in 1..=3 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 9;
            stats.hit_rate = 0.9;
            stats.archived = true;
            cache.learnings.insert(format!("L{:03}", i), stats);
            categories.insert(format!("L{:03}", i), LearningCategory::Domain);
        }

        let insight = generate_high_value_rare_insight(&cache, &categories, 5, 0.7);
        assert!(insight.is_none()); // Archived learnings are ignored
    }

    #[test]
    fn test_high_value_rare_ignores_unsurfaced() {
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 3 learnings that have never been surfaced
        for i in 1..=3 {
            let stats = make_learning_stats(); // surfaced = 0 by default
            cache.learnings.insert(format!("L{:03}", i), stats);
            categories.insert(format!("L{:03}", i), LearningCategory::Domain);
        }

        let insight = generate_high_value_rare_insight(&cache, &categories, 5, 0.7);
        assert!(insight.is_none()); // Unsurfaced learnings are ignored
    }

    #[test]
    fn test_high_value_rare_picks_best_category() {
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 2 Domain learnings with 0.8 hit rate
        for i in 1..=2 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 8;
            stats.hit_rate = 0.8;
            cache.learnings.insert(format!("D{:03}", i), stats);
            categories.insert(format!("D{:03}", i), LearningCategory::Domain);
        }

        // Add 2 Process learnings with 0.9 hit rate (better)
        for i in 1..=2 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 9;
            stats.hit_rate = 0.9;
            cache.learnings.insert(format!("P{:03}", i), stats);
            categories.insert(format!("P{:03}", i), LearningCategory::Process);
        }

        let insight = generate_high_value_rare_insight(&cache, &categories, 5, 0.7);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        // Should pick Process because it has higher hit rate
        assert!(insight.message.contains("Process"));
        assert!(insight.message.contains("90%"));
    }

    #[test]
    fn test_high_value_rare_in_generate_all() {
        let now = Utc::now();
        let mut cache = StatsCache::new();
        let mut categories = HashMap::new();

        // Add 3 learnings with high hit rate
        for i in 1..=3 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 8;
            stats.hit_rate = 0.8;
            cache.learnings.insert(format!("L{:03}", i), stats);
            categories.insert(format!("L{:03}", i), LearningCategory::Debugging);
        }

        let timestamps = HashMap::new();
        let criteria = HashMap::new();
        let context_files = HashMap::new();
        let decay_config = default_decay_config();
        let insight_config = default_insight_config();

        let insights = generate_all(
            &cache,
            &timestamps,
            &categories,
            &criteria,
            &context_files,
            &decay_config,
            &insight_config,
            now,
        );

        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].kind, InsightKind::HighValueRare);
        assert!(insights[0].message.contains("Debugging"));
    }

    // RubberStamping tests

    #[test]
    fn test_rubber_stamping_no_learnings() {
        let criteria: HashMap<String, Vec<WriteGateCriterion>> = HashMap::new();

        let insight = generate_rubber_stamping_insight(&criteria, 10, 0.9);
        assert!(insight.is_none());
    }

    #[test]
    fn test_rubber_stamping_below_min_learnings() {
        let mut criteria: HashMap<String, Vec<WriteGateCriterion>> = HashMap::new();

        // Add 5 learnings (below threshold of 10)
        for i in 1..=5 {
            criteria.insert(
                format!("L{:03}", i),
                vec![WriteGateCriterion::BehaviorChanging],
            );
        }

        let insight = generate_rubber_stamping_insight(&criteria, 10, 0.9);
        assert!(insight.is_none()); // Not enough learnings
    }

    #[test]
    fn test_rubber_stamping_triggers_when_same_criterion() {
        let mut criteria: HashMap<String, Vec<WriteGateCriterion>> = HashMap::new();

        // Add 10 learnings all claiming BehaviorChanging (100%)
        for i in 1..=10 {
            criteria.insert(
                format!("L{:03}", i),
                vec![WriteGateCriterion::BehaviorChanging],
            );
        }

        let insight = generate_rubber_stamping_insight(&criteria, 10, 0.9);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert_eq!(insight.kind, InsightKind::RubberStamping);
        assert!(insight.message.contains("100%"));
        assert!(insight.message.contains("Behavior Changing"));
        assert!(insight.message.contains("10/10"));
    }

    #[test]
    fn test_rubber_stamping_just_above_threshold() {
        let mut criteria: HashMap<String, Vec<WriteGateCriterion>> = HashMap::new();

        // Add 10 learnings with 91% claiming same criterion (above 90% threshold)
        for i in 1..=10 {
            if i == 1 {
                // One different criterion
                criteria.insert(
                    format!("L{:03}", i),
                    vec![WriteGateCriterion::DecisionRationale],
                );
            } else {
                // 9 claiming the same (90% would equal threshold, need >90%)
                criteria.insert(format!("L{:03}", i), vec![WriteGateCriterion::StableFact]);
            }
        }

        let insight = generate_rubber_stamping_insight(&criteria, 10, 0.9);
        // 9/10 = 90% which is not >90%, so should not trigger
        assert!(insight.is_none());
    }

    #[test]
    fn test_rubber_stamping_just_below_threshold() {
        let mut criteria: HashMap<String, Vec<WriteGateCriterion>> = HashMap::new();

        // Add 10 learnings with 89% claiming same criterion (below 90% threshold)
        // We need to add more learnings to make this testable
        for i in 1..=12 {
            if i <= 2 {
                // Two different criteria
                criteria.insert(
                    format!("L{:03}", i),
                    vec![WriteGateCriterion::DecisionRationale],
                );
            } else {
                // 10 claiming the same (10/12 = 83%)
                criteria.insert(format!("L{:03}", i), vec![WriteGateCriterion::StableFact]);
            }
        }

        let insight = generate_rubber_stamping_insight(&criteria, 10, 0.9);
        assert!(insight.is_none()); // 83% is below 90%
    }

    #[test]
    fn test_rubber_stamping_multiple_criteria_per_learning() {
        let mut criteria: HashMap<String, Vec<WriteGateCriterion>> = HashMap::new();

        // Add 10 learnings, each claiming multiple criteria
        // All claim BehaviorChanging, but also claim other criteria
        for i in 1..=10 {
            criteria.insert(
                format!("L{:03}", i),
                vec![
                    WriteGateCriterion::BehaviorChanging,
                    WriteGateCriterion::DecisionRationale,
                ],
            );
        }

        let insight = generate_rubber_stamping_insight(&criteria, 10, 0.9);

        // 100% claim BehaviorChanging (should still trigger)
        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert!(insight.message.contains("100%"));
    }

    #[test]
    fn test_rubber_stamping_diverse_criteria_no_trigger() {
        let mut criteria: HashMap<String, Vec<WriteGateCriterion>> = HashMap::new();

        // Add 10 learnings with diverse criteria distribution
        let criterion_options = [
            WriteGateCriterion::BehaviorChanging,
            WriteGateCriterion::DecisionRationale,
            WriteGateCriterion::StableFact,
            WriteGateCriterion::ExplicitRequest,
        ];

        for i in 1..=10 {
            // Distribute evenly - each criterion gets 2-3 learnings
            let criterion = criterion_options[(i - 1) % 4];
            criteria.insert(format!("L{:03}", i), vec![criterion]);
        }

        let insight = generate_rubber_stamping_insight(&criteria, 10, 0.9);
        assert!(insight.is_none()); // No single criterion exceeds 90%
    }

    #[test]
    fn test_rubber_stamping_finds_most_common_criterion() {
        let mut criteria: HashMap<String, Vec<WriteGateCriterion>> = HashMap::new();

        // Add 11 learnings: 10 with StableFact, 1 with BehaviorChanging
        for i in 1..=11 {
            if i == 1 {
                criteria.insert(
                    format!("L{:03}", i),
                    vec![WriteGateCriterion::BehaviorChanging],
                );
            } else {
                criteria.insert(format!("L{:03}", i), vec![WriteGateCriterion::StableFact]);
            }
        }

        let insight = generate_rubber_stamping_insight(&criteria, 10, 0.9);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        // Should report StableFact (10/11 = 91%)
        assert!(insight.message.contains("Stable Fact"));
        assert!(insight.message.contains("10/11"));
    }

    #[test]
    fn test_rubber_stamping_unique_criteria_not_counted_twice() {
        let mut criteria: HashMap<String, Vec<WriteGateCriterion>> = HashMap::new();

        // Add 10 learnings, each claiming BehaviorChanging twice
        for i in 1..=10 {
            criteria.insert(
                format!("L{:03}", i),
                vec![
                    WriteGateCriterion::BehaviorChanging,
                    WriteGateCriterion::BehaviorChanging, // Duplicate
                ],
            );
        }

        let insight = generate_rubber_stamping_insight(&criteria, 10, 0.9);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        // Should show 10/10, not 20/10 (duplicates should be deduplicated)
        assert!(insight.message.contains("10/10"));
    }

    #[test]
    fn test_rubber_stamping_priority() {
        let mut criteria: HashMap<String, Vec<WriteGateCriterion>> = HashMap::new();

        for i in 1..=10 {
            criteria.insert(
                format!("L{:03}", i),
                vec![WriteGateCriterion::BehaviorChanging],
            );
        }

        let insight = generate_rubber_stamping_insight(&criteria, 10, 0.9);

        assert!(insight.is_some());
        // Should have priority 1 (high priority)
        assert_eq!(insight.unwrap().priority, 1);
    }

    #[test]
    fn test_rubber_stamping_in_generate_all() {
        let now = Utc::now();
        let cache = StatsCache::new();
        let timestamps = HashMap::new();
        let categories = HashMap::new();

        let mut criteria: HashMap<String, Vec<WriteGateCriterion>> = HashMap::new();
        for i in 1..=10 {
            criteria.insert(
                format!("L{:03}", i),
                vec![WriteGateCriterion::ExplicitRequest],
            );
        }

        let context_files = HashMap::new();
        let decay_config = default_decay_config();
        let insight_config = default_insight_config();

        let insights = generate_all(
            &cache,
            &timestamps,
            &categories,
            &criteria,
            &context_files,
            &decay_config,
            &insight_config,
            now,
        );

        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].kind, InsightKind::RubberStamping);
        assert!(insights[0].message.contains("Explicit Request"));
    }

    // WriteGateTooStrict tests

    #[test]
    fn test_write_gate_too_strict_no_evaluations() {
        let cache = StatsCache::new();

        let insight = generate_write_gate_too_strict_insight(&cache, 10, 0.5);
        assert!(insight.is_none()); // No evaluations yet
    }

    #[test]
    fn test_write_gate_too_strict_below_min_evaluations() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 5; // Below threshold of 10
        cache.write_gate.total_accepted = 1;
        cache.write_gate.pass_rate = 0.2;

        let insight = generate_write_gate_too_strict_insight(&cache, 10, 0.5);
        assert!(insight.is_none()); // Not enough evaluations
    }

    #[test]
    fn test_write_gate_too_strict_triggers_when_pass_rate_low() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 6;
        cache.write_gate.pass_rate = 0.3; // Below 0.5 threshold

        let insight = generate_write_gate_too_strict_insight(&cache, 10, 0.5);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert_eq!(insight.kind, InsightKind::WriteGateTooStrict);
        assert!(insight.message.contains("30%"));
        assert!(insight.message.contains("6/20"));
    }

    #[test]
    fn test_write_gate_too_strict_at_threshold_no_trigger() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 10;
        cache.write_gate.total_accepted = 5;
        cache.write_gate.pass_rate = 0.5; // Equal to threshold

        let insight = generate_write_gate_too_strict_insight(&cache, 10, 0.5);
        assert!(insight.is_none()); // Pass rate >= threshold, no trigger
    }

    #[test]
    fn test_write_gate_too_strict_above_threshold_no_trigger() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 10;
        cache.write_gate.total_accepted = 7;
        cache.write_gate.pass_rate = 0.7; // Above 0.5 threshold

        let insight = generate_write_gate_too_strict_insight(&cache, 10, 0.5);
        assert!(insight.is_none()); // Pass rate above threshold
    }

    #[test]
    fn test_write_gate_too_strict_zero_pass_rate() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 15;
        cache.write_gate.total_accepted = 0;
        cache.write_gate.pass_rate = 0.0; // Zero pass rate

        let insight = generate_write_gate_too_strict_insight(&cache, 10, 0.5);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert!(insight.message.contains("0%"));
        assert!(insight.message.contains("0/15"));
    }

    #[test]
    fn test_write_gate_too_strict_priority() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 10;
        cache.write_gate.total_accepted = 2;
        cache.write_gate.pass_rate = 0.2;

        let insight = generate_write_gate_too_strict_insight(&cache, 10, 0.5);

        assert!(insight.is_some());
        // Should have priority 2 (medium priority)
        assert_eq!(insight.unwrap().priority, 2);
    }

    #[test]
    fn test_write_gate_too_strict_suggestion() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 10;
        cache.write_gate.total_accepted = 3;
        cache.write_gate.pass_rate = 0.3;

        let insight = generate_write_gate_too_strict_insight(&cache, 10, 0.5);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert!(insight.suggestion.contains("relaxing"));
    }

    #[test]
    fn test_write_gate_too_strict_custom_thresholds() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 5;
        cache.write_gate.total_accepted = 2;
        cache.write_gate.pass_rate = 0.4;

        // Custom thresholds: min_evaluations=5, threshold=0.6
        let insight = generate_write_gate_too_strict_insight(&cache, 5, 0.6);

        assert!(insight.is_some()); // 0.4 < 0.6, should trigger
        let insight = insight.unwrap();
        assert!(insight.message.contains("40%"));
    }

    #[test]
    fn test_write_gate_too_strict_in_generate_all() {
        let now = Utc::now();
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 4;
        cache.write_gate.pass_rate = 0.2;

        let timestamps = HashMap::new();
        let categories = HashMap::new();
        let criteria = HashMap::new();
        let context_files = HashMap::new();
        let decay_config = default_decay_config();
        let insight_config = default_insight_config();

        let insights = generate_all(
            &cache,
            &timestamps,
            &categories,
            &criteria,
            &context_files,
            &decay_config,
            &insight_config,
            now,
        );

        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].kind, InsightKind::WriteGateTooStrict);
        assert!(insights[0].message.contains("20%"));
    }

    #[test]
    fn test_write_gate_too_strict_with_other_insights() {
        let now = Utc::now();
        let mut cache = StatsCache::new();

        // Set up write gate stats
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 4;
        cache.write_gate.pass_rate = 0.2;

        // Also add cross-pollination to trigger another insight
        for i in 1..=4 {
            cache.cross_pollination.push(CrossPollinationEdge {
                learning_id: format!("L{:03}", i),
                origin_ticket: "T001".to_string(),
                referenced_in: vec![format!("T{:03}", i + 100)],
            });
        }

        let timestamps = HashMap::new();
        let categories = HashMap::new();
        let criteria = HashMap::new();
        let context_files = HashMap::new();
        let decay_config = default_decay_config();
        let insight_config = default_insight_config();

        let insights = generate_all(
            &cache,
            &timestamps,
            &categories,
            &criteria,
            &context_files,
            &decay_config,
            &insight_config,
            now,
        );

        assert_eq!(insights.len(), 2);
        // Both insights have priority 2, but WriteGateTooStrict comes first by insertion order
        // Actually HighCrossPollination has priority 3, so WriteGateTooStrict (priority 2) comes first
        assert_eq!(insights[0].kind, InsightKind::WriteGateTooStrict);
        assert_eq!(insights[1].kind, InsightKind::HighCrossPollination);
    }

    // WriteGateTooLoose tests

    #[test]
    fn test_write_gate_too_loose_no_evaluations() {
        let cache = StatsCache::new();

        let insight = generate_write_gate_too_loose_insight(&cache, 10, 0.95, 0.3);
        assert!(insight.is_none()); // No evaluations yet
    }

    #[test]
    fn test_write_gate_too_loose_below_min_evaluations() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 5; // Below threshold of 10
        cache.write_gate.total_accepted = 5;
        cache.write_gate.pass_rate = 1.0;

        let insight = generate_write_gate_too_loose_insight(&cache, 10, 0.95, 0.3);
        assert!(insight.is_none()); // Not enough evaluations
    }

    #[test]
    fn test_write_gate_too_loose_no_accepted_learnings() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 0;
        cache.write_gate.pass_rate = 0.0;

        let insight = generate_write_gate_too_loose_insight(&cache, 10, 0.95, 0.3);
        assert!(insight.is_none()); // No accepted learnings
    }

    #[test]
    fn test_write_gate_too_loose_triggers_when_pass_rate_high_hit_rate_low() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 19;
        cache.write_gate.pass_rate = 0.96; // Above 0.95 threshold

        // Add learnings with low hit rates
        for i in 1..=5 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 2;
            stats.hit_rate = 0.2; // Below 0.3 threshold
            cache.learnings.insert(format!("L{:03}", i), stats);
        }

        let insight = generate_write_gate_too_loose_insight(&cache, 10, 0.95, 0.3);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert_eq!(insight.kind, InsightKind::WriteGateTooLoose);
        assert!(insight.message.contains("96%"));
        assert!(insight.message.contains("20%"));
    }

    #[test]
    fn test_write_gate_too_loose_at_pass_rate_threshold_no_trigger() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 19;
        cache.write_gate.pass_rate = 0.95; // Equal to threshold

        // Add learnings with low hit rates
        for i in 1..=5 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 1;
            stats.hit_rate = 0.1;
            cache.learnings.insert(format!("L{:03}", i), stats);
        }

        let insight = generate_write_gate_too_loose_insight(&cache, 10, 0.95, 0.3);
        assert!(insight.is_none()); // Pass rate <= threshold, no trigger
    }

    #[test]
    fn test_write_gate_too_loose_high_hit_rate_no_trigger() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 20;
        cache.write_gate.pass_rate = 1.0; // 100% pass rate

        // Add learnings with high hit rates (quality is good)
        for i in 1..=5 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 5;
            stats.hit_rate = 0.5; // Above 0.3 threshold
            cache.learnings.insert(format!("L{:03}", i), stats);
        }

        let insight = generate_write_gate_too_loose_insight(&cache, 10, 0.95, 0.3);
        assert!(insight.is_none()); // Hit rate above threshold
    }

    #[test]
    fn test_write_gate_too_loose_no_surfaced_learnings() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 20;
        cache.write_gate.pass_rate = 1.0;

        // Add learnings but none surfaced
        for i in 1..=5 {
            let stats = make_learning_stats(); // surfaced = 0 by default
            cache.learnings.insert(format!("L{:03}", i), stats);
        }

        let insight = generate_write_gate_too_loose_insight(&cache, 10, 0.95, 0.3);
        assert!(insight.is_none()); // No surfaced learnings to assess
    }

    #[test]
    fn test_write_gate_too_loose_ignores_archived() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 20;
        cache.write_gate.pass_rate = 1.0;

        // Add archived learnings with low hit rates (should be ignored)
        for i in 1..=5 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 1;
            stats.hit_rate = 0.1;
            stats.archived = true;
            cache.learnings.insert(format!("L{:03}", i), stats);
        }

        let insight = generate_write_gate_too_loose_insight(&cache, 10, 0.95, 0.3);
        assert!(insight.is_none()); // Archived learnings ignored
    }

    #[test]
    fn test_write_gate_too_loose_priority() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 20;
        cache.write_gate.pass_rate = 1.0;

        // Add learnings with low hit rates
        for i in 1..=5 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 1;
            stats.hit_rate = 0.1;
            cache.learnings.insert(format!("L{:03}", i), stats);
        }

        let insight = generate_write_gate_too_loose_insight(&cache, 10, 0.95, 0.3);

        assert!(insight.is_some());
        // Should have priority 2 (medium priority)
        assert_eq!(insight.unwrap().priority, 2);
    }

    #[test]
    fn test_write_gate_too_loose_suggestion() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 20;
        cache.write_gate.pass_rate = 1.0;

        // Add learnings with low hit rates
        for i in 1..=5 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 2;
            stats.hit_rate = 0.2;
            cache.learnings.insert(format!("L{:03}", i), stats);
        }

        let insight = generate_write_gate_too_loose_insight(&cache, 10, 0.95, 0.3);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert!(insight.suggestion.contains("tightening"));
    }

    #[test]
    fn test_write_gate_too_loose_custom_thresholds() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 5;
        cache.write_gate.total_accepted = 5;
        cache.write_gate.pass_rate = 1.0;

        // Add learnings with moderate hit rates
        for i in 1..=5 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 4;
            stats.hit_rate = 0.4;
            cache.learnings.insert(format!("L{:03}", i), stats);
        }

        // Custom thresholds: min_evaluations=5, pass_rate=0.8, hit_rate=0.5
        let insight = generate_write_gate_too_loose_insight(&cache, 5, 0.8, 0.5);

        assert!(insight.is_some()); // 1.0 > 0.8 and 0.4 < 0.5
        let insight = insight.unwrap();
        assert!(insight.message.contains("100%"));
        assert!(insight.message.contains("40%"));
    }

    #[test]
    fn test_write_gate_too_loose_in_generate_all() {
        let now = Utc::now();
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 20;
        cache.write_gate.pass_rate = 1.0;

        // Add learnings with low hit rates
        for i in 1..=5 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 1;
            stats.hit_rate = 0.1;
            cache.learnings.insert(format!("L{:03}", i), stats);
        }

        let timestamps = HashMap::new();
        let categories = HashMap::new();
        let criteria = HashMap::new();
        let context_files = HashMap::new();
        let decay_config = default_decay_config();
        let insight_config = default_insight_config();

        let insights = generate_all(
            &cache,
            &timestamps,
            &categories,
            &criteria,
            &context_files,
            &decay_config,
            &insight_config,
            now,
        );

        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].kind, InsightKind::WriteGateTooLoose);
    }

    #[test]
    fn test_write_gate_too_loose_with_mixed_hit_rates() {
        let mut cache = StatsCache::new();
        cache.write_gate.total_evaluated = 20;
        cache.write_gate.total_accepted = 20;
        cache.write_gate.pass_rate = 1.0;

        // Add learnings with mixed hit rates (avg = 0.28)
        // 3 with 0.1, 2 with 0.55 = (0.1*3 + 0.55*2)/5 = (0.3 + 1.1)/5 = 0.28
        for i in 1..=3 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 1;
            stats.hit_rate = 0.1;
            cache.learnings.insert(format!("L{:03}", i), stats);
        }
        for i in 4..=5 {
            let mut stats = make_learning_stats();
            stats.surfaced = 10;
            stats.referenced = 5;
            stats.hit_rate = 0.55;
            cache.learnings.insert(format!("L{:03}", i), stats);
        }

        let insight = generate_write_gate_too_loose_insight(&cache, 10, 0.95, 0.3);

        assert!(insight.is_some()); // Average 0.28 < 0.3
    }

    // SkipMiss tests

    #[test]
    fn test_skip_miss_no_skips() {
        let cache = StatsCache::new();
        let context_files = HashMap::new();

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);
        assert!(insight.is_none()); // No skipped tickets
    }

    #[test]
    fn test_skip_miss_no_learnings() {
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());
        let context_files = HashMap::new();

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);
        assert!(insight.is_none()); // No learnings
    }

    #[test]
    fn test_skip_miss_no_matching_tickets() {
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());

        // Add a learning for a different ticket
        let mut stats = make_learning_stats();
        stats.origin_ticket = Some("ticket-2".to_string());
        cache.learnings.insert("L001".to_string(), stats);
        let context_files = HashMap::new();

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);
        assert!(insight.is_none()); // Different tickets
    }

    #[test]
    fn test_skip_miss_triggers_on_matching_ticket() {
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());

        // Add a learning for the same ticket
        let mut stats = make_learning_stats();
        stats.origin_ticket = Some("ticket-1".to_string());
        cache.learnings.insert("L001".to_string(), stats);
        let context_files = HashMap::new();

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert_eq!(insight.kind, InsightKind::SkipMiss);
        assert!(insight.message.contains("1 learning"));
        assert!(insight.message.contains("ticket match"));
    }

    #[test]
    fn test_skip_miss_multiple_learnings_same_ticket() {
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());

        // Add multiple learnings for the same ticket
        for i in 1..=3 {
            let mut stats = make_learning_stats();
            stats.origin_ticket = Some("ticket-1".to_string());
            cache.learnings.insert(format!("L{:03}", i), stats);
        }
        let context_files = HashMap::new();

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert!(insight.message.contains("3 learnings"));
        assert!(insight.message.contains("3 by ticket"));
    }

    #[test]
    fn test_skip_miss_multiple_tickets() {
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());
        cache.skipped_tickets.insert("ticket-2".to_string());

        // Add learnings for both tickets
        let mut stats1 = make_learning_stats();
        stats1.origin_ticket = Some("ticket-1".to_string());
        cache.learnings.insert("L001".to_string(), stats1);

        let mut stats2 = make_learning_stats();
        stats2.origin_ticket = Some("ticket-2".to_string());
        cache.learnings.insert("L002".to_string(), stats2);
        let context_files = HashMap::new();

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert!(insight.message.contains("2 learnings"));
        assert!(insight.message.contains("2 by ticket"));
    }

    #[test]
    fn test_skip_miss_ignores_archived() {
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());

        // Add an archived learning for the skipped ticket
        let mut stats = make_learning_stats();
        stats.origin_ticket = Some("ticket-1".to_string());
        stats.archived = true;
        cache.learnings.insert("L001".to_string(), stats);
        let context_files = HashMap::new();

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);
        assert!(insight.is_none()); // Archived learnings are ignored
    }

    #[test]
    fn test_skip_miss_ignores_learnings_without_ticket() {
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());

        // Add a learning without a ticket
        let stats = make_learning_stats(); // origin_ticket is None
        cache.learnings.insert("L001".to_string(), stats);
        let context_files = HashMap::new();

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);
        assert!(insight.is_none()); // No matching ticket
    }

    #[test]
    fn test_skip_miss_min_count_threshold() {
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());

        // Add one learning
        let mut stats = make_learning_stats();
        stats.origin_ticket = Some("ticket-1".to_string());
        cache.learnings.insert("L001".to_string(), stats);
        let context_files = HashMap::new();

        // With min_count=2, should not trigger
        let insight = generate_skip_miss_insight(&cache, &context_files, 2);
        assert!(insight.is_none());

        // With min_count=1, should trigger
        let insight = generate_skip_miss_insight(&cache, &context_files, 1);
        assert!(insight.is_some());
    }

    #[test]
    fn test_skip_miss_priority() {
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());

        let mut stats = make_learning_stats();
        stats.origin_ticket = Some("ticket-1".to_string());
        cache.learnings.insert("L001".to_string(), stats);
        let context_files = HashMap::new();

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);

        assert!(insight.is_some());
        // Should have priority 2 (medium priority)
        assert_eq!(insight.unwrap().priority, 2);
    }

    #[test]
    fn test_skip_miss_suggestion() {
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());

        let mut stats = make_learning_stats();
        stats.origin_ticket = Some("ticket-1".to_string());
        cache.learnings.insert("L001".to_string(), stats);
        let context_files = HashMap::new();

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert!(insight.suggestion.contains("skip decisions"));
    }

    #[test]
    fn test_skip_miss_in_generate_all() {
        let now = Utc::now();
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());

        let mut stats = make_learning_stats();
        stats.origin_ticket = Some("ticket-1".to_string());
        cache.learnings.insert("L001".to_string(), stats);

        let timestamps = HashMap::new();
        let categories = HashMap::new();
        let criteria = HashMap::new();
        let context_files = HashMap::new();
        let decay_config = default_decay_config();
        let insight_config = default_insight_config();

        let insights = generate_all(
            &cache,
            &timestamps,
            &categories,
            &criteria,
            &context_files,
            &decay_config,
            &insight_config,
            now,
        );

        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].kind, InsightKind::SkipMiss);
    }

    // File-based skip miss detection tests

    #[test]
    fn test_skip_miss_file_overlap_triggers() {
        let mut cache = StatsCache::new();
        // Add files from a skipped session
        cache.skipped_files.insert("src/main.rs".to_string());
        cache.skipped_files.insert("src/lib.rs".to_string());

        // Add a learning (no ticket match but file overlap)
        let stats = make_learning_stats();
        cache.learnings.insert("L001".to_string(), stats);

        // Context files for this learning overlap with skipped files
        let mut context_files = HashMap::new();
        context_files.insert("L001".to_string(), vec!["src/main.rs".to_string()]);

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert_eq!(insight.kind, InsightKind::SkipMiss);
        assert!(insight.message.contains("file overlap"));
    }

    #[test]
    fn test_skip_miss_file_overlap_no_match() {
        let mut cache = StatsCache::new();
        // Add files from a skipped session
        cache.skipped_files.insert("src/other.rs".to_string());

        // Add a learning
        let stats = make_learning_stats();
        cache.learnings.insert("L001".to_string(), stats);

        // Context files don't overlap with skipped files
        let mut context_files = HashMap::new();
        context_files.insert("L001".to_string(), vec!["src/main.rs".to_string()]);

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);

        assert!(insight.is_none()); // No file overlap
    }

    #[test]
    fn test_skip_miss_ticket_takes_priority_over_file() {
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());
        cache.skipped_files.insert("src/main.rs".to_string());

        // Add a learning with matching ticket AND overlapping files
        let mut stats = make_learning_stats();
        stats.origin_ticket = Some("ticket-1".to_string());
        cache.learnings.insert("L001".to_string(), stats);

        let mut context_files = HashMap::new();
        context_files.insert("L001".to_string(), vec!["src/main.rs".to_string()]);

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        // Should report ticket match, not file overlap (ticket takes priority)
        assert!(insight.message.contains("ticket match"));
        assert!(!insight.message.contains("file overlap"));
    }

    #[test]
    fn test_skip_miss_mixed_ticket_and_file() {
        let mut cache = StatsCache::new();
        cache.skipped_tickets.insert("ticket-1".to_string());
        cache.skipped_files.insert("src/main.rs".to_string());

        // Learning 1: ticket match
        let mut stats1 = make_learning_stats();
        stats1.origin_ticket = Some("ticket-1".to_string());
        cache.learnings.insert("L001".to_string(), stats1);

        // Learning 2: file overlap only (different ticket)
        let mut stats2 = make_learning_stats();
        stats2.origin_ticket = Some("ticket-2".to_string());
        cache.learnings.insert("L002".to_string(), stats2);

        let mut context_files = HashMap::new();
        context_files.insert("L002".to_string(), vec!["src/main.rs".to_string()]);

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);

        assert!(insight.is_some());
        let insight = insight.unwrap();
        assert!(insight.message.contains("2 learnings"));
        assert!(insight.message.contains("1 by ticket"));
        assert!(insight.message.contains("1 by file overlap"));
    }

    #[test]
    fn test_skip_miss_file_overlap_path_suffix_match() {
        let mut cache = StatsCache::new();
        // Add full path from skipped session
        cache
            .skipped_files
            .insert("/project/src/main.rs".to_string());

        // Add a learning
        let stats = make_learning_stats();
        cache.learnings.insert("L001".to_string(), stats);

        // Context files use relative path
        let mut context_files = HashMap::new();
        context_files.insert("L001".to_string(), vec!["src/main.rs".to_string()]);

        let insight = generate_skip_miss_insight(&cache, &context_files, 1);

        assert!(insight.is_some()); // Should match via path suffix
    }
}
