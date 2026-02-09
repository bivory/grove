//! Relevance and composite scoring for Grove learnings.
//!
//! This module provides scoring functions for ranking learnings based on
//! their relevance to a search query, recency, and historical reference rate.
//!
//! ## Relevance Scoring
//!
//! Relevance weights (combined via max, not sum):
//! - Exact tag match: 1.0
//! - Partial tag match: 0.5
//! - File path overlap: 0.8
//! - Keyword in summary: 0.3
//!
//! ## Composite Scoring
//!
//! The full composite score combines three factors:
//!
//! ```text
//! score = relevance × recency_weight × reference_boost
//! ```
//!
//! ### Recency Weight
//!
//! Exponential decay from creation date:
//!
//! ```text
//! recency_weight = e^(-λ × days_since_creation)
//! ```
//!
//! Where λ ≈ 0.01338, tuned so:
//! - 1-day-old learning: ~1.0
//! - 90-day-old learning: ~0.3
//!
//! ### Reference Boost
//!
//! Learnings with proven value get boosted:
//!
//! ```text
//! reference_boost = 0.5 + (hit_rate × 0.5)
//! ```
//!
//! Range: 0.5 (never referenced) to 1.0 (always referenced).
//!
//! ## Strategy Modes
//!
//! - **Conservative**: Favor proven learnings (higher reference boost weight)
//! - **Moderate**: Balanced approach (default)
//! - **Aggressive**: Favor recent learnings (higher recency weight)

use crate::backends::SearchQuery;
use crate::core::CompoundLearning;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Score weights for relevance matching.
pub mod weights {
    /// Weight for exact tag match.
    pub const TAG_EXACT: f64 = 1.0;
    /// Weight for partial tag match (substring).
    pub const TAG_PARTIAL: f64 = 0.5;
    /// Weight for file path overlap.
    pub const FILE_OVERLAP: f64 = 0.8;
    /// Weight for keyword in summary.
    pub const KEYWORD_SUMMARY: f64 = 0.3;
}

/// Constants for recency weight calculation.
pub mod recency {
    /// Decay constant λ, tuned so 90-day-old learning has weight ~0.3.
    ///
    /// Derived from: e^(-λ × 90) = 0.3
    /// Therefore: λ = -ln(0.3) / 90 ≈ 0.01338
    pub const LAMBDA: f64 = 0.01338;

    /// Minimum recency weight (floor to prevent learnings from becoming invisible).
    pub const MIN_WEIGHT: f64 = 0.1;

    /// Maximum recency weight (for brand new learnings).
    pub const MAX_WEIGHT: f64 = 1.0;

    /// Days threshold for "recent" learnings in aggressive mode.
    /// Aggressive mode includes learnings from the last 30 days even without a match.
    pub const AGGRESSIVE_RECENT_DAYS: i64 = 30;
}

/// Constants for reference boost calculation.
pub mod reference {
    /// Base boost for learnings with no references.
    pub const BASE_BOOST: f64 = 0.5;

    /// Maximum additional boost from hit rate.
    pub const MAX_ADDITIONAL: f64 = 0.5;
}

/// Retrieval strategy mode for composite scoring.
///
/// Determines how recency and reference boost are weighted relative to each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// Favor proven learnings with high hit rates.
    /// Reference boost has more influence than recency.
    Conservative,

    /// Balanced approach (default).
    /// Equal weight to recency and reference boost.
    #[default]
    Moderate,

    /// Favor recent learnings.
    /// Recency weight has more influence than reference boost.
    Aggressive,
}

impl Strategy {
    /// Parse strategy from a string.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "conservative" => Some(Self::Conservative),
            "moderate" => Some(Self::Moderate),
            "aggressive" => Some(Self::Aggressive),
            _ => None,
        }
    }

    /// Get the strategy name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Conservative => "conservative",
            Self::Moderate => "moderate",
            Self::Aggressive => "aggressive",
        }
    }

    /// Get the recency weight multiplier for this strategy.
    ///
    /// Higher values give more weight to recent learnings.
    pub fn recency_multiplier(&self) -> f64 {
        match self {
            Strategy::Conservative => 0.7,
            Strategy::Moderate => 1.0,
            Strategy::Aggressive => 1.3,
        }
    }

    /// Get the reference boost multiplier for this strategy.
    ///
    /// Higher values give more weight to proven learnings.
    pub fn reference_multiplier(&self) -> f64 {
        match self {
            Strategy::Conservative => 1.3,
            Strategy::Moderate => 1.0,
            Strategy::Aggressive => 0.7,
        }
    }

    /// Get the default max injections for this strategy.
    ///
    /// Conservative: 3, Moderate: 5, Aggressive: 10
    pub fn default_max_injections(&self) -> usize {
        match self {
            Strategy::Conservative => 3,
            Strategy::Moderate => 5,
            Strategy::Aggressive => 10,
        }
    }

    /// Get the minimum relevance threshold for this strategy.
    ///
    /// Conservative requires exact matches (high threshold).
    /// Moderate accepts partial matches.
    /// Aggressive has no minimum (includes recent even without match).
    pub fn min_relevance_threshold(&self) -> f64 {
        match self {
            // Conservative: Only exact tag (1.0) or file (0.8) matches
            Strategy::Conservative => weights::FILE_OVERLAP,
            // Moderate: Accept any match including partial tags and keywords
            Strategy::Moderate => 0.0,
            // Aggressive: No threshold (can include learnings without match)
            Strategy::Aggressive => 0.0,
        }
    }

    /// Whether this strategy includes recent learnings without matching.
    ///
    /// Only Aggressive mode includes learnings from the last 30 days
    /// even if they don't match the query.
    pub fn includes_recent_without_match(&self) -> bool {
        matches!(self, Strategy::Aggressive)
    }
}

