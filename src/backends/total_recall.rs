//! Total Recall backend adapter for Grove.
//!
//! This module provides integration with Total Recall's tiered memory system.
//! It writes directly to Total Recall's daily log files (`memory/daily/YYYY-MM-DD.md`)
//! and searches by reading files directly.
//!
//! **Why direct file access?** Total Recall's skills (`/recall-write`, `/recall-log`)
//! are interactive Claude Code skills that work within conversations, not CLI commands
//! that can be invoked as subprocesses. Grove writes directly to the daily log files
//! using the same format Total Recall expects.
//!
//! **Fail-open behavior**: If file operations fail, log warning and continue
//! without persistence. Callers can optionally fall back to the markdown backend.

use std::fs::{self, OpenOptions};
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};

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
/// Integrates with Total Recall's tiered memory system by writing directly to
/// daily log files. Total Recall's skills are interactive Claude Code skills,
/// not CLI commands, so Grove writes to the same files those skills use.
///
/// Supports:
/// - Scope routing (project/team/ephemeral → daily log, personal → direct file)
/// - Direct file writes to `memory/daily/YYYY-MM-DD.md`
/// - fail-open behavior for file operation failures
/// - grove: ID prefix for filtering searches
#[derive(Debug, Clone)]
pub struct TotalRecallBackend {
    /// Path to the memory/ directory.
    memory_dir: PathBuf,
    /// Path for personal learnings (bypasses TR).
    personal_path: PathBuf,
}

impl TotalRecallBackend {
    /// Create a new Total Recall backend with the given memory directory.
    ///
    /// The personal learnings path is automatically set to `~/.grove/personal-learnings.md`.
    ///
    /// Note: The `_project_dir` parameter is kept for API compatibility but is no longer used.
    /// Grove now writes directly to memory files instead of invoking CLI commands.
    pub fn new(memory_dir: impl AsRef<Path>, _project_dir: impl AsRef<Path>) -> Self {
        let personal_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".grove")
            .join("personal-learnings.md");

