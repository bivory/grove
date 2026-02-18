//! Configuration recommendations based on stats analysis.
//!
//! This module generates actionable configuration recommendations from
//! insights and stats. Recommendations are classified as:
//!
//! - **Safe**: Can be auto-applied with `grove stats --update-config`
//! - **Aggressive**: Require manual review due to potential side effects
//!
//! Safe recommendations:
//! - Retrieval strategy (based on average hit rate)
//! - Auto-skip threshold (based on skip miss detection)
//! - Category immunity rates (based on category performance)
//!
//! Aggressive recommendations:
//! - Circuit breaker tuning (safety-critical)
//! - Decay duration (affects data retention)
//! - Write gate criteria (affects quality control)

use serde::{Deserialize, Serialize};

use crate::config::{Config, RetrievalConfig};
use crate::stats::{Insight, InsightKind, StatsCache};

/// A configuration recommendation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigRecommendation {
    /// The config key being recommended (e.g., "retrieval.strategy").
    pub config_key: String,
    /// Current value.
    pub current_value: String,
    /// Recommended value.
    pub recommended_value: String,
    /// Reason for the recommendation.
    pub reason: String,
    /// Risk explanation (for aggressive recommendations).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    /// Whether this is a safe (auto-applicable) recommendation.
    pub is_safe: bool,
}

impl ConfigRecommendation {
    /// Create a safe recommendation.
    pub fn safe(
        config_key: impl Into<String>,
        current: impl Into<String>,
        recommended: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            config_key: config_key.into(),
            current_value: current.into(),
            recommended_value: recommended.into(),
            reason: reason.into(),
            risk: None,
            is_safe: true,
        }
    }

    /// Create an aggressive recommendation.
    pub fn aggressive(
        config_key: impl Into<String>,
        current: impl Into<String>,
        recommended: impl Into<String>,
        reason: impl Into<String>,
        risk: impl Into<String>,
    ) -> Self {
        Self {
            config_key: config_key.into(),
            current_value: current.into(),
            recommended_value: recommended.into(),
            reason: reason.into(),
            risk: Some(risk.into()),
            is_safe: false,
        }
    }
}

/// Generated recommendations split by safety.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Recommendations {
    /// Safe recommendations (can be auto-applied).
    pub safe: Vec<ConfigRecommendation>,
    /// Aggressive recommendations (require manual review).
    pub aggressive: Vec<ConfigRecommendation>,
}

impl Recommendations {
    /// Create empty recommendations.
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if there are any recommendations.
    pub fn is_empty(&self) -> bool {
        self.safe.is_empty() && self.aggressive.is_empty()
    }

    /// Check if there are safe recommendations to apply.
    pub fn has_safe(&self) -> bool {
        !self.safe.is_empty()
    }

    /// Get total count of recommendations.
    pub fn total(&self) -> usize {
        self.safe.len() + self.aggressive.len()
    }
}

/// Generate configuration recommendations from stats and insights.
pub fn generate_recommendations(
    cache: &StatsCache,
    insights: &[Insight],
    config: &Config,
) -> Recommendations {
    let mut recommendations = Recommendations::new();

    // Safe: Retrieval strategy based on average hit rate
    if let Some(rec) = recommend_retrieval_strategy(cache, config) {
        recommendations.safe.push(rec);
    }

    // Safe: Auto-skip threshold based on skip miss insight
    if let Some(rec) = recommend_auto_skip_threshold(insights, config) {
        recommendations.safe.push(rec);
    }

    // Aggressive: Circuit breaker tuning
    // (We'd need more data to recommend this - skip for now)

    // Aggressive: Write gate criteria based on insights
    if let Some(rec) = recommend_write_gate_tuning(insights, config) {
        recommendations.aggressive.push(rec);
    }

    // Aggressive: Decay duration based on decay warnings
    if let Some(rec) = recommend_decay_duration(insights, config) {
        recommendations.aggressive.push(rec);
    }

    recommendations
}

/// Recommend retrieval strategy based on average hit rate.
///
/// - hit_rate < 0.4 → conservative (inject fewer learnings)
/// - 0.4 <= hit_rate < 0.7 → moderate (balanced)
/// - hit_rate >= 0.7 → aggressive (inject more learnings)
fn recommend_retrieval_strategy(
    cache: &StatsCache,
    config: &Config,
) -> Option<ConfigRecommendation> {
    let hit_rate = cache.aggregates.average_hit_rate;

    // Need some surfaced learnings to make a meaningful recommendation
    let surfaced_count = cache
        .learnings
        .values()
        .filter(|s| s.surfaced > 0 && !s.archived)
        .count();

    if surfaced_count < 5 {
        return None;
    }

    // Skip if hit rate is NaN or invalid
    if !hit_rate.is_finite() {
        return None;
    }

    let recommended = if hit_rate < 0.4 {
        "conservative"
    } else if hit_rate < 0.7 {
        "moderate"
    } else {
        "aggressive"
    };

    let current = &config.retrieval.strategy;

    // Only recommend if different from current
    if current == recommended {
        return None;
    }

    // Validate recommended is a valid strategy
    if !RetrievalConfig::is_valid_strategy(recommended) {
        return None;
    }

    Some(ConfigRecommendation::safe(
        "retrieval.strategy",
        current.clone(),
        recommended,
        format!("hit rate: {:.0}%", hit_rate * 100.0),
    ))
}