/// Calculate the relevance score of a learning for a given query.
///
/// Uses max-based combination: returns the highest score among all
/// matching criteria rather than summing scores.
///
/// Returns 0.0 if no criteria match.
pub fn score(query: &SearchQuery, learning: &CompoundLearning) -> f64 {
    if query.is_empty() {
        return 0.0;
    }

    let mut max_score = 0.0f64;

    // Score tag matches
    max_score = max_score.max(score_tags(&query.tags, &learning.tags));

    // Score file path overlaps
    if let Some(context_files) = &learning.context_files {
        max_score = max_score.max(score_files(&query.files, context_files));
    }

    // Score keyword matches in summary
    max_score = max_score.max(score_keywords(&query.keywords, &learning.summary));

    max_score
}

/// Score tag matches between query tags and learning tags.
///
/// - Exact match (case-insensitive): 1.0
/// - Partial match (substring): 0.5
fn score_tags(query_tags: &[String], learning_tags: &[String]) -> f64 {
    if query_tags.is_empty() {
        return 0.0;
    }

    let mut max_score = 0.0f64;

    for query_tag in query_tags {
        let query_lower = query_tag.to_lowercase();

        for learning_tag in learning_tags {
            let learning_lower = learning_tag.to_lowercase();

            if query_lower == learning_lower {
                // Exact match
                max_score = max_score.max(weights::TAG_EXACT);
            } else if learning_lower.contains(&query_lower) || query_lower.contains(&learning_lower)
            {
                // Partial match (substring in either direction)
                max_score = max_score.max(weights::TAG_PARTIAL);
            }
        }
    }

    max_score
}

/// Score file path overlaps between query files and learning context files.
///
/// Overlap is detected by:
/// - Exact path match
/// - Filename match (last component)
/// - Path suffix match (one path ends with the other)
fn score_files(query_files: &[String], context_files: &[String]) -> f64 {
    if query_files.is_empty() || context_files.is_empty() {
        return 0.0;
    }

    for query_file in query_files {
        for context_file in context_files {
            if files_overlap(query_file, context_file) {
                return weights::FILE_OVERLAP;
            }
        }
    }

    0.0
}

/// Check if two file paths overlap.
///
/// Returns true if:
/// - Paths are exactly equal (and non-empty)
/// - Same filename (last path component)
/// - One path ends with the other (suffix match)
fn files_overlap(path1: &str, path2: &str) -> bool {
    // Empty paths don't match
    if path1.is_empty() || path2.is_empty() {
        return false;
    }

    if path1 == path2 {
        return true;
    }

    // Compare filenames (last path component)
    let name1 = path1.rsplit('/').next().unwrap_or(path1);
    let name2 = path2.rsplit('/').next().unwrap_or(path2);

    if name1 == name2 && !name1.is_empty() {
        return true;
    }

    // Check suffix match
    let normalized1 = path1.trim_start_matches('/');
    let normalized2 = path2.trim_start_matches('/');

    normalized1.ends_with(normalized2) || normalized2.ends_with(normalized1)
}

/// Score keyword matches in the summary.
///
/// Case-insensitive whole word matching.
fn score_keywords(keywords: &[String], summary: &str) -> f64 {
    if keywords.is_empty() {
        return 0.0;
    }

    let summary_lower = summary.to_lowercase();

    for keyword in keywords {
        let keyword_lower = keyword.to_lowercase();
        if summary_lower.contains(&keyword_lower) {
            return weights::KEYWORD_SUMMARY;
        }
    }

    0.0
}

/// A learning with its computed relevance score.
#[derive(Debug, Clone)]
pub struct ScoredLearning {
    /// The learning.
    pub learning: CompoundLearning,
    /// The relevance score (0.0 to 1.0).
    pub score: f64,
}

impl ScoredLearning {
    /// Create a new scored learning.
    pub fn new(learning: CompoundLearning, score: f64) -> Self {
        Self { learning, score }
    }
}

/// Rank learnings by their relevance to the query.
///
/// Returns the top N learnings sorted by score (highest first).
/// Learnings with score 0.0 are excluded.
pub fn rank(
    query: &SearchQuery,
    learnings: &[CompoundLearning],
    limit: usize,
) -> Vec<ScoredLearning> {
    let mut scored: Vec<ScoredLearning> = learnings
        .iter()
        .map(|l| {
            let s = score(query, l);
            ScoredLearning::new(l.clone(), s)
        })
        .filter(|sl| sl.score > 0.0)
        .collect();

    // Sort by score descending
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Take top N
    scored.truncate(limit);
    scored
}

/// Score and rank, returning only the learnings (without scores).
pub fn rank_learnings(
    query: &SearchQuery,
    learnings: &[CompoundLearning],
    limit: usize,
) -> Vec<CompoundLearning> {
    rank(query, learnings, limit)
        .into_iter()
        .map(|sl| sl.learning)
        .collect()
}

// =============================================================================
// Composite Scoring
// =============================================================================

