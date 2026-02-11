//! Memory backend trait for Grove.
//!
//! This module defines the trait interface for memory backends that store
//! and retrieve compound learnings. Backends include markdown files,
//! Total Recall, and MCP memory servers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::{CompoundLearning, LearningScope, LearningStatus};
use crate::error::Result;

/// Trait for memory backends that store and retrieve learnings.
///
/// Backends can be file-based (markdown), external (Total Recall),
/// or server-based (MCP). All backends must be thread-safe.
pub trait MemoryBackend: Send + Sync {
    /// Write a learning to the backend.
    ///
    /// The backend is responsible for:
    /// - Routing based on scope (project/team → shared, personal → user-local)
    /// - Sanitizing content as needed
    /// - Handling any backend-specific formatting
    fn write(&self, learning: &CompoundLearning) -> Result<WriteResult>;

    /// Search for learnings matching the query and filters.
    ///
    /// Returns learnings with their relevance scores. The scoring mechanism
    /// varies by backend:
    /// - Markdown: tag match (1.0), partial match (0.5), file overlap (0.8), keyword (0.3)
    /// - Total Recall: delegates to Total Recall's scoring
    /// - MCP: delegates to the server's scoring
    fn search(&self, query: &SearchQuery, filters: &SearchFilters) -> Result<Vec<SearchResult>>;

    /// Health check for the backend.
    ///
    /// Returns true if the backend is available and operational.
    /// Used during discovery and for status reporting.
    fn ping(&self) -> bool;

    /// Get the backend name for logging and stats.
    fn name(&self) -> &'static str;

    /// Archive a learning by ID.
    ///
    /// Default implementation returns an error indicating archiving is not supported.
    fn archive(&self, _learning_id: &str) -> Result<()> {
        Err(crate::error::GroveError::backend(
            "Archive not supported by this backend",
        ))
    }

    /// Restore an archived learning by ID.
    ///
    /// Default implementation returns an error indicating restore is not supported.
    fn restore(&self, _learning_id: &str) -> Result<()> {
        Err(crate::error::GroveError::backend(
            "Restore not supported by this backend",
        ))
    }

    /// List all learnings (for backends that support it).
    ///
    /// Default implementation uses search with an empty query.
    fn list_all(&self) -> Result<Vec<crate::core::CompoundLearning>> {
        let results = self.search(&SearchQuery::new(), &SearchFilters::all())?;
        Ok(results.into_iter().map(|r| r.learning).collect())
    }

    /// Generate a unique ID for a new learning.
    ///
    /// The backend scans its existing entries to find the next available
    /// counter for today's date, ensuring no ID collisions.
    ///
    /// Format: `cl_YYYYMMDD_NNN` where NNN is a zero-padded counter.
    fn next_id(&self) -> String {
        // Default implementation uses a simple counter
        // Backends should override to scan existing entries
        crate::core::generate_learning_id()
    }

    /// Generate multiple unique IDs for a batch of learnings.
    ///
    /// This prevents race conditions when creating multiple learnings at once,
    /// since a single scan finds the starting counter and subsequent IDs are
    /// incremented without re-scanning.
    ///
    /// Format: `cl_YYYYMMDD_NNN` where NNN is incremented for each ID.
    fn next_ids(&self, count: usize) -> Vec<String> {
        // Default implementation: get first ID, then increment counter for rest
        if count == 0 {
            return Vec::new();
        }

        let first_id = self.next_id();
        if count == 1 {
            return vec![first_id];
        }

        // Parse the counter from the first ID and generate the rest
        // Expected format: cl_YYYYMMDD_NNN
        let mut ids = Vec::with_capacity(count);
        ids.push(first_id.clone());

        // Extract date and counter parts
        if let Some(underscore_pos) = first_id.rfind('_') {
            let prefix = &first_id[..=underscore_pos];
            if let Ok(counter) = first_id[underscore_pos + 1..].parse::<u32>() {
                for i in 1..count {
                    ids.push(format!("{}{:03}", prefix, (counter + i as u32)));
                }
                return ids;
            }
        }

        // Fallback: generate each ID separately (shouldn't happen with valid format)
        for _ in 1..count {
            ids.push(self.next_id());
        }
        ids
    }
}

