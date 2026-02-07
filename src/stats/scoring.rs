//! Relevance scoring for Grove learnings.
//!
//! This module provides scoring functions for ranking learnings based on
//! their relevance to a search query. Stage 1 uses relevance matching only.
//!
//! Scoring weights:
//! - Exact tag match: 1.0
//! - Partial tag match: 0.5
//! - File path overlap: 0.8
//! - Keyword in summary: 0.3
//!
//! Scores are combined via max (not sum).

use crate::backends::SearchQuery;
use crate::core::CompoundLearning;

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
}
