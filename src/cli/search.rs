//! Search command for Grove.
//!
//! Searches for learnings across active backends.

use serde::{Deserialize, Serialize};

use crate::backends::{MemoryBackend, SearchFilters, SearchQuery, SearchResult};
use crate::config::Config;

/// Options for the search command.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Maximum number of results.
    pub limit: Option<usize>,
    /// Include archived learnings.
    pub include_archived: bool,
}

/// Output format for the search command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchOutput {
    /// Whether the search was successful.
    pub success: bool,
    /// The search query used.
    pub query: String,
    /// Number of results found.
    pub count: usize,
    /// The search results.
    pub results: Vec<SearchResultInfo>,
    /// Error message if search failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Simplified result info for output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultInfo {
    /// Learning ID.
    pub id: String,
    /// Learning summary.
    pub summary: String,
    /// Learning category.
    pub category: String,
    /// Relevance score.
    pub relevance: f64,
    /// Tags.
    pub tags: Vec<String>,
    /// Status.
    pub status: String,
}

impl From<&SearchResult> for SearchResultInfo {
    fn from(result: &SearchResult) -> Self {
        Self {
            id: result.learning.id.clone(),
            summary: result.learning.summary.clone(),
            category: format!("{:?}", result.learning.category).to_lowercase(),
            relevance: result.relevance,
            tags: result.learning.tags.clone(),
            status: format!("{:?}", result.learning.status).to_lowercase(),
        }
    }
}

impl SearchOutput {
    /// Create a successful output.
    pub fn success(query: impl Into<String>, results: Vec<SearchResultInfo>) -> Self {
        let count = results.len();
        Self {
            success: true,
            query: query.into(),
            count,
            results,
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(query: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            success: false,
            query: query.into(),
            count: 0,
            results: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// The search command implementation.
pub struct SearchCommand<B: MemoryBackend> {
    backend: B,
    #[allow(dead_code)]
    config: Config,
}

impl<B: MemoryBackend> SearchCommand<B> {
    /// Create a new search command.
    pub fn new(backend: B, config: Config) -> Self {
        Self { backend, config }
    }

    /// Run the search command with the given query.
    pub fn run(&self, query: &str, options: &SearchOptions) -> SearchOutput {
        let trimmed_query = query.trim();
        if trimmed_query.is_empty() {
            return SearchOutput::failure("", "Search query cannot be empty");
        }

        // Build search query from keywords
        let keywords: Vec<String> = trimmed_query
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        let search_query = SearchQuery::with_keywords(keywords);

        // Build filters
        let mut filters = if options.include_archived {
            SearchFilters::all()
        } else {
            SearchFilters::active_only()
        };
        if let Some(limit) = options.limit {
            filters = filters.max_results(limit);
        }

        // Execute search
        match self.backend.search(&search_query, &filters) {
            Ok(mut results) => {
                // Sort by relevance (highest first)
                results.sort_by(|a, b| {
                    b.relevance
                        .partial_cmp(&a.relevance)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

                // Apply limit
                if let Some(limit) = options.limit {
                    results.truncate(limit);
                }

                let result_infos: Vec<SearchResultInfo> =
                    results.iter().map(SearchResultInfo::from).collect();
                SearchOutput::success(trimmed_query, result_infos)
            }
            Err(e) => SearchOutput::failure(trimmed_query, e.to_string()),
        }
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &SearchOutput, options: &SearchOptions) -> String {
        if options.quiet {
            return String::new();
        }

        if options.json {
            serde_json::to_string_pretty(output).unwrap_or_else(|_| "{}".to_string())
        } else {
            self.format_human_readable(output)
        }
    }

    /// Format output as human-readable text.
    fn format_human_readable(&self, output: &SearchOutput) -> String {
        if !output.success {
            return format!(
                "Search failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            );
        }

        if output.results.is_empty() {
            return format!("No learnings found for query: \"{}\"\n", output.query);
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "Found {} learning(s) for query: \"{}\"\n",
            output.count, output.query
        ));

        for (i, result) in output.results.iter().enumerate() {
            lines.push(format!(
                "{}. [{}] {} (relevance: {:.2})",
                i + 1,
                result.category,
                result.summary,
                result.relevance
            ));
            if !result.tags.is_empty() {
                lines.push(format!("   Tags: {}", result.tags.join(", ")));
            }
            lines.push(format!("   ID: {}", result.id));
            lines.push(String::new());
        }

        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::markdown::MarkdownBackend;
    use std::fs;
    use tempfile::TempDir;

    fn setup_with_learnings() -> (TempDir, MarkdownBackend) {
        let temp = TempDir::new().unwrap();
        let learnings_path = temp.path().join(".grove").join("learnings.md");
        fs::create_dir_all(learnings_path.parent().unwrap()).unwrap();

        let content = r#"# Project Learnings

## cl_20260101_001

**Category:** Pattern
**Summary:** Error handling in async code
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #rust #async #error
**Session:** test-session
**Criteria:** Behavior-Changing
**Created:** 2026-01-01T00:00:00Z

Always use `?` operator with proper error conversion in async functions.

---

## cl_20260101_002

**Category:** Pitfall
**Summary:** Mutex deadlock
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #rust #mutex #concurrency
**Session:** test-session
**Criteria:** Behavior-Changing
**Created:** 2026-01-01T00:00:00Z

Avoid holding mutex across await points.

---

## cl_20260101_003

**Category:** Pattern
**Summary:** Archived pattern
**Scope:** Project | **Confidence:** High | **Status:** Archived
**Tags:** #archived
**Session:** test-session
**Criteria:** Behavior-Changing
**Created:** 2026-01-01T00:00:00Z

This pattern has been archived.

---
"#;
        fs::write(&learnings_path, content).unwrap();

        // Pass the file path, not the directory path
        let backend = MarkdownBackend::new(&learnings_path);
        (temp, backend)
    }

    #[test]
    fn test_search_output_success() {
        let results = vec![SearchResultInfo {
            id: "cl_001".to_string(),
            summary: "Test summary".to_string(),
            category: "pattern".to_string(),
            relevance: 0.9,
            tags: vec!["rust".to_string()],
            status: "active".to_string(),
        }];
        let output = SearchOutput::success("test query", results);

        assert!(output.success);
        assert_eq!(output.query, "test query");
        assert_eq!(output.count, 1);
        assert!(output.error.is_none());
    }

    #[test]
    fn test_search_output_failure() {
        let output = SearchOutput::failure("query", "backend error");

        assert!(!output.success);
        assert_eq!(output.count, 0);
        assert!(output.results.is_empty());
        assert_eq!(output.error, Some("backend error".to_string()));
    }

    #[test]
    fn test_search_basic() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = SearchCommand::new(backend, config);
        let options = SearchOptions::default();

        let output = cmd.run("error", &options);

        assert!(output.success);
        assert!(!output.results.is_empty());
        // Should find the error handling pattern
        assert!(output.results.iter().any(|r| r.summary.contains("Error")));
    }

    #[test]
    fn test_search_with_multiple_keywords() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = SearchCommand::new(backend, config);
        let options = SearchOptions::default();

        let output = cmd.run("mutex deadlock", &options);

        assert!(output.success);
        assert!(output.results.iter().any(|r| r.summary.contains("Mutex")));
    }

    #[test]
    fn test_search_empty_query_fails() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = SearchCommand::new(backend, config);
        let options = SearchOptions::default();

        let output = cmd.run("", &options);

        assert!(!output.success);
        assert!(output.error.unwrap().contains("cannot be empty"));
    }

    #[test]
    fn test_search_whitespace_only_query_fails() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = SearchCommand::new(backend, config);
        let options = SearchOptions::default();

        let output = cmd.run("   \n\t  ", &options);

        assert!(!output.success);
    }

    #[test]
    fn test_search_excludes_archived_by_default() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = SearchCommand::new(backend, config);
        let options = SearchOptions::default();

        let output = cmd.run("archived", &options);

        assert!(output.success);
        // Should not find archived learnings
        assert!(output
            .results
            .iter()
            .all(|r| r.status != "archived" || !r.summary.contains("Archived")));
    }

    #[test]
    fn test_search_includes_archived_when_requested() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = SearchCommand::new(backend, config);
        let options = SearchOptions {
            include_archived: true,
            ..Default::default()
        };

        let output = cmd.run("archived", &options);

        assert!(output.success);
        // Should find archived learnings
        assert!(output.results.iter().any(|r| r.status == "archived"));
    }

    #[test]
    fn test_search_with_limit() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = SearchCommand::new(backend, config);
        let options = SearchOptions {
            limit: Some(1),
            ..Default::default()
        };

        // Search for something that matches multiple learnings
        let output = cmd.run("rust", &options);

        assert!(output.success);
        assert!(output.results.len() <= 1);
    }