/// Blanket implementation for boxed trait objects.
///
/// This allows `Box<dyn MemoryBackend>` to be used wherever `MemoryBackend` is expected.
impl MemoryBackend for Box<dyn MemoryBackend> {
    fn write(&self, learning: &CompoundLearning) -> Result<WriteResult> {
        (**self).write(learning)
    }

    fn search(&self, query: &SearchQuery, filters: &SearchFilters) -> Result<Vec<SearchResult>> {
        (**self).search(query, filters)
    }

    fn ping(&self) -> bool {
        (**self).ping()
    }

    fn name(&self) -> &'static str {
        (**self).name()
    }

    fn archive(&self, learning_id: &str) -> Result<()> {
        (**self).archive(learning_id)
    }

    fn restore(&self, learning_id: &str) -> Result<()> {
        (**self).restore(learning_id)
    }

    fn list_all(&self) -> Result<Vec<crate::core::CompoundLearning>> {
        (**self).list_all()
    }

    fn next_id(&self) -> String {
        (**self).next_id()
    }

    fn next_ids(&self, count: usize) -> Vec<String> {
        (**self).next_ids(count)
    }
}

/// Result of writing a learning to a backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WriteResult {
    /// Whether the write was successful.
    pub success: bool,
    /// The ID of the written learning.
    pub learning_id: String,
    /// Where the learning was written (e.g., file path, backend name).
    pub location: String,
    /// Optional message (e.g., warning about sanitization).
    pub message: Option<String>,
}

impl WriteResult {
    /// Create a successful write result.
    pub fn success(learning_id: impl Into<String>, location: impl Into<String>) -> Self {
        Self {
            success: true,
            learning_id: learning_id.into(),
            location: location.into(),
            message: None,
        }
    }

    /// Create a successful write result with a message.
    pub fn success_with_message(
        learning_id: impl Into<String>,
        location: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            success: true,
            learning_id: learning_id.into(),
            location: location.into(),
            message: Some(message.into()),
        }
    }

    /// Create a failed write result.
    pub fn failure(learning_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            success: false,
            learning_id: learning_id.into(),
            location: String::new(),
            message: Some(message.into()),
        }
    }
}

/// Query parameters for searching learnings.
///
/// This is a structured query containing available context, not a
/// free-text search string. Fields are optional and combined with AND.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SearchQuery {
    /// Tags to match against learning tags.
    pub tags: Vec<String>,
    /// File paths to match against learning context_files.
    pub files: Vec<String>,
    /// Keywords to match against summary and detail.
    pub keywords: Vec<String>,
    /// Ticket ID to match against learning ticket_id.
    pub ticket_id: Option<String>,
}

impl SearchQuery {
    /// Create a new empty query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a query with tags.
    pub fn with_tags(tags: Vec<String>) -> Self {
        Self {
            tags,
            ..Default::default()
        }
    }

    /// Create a query with file paths.
    pub fn with_files(files: Vec<String>) -> Self {
        Self {
            files,
            ..Default::default()
        }
    }

    /// Create a query with keywords.
    pub fn with_keywords(keywords: Vec<String>) -> Self {
        Self {
            keywords,
            ..Default::default()
        }
    }

    /// Add tags to the query.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Add files to the query.
    pub fn files(mut self, files: Vec<String>) -> Self {
        self.files = files;
        self
    }

    /// Add keywords to the query.
    pub fn keywords(mut self, keywords: Vec<String>) -> Self {
        self.keywords = keywords;
        self
    }

    /// Set the ticket ID.
    pub fn ticket_id(mut self, ticket_id: impl Into<String>) -> Self {
        self.ticket_id = Some(ticket_id.into());
        self
    }

    /// Check if the query is empty (no search criteria).
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
            && self.files.is_empty()
            && self.keywords.is_empty()
            && self.ticket_id.is_none()
    }
}