/// Calculate the recency weight for a learning based on its age.
///
/// Uses exponential decay: e^(-λ × days_since_creation)
///
/// - 1-day-old learning: ~1.0
/// - 90-day-old learning: ~0.3
/// - Very old learnings: clamped to MIN_WEIGHT (0.1)
pub fn recency_weight(created_at: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    let days = (now - created_at).num_days().max(0) as f64;

    let weight = (-recency::LAMBDA * days).exp();

    // Clamp to valid range
    weight.clamp(recency::MIN_WEIGHT, recency::MAX_WEIGHT)
}

/// Calculate the reference boost based on hit rate.
///
/// Hit rate is: referenced / surfaced (or 0.0 if never surfaced).
///
/// Returns: 0.5 + (hit_rate × 0.5)
/// Range: 0.5 (never referenced) to 1.0 (always referenced)
pub fn reference_boost(hit_rate: f64) -> f64 {
    let clamped_rate = hit_rate.clamp(0.0, 1.0);
    reference::BASE_BOOST + (clamped_rate * reference::MAX_ADDITIONAL)
}

/// Learning statistics needed for composite scoring.
#[derive(Debug, Clone, Default)]
pub struct LearningStats {
    /// Number of times the learning was surfaced (injected at session start).
    pub surfaced: u32,
    /// Number of times the learning was referenced (used in reflection).
    pub referenced: u32,
}

impl LearningStats {
    /// Create new stats with the given counts.
    pub fn new(surfaced: u32, referenced: u32) -> Self {
        Self {
            surfaced,
            referenced,
        }
    }

    /// Calculate the hit rate (referenced / surfaced).
    ///
    /// Returns 0.0 if never surfaced.
    pub fn hit_rate(&self) -> f64 {
        if self.surfaced == 0 {
            0.0
        } else {
            self.referenced as f64 / self.surfaced as f64
        }
    }
}

/// A learning with its computed composite score.
#[derive(Debug, Clone)]
pub struct CompositeScore {
    /// The learning.
    pub learning: CompoundLearning,
    /// The relevance score component (0.0 to 1.0).
    pub relevance: f64,
    /// The recency weight component (0.1 to 1.0).
    pub recency: f64,
    /// The reference boost component (0.5 to 1.0).
    pub reference: f64,
    /// The final composite score.
    pub score: f64,
}

impl CompositeScore {
    /// Create a new composite score.
    pub fn new(
        learning: CompoundLearning,
        relevance: f64,
        recency: f64,
        reference: f64,
        strategy: Strategy,
    ) -> Self {
        // Apply strategy multipliers
        let adjusted_recency = (recency * strategy.recency_multiplier()).min(1.0);
        let adjusted_reference = (reference * strategy.reference_multiplier()).min(1.0);

        // Composite score is the product of all factors
        let score = relevance * adjusted_recency * adjusted_reference;

        Self {
            learning,
            relevance,
            recency,
            reference,
            score,
        }
    }
}

/// Calculate composite scores for learnings with full ranking.
///
/// This is the main entry point for retrieval scoring that combines:
/// - Relevance (from search query matching)
/// - Recency (exponential decay from creation date)
/// - Reference boost (from historical hit rate)
///
/// Strategy-specific behavior:
/// - **Conservative**: Only includes learnings with exact tag/file matches
///   (relevance >= 0.8). Caps at 3 results.
/// - **Moderate**: Includes all matching learnings. Caps at 5 results.
/// - **Aggressive**: Includes all matching learnings plus recent learnings
///   (last 30 days) even without a match. Caps at 10 results.
///
/// # Arguments
///
/// * `query` - The search query for relevance scoring
/// * `learnings` - The learnings to score
/// * `stats` - Function to retrieve stats for a learning by ID
/// * `strategy` - The scoring strategy to use
/// * `now` - The current time (for recency calculation)
/// * `limit` - Maximum number of results to return (capped by strategy default)
pub fn composite_rank<F>(
    query: &SearchQuery,
    learnings: &[CompoundLearning],
    stats: F,
    strategy: Strategy,
    now: DateTime<Utc>,
    limit: usize,
) -> Vec<CompositeScore>
where
    F: Fn(&str) -> LearningStats,
{
    let min_threshold = strategy.min_relevance_threshold();

    let mut scored: Vec<CompositeScore> = learnings
        .iter()
        .filter_map(|l| {
            // Calculate relevance
            let relevance = score(query, l);

            // Check if this learning qualifies based on strategy
            let qualifies = if relevance >= min_threshold && relevance > 0.0 {
                // Matches according to strategy threshold
                true
            } else if strategy.includes_recent_without_match() {
                // Aggressive mode: include recent learnings even without match
                let days_old = (now - l.timestamp).num_days();
                (0..=recency::AGGRESSIVE_RECENT_DAYS).contains(&days_old)
            } else {
                false
            };

            if !qualifies {
                return None;
            }

            // Calculate recency
            let recency = recency_weight(l.timestamp, now);

            // Get stats and calculate reference boost
            let learning_stats = stats(&l.id);
            let reference = reference_boost(learning_stats.hit_rate());

            Some(CompositeScore::new(
                l.clone(),
                relevance,
                recency,
                reference,
                strategy,
            ))
        })
        .collect();

    // Sort by composite score descending, with recency as tiebreaker
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                // Tiebreaker: more recent first
                b.learning.timestamp.cmp(&a.learning.timestamp)
            })
    });

    // Apply limit, respecting strategy default
    let effective_limit = limit.min(strategy.default_max_injections());
    scored.truncate(effective_limit);
    scored
}

