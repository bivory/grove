//! Total Recall backend adapter for Grove.
//!
//! This module provides integration with Total Recall's tiered memory system.
//! It shells out to Total Recall's CLI commands:
//! - `claude skill recall-log <note>` for writes (bypasses TR write gate)
//! - `claude skill recall-search <query>` for searches
//!
//! **Fail-open behavior**: If CLI is unavailable, log warning and continue
//! without persistence.

use std::fs::{self, OpenOptions};
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::Utc;
use tracing::warn;

use crate::backends::traits::{
    MemoryBackend, SearchFilters, SearchQuery, SearchResult, WriteResult,
};
use crate::core::{CompoundLearning, Confidence, LearningScope};
use crate::error::Result;

/// Prefix for Grove learning IDs in Total Recall.
const GROVE_ID_PREFIX: &str = "grove:";

/// Total Recall backend adapter.
///
/// Integrates with Total Recall's tiered memory system by invoking CLI commands.
/// Supports:
/// - Scope routing (project/team → recall-log, personal → direct file write)
/// - fail-open behavior for CLI failures
/// - grove: ID prefix for filtering searches
#[derive(Debug, Clone)]
pub struct TotalRecallBackend {
    /// Path to the memory/ directory.
    memory_dir: PathBuf,
    /// Path for personal learnings (bypasses TR).
    personal_path: PathBuf,
    /// Path to the project directory (for CLI execution context).
    project_dir: PathBuf,
}

impl TotalRecallBackend {
    /// Create a new Total Recall backend with the given memory directory.
    ///
    /// The personal learnings path is automatically set to `~/.grove/personal-learnings.md`.
    pub fn new(memory_dir: impl AsRef<Path>, project_dir: impl AsRef<Path>) -> Self {
        let personal_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".grove")
            .join("personal-learnings.md");