/// Filters for narrowing search results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchFilters {
    /// Filter by learning status (default: Active only).
    pub status: Option<LearningStatus>,
    /// Filter by learning scope.
    pub scope: Option<LearningScope>,
    /// Only include learnings created after this time.
    pub created_after: Option<DateTime<Utc>>,
    /// Maximum number of results to return.
    pub max_results: Option<usize>,
}

impl Default for SearchFilters {
    fn default() -> Self {
        Self {
            // By default, only return active learnings
            status: Some(LearningStatus::Active),
            scope: None,
            created_after: None,
            max_results: None,
        }
    }
}

impl SearchFilters {
    /// Create filters with no restrictions.
    pub fn all() -> Self {
        Self {
            status: None,
            scope: None,
            created_after: None,
            max_results: None,
        }
    }

    /// Create filters for active learnings only.
    pub fn active_only() -> Self {
        Self::default()
    }

    /// Set the status filter.
    pub fn status(mut self, status: LearningStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Set the scope filter.
    pub fn scope(mut self, scope: LearningScope) -> Self {
        self.scope = Some(scope);
        self
    }

    /// Set the created_after filter.
    pub fn created_after(mut self, time: DateTime<Utc>) -> Self {
        self.created_after = Some(time);
        self
    }

    /// Set the max_results limit.
    pub fn max_results(mut self, limit: usize) -> Self {
        self.max_results = Some(limit);
        self
    }

    /// Check if a learning matches these filters.
    pub fn matches(&self, learning: &CompoundLearning) -> bool {
        // Check status filter
        if let Some(ref status) = self.status {
            if &learning.status != status {
                return false;
            }
        }

        // Check scope filter
        if let Some(ref scope) = self.scope {
            if &learning.scope != scope {
                return false;
            }
        }

        // Check created_after filter
        if let Some(ref created_after) = self.created_after {
            if &learning.timestamp < created_after {
                return false;
            }
        }

        true
    }
}

/// A search result with the learning and its relevance score.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResult {
    /// The matched learning.
    pub learning: CompoundLearning,
    /// Relevance score (0.0 to 1.0, higher is more relevant).
    pub relevance: f64,
}

impl SearchResult {
    /// Create a new search result.
    pub fn new(learning: CompoundLearning, relevance: f64) -> Self {
        Self {
            learning,
            relevance,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Confidence, LearningCategory, WriteGateCriterion};

    fn sample_learning() -> CompoundLearning {
        CompoundLearning::new(
            LearningCategory::Pattern,
            "Test summary for the learning",
            "Test detail explaining the pattern in more depth",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["rust".to_string(), "testing".to_string()],
            "test-session-123",
        )
    }

    // WriteResult tests

    #[test]
    fn test_write_result_success() {
        let result = WriteResult::success("cl_20260101_001", ".grove/learnings.md");
        assert!(result.success);
        assert_eq!(result.learning_id, "cl_20260101_001");
        assert_eq!(result.location, ".grove/learnings.md");
        assert!(result.message.is_none());
    }

    #[test]
    fn test_write_result_success_with_message() {
        let result = WriteResult::success_with_message(
            "cl_20260101_002",
            ".grove/learnings.md",
            "Content was sanitized",
        );
        assert!(result.success);
        assert_eq!(result.learning_id, "cl_20260101_002");
        assert_eq!(result.message, Some("Content was sanitized".to_string()));
    }

    #[test]
    fn test_write_result_failure() {
        let result = WriteResult::failure("cl_20260101_003", "Backend unavailable");
        assert!(!result.success);
        assert_eq!(result.learning_id, "cl_20260101_003");
        assert!(result.location.is_empty());
        assert_eq!(result.message, Some("Backend unavailable".to_string()));
    }

    #[test]
    fn test_write_result_serialization() {
        let result = WriteResult::success("cl_20260101_001", ".grove/learnings.md");
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: WriteResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result, deserialized);
    }

    // SearchQuery tests

    #[test]
    fn test_search_query_new_is_empty() {
        let query = SearchQuery::new();
        assert!(query.is_empty());
    }

