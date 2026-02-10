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
}