        Self {
            memory_dir: memory_dir.as_ref().to_path_buf(),
            personal_path,
        }
    }

    /// Create a backend with explicit paths for testing.
    ///
    /// Note: The `_project_dir` parameter is kept for API compatibility but is no longer used.
    pub fn with_paths(
        memory_dir: impl AsRef<Path>,
        personal_path: impl AsRef<Path>,
        _project_dir: impl AsRef<Path>,
    ) -> Self {
        Self {
            memory_dir: memory_dir.as_ref().to_path_buf(),
            personal_path: personal_path.as_ref().to_path_buf(),
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

    /// Write a learning directly to the daily log file.
    ///
    /// Appends the formatted learning to `memory/daily/YYYY-MM-DD.md` under
    /// the `## Learnings` section. Creates the file if it doesn't exist.
    fn write_to_daily_log(&self, note: &str) -> std::result::Result<String, String> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let daily_dir = self.memory_dir.join("daily");
        let daily_path = daily_dir.join(format!("{}.md", today));

        // Ensure daily directory exists
        fs::create_dir_all(&daily_dir).map_err(|e| {
            format!(
                "Failed to create daily directory {}: {}",
                daily_dir.display(),
                e
            )
        })?;

        // Read existing content or create template
        let mut content = if daily_path.exists() {
            fs::read_to_string(&daily_path)
                .map_err(|e| format!("Failed to read daily log {}: {}", daily_path.display(), e))?
        } else {
            self.daily_log_template(&today)
        };

        // Find or create Learnings section and append
        content = self.append_to_learnings_section(&content, note);

        // Write back atomically (write to temp then rename would be better, but this is simpler)
        fs::write(&daily_path, &content)
            .map_err(|e| format!("Failed to write daily log {}: {}", daily_path.display(), e))?;

        Ok(format!("memory/daily/{}.md", today))
    }

    /// Create a new daily log template.
    fn daily_log_template(&self, date: &str) -> String {
        format!(
            r#"# {}

## Decisions

## Corrections

## Commitments

## Open Loops

## Notes

## Learnings

"#,
            date
        )
    }

    /// Append a note to the Learnings section of the daily log content.
    fn append_to_learnings_section(&self, content: &str, note: &str) -> String {
        // Look for ## Learnings section
        if let Some(learnings_pos) = content.find("## Learnings") {
            // Find where Learnings section ends (next ## or end of file)
            let after_header = learnings_pos + "## Learnings".len();
            let section_end = content[after_header..]
                .find("\n## ")
                .map(|pos| after_header + pos)
                .unwrap_or(content.len());

            // Insert note at end of Learnings section
            let mut new_content = content[..section_end].to_string();
            if !new_content.ends_with('\n') {
                new_content.push('\n');
            }
            new_content.push('\n');
            new_content.push_str(note);
            new_content.push('\n');
            new_content.push_str(&content[section_end..]);
            new_content
        } else {
            // No Learnings section found, append one
            let mut new_content = content.to_string();
            if !new_content.ends_with('\n') {
                new_content.push('\n');
            }
            new_content.push_str("\n## Learnings\n\n");
            new_content.push_str(note);
            new_content.push('\n');
            new_content
        }
    }

    /// Search for grove learnings in Total Recall's memory files.
    ///
    /// Searches across daily logs and registers for entries with `grove:` prefix.
    fn search_memory_files(&self, query: &str) -> std::result::Result<String, String> {
        let mut results = String::new();

        // Search daily logs (most recent first)
        let daily_dir = self.memory_dir.join("daily");
        if daily_dir.is_dir() {
            let mut daily_files: Vec<_> = fs::read_dir(&daily_dir)
                .map_err(|e| format!("Failed to read daily dir: {}", e))?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
                .collect();

            // Sort by name descending (most recent first)
            daily_files.sort_by_key(|b| std::cmp::Reverse(b.file_name()));

            // Search last 14 days of logs
            for entry in daily_files.into_iter().take(14) {
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    // Only include if it has grove entries and matches query
                    if content.contains(GROVE_ID_PREFIX) {
                        let query_lower = query.to_lowercase();
                        if content.to_lowercase().contains(&query_lower) {
                            results.push_str(&format!("[{}]\n", entry.path().display()));
                            // Extract grove entries
                            for line in content.lines() {
                                if line.contains(GROVE_ID_PREFIX)
                                    || line.starts_with("> ")
                                    || line.starts_with("Tags:")
                                {
                                    results.push_str(line);
                                    results.push('\n');
                                }
                            }
                            results.push_str("\n---\n\n");
                        }
                    }
                }
            }
        }

        // Search registers
        let registers_dir = self.memory_dir.join("registers");
        if registers_dir.is_dir() {
            if let Ok(entries) = fs::read_dir(&registers_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    if entry.path().extension().is_some_and(|ext| ext == "md") {
                        if let Ok(content) = fs::read_to_string(entry.path()) {
                            if content.contains(GROVE_ID_PREFIX) {
                                let query_lower = query.to_lowercase();
                                if content.to_lowercase().contains(&query_lower) {
                                    results.push_str(&format!("[{}]\n", entry.path().display()));
                                    for line in content.lines() {
                                        if line.contains(GROVE_ID_PREFIX)
                                            || line.starts_with("> ")
                                            || line.starts_with("Tags:")
                                        {
                                            results.push_str(line);
                                            results.push('\n');
                                        }
                                    }
                                    results.push_str("\n---\n\n");
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(results)
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

        // Write directly to daily log file
        match self.write_to_daily_log(&note) {
            Ok(location) => Ok(WriteResult::success(&learning.id, location)),
            Err(err) => {
                // Fail-open: log warning, return failure result but don't block
                warn!("Total Recall write failed: {}", err);
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

        match self.search_memory_files(&search_term) {
            Ok(output) => Ok(self.parse_search_results(&output, filters)),
            Err(err) => {
                // Fail-open: log warning, return empty results
                warn!("Total Recall search failed: {}", err);
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
    fn test_write_project_scope_to_daily_log() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());
        let learning = sample_learning(); // Project scope by default

        // Project scope should write directly to daily log
        let result = backend.write(&learning);

        assert!(result.is_ok());
        let write_result = result.unwrap();
        assert!(write_result.success);
        assert!(!write_result.learning_id.is_empty());
        assert!(write_result.location.contains("memory/daily/"));

        // Verify file was created with correct content
        let daily_path = memory_dir
            .join("daily")
            .join(format!("{}.md", chrono::Utc::now().format("%Y-%m-%d")));
        assert!(daily_path.exists());
        let content = fs::read_to_string(&daily_path).unwrap();
        assert!(content.contains("## Learnings"));
        assert!(content.contains(&format!("grove:{}", learning.id)));
        assert!(content.contains("Avoid N+1 queries"));
    }

    #[test]
    fn test_write_team_scope_to_daily_log() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let mut learning = sample_learning();
        learning.scope = LearningScope::Team;

        // Team scope should also write to daily log (same as Project)
        let result = backend.write(&learning);

        assert!(result.is_ok());
        let write_result = result.unwrap();
        assert!(write_result.success);
        assert!(!write_result.learning_id.is_empty());
        assert!(write_result.location.contains("memory/daily/"));
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
    fn test_write_always_succeeds_with_valid_memory_dir() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());
        let learning = sample_learning();

        // Direct file writes should always succeed with a valid memory directory
        let result = backend.write(&learning);

        assert!(result.is_ok());
        let write_result = result.unwrap();
        assert!(write_result.success);
        assert!(!write_result.learning_id.is_empty());
    }

    #[test]
    fn test_search_no_grove_entries_returns_empty() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let query = SearchQuery::with_keywords(vec!["test".to_string()]);

        // No grove entries exist yet, should return empty
        let results = backend.search(&query, &SearchFilters::default()).unwrap();

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