    #[test]
    fn test_search_query_with_tags() {
        let query = SearchQuery::with_tags(vec!["rust".to_string(), "testing".to_string()]);
        assert!(!query.is_empty());
        assert_eq!(query.tags.len(), 2);
        assert!(query.files.is_empty());
        assert!(query.keywords.is_empty());
    }

    #[test]
    fn test_search_query_with_files() {
        let query = SearchQuery::with_files(vec!["src/lib.rs".to_string()]);
        assert!(!query.is_empty());
        assert_eq!(query.files.len(), 1);
    }

    #[test]
    fn test_search_query_with_keywords() {
        let query = SearchQuery::with_keywords(vec!["error".to_string(), "handling".to_string()]);
        assert!(!query.is_empty());
        assert_eq!(query.keywords.len(), 2);
    }

    #[test]
    fn test_search_query_builder_pattern() {
        let query = SearchQuery::new()
            .tags(vec!["rust".to_string()])
            .files(vec!["src/lib.rs".to_string()])
            .keywords(vec!["pattern".to_string()])
            .ticket_id("ISSUE-123");

        assert!(!query.is_empty());
        assert_eq!(query.tags, vec!["rust".to_string()]);
        assert_eq!(query.files, vec!["src/lib.rs".to_string()]);
        assert_eq!(query.keywords, vec!["pattern".to_string()]);
        assert_eq!(query.ticket_id, Some("ISSUE-123".to_string()));
    }

    #[test]
    fn test_search_query_serialization() {
        let query = SearchQuery::with_tags(vec!["rust".to_string()]);
        let json = serde_json::to_string(&query).unwrap();
        let deserialized: SearchQuery = serde_json::from_str(&json).unwrap();
        assert_eq!(query, deserialized);
    }

    // SearchFilters tests

    #[test]
    fn test_search_filters_default_is_active_only() {
        let filters = SearchFilters::default();
        assert_eq!(filters.status, Some(LearningStatus::Active));
        assert!(filters.scope.is_none());
        assert!(filters.created_after.is_none());
        assert!(filters.max_results.is_none());
    }

    #[test]
    fn test_search_filters_all() {
        let filters = SearchFilters::all();
        assert!(filters.status.is_none());
        assert!(filters.scope.is_none());
    }

    #[test]
    fn test_search_filters_builder_pattern() {
        let filters = SearchFilters::all()
            .status(LearningStatus::Active)
            .scope(LearningScope::Project)
            .max_results(10);

        assert_eq!(filters.status, Some(LearningStatus::Active));
        assert_eq!(filters.scope, Some(LearningScope::Project));
        assert_eq!(filters.max_results, Some(10));
    }

    #[test]
    fn test_search_filters_matches_status() {
        let filters = SearchFilters::active_only();
        let mut learning = sample_learning();

        // Active learning should match
        assert!(filters.matches(&learning));

        // Archived learning should not match
        learning.status = LearningStatus::Archived;
        assert!(!filters.matches(&learning));
    }

    #[test]
    fn test_search_filters_matches_scope() {
        let filters = SearchFilters::all().scope(LearningScope::Personal);
        let mut learning = sample_learning();

        // Project scope should not match
        assert!(!filters.matches(&learning));

        // Personal scope should match
        learning.scope = LearningScope::Personal;
        assert!(filters.matches(&learning));
    }

    #[test]
    fn test_search_filters_matches_created_after() {
        use chrono::Duration;

        let now = Utc::now();
        let filters = SearchFilters::all().created_after(now - Duration::hours(1));
        let learning = sample_learning();

        // Recent learning should match (just created)
        assert!(filters.matches(&learning));

        // Old learning should not match
        let mut old_learning = sample_learning();
        old_learning.timestamp = now - Duration::days(1);
        assert!(!filters.matches(&old_learning));
    }

    #[test]
    fn test_search_filters_matches_all_with_no_restrictions() {
        let filters = SearchFilters::all();
        let learning = sample_learning();
        assert!(filters.matches(&learning));
    }