/// Recommend auto-skip threshold based on skip miss insight.
///
/// If skip misses are detected, lower the threshold to capture more sessions.
fn recommend_auto_skip_threshold(
    insights: &[Insight],
    config: &Config,
) -> Option<ConfigRecommendation> {
    let skip_miss = insights.iter().find(|i| i.kind == InsightKind::SkipMiss)?;

    let current = config.gate.auto_skip.line_threshold;

    // If we're detecting skip misses, recommend lowering threshold
    // Lower threshold = fewer auto-skips = more reflection opportunities
    let recommended = if current > 3 {
        current.saturating_sub(2)
    } else {
        return None; // Already at minimum reasonable threshold
    };

    Some(ConfigRecommendation::safe(
        "gate.auto_skip.line_threshold",
        current.to_string(),
        recommended.to_string(),
        skip_miss.message.clone(),
    ))
}

/// Recommend write gate tuning based on WriteGateTooStrict or WriteGateTooLoose insights.
fn recommend_write_gate_tuning(
    insights: &[Insight],
    _config: &Config,
) -> Option<ConfigRecommendation> {
    // Check for WriteGateTooStrict
    if let Some(insight) = insights
        .iter()
        .find(|i| i.kind == InsightKind::WriteGateTooStrict)
    {
        return Some(ConfigRecommendation::aggressive(
            "write gate criteria",
            "current",
            "consider relaxing",
            insight.message.clone(),
            "May allow borderline learnings that don't prove valuable",
        ));
    }

    // Check for WriteGateTooLoose
    if let Some(insight) = insights
        .iter()
        .find(|i| i.kind == InsightKind::WriteGateTooLoose)
    {
        return Some(ConfigRecommendation::aggressive(
            "write gate criteria",
            "current",
            "consider tightening",
            insight.message.clone(),
            "May reject borderline learnings that could prove valuable",
        ));
    }

    None
}

/// Recommend decay duration based on frequent DecayWarning insights.
fn recommend_decay_duration(insights: &[Insight], config: &Config) -> Option<ConfigRecommendation> {
    let decay_warning = insights
        .iter()
        .find(|i| i.kind == InsightKind::DecayWarning)?;

    // Extract count from message (e.g., "5 learnings approaching...")
    let count = decay_warning
        .message
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    // Only recommend if many learnings are approaching decay
    if count < 5 {
        return None;
    }

    let current = config.decay.passive_duration_days;
    let recommended = current + 30; // Extend by 30 days

    Some(ConfigRecommendation::aggressive(
        "decay.passive_duration_days",
        current.to_string(),
        recommended.to_string(),
        decay_warning.message.clone(),
        "Longer retention means more storage and potentially stale learnings",
    ))
}