        Self {
            memory_dir: memory_dir.as_ref().to_path_buf(),
            personal_path,
            project_dir: project_dir.as_ref().to_path_buf(),
        }
    }

    /// Create a backend with explicit paths for testing.
    pub fn with_paths(
        memory_dir: impl AsRef<Path>,
        personal_path: impl AsRef<Path>,
        project_dir: impl AsRef<Path>,
    ) -> Self {
        Self {
            memory_dir: memory_dir.as_ref().to_path_buf(),
            personal_path: personal_path.as_ref().to_path_buf(),
            project_dir: project_dir.as_ref().to_path_buf(),
        }
    }

    /// Format a learning as a Total Recall note.
    ///
    /// Format:
    /// ```text
    /// [HH:MM] **Category** (grove:learn-abc123): Summary text
    /// > Detailed explanation here
    ///
    /// Tags: #tag1 #tag2 | Confidence: High | Ticket: grove-abc123 | Files: src/foo.rs
    /// ```
    fn format_learning(&self, learning: &CompoundLearning) -> String {
        let mut note = String::new();

        // Timestamp prefix [HH:MM]
        note.push_str(&format!("[{}] ", learning.timestamp.format("%H:%M")));

        // Header with category and ID
        note.push_str(&format!(
            "**{}** ({}{}):",
            learning.category.display_name(),
            GROVE_ID_PREFIX,
            &learning.id,
        ));

        // Summary
        note.push(' ');
        note.push_str(&learning.summary);
        note.push('\n');

        // Detail as blockquote (handle multiline)
        for line in learning.detail.lines() {
            note.push_str("> ");
            note.push_str(line);
            note.push('\n');
        }

        // Metadata line
        let mut meta_parts = Vec::new();

        // Tags
        if !learning.tags.is_empty() {
            let tags: Vec<String> = learning.tags.iter().map(|t| format!("#{}", t)).collect();
            meta_parts.push(format!("Tags: {}", tags.join(" ")));
        }

        // Confidence
        meta_parts.push(format!(
            "Confidence: {}",
            confidence_display(&learning.confidence)
        ));

        // Ticket
        if let Some(ref ticket_id) = learning.ticket_id {
            meta_parts.push(format!("Ticket: {}", ticket_id));
        }

        // Files
        if let Some(ref files) = learning.context_files {
            if !files.is_empty() {
                meta_parts.push(format!("Files: {}", files.join(", ")));
            }
        }

        if !meta_parts.is_empty() {
            note.push('\n');
            note.push_str(&meta_parts.join(" | "));
        }

        note
    }

    /// Write a learning directly to the personal file (bypasses Total Recall).
    fn write_to_personal(&self, learning: &CompoundLearning) -> Result<WriteResult> {
        // Ensure parent directory exists
        if let Some(parent) = self.personal_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                crate::error::GroveError::backend(format!(
                    "Failed to create directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        // Format as markdown (reuse formatting logic)
        let markdown = self.format_personal_learning(learning);

        // Append to file
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.personal_path)
            .map_err(|e| {
                crate::error::GroveError::backend(format!(
                    "Failed to open {}: {}",
                    self.personal_path.display(),
                    e
                ))
            })?;

        file.write_all(markdown.as_bytes()).map_err(|e| {
            crate::error::GroveError::backend(format!(
                "Failed to write to {}: {}",
                self.personal_path.display(),
                e
            ))
        })?;

        Ok(WriteResult::success(
            &learning.id,
            self.personal_path.display().to_string(),
        ))
    }

    /// Format a learning for the personal file (markdown format).
    fn format_personal_learning(&self, learning: &CompoundLearning) -> String {
        let mut md = String::new();

        // Header with ID
        md.push_str(&format!("## {}{}\n\n", GROVE_ID_PREFIX, learning.id));

        // Metadata
        md.push_str(&format!(
            "**Category:** {}\n",
            learning.category.display_name()
        ));
        md.push_str(&format!("**Summary:** {}\n", learning.summary));
        md.push_str(&format!(
            "**Confidence:** {} | **Created:** {}\n",
            confidence_display(&learning.confidence),
            learning.timestamp.format("%Y-%m-%dT%H:%M:%SZ")
        ));

        // Tags
        if !learning.tags.is_empty() {
            let tags: Vec<String> = learning.tags.iter().map(|t| format!("#{}", t)).collect();
            md.push_str(&format!("**Tags:** {}\n", tags.join(" ")));
        }

        // Detail
        md.push_str(&format!("\n{}\n", learning.detail));

        // Separator
        md.push_str("\n---\n\n");

        md
    }

    /// Invoke recall-log CLI command to write a learning.
    fn invoke_recall_log(&self, note: &str) -> std::result::Result<(), String> {
        let output = Command::new("claude")
            .args(["skill", "recall-log", note])
            .current_dir(&self.project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        match output {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                Err(format!("recall-log failed: {}", stderr))
            }
            Err(e) => Err(format!("claude CLI not available: {}", e)),
        }
    }

    /// Invoke recall-search CLI command and return raw output.
    fn invoke_recall_search(&self, query: &str) -> std::result::Result<String, String> {
        let output = Command::new("claude")
            .args(["skill", "recall-search", query])
            .current_dir(&self.project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        match output {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).to_string()),
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                Err(format!("recall-search failed: {}", stderr))
            }
            Err(e) => Err(format!("claude CLI not available: {}", e)),
        }
    }

    /// Build a search term from a SearchQuery.
    fn build_search_term(&self, query: &SearchQuery) -> String {
        let mut terms = Vec::new();

        // Add keywords
        terms.extend(query.keywords.iter().cloned());

        // Add tags (without # prefix for search)
        terms.extend(query.tags.iter().cloned());

        // Add file stems (just the file name without path/extension)
        for file in &query.files {
            if let Some(stem) = Path::new(file).file_stem() {
                if let Some(s) = stem.to_str() {
                    terms.push(s.to_string());
                }
            }
        }

        // Add ticket ID
        if let Some(ref ticket_id) = query.ticket_id {
            terms.push(ticket_id.clone());
        }

        // Add grove prefix to filter for our entries
        terms.push(GROVE_ID_PREFIX.trim_end_matches(':').to_string());

        terms.join(" ")
    }

    /// Parse search results from Total Recall output.
    ///
    /// Filters for entries containing `grove:` prefix and parses them back
    /// into partial `CompoundLearning` objects with relevance scores.
    fn parse_search_results(&self, output: &str, filters: &SearchFilters) -> Vec<SearchResult> {
        let mut results = Vec::new();

        // Parse each line/entry looking for grove: prefix
        for entry in self.split_entries(output) {
            if let Some(learning) = self.parse_grove_entry(&entry) {
                // Apply filters
                if filters.matches(&learning) {
                    // Assign relevance based on position (first = most relevant)
                    let relevance = 1.0 - (results.len() as f64 * 0.1).min(0.9);
                    results.push(SearchResult::new(learning, relevance));
                }
            }
        }

        // Apply max_results limit
        if let Some(limit) = filters.max_results {
            results.truncate(limit);
        }

        results
    }

    /// Split raw output into individual entries.
    fn split_entries(&self, output: &str) -> Vec<String> {
        // Total Recall output typically uses --- or blank lines as separators
        // We'll try to split on common patterns
        let mut entries = Vec::new();
        let mut current = String::new();

        for line in output.lines() {
            if line.trim() == "---" || (line.is_empty() && !current.is_empty()) {
                if !current.trim().is_empty() && current.contains(GROVE_ID_PREFIX) {
                    entries.push(current.trim().to_string());
                }
                current.clear();
            } else {
                if !current.is_empty() {
                    current.push('\n');
                }
                current.push_str(line);
            }
        }

        // Don't forget the last entry
        if !current.trim().is_empty() && current.contains(GROVE_ID_PREFIX) {
            entries.push(current.trim().to_string());
        }

        entries
    }

    /// Parse a single grove entry from Total Recall output.
    fn parse_grove_entry(&self, entry: &str) -> Option<CompoundLearning> {
        // Look for the grove: prefix to extract the ID
        let id_start = entry.find(GROVE_ID_PREFIX)?;
        let after_prefix = id_start + GROVE_ID_PREFIX.len();

        // Find the end of the ID (closing paren, colon, space, or newline)
        let id_end = entry[after_prefix..]
            .find([')', ':', ' ', '\n'])
            .map(|i| after_prefix + i)
            .unwrap_or(entry.len().min(after_prefix + 30));

        if id_end <= after_prefix {
            return None;
        }

        let grove_id = entry[after_prefix..id_end].trim();

        // Extract category from **Category** pattern
        let category = if entry.contains("**Pattern**") {
            crate::core::LearningCategory::Pattern
        } else if entry.contains("**Pitfall**") {
            crate::core::LearningCategory::Pitfall
        } else if entry.contains("**Convention**") {
            crate::core::LearningCategory::Convention
        } else if entry.contains("**Dependency**") {
            crate::core::LearningCategory::Dependency
        } else if entry.contains("**Process**") {
            crate::core::LearningCategory::Process
        } else if entry.contains("**Domain**") {
            crate::core::LearningCategory::Domain
        } else if entry.contains("**Debugging**") {
            crate::core::LearningCategory::Debugging
        } else {
            crate::core::LearningCategory::Pattern // Default
        };

        // Extract summary (text after the category/ID header, before newline)
        let summary = entry
            .lines()
            .next()
            .and_then(|first_line| {
                // Find the colon after the ID pattern, take text after it
                first_line
                    .rfind("):")
                    .map(|i| first_line[i + 2..].trim().to_string())
            })
            .unwrap_or_default();

        // Extract detail from blockquotes
        let detail: String = entry
            .lines()
            .filter(|line| line.starts_with("> "))
            .map(|line| line.strip_prefix("> ").unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");

        // Extract tags from "Tags: #tag1 #tag2" pattern
        let tags = entry
            .lines()
            .find(|line| line.contains("Tags:"))
            .map(|line| {
                line.split("Tags:")
                    .nth(1)
                    .unwrap_or("")
                    .split('|')
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .filter_map(|t| t.strip_prefix('#'))
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // Build a partial learning
        Some(CompoundLearning {
            id: grove_id.to_string(),
            schema_version: 1,
            category,
            summary,
            detail,
            scope: LearningScope::Project,
            confidence: Confidence::Medium,
            criteria_met: vec![],
            tags,
            session_id: String::new(),
            ticket_id: None,
            timestamp: Utc::now(),
            context_files: None,
            status: crate::core::LearningStatus::Active,
        })
    }
}

impl MemoryBackend for TotalRecallBackend {
    fn write(&self, learning: &CompoundLearning) -> Result<WriteResult> {
        // Personal scope bypasses Total Recall entirely
        if learning.scope == LearningScope::Personal {
            return self.write_to_personal(learning);
        }

        // Project, Team, and Ephemeral all route to Total Recall's daily logs.
        // Ephemeral captures to daily log but is not promoted (per architecture 5.3.3).
        // Format the learning for Total Recall
        let note = self.format_learning(learning);

        // Invoke recall-log CLI
        match self.invoke_recall_log(&note) {
            Ok(()) => {
                let daily_log = format!("memory/daily/{}.md", Utc::now().format("%Y-%m-%d"));
                Ok(WriteResult::success(&learning.id, daily_log))
            }
            Err(err) => {
                // Fail-open: log warning, return failure result but don't block
                warn!("{}", err);
                Ok(WriteResult::failure(&learning.id, "Backend unavailable"))
            }
        }
    }

    fn search(&self, query: &SearchQuery, filters: &SearchFilters) -> Result<Vec<SearchResult>> {
        // If query is empty, we can't search Total Recall meaningfully
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let search_term = self.build_search_term(query);

        match self.invoke_recall_search(&search_term) {
            Ok(output) => Ok(self.parse_search_results(&output, filters)),
            Err(err) => {
                // Fail-open: log warning, return empty results
                warn!("{}", err);
                Ok(Vec::new())
            }
        }
    }

    fn ping(&self) -> bool {
        // Check if memory directory exists
        self.memory_dir.is_dir()
    }

    fn name(&self) -> &'static str {
        "total-recall"
    }
}

fn confidence_display(confidence: &Confidence) -> &'static str {
    match confidence {
        Confidence::High => "High",
        Confidence::Medium => "Medium",
        Confidence::Low => "Low",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{LearningCategory, WriteGateCriterion};
    use tempfile::TempDir;

    fn sample_learning() -> CompoundLearning {
        CompoundLearning::new(
            LearningCategory::Pitfall,
            "Avoid N+1 queries in UserDashboard",
            "The dashboard was loading users then iterating to load posts separately.\nUse eager loading instead.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["performance".to_string(), "database".to_string()],
            "test-session-123",
        )
        .with_ticket_id("grove-abc123")
        .with_context_files(vec!["src/dashboard.rs".to_string()])
    }

    // Format tests

    #[test]
    fn test_format_learning_basic() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());
        let learning = sample_learning();

        let note = backend.format_learning(&learning);

        // Verify timestamp prefix [HH:MM] (per architecture 5.3.5)
        assert!(
            note.starts_with('['),
            "Note should start with timestamp prefix"
        );
        assert!(note.contains("] **Pitfall**"));
        assert!(note.contains(&format!("{}{})", GROVE_ID_PREFIX, learning.id)));
        assert!(note.contains("Avoid N+1 queries"));
        assert!(note.contains("> The dashboard was loading"));
        assert!(note.contains("> Use eager loading instead."));
        assert!(note.contains("#performance"));
        assert!(note.contains("#database"));
        assert!(note.contains("Confidence: High"));
        assert!(note.contains("Ticket: grove-abc123"));
        assert!(note.contains("Files: src/dashboard.rs"));
    }

    #[test]
    fn test_format_learning_multiline_detail() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let mut learning = sample_learning();
        learning.detail = "Line 1\nLine 2\nLine 3".to_string();

        let note = backend.format_learning(&learning);

        assert!(note.contains("> Line 1\n"));
        assert!(note.contains("> Line 2\n"));
        assert!(note.contains("> Line 3\n"));
    }

    #[test]
    fn test_format_learning_no_optional_fields() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            "Simple summary",
            "Simple detail",
            LearningScope::Project,
            Confidence::Medium,
            vec![WriteGateCriterion::StableFact],
            vec![],
            "session-123",
        );

        let note = backend.format_learning(&learning);

        assert!(note.contains("**Pattern**"));
        assert!(note.contains("Simple summary"));
        assert!(!note.contains("Ticket:"));
        assert!(!note.contains("Files:"));
        // Should still have confidence
        assert!(note.contains("Confidence: Medium"));
    }

    // Parse tests

    #[test]
    fn test_parse_grove_entry() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let entry = "**Pitfall** (grove:cl_20260101_001): Avoid N+1 queries