/// Simplified composite ranking that returns only learnings.
pub fn composite_rank_learnings<F>(
    query: &SearchQuery,
    learnings: &[CompoundLearning],
    stats: F,
    strategy: Strategy,
    now: DateTime<Utc>,
    limit: usize,
) -> Vec<CompoundLearning>
where
    F: Fn(&str) -> LearningStats,
{
    composite_rank(query, learnings, stats, strategy, now, limit)
        .into_iter()
        .map(|cs| cs.learning)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Confidence, LearningCategory, LearningScope, WriteGateCriterion};

    fn make_learning(summary: &str, tags: Vec<&str>, files: Option<Vec<&str>>) -> CompoundLearning {
        let mut learning = CompoundLearning::new(
            LearningCategory::Pattern,
            summary,
            "This is a detailed explanation for testing purposes.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            tags.into_iter().map(|s| s.to_string()).collect(),
            "test-session",
        );
        if let Some(f) = files {
            learning = learning.with_context_files(f.into_iter().map(|s| s.to_string()).collect());
        }
        learning
    }

    // Tag scoring tests

    #[test]
    fn test_score_exact_tag_match() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let learning = make_learning("Test summary", vec!["rust", "testing"], None);

        let s = score(&query, &learning);
        assert!((s - weights::TAG_EXACT).abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_exact_tag_match_case_insensitive() {
        let query = SearchQuery::with_tags(vec!["RUST".to_string()]);
        let learning = make_learning("Test summary", vec!["rust"], None);

        let s = score(&query, &learning);
        assert!((s - weights::TAG_EXACT).abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_partial_tag_match() {
        let query = SearchQuery::with_tags(vec!["error".to_string()]);
        let learning = make_learning("Test summary", vec!["error-handling"], None);

        let s = score(&query, &learning);
        assert!((s - weights::TAG_PARTIAL).abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_partial_tag_match_reverse() {
        // Query tag contains learning tag as substring
        let query = SearchQuery::with_tags(vec!["error-handling".to_string()]);
        let learning = make_learning("Test summary", vec!["error"], None);

        let s = score(&query, &learning);
        assert!((s - weights::TAG_PARTIAL).abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_no_tag_match() {
        let query = SearchQuery::with_tags(vec!["python".to_string()]);
        let learning = make_learning("Test summary", vec!["rust", "testing"], None);

        let s = score(&query, &learning);
        assert!(s.abs() < f64::EPSILON);
    }

    // File scoring tests

    #[test]
    fn test_score_exact_file_match() {
        let query = SearchQuery::with_files(vec!["src/main.rs".to_string()]);
        let learning = make_learning("Test", vec!["test"], Some(vec!["src/main.rs"]));

        let s = score(&query, &learning);
        assert!((s - weights::FILE_OVERLAP).abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_filename_match() {
        let query = SearchQuery::with_files(vec!["src/main.rs".to_string()]);
        let learning = make_learning("Test", vec!["test"], Some(vec!["other/main.rs"]));

        let s = score(&query, &learning);
        assert!((s - weights::FILE_OVERLAP).abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_file_suffix_match() {
        let query = SearchQuery::with_files(vec!["/project/src/lib.rs".to_string()]);
        let learning = make_learning("Test", vec!["test"], Some(vec!["src/lib.rs"]));

        let s = score(&query, &learning);
        assert!((s - weights::FILE_OVERLAP).abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_no_file_match() {
        let query = SearchQuery::with_files(vec!["src/main.rs".to_string()]);
        let learning = make_learning("Test", vec!["test"], Some(vec!["src/lib.rs"]));

        let s = score(&query, &learning);
        assert!(s.abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_no_context_files() {
        let query = SearchQuery::with_files(vec!["src/main.rs".to_string()]);
        let learning = make_learning("Test", vec!["test"], None);

        let s = score(&query, &learning);
        assert!(s.abs() < f64::EPSILON);
    }

    // Keyword scoring tests

    #[test]
    fn test_score_keyword_in_summary() {
        let query = SearchQuery::with_keywords(vec!["error".to_string()]);
        let learning = make_learning("Handle error gracefully", vec!["test"], None);

        let s = score(&query, &learning);
        assert!((s - weights::KEYWORD_SUMMARY).abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_keyword_case_insensitive() {
        let query = SearchQuery::with_keywords(vec!["ERROR".to_string()]);
        let learning = make_learning("Handle error gracefully", vec!["test"], None);

        let s = score(&query, &learning);
        assert!((s - weights::KEYWORD_SUMMARY).abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_no_keyword_match() {
        let query = SearchQuery::with_keywords(vec!["database".to_string()]);
        let learning = make_learning("Handle error gracefully", vec!["test"], None);

        let s = score(&query, &learning);
        assert!(s.abs() < f64::EPSILON);
    }

    // Combined scoring tests

    #[test]
    fn test_score_uses_max_not_sum() {
        // Query matches on both tags (1.0) and keywords (0.3)
        // Should return max (1.0), not sum (1.3)
        let query = SearchQuery::new()
            .tags(vec!["rust".to_string()])
            .keywords(vec!["error".to_string()]);
        let learning = make_learning("Handle error", vec!["rust"], None);

        let s = score(&query, &learning);
        assert!((s - weights::TAG_EXACT).abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_empty_query() {
        let query = SearchQuery::new();
        let learning = make_learning("Test summary", vec!["rust"], None);

        let s = score(&query, &learning);
        assert!(s.abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_multiple_matches_picks_highest() {
        // Query matches partial tag (0.5) and file (0.8)
        // Should return 0.8
        let query = SearchQuery::new()
            .tags(vec!["err".to_string()])
            .files(vec!["src/main.rs".to_string()]);
        let learning = make_learning("Test", vec!["error"], Some(vec!["src/main.rs"]));

        let s = score(&query, &learning);
        assert!((s - weights::FILE_OVERLAP).abs() < f64::EPSILON);
    }

    // Ranking tests

    #[test]
    fn test_rank_sorts_by_score() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);

        let learnings = vec![
            make_learning("Test", vec!["testing"], None), // 0.0
            make_learning("Rust learning", vec!["rust"], None), // 1.0 exact
            make_learning("Partial", vec!["rusty"], None), // 0.5 partial
        ];

        let ranked = rank(&query, &learnings, 10);

        assert_eq!(ranked.len(), 2); // Two have non-zero scores
        assert!((ranked[0].score - weights::TAG_EXACT).abs() < f64::EPSILON);
        assert!((ranked[1].score - weights::TAG_PARTIAL).abs() < f64::EPSILON);
        assert_eq!(ranked[0].learning.summary, "Rust learning");
    }

    #[test]
    fn test_rank_respects_limit() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);

        let learnings = vec![
            make_learning("One", vec!["rust"], None),
            make_learning("Two", vec!["rust"], None),
            make_learning("Three", vec!["rust"], None),
        ];

        let ranked = rank(&query, &learnings, 2);

        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn test_rank_excludes_zero_scores() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);

        let learnings = vec![
            make_learning("No match", vec!["python"], None),
            make_learning("Match", vec!["rust"], None),
        ];

        let ranked = rank(&query, &learnings, 10);

        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].learning.summary, "Match");
    }

    #[test]
    fn test_rank_empty_input() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let learnings: Vec<CompoundLearning> = vec![];

        let ranked = rank(&query, &learnings, 10);

        assert!(ranked.is_empty());
    }

    #[test]
    fn test_rank_learnings_returns_only_learnings() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);

        let learnings = vec![make_learning("Match", vec!["rust"], None)];

        let result = rank_learnings(&query, &learnings, 10);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].summary, "Match");
    }

    // Edge cases

    #[test]
    fn test_files_overlap_exact() {
        assert!(files_overlap("src/main.rs", "src/main.rs"));
    }

    #[test]
    fn test_files_overlap_filename_only() {
        assert!(files_overlap("a/b/main.rs", "x/y/main.rs"));
    }

    #[test]
    fn test_files_overlap_suffix() {
        assert!(files_overlap("/full/path/src/main.rs", "src/main.rs"));
    }

    #[test]
    fn test_files_overlap_no_match() {
        assert!(!files_overlap("src/main.rs", "src/lib.rs"));
    }

    #[test]
    fn test_files_overlap_empty_path() {
        assert!(!files_overlap("", ""));
        assert!(!files_overlap("src/main.rs", ""));
    }

    // =======================================================================
    // Recency Weight Tests
    // =======================================================================

    #[test]
    fn test_recency_weight_brand_new() {
        let now = Utc::now();
        let created = now; // Just created

        let weight = recency_weight(created, now);
        assert!((weight - 1.0).abs() < 0.01); // ~1.0
    }

    #[test]
    fn test_recency_weight_one_day_old() {
        let now = Utc::now();
        let created = now - chrono::Duration::days(1);

        let weight = recency_weight(created, now);
        // e^(-0.01338 * 1) ≈ 0.987
        assert!(weight > 0.98 && weight < 1.0);
    }

    #[test]
    fn test_recency_weight_90_days_old() {
        let now = Utc::now();
        let created = now - chrono::Duration::days(90);

        let weight = recency_weight(created, now);
        // e^(-0.01338 * 90) ≈ 0.3
        assert!(weight > 0.25 && weight < 0.35);
    }

    #[test]
    fn test_recency_weight_very_old() {
        let now = Utc::now();
        let created = now - chrono::Duration::days(365);

        let weight = recency_weight(created, now);
        // Should be clamped to MIN_WEIGHT (0.1)
        assert!(weight >= recency::MIN_WEIGHT);
    }

    #[test]
    fn test_recency_weight_future_date() {
        let now = Utc::now();
        let created = now + chrono::Duration::days(1); // Future date

        let weight = recency_weight(created, now);
        // Should treat as 0 days old
        assert!((weight - 1.0).abs() < 0.01);
    }

    // =======================================================================
    // Reference Boost Tests
    // =======================================================================

    #[test]
    fn test_reference_boost_never_referenced() {
        let boost = reference_boost(0.0);
        assert!((boost - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_reference_boost_always_referenced() {
        let boost = reference_boost(1.0);
        assert!((boost - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_reference_boost_half_referenced() {
        let boost = reference_boost(0.5);
        assert!((boost - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn test_reference_boost_clamps_negative() {
        let boost = reference_boost(-0.5);
        assert!((boost - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_reference_boost_clamps_over_one() {
        let boost = reference_boost(1.5);
        assert!((boost - 1.0).abs() < f64::EPSILON);
    }

    // =======================================================================
    // Learning Stats Tests
    // =======================================================================

    #[test]
    fn test_learning_stats_hit_rate_never_surfaced() {
        let stats = LearningStats::new(0, 0);
        assert!((stats.hit_rate() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_learning_stats_hit_rate_always_referenced() {
        let stats = LearningStats::new(10, 10);
        assert!((stats.hit_rate() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_learning_stats_hit_rate_half() {
        let stats = LearningStats::new(10, 5);
        assert!((stats.hit_rate() - 0.5).abs() < f64::EPSILON);
    }

    // =======================================================================
    // Strategy Tests
    // =======================================================================

    #[test]
    fn test_strategy_default_is_moderate() {
        assert_eq!(Strategy::default(), Strategy::Moderate);
    }

    #[test]
    fn test_strategy_conservative_multipliers() {
        let s = Strategy::Conservative;
        assert!(s.recency_multiplier() < 1.0);
        assert!(s.reference_multiplier() > 1.0);
    }

    #[test]
    fn test_strategy_moderate_multipliers() {
        let s = Strategy::Moderate;
        assert!((s.recency_multiplier() - 1.0).abs() < f64::EPSILON);
        assert!((s.reference_multiplier() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_strategy_aggressive_multipliers() {
        let s = Strategy::Aggressive;
        assert!(s.recency_multiplier() > 1.0);
        assert!(s.reference_multiplier() < 1.0);
    }

    #[test]
    fn test_strategy_parse_valid() {
        assert_eq!(
            Strategy::parse("conservative"),
            Some(Strategy::Conservative)
        );
        assert_eq!(Strategy::parse("moderate"), Some(Strategy::Moderate));
        assert_eq!(Strategy::parse("aggressive"), Some(Strategy::Aggressive));
    }

    #[test]
    fn test_strategy_parse_case_insensitive() {
        assert_eq!(
            Strategy::parse("CONSERVATIVE"),
            Some(Strategy::Conservative)
        );
        assert_eq!(Strategy::parse("Moderate"), Some(Strategy::Moderate));
        assert_eq!(Strategy::parse("AgGrEsSiVe"), Some(Strategy::Aggressive));
    }

    #[test]
    fn test_strategy_parse_invalid() {
        assert_eq!(Strategy::parse("unknown"), None);
        assert_eq!(Strategy::parse(""), None);
        assert_eq!(Strategy::parse("mod"), None);
    }

    #[test]
    fn test_strategy_as_str() {
        assert_eq!(Strategy::Conservative.as_str(), "conservative");
        assert_eq!(Strategy::Moderate.as_str(), "moderate");
        assert_eq!(Strategy::Aggressive.as_str(), "aggressive");
    }

    #[test]
    fn test_strategy_parse_roundtrip() {
        for s in [
            Strategy::Conservative,
            Strategy::Moderate,
            Strategy::Aggressive,
        ] {
            assert_eq!(Strategy::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn test_strategy_default_max_injections() {
        assert_eq!(Strategy::Conservative.default_max_injections(), 3);
        assert_eq!(Strategy::Moderate.default_max_injections(), 5);
        assert_eq!(Strategy::Aggressive.default_max_injections(), 10);
    }

    #[test]
    fn test_strategy_min_relevance_threshold() {
        // Conservative requires file-level match (0.8)
        assert!((Strategy::Conservative.min_relevance_threshold() - 0.8).abs() < f64::EPSILON);
        // Moderate accepts any match
        assert!((Strategy::Moderate.min_relevance_threshold() - 0.0).abs() < f64::EPSILON);
        // Aggressive has no threshold
        assert!((Strategy::Aggressive.min_relevance_threshold() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_strategy_includes_recent_without_match() {
        assert!(!Strategy::Conservative.includes_recent_without_match());
        assert!(!Strategy::Moderate.includes_recent_without_match());
        assert!(Strategy::Aggressive.includes_recent_without_match());
    }

    // =======================================================================
    // Composite Score Tests
    // =======================================================================

    #[test]
    fn test_composite_score_all_max() {
        let learning = make_learning("Test", vec!["rust"], None);

        // relevance=1.0, recency=1.0, reference=1.0
        let cs = CompositeScore::new(learning, 1.0, 1.0, 1.0, Strategy::Moderate);

        assert!((cs.score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_composite_score_low_reference() {
        let learning = make_learning("Test", vec!["rust"], None);

        // relevance=1.0, recency=1.0, reference=0.5
        let cs = CompositeScore::new(learning, 1.0, 1.0, 0.5, Strategy::Moderate);

        assert!((cs.score - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_composite_score_low_recency() {
        let learning = make_learning("Test", vec!["rust"], None);

        // relevance=1.0, recency=0.3, reference=1.0
        let cs = CompositeScore::new(learning, 1.0, 0.3, 1.0, Strategy::Moderate);

        assert!((cs.score - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_composite_score_conservative_strategy() {
        let learning = make_learning("Test", vec!["rust"], None);

        // With conservative, reference boost is amplified
        let cs = CompositeScore::new(learning.clone(), 1.0, 0.5, 0.8, Strategy::Conservative);

        // recency: 0.5 * 0.7 = 0.35
        // reference: 0.8 * 1.3 = 1.04 -> clamped to 1.0
        // score = 1.0 * 0.35 * 1.0 = 0.35
        assert!((cs.score - 0.35).abs() < 0.01);
    }

    #[test]
    fn test_composite_score_aggressive_strategy() {
        let learning = make_learning("Test", vec!["rust"], None);

        // With aggressive, recency is amplified
        let cs = CompositeScore::new(learning.clone(), 1.0, 0.5, 0.8, Strategy::Aggressive);

        // recency: 0.5 * 1.3 = 0.65
        // reference: 0.8 * 0.7 = 0.56
        // score = 1.0 * 0.65 * 0.56 = 0.364
        assert!((cs.score - 0.364).abs() < 0.01);
    }

    // =======================================================================
    // Composite Ranking Tests
    // =======================================================================

    #[test]
    fn test_composite_rank_excludes_zero_relevance() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let learnings = vec![
            make_learning("No match", vec!["python"], None),
            make_learning("Match", vec!["rust"], None),
        ];

        let now = Utc::now();
        let stats = |_: &str| LearningStats::new(10, 5);

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Moderate, now, 10);

        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].learning.summary, "Match");
    }

    #[test]
    fn test_composite_rank_sorts_by_score() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);

        // Create learnings with different ages
        let mut learning1 = make_learning("Old match", vec!["rust"], None);
        learning1.timestamp = Utc::now() - chrono::Duration::days(60);

        let mut learning2 = make_learning("Recent match", vec!["rust"], None);
        learning2.timestamp = Utc::now() - chrono::Duration::days(1);

        let learnings = vec![learning1, learning2];
        let now = Utc::now();

        // Same stats for both
        let stats = |_: &str| LearningStats::new(10, 5);

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Moderate, now, 10);

        assert_eq!(ranked.len(), 2);
        // Recent learning should rank higher
        assert_eq!(ranked[0].learning.summary, "Recent match");
        assert_eq!(ranked[1].learning.summary, "Old match");
    }

    #[test]
    fn test_composite_rank_respects_limit() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let learnings = vec![
            make_learning("One", vec!["rust"], None),
            make_learning("Two", vec!["rust"], None),
            make_learning("Three", vec!["rust"], None),
        ];

        let now = Utc::now();
        let stats = |_: &str| LearningStats::new(10, 5);

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Moderate, now, 2);

        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn test_composite_rank_uses_stats() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);

        // Same age learnings
        let learnings = vec![
            make_learning("High hit rate", vec!["rust"], None),
            make_learning("Low hit rate", vec!["rust"], None),
        ];

        let now = Utc::now();

        // Different stats for each
        let stats = |id: &str| {
            if id == learnings[0].id {
                LearningStats::new(10, 10) // 100% hit rate
            } else {
                LearningStats::new(10, 1) // 10% hit rate
            }
        };

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Moderate, now, 10);

        assert_eq!(ranked.len(), 2);
        // Higher hit rate should rank higher
        assert!(ranked[0].reference > ranked[1].reference);
    }

    #[test]
    fn test_composite_rank_learnings_returns_only_learnings() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let learnings = vec![make_learning("Match", vec!["rust"], None)];

        let now = Utc::now();
        let stats = |_: &str| LearningStats::new(10, 5);

        let result =
            composite_rank_learnings(&query, &learnings, stats, Strategy::Moderate, now, 10);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].summary, "Match");
    }

    // =======================================================================
    // Strategy-Specific Filtering Tests
    // =======================================================================

    #[test]
    fn test_conservative_filters_partial_matches() {
        // Conservative only accepts exact tag (1.0) or file (0.8) matches
        let query = SearchQuery::with_tags(vec!["err".to_string()]);

        let learnings = vec![
            // Partial tag match (0.5) - should be filtered
            make_learning("Partial match", vec!["error-handling"], None),
            // Exact tag match (1.0) - should pass
            make_learning("Exact match", vec!["err"], None),
        ];

        let now = Utc::now();
        let stats = |_: &str| LearningStats::new(10, 5);

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Conservative, now, 10);

        // Only exact match should pass
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].learning.summary, "Exact match");
    }

    #[test]
    fn test_conservative_accepts_file_matches() {
        let query = SearchQuery::with_files(vec!["src/main.rs".to_string()]);

        let learnings = vec![
            // File match (0.8) - should pass conservative threshold
            make_learning("File match", vec!["test"], Some(vec!["src/main.rs"])),
        ];

        let now = Utc::now();
        let stats = |_: &str| LearningStats::new(10, 5);

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Conservative, now, 10);

        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].learning.summary, "File match");
    }

    #[test]
    fn test_conservative_filters_keyword_only_matches() {
        // Keyword matches (0.3) are below conservative threshold (0.8)
        let query = SearchQuery::with_keywords(vec!["database".to_string()]);

        let learnings = vec![make_learning(
            "Database optimization tips",
            vec!["unrelated"],
            None,
        )];

        let now = Utc::now();
        let stats = |_: &str| LearningStats::new(10, 5);

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Conservative, now, 10);

        // Keyword-only match (0.3) should be filtered by conservative (threshold 0.8)
        assert_eq!(ranked.len(), 0);
    }

    #[test]
    fn test_moderate_accepts_partial_matches() {
        let query = SearchQuery::with_tags(vec!["err".to_string()]);

        let learnings = vec![
            // Partial tag match (0.5) - should pass for moderate
            make_learning("Partial match", vec!["error-handling"], None),
        ];

        let now = Utc::now();
        let stats = |_: &str| LearningStats::new(10, 5);

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Moderate, now, 10);

        assert_eq!(ranked.len(), 1);
    }

    #[test]
    fn test_moderate_accepts_keyword_matches() {
        let query = SearchQuery::with_keywords(vec!["database".to_string()]);

        let learnings = vec![make_learning(
            "Database optimization tips",
            vec!["unrelated"],
            None,
        )];

        let now = Utc::now();
        let stats = |_: &str| LearningStats::new(10, 5);

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Moderate, now, 10);

        // Keyword match (0.3) should pass for moderate (threshold 0.0)
        assert_eq!(ranked.len(), 1);
    }

    #[test]
    fn test_aggressive_includes_recent_without_match() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let now = Utc::now();

        // Recent learning (10 days old) with no match
        let mut recent_no_match = make_learning("Recent unrelated", vec!["python"], None);
        recent_no_match.timestamp = now - chrono::Duration::days(10);

        // Old learning (60 days old) with no match
        let mut old_no_match = make_learning("Old unrelated", vec!["java"], None);
        old_no_match.timestamp = now - chrono::Duration::days(60);

        // Learning with match
        let matching = make_learning("Matching", vec!["rust"], None);

        let learnings = vec![recent_no_match, old_no_match, matching];
        let stats = |_: &str| LearningStats::new(10, 5);

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Aggressive, now, 10);

        // Aggressive should include: matching (any) + recent_no_match (within 30 days)
        // But NOT old_no_match (outside 30 days and no match)
        assert_eq!(ranked.len(), 2);
        let summaries: Vec<_> = ranked.iter().map(|r| r.learning.summary.as_str()).collect();
        assert!(summaries.contains(&"Matching"));
        assert!(summaries.contains(&"Recent unrelated"));
        assert!(!summaries.contains(&"Old unrelated"));
    }

    #[test]
    fn test_aggressive_recent_boundary() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let now = Utc::now();

        // Learning at exactly 30 days - should be included
        let mut at_boundary = make_learning("At boundary", vec!["python"], None);
        at_boundary.timestamp = now - chrono::Duration::days(30);

        // Learning at 31 days - should NOT be included (no match)
        let mut beyond_boundary = make_learning("Beyond boundary", vec!["java"], None);
        beyond_boundary.timestamp = now - chrono::Duration::days(31);

        let learnings = vec![at_boundary, beyond_boundary];
        let stats = |_: &str| LearningStats::new(10, 5);

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Aggressive, now, 10);

        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].learning.summary, "At boundary");
    }

    #[test]
    fn test_moderate_does_not_include_recent_without_match() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let now = Utc::now();

        // Recent learning with no match
        let mut recent_no_match = make_learning("Recent unrelated", vec!["python"], None);
        recent_no_match.timestamp = now - chrono::Duration::days(10);

        let learnings = vec![recent_no_match];
        let stats = |_: &str| LearningStats::new(10, 5);

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Moderate, now, 10);

        // Moderate should NOT include learnings without a match
        assert_eq!(ranked.len(), 0);
    }

    #[test]
    fn test_conservative_does_not_include_recent_without_match() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let now = Utc::now();

        // Recent learning with no match
        let mut recent_no_match = make_learning("Recent unrelated", vec!["python"], None);
        recent_no_match.timestamp = now - chrono::Duration::days(10);

        let learnings = vec![recent_no_match];
        let stats = |_: &str| LearningStats::new(10, 5);

        let ranked = composite_rank(&query, &learnings, stats, Strategy::Conservative, now, 10);

        // Conservative should NOT include learnings without a match
        assert_eq!(ranked.len(), 0);
    }

    // =======================================================================
    // Strategy Limit Cap Tests
    // =======================================================================

    #[test]
    fn test_conservative_caps_at_three() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let learnings: Vec<_> = (0..10)
            .map(|i| make_learning(&format!("Learning {}", i), vec!["rust"], None))
            .collect();

        let now = Utc::now();
        let stats = |_: &str| LearningStats::new(10, 5);

        // Request 10 but conservative should cap at 3
        let ranked = composite_rank(&query, &learnings, stats, Strategy::Conservative, now, 10);

        assert_eq!(ranked.len(), 3);
    }

    #[test]
    fn test_moderate_caps_at_five() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let learnings: Vec<_> = (0..10)
            .map(|i| make_learning(&format!("Learning {}", i), vec!["rust"], None))
            .collect();

        let now = Utc::now();
        let stats = |_: &str| LearningStats::new(10, 5);

        // Request 10 but moderate should cap at 5
        let ranked = composite_rank(&query, &learnings, stats, Strategy::Moderate, now, 10);

        assert_eq!(ranked.len(), 5);
    }

    #[test]
    fn test_aggressive_caps_at_ten() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let learnings: Vec<_> = (0..15)
            .map(|i| make_learning(&format!("Learning {}", i), vec!["rust"], None))
            .collect();

        let now = Utc::now();
        let stats = |_: &str| LearningStats::new(10, 5);

        // Request 15 but aggressive should cap at 10
        let ranked = composite_rank(&query, &learnings, stats, Strategy::Aggressive, now, 15);

        assert_eq!(ranked.len(), 10);
    }

    #[test]
    fn test_user_limit_respected_when_lower_than_strategy() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let learnings: Vec<_> = (0..10)
            .map(|i| make_learning(&format!("Learning {}", i), vec!["rust"], None))
            .collect();

        let now = Utc::now();
        let stats = |_: &str| LearningStats::new(10, 5);

        // Request 2 - should respect user limit even though aggressive allows 10
        let ranked = composite_rank(&query, &learnings, stats, Strategy::Aggressive, now, 2);

        assert_eq!(ranked.len(), 2);
    }
}