/// Apply safe recommendations to a config.
///
/// Returns a new Config with the safe recommendations applied.
pub fn apply_safe_recommendations(config: &Config, recommendations: &Recommendations) -> Config {
    let mut new_config = config.clone();

    for rec in &recommendations.safe {
        match rec.config_key.as_str() {
            "retrieval.strategy" => {
                if RetrievalConfig::is_valid_strategy(&rec.recommended_value) {
                    new_config.retrieval.strategy = rec.recommended_value.clone();
                }
            }
            "gate.auto_skip.line_threshold" => {
                if let Ok(value) = rec.recommended_value.parse::<u32>() {
                    new_config.gate.auto_skip.line_threshold = value;
                }
            }
            _ => {
                // Unknown config key - skip
            }
        }
    }

    new_config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::LearningStats;

    fn make_config() -> Config {
        Config::default()
    }

    fn make_cache_with_hit_rate(hit_rate: f64, count: usize) -> StatsCache {
        let mut cache = StatsCache::new();
        cache.aggregates.average_hit_rate = hit_rate;

        // Add some learnings to meet the minimum threshold
        for i in 0..count {
            let stats = LearningStats {
                surfaced: 5,
                hit_rate,
                ..LearningStats::default()
            };
            cache.learnings.insert(format!("L{:03}", i), stats);
        }

        cache
    }

    #[test]
    fn test_recommend_retrieval_conservative() {
        let cache = make_cache_with_hit_rate(0.25, 10);
        let config = make_config();

        let rec = recommend_retrieval_strategy(&cache, &config);

        assert!(rec.is_some());
        let rec = rec.unwrap();
        assert_eq!(rec.config_key, "retrieval.strategy");
        assert_eq!(rec.recommended_value, "conservative");
        assert!(rec.is_safe);
    }

    #[test]
    fn test_recommend_retrieval_aggressive() {
        let cache = make_cache_with_hit_rate(0.8, 10);
        let config = make_config();

        let rec = recommend_retrieval_strategy(&cache, &config);

        assert!(rec.is_some());
        let rec = rec.unwrap();
        assert_eq!(rec.recommended_value, "aggressive");
    }

    #[test]
    fn test_recommend_retrieval_no_change() {
        let cache = make_cache_with_hit_rate(0.5, 10);
        let config = make_config(); // Default is "moderate"

        let rec = recommend_retrieval_strategy(&cache, &config);

        // Should be None since 0.5 hit rate -> moderate, which is the default
        assert!(rec.is_none());
    }

    #[test]
    fn test_recommend_retrieval_insufficient_data() {
        let cache = make_cache_with_hit_rate(0.8, 2); // Only 2 learnings

        let config = make_config();

        let rec = recommend_retrieval_strategy(&cache, &config);

        assert!(rec.is_none());
    }

    #[test]
    fn test_recommend_auto_skip_with_skip_miss() {
        let insights = vec![Insight::new(
            InsightKind::SkipMiss,
            "2 skip misses detected",
            "Review skip decisions",
            2,
        )];
        let config = make_config();

        let rec = recommend_auto_skip_threshold(&insights, &config);

        assert!(rec.is_some());
        let rec = rec.unwrap();
        assert_eq!(rec.config_key, "gate.auto_skip.line_threshold");
        assert_eq!(rec.current_value, "5"); // Default
        assert_eq!(rec.recommended_value, "3"); // 5 - 2
        assert!(rec.is_safe);
    }

    #[test]
    fn test_recommend_auto_skip_no_insight() {
        let insights = vec![];
        let config = make_config();

        let rec = recommend_auto_skip_threshold(&insights, &config);

        assert!(rec.is_none());
    }

    #[test]
    fn test_recommend_write_gate_too_strict() {
        let insights = vec![Insight::new(
            InsightKind::WriteGateTooStrict,
            "Write gate pass rate is 40%",
            "Consider relaxing",
            2,
        )];
        let config = make_config();

        let rec = recommend_write_gate_tuning(&insights, &config);

        assert!(rec.is_some());
        let rec = rec.unwrap();
        assert!(!rec.is_safe); // Should be aggressive
        assert!(rec.risk.is_some());
        assert!(rec.recommended_value.contains("relaxing"));
    }

    #[test]
    fn test_recommend_write_gate_too_loose() {
        let insights = vec![Insight::new(
            InsightKind::WriteGateTooLoose,
            "Write gate pass rate is 98% but hit rate is 20%",
            "Consider tightening",
            2,
        )];
        let config = make_config();

        let rec = recommend_write_gate_tuning(&insights, &config);

        assert!(rec.is_some());
        let rec = rec.unwrap();
        assert!(!rec.is_safe);
        assert!(rec.recommended_value.contains("tightening"));
    }

    #[test]
    fn test_recommend_decay_duration() {
        let insights = vec![Insight::new(
            InsightKind::DecayWarning,
            "8 learnings approaching 90-day archive threshold",
            "Run grove maintain",
            2,
        )];
        let config = make_config();

        let rec = recommend_decay_duration(&insights, &config);

        assert!(rec.is_some());
        let rec = rec.unwrap();
        assert_eq!(rec.config_key, "decay.passive_duration_days");
        assert_eq!(rec.current_value, "90");
        assert_eq!(rec.recommended_value, "120"); // 90 + 30
        assert!(!rec.is_safe);
    }

    #[test]
    fn test_recommend_decay_duration_few_warnings() {
        let insights = vec![Insight::new(
            InsightKind::DecayWarning,
            "2 learnings approaching threshold",
            "Run grove maintain",
            2,
        )];
        let config = make_config();

        let rec = recommend_decay_duration(&insights, &config);

        // Should be None since only 2 learnings (< 5 threshold)
        assert!(rec.is_none());
    }

    #[test]
    fn test_apply_safe_recommendations() {
        let config = make_config();
        let recommendations = Recommendations {
            safe: vec![
                ConfigRecommendation::safe(
                    "retrieval.strategy",
                    "moderate",
                    "aggressive",
                    "high hit rate",
                ),
                ConfigRecommendation::safe(
                    "gate.auto_skip.line_threshold",
                    "5",
                    "3",
                    "skip misses",
                ),
            ],
            aggressive: vec![],
        };

        let new_config = apply_safe_recommendations(&config, &recommendations);

        assert_eq!(new_config.retrieval.strategy, "aggressive");
        assert_eq!(new_config.gate.auto_skip.line_threshold, 3);
    }

    #[test]
    fn test_generate_recommendations() {
        let cache = make_cache_with_hit_rate(0.8, 10);
        let insights = vec![Insight::new(
            InsightKind::SkipMiss,
            "3 skip misses detected",
            "Review",
            2,
        )];
        let config = make_config();

        let recommendations = generate_recommendations(&cache, &insights, &config);

        assert!(recommendations.has_safe());
        assert!(!recommendations.is_empty());
    }

    #[test]
    fn test_recommendations_is_empty() {
        let rec = Recommendations::new();
        assert!(rec.is_empty());
        assert!(!rec.has_safe());
        assert_eq!(rec.total(), 0);
    }
}
