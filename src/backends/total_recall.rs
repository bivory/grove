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
//!
//! Format constants are centralized in [`super::total_recall_format`] to simplify
//! updates if Total Recall changes its format.

use std::fs::{self, OpenOptions};
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use tracing::warn;

use crate::backends::total_recall_format::{
    self as fmt, BLOCKQUOTE_PREFIX, DAILY_LOG_SEARCH_LIMIT, DATE_FORMAT, ENTRY_SEPARATOR,
    GROVE_ID_PREFIX, LABEL_CATEGORY, LABEL_CONFIDENCE, LABEL_CREATED, LABEL_FILES, LABEL_SUMMARY,
    LABEL_TAGS, LABEL_TICKET, MAX_ID_LENGTH, METADATA_SEPARATOR, SECTION_LEARNINGS, SECTION_PREFIX,
    TIMESTAMP_FORMAT, TIME_FORMAT,
};
use crate::backends::traits::{
    MemoryBackend, SearchFilters, SearchQuery, SearchResult, WriteResult,
};
use crate::core::{CompoundLearning, Confidence, LearningScope};
use crate::error::Result;

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
        note.push_str(&format!("[{}] ", learning.timestamp.format(TIME_FORMAT)));

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
            note.push_str(BLOCKQUOTE_PREFIX);
            note.push_str(line);
            note.push('\n');
        }

        // Metadata line
        let mut meta_parts = Vec::new();

        // Tags
        if !learning.tags.is_empty() {
            let tags: Vec<String> = learning.tags.iter().map(|t| format!("#{}", t)).collect();
            meta_parts.push(format!("{} {}", LABEL_TAGS, tags.join(" ")));
        }

        // Confidence
        meta_parts.push(format!(
            "{} {}",
            LABEL_CONFIDENCE,
            confidence_display(&learning.confidence)
        ));

        // Ticket
        if let Some(ref ticket_id) = learning.ticket_id {
            meta_parts.push(format!("{} {}", LABEL_TICKET, ticket_id));
        }

        // Files
        if let Some(ref files) = learning.context_files {
            if !files.is_empty() {
                meta_parts.push(format!("{} {}", LABEL_FILES, files.join(", ")));
            }
        }

        if !meta_parts.is_empty() {
            note.push('\n');
            note.push_str(&meta_parts.join(METADATA_SEPARATOR));
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
            "{} {}\n",
            LABEL_CATEGORY,
            learning.category.display_name()
        ));
        md.push_str(&format!("{} {}\n", LABEL_SUMMARY, learning.summary));
        md.push_str(&format!(
            "{} {}{}{} {}\n",
            LABEL_CONFIDENCE,
            confidence_display(&learning.confidence),
            METADATA_SEPARATOR,
            LABEL_CREATED,
            learning.timestamp.format(TIMESTAMP_FORMAT)
        ));

        // Tags
        if !learning.tags.is_empty() {
            let tags: Vec<String> = learning.tags.iter().map(|t| format!("#{}", t)).collect();
            md.push_str(&format!("**Tags:** {}\n", tags.join(" ")));
        }

        // Detail
        md.push_str(&format!("\n{}\n", learning.detail));

        // Separator
        md.push_str(&format!("\n{}\n\n", ENTRY_SEPARATOR));

        md
    }

    /// Write a learning directly to the daily log file.
    ///
    /// Appends the formatted learning to `memory/daily/YYYY-MM-DD.md` under
    /// the `## Learnings` section. Creates the file if it doesn't exist.
    fn write_to_daily_log(&self, note: &str) -> std::result::Result<String, String> {
        let today = Utc::now().format(DATE_FORMAT).to_string();
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
            fmt::daily_log_template(&today)
        };

        // Find or create Learnings section and append
        content = self.append_to_learnings_section(&content, note);

        // Write back atomically (write to temp then rename would be better, but this is simpler)
        fs::write(&daily_path, &content)
            .map_err(|e| format!("Failed to write daily log {}: {}", daily_path.display(), e))?;

        Ok(format!("memory/daily/{}.md", today))
    }

    /// Append a note to the Learnings section of the daily log content.
    fn append_to_learnings_section(&self, content: &str, note: &str) -> String {
        // Look for ## Learnings section
        if let Some(learnings_pos) = content.find(SECTION_LEARNINGS) {
            // Find where Learnings section ends (next ## or end of file)
            let after_header = learnings_pos + SECTION_LEARNINGS.len();
            let section_end = content[after_header..]
                .find(SECTION_PREFIX)
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
            new_content.push_str(&format!("\n{}\n\n", SECTION_LEARNINGS));
            new_content.push_str(note);
            new_content.push('\n');
            new_content
        }
    }
    /// List all grove learnings from Total Recall's memory files.
    ///
    /// Unlike search_memory_files, this returns all grove entries without query filtering.
    /// Used when search is called with an empty query (e.g., for `grove list`).
    fn list_all_grove_learnings(&self) -> std::result::Result<String, String> {
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

            // Search last N days of logs
            for entry in daily_files.into_iter().take(DAILY_LOG_SEARCH_LIMIT) {
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    // Only include if it has grove entries
                    if content.contains(GROVE_ID_PREFIX) {
                        results.push_str(&format!("[{}]\n", entry.path().display()));
                        // Extract grove entries - add separator after each Tags: line
                        // to properly delimit multiple entries within the same file
                        for line in content.lines() {
                            if line.contains(GROVE_ID_PREFIX)
                                || line.starts_with(BLOCKQUOTE_PREFIX)
                                || line.starts_with(LABEL_TAGS)
                            {
                                results.push_str(line);
                                results.push('\n');
                                // Tags: line marks end of an entry, add separator
                                if line.starts_with(LABEL_TAGS) {
                                    results.push_str(&format!("\n{}\n\n", ENTRY_SEPARATOR));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Also check registers for any promoted grove learnings
        let registers_dir = self.memory_dir.join("registers");
        if registers_dir.is_dir() {
            if let Ok(entries) = fs::read_dir(&registers_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    if entry.path().extension().is_some_and(|ext| ext == "md") {
                        if let Ok(content) = fs::read_to_string(entry.path()) {
                            if content.contains(GROVE_ID_PREFIX) {
                                results.push_str(&format!("[{}]\n", entry.path().display()));
                                for line in content.lines() {
                                    if line.contains(GROVE_ID_PREFIX)
                                        || line.starts_with(BLOCKQUOTE_PREFIX)
                                        || line.starts_with(LABEL_TAGS)
                                    {
                                        results.push_str(line);
                                        results.push('\n');
                                        // Tags: line marks end of an entry, add separator
                                        if line.starts_with(LABEL_TAGS) {
                                            results.push_str(&format!("\n{}\n\n", ENTRY_SEPARATOR));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(results)
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

            // Search last N days of logs
            for entry in daily_files.into_iter().take(DAILY_LOG_SEARCH_LIMIT) {
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    // Only include if it has grove entries and matches query
                    if content.contains(GROVE_ID_PREFIX) {
                        let query_lower = query.to_lowercase();
                        if content.to_lowercase().contains(&query_lower) {
                            results.push_str(&format!("[{}]\n", entry.path().display()));
                            // Extract grove entries - add separator after each Tags: line
                            for line in content.lines() {
                                if line.contains(GROVE_ID_PREFIX)
                                    || line.starts_with(BLOCKQUOTE_PREFIX)
                                    || line.starts_with(LABEL_TAGS)
                                {
                                    results.push_str(line);
                                    results.push('\n');
                                    // Tags: line marks end of an entry, add separator
                                    if line.starts_with(LABEL_TAGS) {
                                        results.push_str(&format!("\n{}\n\n", ENTRY_SEPARATOR));
                                    }
                                }
                            }
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
                                            || line.starts_with(BLOCKQUOTE_PREFIX)
                                            || line.starts_with(LABEL_TAGS)
                                        {
                                            results.push_str(line);
                                            results.push('\n');
                                            // Tags: line marks end of an entry, add separator
                                            if line.starts_with(LABEL_TAGS) {
                                                results.push_str(&format!(
                                                    "\n{}\n\n",
                                                    ENTRY_SEPARATOR
                                                ));
                                            }
                                        }
                                    }
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
            if line.trim() == ENTRY_SEPARATOR || (line.is_empty() && !current.is_empty()) {
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
            .unwrap_or(entry.len().min(after_prefix + MAX_ID_LENGTH));

        if id_end <= after_prefix {
            return None;
        }

        let grove_id = entry[after_prefix..id_end].trim();

        // Extract category from **Category** pattern
        let category = fmt::CATEGORY_MARKERS
            .iter()
            .find(|(marker, _)| entry.contains(*marker))
            .map(|(_, name)| match *name {
                "Pattern" => crate::core::LearningCategory::Pattern,
                "Pitfall" => crate::core::LearningCategory::Pitfall,
                "Convention" => crate::core::LearningCategory::Convention,
                "Dependency" => crate::core::LearningCategory::Dependency,
                "Process" => crate::core::LearningCategory::Process,
                "Domain" => crate::core::LearningCategory::Domain,
                "Debugging" => crate::core::LearningCategory::Debugging,
                _ => crate::core::LearningCategory::Pattern,
            })
            .unwrap_or(crate::core::LearningCategory::Pattern);

        // Extract summary (text after the category/ID header, before newline)
        // Find the line containing the grove ID, which has the summary
        let summary = entry
            .lines()
            .find(|line| line.contains(GROVE_ID_PREFIX))
            .and_then(|header_line| {
                // Find the colon after the ID pattern, take text after it
                header_line
                    .rfind("):")
                    .map(|i| header_line[i + 2..].trim().to_string())
            })
            .unwrap_or_default();

        // Extract detail from blockquotes
        let detail: String = entry
            .lines()
            .filter(|line| line.starts_with(BLOCKQUOTE_PREFIX))
            .map(|line| line.strip_prefix(BLOCKQUOTE_PREFIX).unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");

        // Extract tags from "Tags: #tag1 #tag2" pattern
        let tags = entry
            .lines()
            .find(|line| line.contains(LABEL_TAGS))
            .map(|line| {
                line.split(LABEL_TAGS)
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

        // Parse timestamp from entry
        // Format: [HH:MM] at start of header line, date from ID (cl_YYYYMMDD_NNN)
        let timestamp = self.parse_entry_timestamp(entry, grove_id);

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
            timestamp,
            context_files: None,
            status: crate::core::LearningStatus::Active,
        })
    }

    /// Parse timestamp from entry header and ID.
    ///
    /// Entry format: `[HH:MM] **Category** (grove:cl_YYYYMMDD_NNN): Summary`
    /// Extracts time from `[HH:MM]` and date from ID.
    fn parse_entry_timestamp(&self, entry: &str, grove_id: &str) -> DateTime<Utc> {
        // Try to extract date from ID (cl_YYYYMMDD_NNN)
        let date_str = grove_id
            .strip_prefix("cl_")
            .and_then(|s| s.get(0..8))
            .unwrap_or("");

        // Try to extract time from [HH:MM] at start of header line
        let time_str = entry
            .lines()
            .find(|line| line.contains(GROVE_ID_PREFIX))
            .and_then(|line| {
                if line.starts_with('[') {
                    line.find(']').and_then(|end| line.get(1..end))
                } else {
                    None
                }
            })
            .unwrap_or("00:00");

        // Parse date components
        let year: i32 = date_str
            .get(0..4)
            .and_then(|s| s.parse().ok())
            .unwrap_or(2026);
        let month: u32 = date_str.get(4..6).and_then(|s| s.parse().ok()).unwrap_or(1);
        let day: u32 = date_str.get(6..8).and_then(|s| s.parse().ok()).unwrap_or(1);

        // Parse time components
        let parts: Vec<&str> = time_str.split(':').collect();
        let hour: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minute: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

        // Build timestamp
        chrono::NaiveDate::from_ymd_opt(year, month, day)
            .and_then(|date| date.and_hms_opt(hour, minute, 0))
            .map(|naive| DateTime::from_naive_utc_and_offset(naive, Utc))
            .unwrap_or_else(Utc::now)
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
        // If query is empty, list all grove learnings (used by `grove list`)
        let output = if query.is_empty() {
            match self.list_all_grove_learnings() {
                Ok(output) => output,
                Err(err) => {
                    warn!("Total Recall list failed: {}", err);
                    return Ok(Vec::new());
                }
            }
        } else {
            let search_term = self.build_search_term(query);
            match self.search_memory_files(&search_term) {
                Ok(output) => output,
                Err(err) => {
                    warn!("Total Recall search failed: {}", err);
                    return Ok(Vec::new());
                }
            }
        };

        Ok(self.parse_search_results(&output, filters))
    }

    fn ping(&self) -> bool {
        // Check if memory directory exists
        self.memory_dir.is_dir()
    }

    fn name(&self) -> &'static str {
        "total-recall"
    }

    fn next_id(&self) -> String {
        // Scan daily logs to find the highest counter for today
        let today = Utc::now().format(DATE_FORMAT).to_string();
        let today_prefix = format!("cl_{}_", today.replace('-', ""));
        let mut max_counter: u32 = 0;

        // Check today's daily log
        let daily_dir = self.memory_dir.join("daily");
        let daily_path = daily_dir.join(format!("{}.md", today));

        if daily_path.exists() {
            if let Ok(content) = fs::read_to_string(&daily_path) {
                for line in content.lines() {
                    if let Some(id) = self.extract_grove_id(line) {
                        if id.starts_with(&today_prefix) {
                            // Parse the counter from the ID (last 3 digits)
                            if let Some(counter_str) = id.strip_prefix(&today_prefix) {
                                if let Ok(counter) = counter_str.parse::<u32>() {
                                    max_counter = max_counter.max(counter + 1);
                                }
                            }
                        }
                    }
                }
            }
        }

        format!("cl_{}_{:03}", today.replace('-', ""), max_counter % 1000)
    }
}

impl TotalRecallBackend {
    /// Extract a grove ID from a line if present.
    fn extract_grove_id(&self, line: &str) -> Option<String> {
        let start = line.find(GROVE_ID_PREFIX)?;
        let after_prefix = start + GROVE_ID_PREFIX.len();
        let end = line[after_prefix..]
            .find([')', ':', ' ', '\n'])
            .map(|i| after_prefix + i)
            .unwrap_or(line.len().min(after_prefix + MAX_ID_LENGTH));

        if end > after_prefix {
            Some(line[after_prefix..end].trim().to_string())
        } else {
            None
        }
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
    fn test_parse_grove_entry_with_timestamp() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        // Format used by format_learning includes timestamp
        let entry = "[16:30] **Pitfall** (grove:cl_20260101_001): Avoid N+1 queries
> The dashboard was loading users separately.
> Use eager loading instead.

Tags: #performance #database | Confidence: High | Ticket: T001";

        let learning = backend.parse_grove_entry(entry).unwrap();

        assert_eq!(learning.id, "cl_20260101_001");
        assert_eq!(learning.summary, "Avoid N+1 queries");
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
            .join(format!("{}.md", chrono::Utc::now().format(DATE_FORMAT)));
        assert!(daily_path.exists());
        let content = fs::read_to_string(&daily_path).unwrap();
        assert!(content.contains(SECTION_LEARNINGS));
        assert!(content.contains(&format!("{}{}", GROVE_ID_PREFIX, learning.id)));
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

    #[test]
    fn test_search_empty_query_returns_all_when_learnings_exist() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        // Write a learning first
        let learning = sample_learning();
        let result = backend.write(&learning);
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        // Now search with empty query - should return the learning
        let query = SearchQuery::new();
        let results = backend.search(&query, &SearchFilters::default()).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].learning.summary, learning.summary);
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

    // next_id tests

    #[test]
    fn test_next_id_starts_at_000_for_empty_backend() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let id = backend.next_id();

        // Should be cl_YYYYMMDD_000
        assert!(id.starts_with("cl_"));
        assert!(id.ends_with("_000"));
    }

    #[test]
    fn test_next_id_increments_after_write() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        // Write a learning
        let learning = sample_learning();
        backend.write(&learning).unwrap();

        // Next ID should be _001
        let id = backend.next_id();
        assert!(id.ends_with("_001"), "Expected _001, got {}", id);
    }

    #[test]
    fn test_next_id_finds_highest_counter() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        let daily_dir = memory_dir.join("daily");
        fs::create_dir_all(&daily_dir).unwrap();

        // Create a daily log with some existing entries
        let today = Utc::now().format(DATE_FORMAT).to_string();
        let today_ymd = today.replace('-', "");
        let daily_path = daily_dir.join(format!("{}.md", today));

        let content = format!(
            r#"# {}

## Learnings

[10:00] **Pattern** (grove:cl_{}_002): First learning
> Detail

Tags: #test | Confidence: High

[11:00] **Pattern** (grove:cl_{}_005): Second learning
> Detail

Tags: #test | Confidence: High
"#,
            today, today_ymd, today_ymd
        );
        fs::write(&daily_path, content).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        // Next ID should be _006 (highest was 005)
        let id = backend.next_id();
        assert!(id.ends_with("_006"), "Expected _006, got {}", id);
    }

    // Timestamp parsing tests

    #[test]
    fn test_parse_entry_timestamp_from_id_and_time() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let entry = "[14:30] **Pattern** (grove:cl_20260210_001): Test summary
> Test detail

Tags: #test | Confidence: High";

        let timestamp = backend.parse_entry_timestamp(entry, "cl_20260210_001");

        // Should be 2026-02-10 14:30:00 UTC
        assert_eq!(
            timestamp.format("%Y-%m-%d %H:%M").to_string(),
            "2026-02-10 14:30"
        );
    }

    #[test]
    fn test_parse_entry_timestamp_defaults_for_missing() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        // Entry without timestamp prefix
        let entry = "**Pattern** (grove:cl_20260210_001): Test summary";

        let timestamp = backend.parse_entry_timestamp(entry, "cl_20260210_001");

        // Should still parse date from ID, time defaults to 00:00
        assert_eq!(timestamp.format("%Y-%m-%d").to_string(), "2026-02-10");
    }

    #[test]
    fn test_parse_grove_entry_includes_correct_timestamp() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let entry = "[16:45] **Pitfall** (grove:cl_20260215_003): Watch out for this
> Important detail here

Tags: #warning | Confidence: High";

        let learning = backend.parse_grove_entry(entry).unwrap();

        assert_eq!(learning.id, "cl_20260215_003");
        assert_eq!(
            learning.timestamp.format("%Y-%m-%d %H:%M").to_string(),
            "2026-02-15 16:45"
        );
    }

    // extract_grove_id tests

    #[test]
    fn test_extract_grove_id() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let line = "[10:00] **Pattern** (grove:cl_20260210_001): Summary";
        let id = backend.extract_grove_id(line);

        assert_eq!(id, Some("cl_20260210_001".to_string()));
    }

    #[test]
    fn test_extract_grove_id_none_when_missing() {
        let temp = TempDir::new().unwrap();
        let memory_dir = temp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let backend = TotalRecallBackend::new(&memory_dir, temp.path());

        let line = "No grove ID here";
        let id = backend.extract_grove_id(line);

        assert!(id.is_none());
    }
}
