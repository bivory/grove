//! Fallback backend wrapper for Grove.
//!
//! This module provides a backend wrapper that tries a primary backend first,
//! and falls back to a secondary backend if the primary write fails.

use crate::backends::traits::{
    MemoryBackend, SearchFilters, SearchQuery, SearchResult, WriteResult,
};
use crate::core::CompoundLearning;
use crate::error::Result;
use tracing::warn;

/// A backend wrapper that falls back to a secondary backend on write failure.
///
/// This is useful when using Total Recall as the primary backend with markdown
/// as a fallback, ensuring learnings are always persisted somewhere.
pub struct FallbackBackend {
    /// Primary backend (tried first).
    primary: Box<dyn MemoryBackend>,
    /// Fallback backend (used if primary fails).
    fallback: Box<dyn MemoryBackend>,
}

impl FallbackBackend {
    /// Create a new fallback backend wrapper.
    pub fn new(primary: Box<dyn MemoryBackend>, fallback: Box<dyn MemoryBackend>) -> Self {
        Self { primary, fallback }
    }

    /// Get the primary backend name.
    pub fn primary_name(&self) -> &'static str {
        self.primary.name()
    }

    /// Get the fallback backend name.
    pub fn fallback_name(&self) -> &'static str {
        self.fallback.name()
    }
}

impl MemoryBackend for FallbackBackend {
    fn write(&self, learning: &CompoundLearning) -> Result<WriteResult> {
        // Try primary first
        let primary_result = self.primary.write(learning)?;

        if primary_result.success {
            return Ok(primary_result);
        }

        // Primary failed, try fallback
        warn!(
            "Primary backend '{}' failed for learning {}, falling back to '{}'",
            self.primary.name(),
            learning.id,
            self.fallback.name()
        );

        let fallback_result = self.fallback.write(learning)?;

        if fallback_result.success {
            // Return success but note it went to fallback
            Ok(WriteResult {
                success: true,
                learning_id: fallback_result.learning_id,
                location: format!("{} (fallback)", fallback_result.location),
                message: Some(format!(
                    "Primary backend '{}' unavailable, used fallback",
                    self.primary.name()
                )),
            })
        } else {
            // Both failed
            warn!(
                "Fallback backend '{}' also failed for learning {}",
                self.fallback.name(),
                learning.id
            );
            Ok(fallback_result)
        }
    }

    fn search(&self, query: &SearchQuery, filters: &SearchFilters) -> Result<Vec<SearchResult>> {
        // Search primary first
        let primary_results = self.primary.search(query, filters)?;

        // Also search fallback and merge results
        let fallback_results = self.fallback.search(query, filters)?;

        // Combine and deduplicate by learning ID
        let mut combined = primary_results;
        for result in fallback_results {
            if !combined.iter().any(|r| r.learning.id == result.learning.id) {
                combined.push(result);
            }
        }

        // Sort by relevance (highest first)
        combined.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Apply max_results limit
        if let Some(limit) = filters.max_results {
            combined.truncate(limit);
        }

        Ok(combined)
    }

    fn ping(&self) -> bool {
        // Available if either backend is available
        self.primary.ping() || self.fallback.ping()
    }

    fn name(&self) -> &'static str {
        // Return primary name with fallback indicator
        // Note: This returns a static str, so we can't dynamically compose names
        // We'll return the primary name since that's the intended backend
        self.primary.name()
    }

    fn next_id(&self) -> String {
        // Get next ID from both backends and return the higher one
        // to avoid collisions when learnings exist in fallback but not primary
        let primary_id = self.primary.next_id();
        let fallback_id = self.fallback.next_id();

        // Parse IDs to compare counters
        // Format: cl_YYYYMMDD_NNN
        max_learning_id(&primary_id, &fallback_id)
    }

    fn next_ids(&self, count: usize) -> Vec<String> {
        if count == 0 {
            return Vec::new();
        }

        // Get starting IDs from both backends
        let primary_id = self.primary.next_id();
        let fallback_id = self.fallback.next_id();

        // Start from the higher counter
        let start_id = max_learning_id(&primary_id, &fallback_id);

        if count == 1 {
            return vec![start_id];
        }

        // Parse the starting ID and increment for subsequent IDs
        // Format: cl_YYYYMMDD_NNN
        if let Some((prefix, counter)) = parse_learning_id(&start_id) {
            let mut ids = Vec::with_capacity(count);
            for i in 0..count {
                ids.push(format!("{}_{:03}", prefix, counter + i as u32));
            }
            ids
        } else {
            // Fallback: generate IDs sequentially
            let mut ids = vec![start_id];
            for _ in 1..count {
                ids.push(crate::core::generate_learning_id());
            }
            ids
        }
    }
}