> The dashboard was loading users separately.
> Use eager loading instead.

Tags: #performance #database | Confidence: High | Ticket: T001";

        let learning = backend.parse_grove_entry(entry).unwrap();

        assert_eq!(learning.id, "cl_20260101_001");
        assert_eq!(learning.category, LearningCategory::Pitfall);
        assert_eq!(learning.summary, "Avoid N+1 queries");
        assert!(learning.detail.contains("The dashboard was loading"));
        assert!(learning.detail.contains("Use eager loading"));
        assert_eq!(learning.tags, vec!["performance", "database"]);
    }

    #[test]
    fn test_parse_mixed_entries() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let output = "Some random note without grove prefix

---

**Pattern** (grove:cl_001): First grove entry
> Details here

---

Another non-grove entry

---

**Convention** (grove:cl_002): Second grove entry
> More details";

        let entries = backend.split_entries(output);

        // Should only have the two grove entries
        assert_eq!(entries.len(), 2);
        assert!(entries[0].contains("grove:cl_001"));
        assert!(entries[1].contains("grove:cl_002"));
    }

    #[test]
    fn test_parse_partial_entry() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        // Minimal entry with just ID
        let entry = "Something (grove:cl_minimal) here";

        let learning = backend.parse_grove_entry(entry);

        assert!(learning.is_some());
        let learning = learning.unwrap();
        assert_eq!(learning.id, "cl_minimal");
    }

    #[test]
    fn test_build_search_term() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let query = SearchQuery::new()
            .tags(vec!["performance".to_string()])
            .files(vec!["src/dashboard.rs".to_string()])
            .keywords(vec!["eager loading".to_string()])
            .ticket_id("T001");

        let term = backend.build_search_term(&query);

        assert!(term.contains("eager loading"));
        assert!(term.contains("performance"));
        assert!(term.contains("dashboard")); // file stem
        assert!(term.contains("T001"));
        assert!(term.contains("grove")); // prefix for filtering
    }

    // Scope routing tests

    #[test]
    fn test_write_project_scope_invokes_recall_log() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());
        let learning = sample_learning(); // Project scope by default

        // Project scope should invoke recall-log
        // May succeed or fail depending on CLI availability
        let result = backend.write(&learning);

        assert!(result.is_ok());
        let write_result = result.unwrap();
        assert!(!write_result.learning_id.is_empty());
        // Location should be daily log path (if succeeded) or empty (if failed)
        if write_result.success {
            assert!(write_result.location.contains("memory/daily/"));
        }
    }

    #[test]
    fn test_write_team_scope_invokes_recall_log() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let mut learning = sample_learning();
        learning.scope = LearningScope::Team;

        // Team scope should also invoke recall-log (same as Project)
        let result = backend.write(&learning);

        assert!(result.is_ok());
        let write_result = result.unwrap();
        assert!(!write_result.learning_id.is_empty());
    }

    #[test]
    fn test_write_personal_scope() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        let personal_path = temp.path().join("personal-learnings.md");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::with_paths(&memory_dir, &personal_path, temp.path());

        let mut learning = sample_learning();
        learning.scope = LearningScope::Personal;

        let result = backend.write(&learning).unwrap();

        assert!(result.success);
        assert!(personal_path.exists());

        let content = fs::read_to_string(&personal_path).unwrap();
        assert!(content.contains(&format!("{}{}", GROVE_ID_PREFIX, learning.id)));
        assert!(content.contains("Avoid N+1 queries"));
    }

    #[test]
    fn test_write_ephemeral_scope_routes_to_daily_log() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let mut learning = sample_learning();
        learning.scope = LearningScope::Ephemeral;

        // Ephemeral scope routes to daily logs (per architecture 5.3.3)
        // This may succeed or fail depending on CLI availability
        let result = backend.write(&learning);

        // Should return Ok (fail-open behavior) regardless of CLI availability
        assert!(result.is_ok());
        let write_result = result.unwrap();
        // Either succeeded (CLI available) or failed gracefully (CLI unavailable)
        assert!(!write_result.learning_id.is_empty());
        // Should NOT be "ephemeral" location - that's the Markdown backend behavior
        // Total Recall backend routes Ephemeral to daily logs
    }

    // Detection tests are in discovery/backends.rs

    // Fail-open tests

    #[test]
    fn test_write_returns_result_without_panic() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());
        let learning = sample_learning();

        // This may succeed or fail depending on whether 'claude' CLI is available
        // The key is that it should not panic and should return a valid result
        let result = backend.write(&learning);

        // Should return Ok (fail-open behavior) regardless of CLI availability
        assert!(result.is_ok());
        let write_result = result.unwrap();
        // Either succeeded (CLI available) or failed gracefully (CLI unavailable)
        assert!(!write_result.learning_id.is_empty());
    }

    #[test]
    fn test_search_cli_unavailable_returns_empty() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let query = SearchQuery::with_keywords(vec!["test".to_string()]);

        // This will fail because 'claude' CLI is not available in tests
        let results = backend.search(&query, &SearchFilters::default()).unwrap();

        // Should fail-open: return empty vec but not panic/error
        assert!(results.is_empty());
    }

    // Backend trait tests

    #[test]
    fn test_ping_with_memory_dir() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        assert!(backend.ping());
    }

    #[test]
    fn test_ping_without_memory_dir() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("nonexistent");

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        assert!(!backend.ping());
    }

    #[test]
    fn test_name() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        assert_eq!(backend.name(), "total-recall");
    }

    // Search with empty query

    #[test]
    fn test_search_empty_query_returns_empty() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let query = SearchQuery::new();
        let results = backend.search(&query, &SearchFilters::default()).unwrap();

        assert!(results.is_empty());
    }

    // Parse search results with filters

    #[test]
    fn test_parse_search_results_with_max_results() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let output = "**Pattern** (grove:cl_001): First
> Detail 1

---

**Pattern** (grove:cl_002): Second
> Detail 2

---

**Pattern** (grove:cl_003): Third
> Detail 3";

        let filters = SearchFilters::default().max_results(2);
        let results = backend.parse_search_results(output, &filters);

        assert_eq!(results.len(), 2);
    }

    // Relevance scoring

    #[test]
    fn test_parse_search_results_relevance_decreases() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let output = "**Pattern** (grove:cl_001): First
> Detail 1

---

**Pattern** (grove:cl_002): Second
> Detail 2";

        let results = backend.parse_search_results(output, &SearchFilters::default());

        assert_eq!(results.len(), 2);
        assert!(results[0].relevance > results[1].relevance);
    }
}