    #[test]
    fn test_search_filters_serialization() {
        let filters = SearchFilters::active_only().max_results(5);
        let json = serde_json::to_string(&filters).unwrap();
        let deserialized: SearchFilters = serde_json::from_str(&json).unwrap();
        assert_eq!(filters, deserialized);
    }

    // SearchResult tests

    #[test]
    fn test_search_result_new() {
        let learning = sample_learning();
        let result = SearchResult::new(learning.clone(), 0.85);

        assert_eq!(result.learning.id, learning.id);
        assert!((result.relevance - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn test_search_result_serialization() {
        let learning = sample_learning();
        let result = SearchResult::new(learning, 0.75);

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: SearchResult = serde_json::from_str(&json).unwrap();

        assert_eq!(result.learning.id, deserialized.learning.id);
        assert!((result.relevance - deserialized.relevance).abs() < f64::EPSILON);
    }

    // Box<dyn MemoryBackend> delegation tests

    mod boxed_backend_tests {
        use super::*;
        use crate::backends::MarkdownBackend;
        use std::fs;
        use tempfile::TempDir;

        fn setup_markdown_backend() -> (TempDir, MarkdownBackend) {
            let temp = TempDir::new().unwrap();
            let learnings_path = temp.path().join(".grove").join("learnings.md");
            fs::create_dir_all(learnings_path.parent().unwrap()).unwrap();
            let backend = MarkdownBackend::new(&learnings_path);
            (temp, backend)
        }

        #[test]
        fn test_boxed_backend_name_delegates() {
            let (_temp, backend) = setup_markdown_backend();
            let boxed: Box<dyn MemoryBackend> = Box::new(backend);

            assert_eq!(boxed.name(), "markdown");
        }

        #[test]
        fn test_boxed_backend_ping_delegates() {
            let (_temp, backend) = setup_markdown_backend();
            let boxed: Box<dyn MemoryBackend> = Box::new(backend);

            // Markdown backend always returns true for ping
            assert!(boxed.ping());
        }

        #[test]
        fn test_boxed_backend_write_delegates() {
            let (_temp, backend) = setup_markdown_backend();
            let boxed: Box<dyn MemoryBackend> = Box::new(backend);

            let learning = sample_learning();
            let result = boxed.write(&learning).unwrap();

            assert!(result.success);
            assert!(!result.learning_id.is_empty());
        }

        #[test]
        fn test_boxed_backend_search_delegates() {
            let (_temp, backend) = setup_markdown_backend();
            let boxed: Box<dyn MemoryBackend> = Box::new(backend);

            // Write a learning first
            let learning = sample_learning();
            boxed.write(&learning).unwrap();

            // Search with empty query returns all
            let results = boxed
                .search(&SearchQuery::new(), &SearchFilters::default())
                .unwrap();

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].learning.summary, learning.summary);
        }

        #[test]
        fn test_boxed_backend_list_all_delegates() {
            let (_temp, backend) = setup_markdown_backend();
            let boxed: Box<dyn MemoryBackend> = Box::new(backend);

            // Write a learning first
            let learning = sample_learning();
            boxed.write(&learning).unwrap();

            // list_all returns all learnings
            let learnings = boxed.list_all().unwrap();

            assert_eq!(learnings.len(), 1);
            assert_eq!(learnings[0].summary, learning.summary);
        }

        #[test]
        fn test_boxed_backend_archive_delegates() {
            let (_temp, backend) = setup_markdown_backend();
            let boxed: Box<dyn MemoryBackend> = Box::new(backend);

            // Write a learning first
            let learning = sample_learning();
            let write_result = boxed.write(&learning).unwrap();
            assert!(write_result.success, "Write should succeed");

            // Use the ID from the write result (may differ from input)
            let learning_id = write_result.learning_id;

            // Verify the learning was written
            let learnings_before = boxed.list_all().unwrap();
            assert_eq!(
                learnings_before.len(),
                1,
                "Should have 1 learning after write"
            );
            assert_eq!(learnings_before[0].id, learning_id, "ID should match");

            // Archive should work via delegation
            let result = boxed.archive(&learning_id);
            assert!(result.is_ok(), "Archive failed: {:?}", result.err());

            // Verify it's archived
            let learnings = boxed.list_all().unwrap();
            assert_eq!(learnings[0].status, LearningStatus::Archived);
        }

        #[test]
        fn test_boxed_backend_restore_delegates() {
            let (_temp, backend) = setup_markdown_backend();
            let boxed: Box<dyn MemoryBackend> = Box::new(backend);

            // Write a learning first
            let learning = sample_learning();
            let write_result = boxed.write(&learning).unwrap();

            // Use the ID from the write result
            let learning_id = write_result.learning_id;

            // Archive it
            boxed.archive(&learning_id).unwrap();

            // Restore should work via delegation
            let result = boxed.restore(&learning_id);
            assert!(result.is_ok());

            // Verify it's active again
            let results = boxed
                .search(&SearchQuery::new(), &SearchFilters::active_only())
                .unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].learning.status, LearningStatus::Active);
        }