    #[test]
    fn test_search_no_results() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();

        let cmd = SearchCommand::new(backend, config);
        let options = SearchOptions::default();

        let output = cmd.run("nonexistentkeyword12345", &options);

        assert!(output.success);
        assert_eq!(output.count, 0);
        assert!(output.results.is_empty());
    }

    #[test]
    fn test_format_output_json() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = SearchCommand::new(backend, config);

        let output = SearchOutput::success("test", vec![]);
        let options = SearchOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"query\": \"test\""));
    }

    #[test]
    fn test_format_output_quiet() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = SearchCommand::new(backend, config);

        let output = SearchOutput::success("test", vec![]);
        let options = SearchOptions {
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
        let cmd = SearchCommand::new(backend, config);

        let results = vec![SearchResultInfo {
            id: "cl_001".to_string(),
            summary: "Test pattern".to_string(),
            category: "pattern".to_string(),
            relevance: 0.9,
            tags: vec!["rust".to_string()],
            status: "active".to_string(),
        }];
        let output = SearchOutput::success("test", results);
        let options = SearchOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Found 1 learning(s)"));
        assert!(formatted.contains("Test pattern"));
        assert!(formatted.contains("relevance: 0.90"));
    }

    #[test]
    fn test_format_output_no_results() {
        let (_temp, backend) = setup_with_learnings();
        let config = Config::default();
        let cmd = SearchCommand::new(backend, config);

        let output = SearchOutput::success("missing", vec![]);
        let options = SearchOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("No learnings found"));
    }
}