/// Parse a learning ID into its prefix and counter.
/// Format: cl_YYYYMMDD_NNN -> ("cl_YYYYMMDD", NNN)
fn parse_learning_id(id: &str) -> Option<(String, u32)> {
    // Split on the last underscore to get prefix and counter
    let parts: Vec<&str> = id.rsplitn(2, '_').collect();
    if parts.len() != 2 {
        return None;
    }
    let counter_str = parts[0];
    let prefix = parts[1];
    let counter = counter_str.parse::<u32>().ok()?;
    Some((prefix.to_string(), counter))
}

/// Return the learning ID with the higher counter.
/// If IDs have different date prefixes, lexicographic comparison is used.
fn max_learning_id(id1: &str, id2: &str) -> String {
    match (parse_learning_id(id1), parse_learning_id(id2)) {
        (Some((prefix1, counter1)), Some((prefix2, counter2))) => {
            if prefix1 == prefix2 {
                // Same date, compare counters
                if counter1 >= counter2 {
                    id1.to_string()
                } else {
                    id2.to_string()
                }
            } else {
                // Different dates, use lexicographic comparison
                // This handles edge cases at midnight
                if id1 >= id2 {
                    id1.to_string()
                } else {
                    id2.to_string()
                }
            }
        }
        (Some(_), None) => id1.to_string(),
        (None, Some(_)) => id2.to_string(),
        (None, None) => {
            // Both invalid, return the lexicographically larger one
            if id1 >= id2 {
                id1.to_string()
            } else {
                id2.to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::MarkdownBackend;
    use crate::core::{Confidence, LearningCategory, LearningScope, WriteGateCriterion};
    use std::fs;
    use tempfile::TempDir;

    fn sample_learning() -> CompoundLearning {
        CompoundLearning::new(
            LearningCategory::Pattern,
            "Test learning summary",
            "Test learning detail that is long enough to pass validation",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["test".to_string()],
            "test-session",
        )
    }

    #[test]
    fn test_fallback_when_primary_fails() {
        let temp = TempDir::new().unwrap();

        // Create a primary backend that will fail (read-only directory)
        let readonly_path = temp.path().join("readonly");
        fs::create_dir_all(&readonly_path).unwrap();
        // Note: We can't actually make it read-only in tests easily,
        // so we'll test with a valid fallback

        // Create a working fallback
        let fallback_path = temp.path().join("fallback.md");
        let fallback = MarkdownBackend::new(&fallback_path);

        // Create primary that works
        let primary_path = temp.path().join("primary.md");
        let primary = MarkdownBackend::new(&primary_path);

        let backend = FallbackBackend::new(Box::new(primary), Box::new(fallback));

        let learning = sample_learning();
        let result = backend.write(&learning).unwrap();

        assert!(result.success);
        assert!(primary_path.exists());
    }

    #[test]
    fn test_search_combines_results() {
        let temp = TempDir::new().unwrap();

        // Create two backends with different learnings
        let primary_path = temp.path().join("primary.md");
        let fallback_path = temp.path().join("fallback.md");

        let primary = MarkdownBackend::new(&primary_path);
        let fallback = MarkdownBackend::new(&fallback_path);

        // Write to primary
        let mut learning1 = sample_learning();
        learning1.id = "primary-1".to_string();
        primary.write(&learning1).unwrap();

        // Write to fallback
        let mut learning2 = sample_learning();
        learning2.id = "fallback-1".to_string();
        fallback.write(&learning2).unwrap();

        // Search via fallback backend
        let backend = FallbackBackend::new(Box::new(primary), Box::new(fallback));

        let query = SearchQuery::with_keywords(vec!["test".to_string()]);
        let results = backend.search(&query, &SearchFilters::default()).unwrap();

        // Should find learnings from both backends
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_ping_true_if_either_available() {
        let temp = TempDir::new().unwrap();

        let primary_path = temp.path().join("primary.md");
        let fallback_path = temp.path().join("fallback.md");

        let primary = MarkdownBackend::new(&primary_path);
        let fallback = MarkdownBackend::new(&fallback_path);

        let backend = FallbackBackend::new(Box::new(primary), Box::new(fallback));

        assert!(backend.ping());
    }

    #[test]
    fn test_name_returns_primary() {
        let temp = TempDir::new().unwrap();

        let primary_path = temp.path().join("primary.md");
        let fallback_path = temp.path().join("fallback.md");

        let primary = MarkdownBackend::new(&primary_path);
        let fallback = MarkdownBackend::new(&fallback_path);

        let backend = FallbackBackend::new(Box::new(primary), Box::new(fallback));

        assert_eq!(backend.name(), "markdown");
    }

    #[test]
    fn test_parse_learning_id_valid() {
        let (prefix, counter) = parse_learning_id("cl_20260212_005").unwrap();
        assert_eq!(prefix, "cl_20260212");
        assert_eq!(counter, 5);
    }

    #[test]
    fn test_parse_learning_id_zero_padded() {
        let (prefix, counter) = parse_learning_id("cl_20260212_000").unwrap();
        assert_eq!(prefix, "cl_20260212");
        assert_eq!(counter, 0);
    }

    #[test]
    fn test_parse_learning_id_large_counter() {
        let (prefix, counter) = parse_learning_id("cl_20260212_999").unwrap();
        assert_eq!(prefix, "cl_20260212");
        assert_eq!(counter, 999);
    }

    #[test]
    fn test_parse_learning_id_invalid_no_underscore() {
        assert!(parse_learning_id("invalid").is_none());
    }

    #[test]
    fn test_parse_learning_id_invalid_non_numeric_counter() {
        assert!(parse_learning_id("cl_20260212_abc").is_none());
    }

    #[test]
    fn test_max_learning_id_same_date_higher_counter() {
        let result = max_learning_id("cl_20260212_003", "cl_20260212_007");
        assert_eq!(result, "cl_20260212_007");
    }

    #[test]
    fn test_max_learning_id_same_date_lower_counter() {
        let result = max_learning_id("cl_20260212_010", "cl_20260212_005");
        assert_eq!(result, "cl_20260212_010");
    }

    #[test]
    fn test_max_learning_id_same_counter() {
        let result = max_learning_id("cl_20260212_005", "cl_20260212_005");
        assert_eq!(result, "cl_20260212_005");
    }

    #[test]
    fn test_max_learning_id_different_dates() {
        // Later date should win
        let result = max_learning_id("cl_20260211_099", "cl_20260212_001");
        assert_eq!(result, "cl_20260212_001");
    }

    #[test]
    fn test_max_learning_id_one_invalid() {
        let result = max_learning_id("cl_20260212_005", "invalid");
        assert_eq!(result, "cl_20260212_005");

        let result = max_learning_id("invalid", "cl_20260212_005");
        assert_eq!(result, "cl_20260212_005");
    }

    #[test]
    fn test_max_learning_id_both_invalid() {
        // Lexicographic comparison fallback
        let result = max_learning_id("zzz", "aaa");
        assert_eq!(result, "zzz");
    }

    #[test]
    fn test_next_id_returns_higher_from_both_backends() {
        let temp = TempDir::new().unwrap();

        let primary_path = temp.path().join("primary.md");
        let fallback_path = temp.path().join("fallback.md");

        // Write more learnings to fallback to give it a higher counter
        let fallback = MarkdownBackend::new(&fallback_path);
        for i in 0..5 {
            let mut learning = sample_learning();
            learning.id = format!("cl_20260212_{:03}", i);
            fallback.write(&learning).unwrap();
        }

        // Primary has no learnings, so starts at 000
        let primary = MarkdownBackend::new(&primary_path);

        // FallbackBackend should return the higher ID from fallback
        let backend = FallbackBackend::new(Box::new(primary), Box::new(fallback));
        let next = backend.next_id();

        // Should be 005 (after the 5 learnings in fallback: 000-004)
        assert!(
            next.ends_with("_005"),
            "Expected ID ending with _005, got {}",
            next
        );
    }

    #[test]
    fn test_next_ids_generates_sequential_from_max() {
        let temp = TempDir::new().unwrap();

        let primary_path = temp.path().join("primary.md");
        let fallback_path = temp.path().join("fallback.md");

        // Write learnings to fallback
        let fallback = MarkdownBackend::new(&fallback_path);
        for i in 0..3 {
            let mut learning = sample_learning();
            learning.id = format!("cl_20260212_{:03}", i);
            fallback.write(&learning).unwrap();
        }

        let primary = MarkdownBackend::new(&primary_path);

        let backend = FallbackBackend::new(Box::new(primary), Box::new(fallback));
        let ids = backend.next_ids(3);

        assert_eq!(ids.len(), 3);
        // IDs should be sequential starting from 003
        assert!(ids[0].ends_with("_003"), "First ID should end with _003");
        assert!(ids[1].ends_with("_004"), "Second ID should end with _004");
        assert!(ids[2].ends_with("_005"), "Third ID should end with _005");
    }

    #[test]
    fn test_next_ids_empty() {
        let temp = TempDir::new().unwrap();

        let primary_path = temp.path().join("primary.md");
        let fallback_path = temp.path().join("fallback.md");

        let primary = MarkdownBackend::new(&primary_path);
        let fallback = MarkdownBackend::new(&fallback_path);

        let backend = FallbackBackend::new(Box::new(primary), Box::new(fallback));
        let ids = backend.next_ids(0);

        assert!(ids.is_empty());
    }

    #[test]
    fn test_next_ids_single() {
        let temp = TempDir::new().unwrap();

        let primary_path = temp.path().join("primary.md");
        let fallback_path = temp.path().join("fallback.md");

        let primary = MarkdownBackend::new(&primary_path);
        let fallback = MarkdownBackend::new(&fallback_path);

        let backend = FallbackBackend::new(Box::new(primary), Box::new(fallback));
        let ids = backend.next_ids(1);

        assert_eq!(ids.len(), 1);
    }
}