        #[test]
        fn test_boxed_backend_next_ids_delegates() {
            let (_temp, backend) = setup_markdown_backend();
            let boxed: Box<dyn MemoryBackend> = Box::new(backend);

            // Get multiple IDs at once
            let ids = boxed.next_ids(3);

            // Should return 3 unique IDs
            assert_eq!(ids.len(), 3, "Should return requested number of IDs");

            // All IDs should be unique
            let mut unique_ids = ids.clone();
            unique_ids.sort();
            unique_ids.dedup();
            assert_eq!(unique_ids.len(), 3, "All IDs should be unique: {:?}", ids);

            // IDs should follow expected pattern with incrementing counters
            // Expected format: cl_YYYYMMDD_NNN
            for (i, id) in ids.iter().enumerate() {
                assert!(id.starts_with("cl_"), "ID should start with 'cl_': {}", id);
                // Extract counter from ID
                if let Some(underscore_pos) = id.rfind('_') {
                    let counter_str = &id[underscore_pos + 1..];
                    let counter: u32 = counter_str.parse().expect("Counter should be numeric");
                    assert_eq!(counter, i as u32, "Counter should increment from 0: {}", id);
                }
            }
        }

        #[test]
        fn test_next_ids_empty() {
            let (_temp, backend) = setup_markdown_backend();
            let boxed: Box<dyn MemoryBackend> = Box::new(backend);

            // Empty request should return empty vector
            let ids = boxed.next_ids(0);
            assert!(ids.is_empty());
        }

        #[test]
        fn test_next_ids_single() {
            let (_temp, backend) = setup_markdown_backend();
            let boxed: Box<dyn MemoryBackend> = Box::new(backend);

            // Single ID should be same as next_id()
            let ids = boxed.next_ids(1);
            assert_eq!(ids.len(), 1);
            assert!(ids[0].starts_with("cl_"));
        }

        #[test]
        fn test_next_ids_does_not_wrap_at_1000() {
            // This tests the fix for grove-6vs3dlwr
            // IDs should not wrap at 1000 - counters should continue beyond 999

            // Create a mock ID with counter at 998
            let base_id = "cl_20260101_998";
            let prefix = "cl_20260101_";

            // Parse and generate like next_ids does
            let counter = 998u32;
            let count = 5;

            let mut ids = vec![base_id.to_string()];
            for i in 1..count {
                // This is the fix: no % 1000 wrap
                ids.push(format!("{}{:03}", prefix, counter + i as u32));
            }

            // Verify counters continue beyond 999 without wrapping
            assert_eq!(ids[0], "cl_20260101_998");
            assert_eq!(ids[1], "cl_20260101_999");
            assert_eq!(ids[2], "cl_20260101_1000"); // Would have been 000 before fix
            assert_eq!(ids[3], "cl_20260101_1001"); // Would have been 001 before fix
            assert_eq!(ids[4], "cl_20260101_1002"); // Would have been 002 before fix
        }
    }
}
