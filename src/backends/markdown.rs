//! Markdown-based memory backend for Grove.
//!
//! This module provides an append-only markdown backend that stores learnings
//! in `.grove/learnings.md` (or `~/.grove/personal-learnings.md` for personal scope).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::backends::traits::{
    MemoryBackend, SearchFilters, SearchQuery, SearchResult, WriteResult,
};
use crate::core::{
    CompoundLearning, Confidence, LearningCategory, LearningScope, LearningStatus,
    WriteGateCriterion,
};
use crate::error::{GroveError, Result};

/// Relevance score weights for search matching.
mod scores {
    /// Exact tag match score.
    pub const TAG_EXACT: f64 = 1.0;
    /// Partial tag match score (substring).
    pub const TAG_PARTIAL: f64 = 0.5;
    /// File path overlap score.
    pub const FILE_OVERLAP: f64 = 0.8;
    /// Keyword match score.
    pub const KEYWORD: f64 = 0.3;
}

/// Markdown-based memory backend.
///
/// Stores learnings in append-only markdown format. Supports:
/// - Scope routing (project/team → shared file, personal → user file)
/// - Content sanitization
/// - Search with relevance scoring
/// - In-place status updates for archiving
#[derive(Debug, Clone)]
pub struct MarkdownBackend {
    /// Path to the project learnings file (.grove/learnings.md).
    project_path: PathBuf,
    /// Path to the personal learnings file (~/.grove/personal-learnings.md).
    personal_path: PathBuf,
}

impl MarkdownBackend {
    /// Create a new markdown backend with the given project learnings path.
    ///
    /// The personal learnings path is automatically set to `~/.grove/personal-learnings.md`.
    pub fn new(project_path: impl AsRef<Path>) -> Self {
        let personal_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".grove")
            .join("personal-learnings.md");

        Self {
            project_path: project_path.as_ref().to_path_buf(),
            personal_path,
        }
    }

    /// Create a backend with explicit paths for both project and personal files.
    ///
    /// Useful for testing.
    pub fn with_paths(project_path: impl AsRef<Path>, personal_path: impl AsRef<Path>) -> Self {
        Self {
            project_path: project_path.as_ref().to_path_buf(),
            personal_path: personal_path.as_ref().to_path_buf(),
        }
    }

    /// Get the file path for a learning based on its scope.
    fn path_for_scope(&self, scope: &LearningScope) -> Option<&Path> {
        match scope {
            LearningScope::Project | LearningScope::Team => Some(&self.project_path),
            LearningScope::Personal => Some(&self.personal_path),
            LearningScope::Ephemeral => None, // Ephemeral learnings are discarded
        }
    }

    /// Parse all learnings from a markdown file.
    pub fn parse_file(&self, path: &Path) -> Result<Vec<CompoundLearning>> {
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(path).map_err(|e| {
            GroveError::backend(format!("Failed to read {}: {}", path.display(), e))
        })?;

        parse_learnings_from_markdown(&content)
    }

    /// Parse all learnings from the project file.
    pub fn parse_learnings(&self) -> Result<Vec<CompoundLearning>> {
        self.parse_file(&self.project_path)
    }

    /// Parse all learnings from the personal file.
    pub fn parse_personal_learnings(&self) -> Result<Vec<CompoundLearning>> {
        self.parse_file(&self.personal_path)
    }

    /// Parse all learnings from both project and personal files.
    pub fn parse_all_learnings(&self) -> Result<Vec<CompoundLearning>> {
        let mut learnings = self.parse_learnings()?;
        learnings.extend(self.parse_personal_learnings()?);
        Ok(learnings)
    }

    /// Archive a learning by ID (changes status in-place).
    pub fn archive(&self, learning_id: &str) -> Result<()> {
        self.update_status(learning_id, LearningStatus::Archived)
    }

    /// Restore an archived learning (changes status back to Active).
    pub fn restore(&self, learning_id: &str) -> Result<()> {
        self.update_status(learning_id, LearningStatus::Active)
    }

    /// Update the status of a learning in-place.
    fn update_status(&self, learning_id: &str, new_status: LearningStatus) -> Result<()> {
        // Try project file first, then personal
        if self.update_status_in_file(&self.project_path, learning_id, new_status)? {
            return Ok(());
        }
        if self.update_status_in_file(&self.personal_path, learning_id, new_status)? {
            return Ok(());
        }

        Err(GroveError::backend(format!(
            "Learning {} not found",
            learning_id
        )))
    }

    /// Update the status of a learning in a specific file.
    ///
    /// Returns true if the learning was found and updated.
    fn update_status_in_file(
        &self,
        path: &Path,
        learning_id: &str,
        new_status: LearningStatus,
    ) -> Result<bool> {
        if !path.exists() {
            return Ok(false);
        }

        let content = fs::read_to_string(path).map_err(|e| {
            GroveError::backend(format!("Failed to read {}: {}", path.display(), e))
        })?;

        let header = format!("## {}", learning_id);
        if !content.contains(&header) {
            return Ok(false);
        }

        // Parse and rewrite the file with updated status
        let learnings = parse_learnings_from_markdown(&content)?;
        let mut found = false;
        let mut updated_learnings = Vec::new();

        for mut learning in learnings {
            if learning.id == learning_id {
                learning.status = new_status;
                found = true;
            }
            updated_learnings.push(learning);
        }

        if !found {
            return Ok(false);
        }

        // Rewrite the file
        self.write_all_learnings(path, &updated_learnings)?;

        Ok(true)
    }

    /// Write all learnings to a file (used for status updates).
    fn write_all_learnings(&self, path: &Path, learnings: &[CompoundLearning]) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                GroveError::backend(format!(
                    "Failed to create directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|e| {
                GroveError::backend(format!("Failed to open {}: {}", path.display(), e))
            })?;

        for learning in learnings {
            let markdown = format_learning_as_markdown(learning);
            file.write_all(markdown.as_bytes()).map_err(|e| {
                GroveError::backend(format!("Failed to write to {}: {}", path.display(), e))
            })?;
        }

        Ok(())
    }

    /// Compute relevance score for a learning against a query.
    fn compute_relevance(&self, learning: &CompoundLearning, query: &SearchQuery) -> f64 {
        let mut score = 0.0;
        let mut matches = 0;

        // Tag matching
        for query_tag in &query.tags {
            let query_tag_lower = query_tag.to_lowercase();
            for learning_tag in &learning.tags {
                let learning_tag_lower = learning_tag.to_lowercase();
                if learning_tag_lower == query_tag_lower {
                    score += scores::TAG_EXACT;
                    matches += 1;
                } else if learning_tag_lower.contains(&query_tag_lower)
                    || query_tag_lower.contains(&learning_tag_lower)
                {
                    score += scores::TAG_PARTIAL;
                    matches += 1;
                }
            }
        }

        // File overlap matching
        if let Some(ref context_files) = learning.context_files {
            for query_file in &query.files {
                for context_file in context_files {
                    if files_overlap(query_file, context_file) {
                        score += scores::FILE_OVERLAP;
                        matches += 1;
                    }
                }
            }
        }

        // Keyword matching (in summary and detail)
        let summary_lower = learning.summary.to_lowercase();
        let detail_lower = learning.detail.to_lowercase();
        for keyword in &query.keywords {
            let keyword_lower = keyword.to_lowercase();
            if summary_lower.contains(&keyword_lower) || detail_lower.contains(&keyword_lower) {
                score += scores::KEYWORD;
                matches += 1;
            }
        }

        // Ticket ID matching (exact)
        if let Some(ref query_ticket) = query.ticket_id {
            if let Some(ref learning_ticket) = learning.ticket_id {
                if learning_ticket == query_ticket {
                    score += scores::TAG_EXACT;
                    matches += 1;
                }
            }
        }

        // Normalize score to 0.0 - 1.0 range if we have matches
        if matches > 0 {
            // Cap at 1.0
            score.min(1.0)
        } else {
            0.0
        }
    }
}

impl MemoryBackend for MarkdownBackend {
    fn write(&self, learning: &CompoundLearning) -> Result<WriteResult> {
        let path = match self.path_for_scope(&learning.scope) {
            Some(p) => p,
            None => {
                // Ephemeral scope - discard
                return Ok(WriteResult::success_with_message(
                    &learning.id,
                    "ephemeral",
                    "Ephemeral learning discarded (not persisted)",
                ));
            }
        };

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                GroveError::backend(format!(
                    "Failed to create directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        // Sanitize and format the learning
        let sanitized = sanitize_learning(learning);
        let markdown = format_learning_as_markdown(&sanitized);

        // Append to file
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| {
                GroveError::backend(format!("Failed to open {}: {}", path.display(), e))
            })?;

        file.write_all(markdown.as_bytes()).map_err(|e| {
            GroveError::backend(format!("Failed to write to {}: {}", path.display(), e))
        })?;

        let message = if learning_was_sanitized(learning, &sanitized) {
            Some("Content was sanitized".to_string())
        } else {
            None
        };

        Ok(WriteResult {
            success: true,
            learning_id: learning.id.clone(),
            location: path.display().to_string(),
            message,
        })
    }

    fn search(&self, query: &SearchQuery, filters: &SearchFilters) -> Result<Vec<SearchResult>> {
        // If query is empty, return all learnings that match filters
        let all_learnings = self.parse_all_learnings()?;

        let mut results: Vec<SearchResult> = all_learnings
            .into_iter()
            .filter(|learning| filters.matches(learning))
            .filter_map(|learning| {
                let relevance = if query.is_empty() {
                    // If no query, all matching learnings are equally relevant
                    1.0
                } else {
                    self.compute_relevance(&learning, query)
                };

                if relevance > 0.0 || query.is_empty() {
                    Some(SearchResult::new(learning, relevance))
                } else {
                    None
                }
            })
            .collect();

        // Sort by relevance (highest first)
        results.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Apply max_results limit
        if let Some(limit) = filters.max_results {
            results.truncate(limit);
        }

        Ok(results)
    }

    fn ping(&self) -> bool {
        // Check if we can access the project path (or its parent directory)
        if let Some(parent) = self.project_path.parent() {
            if parent.exists() {
                return true;
            }
            // Try to create the directory
            if fs::create_dir_all(parent).is_ok() {
                return true;
            }
        }
        false
    }

    fn name(&self) -> &'static str {
        "markdown"
    }

    fn archive(&self, learning_id: &str) -> Result<()> {
        // Delegate to the inherent method
        self.update_status(learning_id, LearningStatus::Archived)
    }

    fn restore(&self, learning_id: &str) -> Result<()> {
        // Delegate to the inherent method
        self.update_status(learning_id, LearningStatus::Active)
    }

    fn next_id(&self) -> String {
        // Scan existing learnings to find the highest counter for today
        let today = chrono::Utc::now().format("%Y%m%d").to_string();
        let today_prefix = format!("cl_{}_", today);
        let mut max_counter: u32 = 0;

        // Parse learnings from both project and personal files
        for path in [&self.project_path, &self.personal_path] {
            if let Ok(learnings) = self.parse_file(path) {
                for learning in learnings {
                    if learning.id.starts_with(&today_prefix) {
                        // Parse the counter from the ID (last 3 digits)
                        if let Some(counter_str) = learning.id.strip_prefix(&today_prefix) {
                            if let Ok(counter) = counter_str.parse::<u32>() {
                                max_counter = max_counter.max(counter + 1);
                            }
                        }
                    }
                }
            }
        }

        format!("cl_{}_{:03}", today, max_counter % 1000)
    }
}

/// Check if two file paths overlap (same file or one contains the other).
fn files_overlap(path1: &str, path2: &str) -> bool {
    let p1 = Path::new(path1);
    let p2 = Path::new(path2);

    // Exact match
    if p1 == p2 {
        return true;
    }

    // Check if file names match (for cases like "src/foo.rs" vs "foo.rs")
    if let (Some(name1), Some(name2)) = (p1.file_name(), p2.file_name()) {
        if name1 == name2 {
            return true;
        }
    }

    // Check if one path ends with the other
    let s1 = path1.replace('\\', "/");
    let s2 = path2.replace('\\', "/");
    s1.ends_with(&s2) || s2.ends_with(&s1)
}

/// Sanitize a learning before writing.
fn sanitize_learning(learning: &CompoundLearning) -> CompoundLearning {
    let mut sanitized = learning.clone();
    sanitized.summary = sanitize_summary(&learning.summary);
    sanitized.detail = sanitize_detail(&learning.detail);
    sanitized.tags = learning.tags.iter().map(|t| sanitize_tag(t)).collect();
    sanitized
}

/// Check if any sanitization was applied.
fn learning_was_sanitized(original: &CompoundLearning, sanitized: &CompoundLearning) -> bool {
    original.summary != sanitized.summary
        || original.detail != sanitized.detail
        || original.tags != sanitized.tags
}

/// Sanitize a summary: single line, escape # and |.
pub fn sanitize_summary(summary: &str) -> String {
    summary
        .lines()
        .next()
        .unwrap_or("")
        .replace('#', "\\#")
        .replace('|', "\\|")
        .trim()
        .to_string()
}

/// Sanitize detail: balance unbalanced code fences.
pub fn sanitize_detail(detail: &str) -> String {
    let fence_count = detail.matches("```").count();
    if fence_count % 2 == 1 {
        // Unbalanced - add closing fence
        format!("{}\n```", detail.trim())
    } else {
        detail.trim().to_string()
    }
}

/// Sanitize a tag: alphanumeric + hyphens only, lowercase.
pub fn sanitize_tag(tag: &str) -> String {
    tag.chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect::<String>()
        .to_lowercase()
}

/// Format a learning as markdown.
fn format_learning_as_markdown(learning: &CompoundLearning) -> String {
    let mut md = String::new();

    // Header with ID
    md.push_str(&format!("## {}\n\n", learning.id));

    // Metadata
    md.push_str(&format!(
        "**Category:** {}\n",
        learning.category.display_name()
    ));
    md.push_str(&format!("**Summary:** {}\n", learning.summary));
    md.push_str(&format!(
        "**Scope:** {} | **Confidence:** {} | **Status:** {}\n",
        scope_display(&learning.scope),
        confidence_display(&learning.confidence),
        status_display(&learning.status)
    ));

    // Tags
    if !learning.tags.is_empty() {
        let tags: Vec<String> = learning.tags.iter().map(|t| format!("#{}", t)).collect();
        md.push_str(&format!("**Tags:** {}\n", tags.join(" ")));
    }

    // Session and ticket
    if let Some(ref ticket_id) = learning.ticket_id {
        md.push_str(&format!(
            "**Ticket:** {} | **Session:** {}\n",
            ticket_id, learning.session_id
        ));
    } else {
        md.push_str(&format!("**Session:** {}\n", learning.session_id));
    }

    // Context files
    if let Some(ref files) = learning.context_files {
        if !files.is_empty() {
            md.push_str(&format!("**Context Files:** {}\n", files.join(", ")));
        }
    }

    // Criteria met
    if !learning.criteria_met.is_empty() {
        let criteria: Vec<&str> = learning
            .criteria_met
            .iter()
            .map(|c| c.display_name())
            .collect();
        md.push_str(&format!("**Criteria:** {}\n", criteria.join(", ")));
    }

    // Timestamp
    md.push_str(&format!(
        "**Created:** {}\n",
        learning.timestamp.format("%Y-%m-%dT%H:%M:%SZ")
    ));

    // Detail (the main content)
    md.push_str(&format!("\n{}\n", learning.detail));

    // Separator
    md.push_str("\n---\n\n");

    md
}

fn scope_display(scope: &LearningScope) -> &'static str {
    match scope {
        LearningScope::Project => "Project",
        LearningScope::Team => "Team",
        LearningScope::Personal => "Personal",
        LearningScope::Ephemeral => "Ephemeral",
    }
}

fn confidence_display(confidence: &Confidence) -> &'static str {
    match confidence {
        Confidence::High => "High",
        Confidence::Medium => "Medium",
        Confidence::Low => "Low",
    }
}

fn status_display(status: &LearningStatus) -> &'static str {
    match status {
        LearningStatus::Active => "Active",
        LearningStatus::Archived => "Archived",
        LearningStatus::Superseded => "Superseded",
    }
}

/// Parse learnings from markdown content.
fn parse_learnings_from_markdown(content: &str) -> Result<Vec<CompoundLearning>> {
    let mut learnings = Vec::new();
    let mut current_learning: Option<LearningBuilder> = None;
    let mut in_detail = false;
    let mut detail_lines = Vec::new();

    for line in content.lines() {
        // Check for new learning header
        if let Some(id_part) = line.strip_prefix("## ") {
            // Save previous learning if any
            if let Some(builder) = current_learning.take() {
                if in_detail && !detail_lines.is_empty() {
                    let detail = detail_lines.join("\n").trim().to_string();
                    learnings.push(builder.with_detail(detail).build()?);
                } else {
                    learnings.push(builder.build()?);
                }
            }

            // Start new learning
            let id = id_part.trim().to_string();
            current_learning = Some(LearningBuilder::new(id));
            in_detail = false;
            detail_lines.clear();
            continue;
        }

        // Skip if no current learning
        let builder = match current_learning.as_mut() {
            Some(b) => b,
            None => continue,
        };

        // Check for separator (end of learning)
        if line.trim() == "---" {
            if in_detail && !detail_lines.is_empty() {
                let detail = detail_lines.join("\n").trim().to_string();
                builder.detail = Some(detail);
            }
            in_detail = false;
            detail_lines.clear();
            continue;
        }

        // Parse metadata lines
        if !in_detail {
            if let Some(rest) = line.strip_prefix("**Category:**") {
                builder.category = parse_category(rest);
            } else if let Some(rest) = line.strip_prefix("**Summary:**") {
                builder.summary = Some(rest.trim().to_string());
            } else if line.starts_with("**Scope:**") {
                // Parse: **Scope:** X | **Confidence:** Y | **Status:** Z
                let parts: Vec<&str> = line.split('|').collect();
                for part in parts {
                    let part = part.trim();
                    if let Some(rest) = part.strip_prefix("**Scope:**") {
                        builder.scope = parse_scope(rest);
                    } else if let Some(rest) = part.strip_prefix("**Confidence:**") {
                        builder.confidence = parse_confidence(rest);
                    } else if let Some(rest) = part.strip_prefix("**Status:**") {
                        builder.status = parse_status(rest);
                    }
                }
            } else if let Some(rest) = line.strip_prefix("**Tags:**") {
                builder.tags = parse_tags(rest);
            } else if line.starts_with("**Ticket:**") {
                // Parse: **Ticket:** X | **Session:** Y
                let parts: Vec<&str> = line.split('|').collect();
                for part in parts {
                    let part = part.trim();
                    if let Some(rest) = part.strip_prefix("**Ticket:**") {
                        builder.ticket_id = Some(rest.trim().to_string());
                    } else if let Some(rest) = part.strip_prefix("**Session:**") {
                        builder.session_id = Some(rest.trim().to_string());
                    }
                }
            } else if let Some(rest) = line.strip_prefix("**Session:**") {
                builder.session_id = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("**Context Files:**") {
                builder.context_files = Some(parse_context_files(rest));
            } else if let Some(rest) = line.strip_prefix("**Criteria:**") {
                builder.criteria_met = parse_criteria(rest);
            } else if let Some(rest) = line.strip_prefix("**Created:**") {
                builder.timestamp = parse_timestamp(rest);
            } else if line.is_empty() && builder.summary.is_some() {
                // Empty line after metadata - switch to detail mode
                in_detail = true;
            }
        } else {
            // Collecting detail lines
            detail_lines.push(line);
        }
    }

    // Handle last learning
    if let Some(builder) = current_learning {
        if in_detail && !detail_lines.is_empty() {
            let detail = detail_lines.join("\n").trim().to_string();
            learnings.push(builder.with_detail(detail).build()?);
        } else if builder.detail.is_some() {
            learnings.push(builder.build()?);
        }
    }

    Ok(learnings)
}

/// Builder for parsing learnings from markdown.
#[derive(Debug)]
struct LearningBuilder {
    id: String,
    category: Option<LearningCategory>,
    summary: Option<String>,
    detail: Option<String>,
    scope: Option<LearningScope>,
    confidence: Option<Confidence>,
    status: Option<LearningStatus>,
    tags: Vec<String>,
    session_id: Option<String>,
    ticket_id: Option<String>,
    context_files: Option<Vec<String>>,
    criteria_met: Vec<WriteGateCriterion>,
    timestamp: Option<DateTime<Utc>>,
}

impl LearningBuilder {
    fn new(id: String) -> Self {
        Self {
            id,
            category: None,
            summary: None,
            detail: None,
            scope: None,
            confidence: None,
            status: None,
            tags: Vec::new(),
            session_id: None,
            ticket_id: None,
            context_files: None,
            criteria_met: Vec::new(),
            timestamp: None,
        }
    }

    fn with_detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }

    fn build(self) -> Result<CompoundLearning> {
        Ok(CompoundLearning {
            id: self.id,
            schema_version: 1,
            category: self.category.unwrap_or(LearningCategory::Pattern),
            summary: self.summary.unwrap_or_default(),
            detail: self.detail.unwrap_or_default(),
            scope: self.scope.unwrap_or(LearningScope::Project),
            confidence: self.confidence.unwrap_or(Confidence::Medium),
            criteria_met: self.criteria_met,
            tags: self.tags,
            session_id: self.session_id.unwrap_or_default(),
            ticket_id: self.ticket_id,
            timestamp: self.timestamp.unwrap_or_else(Utc::now),
            context_files: self.context_files,
            status: self.status.unwrap_or(LearningStatus::Active),
        })
    }
}

fn parse_category(value: &str) -> Option<LearningCategory> {
    match value.trim() {
        "Pattern" => Some(LearningCategory::Pattern),
        "Pitfall" => Some(LearningCategory::Pitfall),
        "Convention" => Some(LearningCategory::Convention),
        "Dependency" => Some(LearningCategory::Dependency),
        "Process" => Some(LearningCategory::Process),
        "Domain" => Some(LearningCategory::Domain),
        "Debugging" => Some(LearningCategory::Debugging),
        _ => None,
    }
}

fn parse_scope(value: &str) -> Option<LearningScope> {
    match value.trim() {
        "Project" => Some(LearningScope::Project),
        "Team" => Some(LearningScope::Team),
        "Personal" => Some(LearningScope::Personal),
        "Ephemeral" => Some(LearningScope::Ephemeral),
        _ => None,
    }
}

fn parse_confidence(value: &str) -> Option<Confidence> {
    match value.trim() {
        "High" => Some(Confidence::High),
        "Medium" => Some(Confidence::Medium),
        "Low" => Some(Confidence::Low),
        _ => None,
    }
}

fn parse_status(value: &str) -> Option<LearningStatus> {
    match value.trim() {
        "Active" => Some(LearningStatus::Active),
        "Archived" => Some(LearningStatus::Archived),
        "Superseded" => Some(LearningStatus::Superseded),
        _ => None,
    }
}

fn parse_tags(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .filter_map(|t| {
            let tag = t.trim_start_matches('#');
            if tag.is_empty() {
                None
            } else {
                Some(tag.to_string())
            }
        })
        .collect()
}

fn parse_context_files(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_criteria(value: &str) -> Vec<WriteGateCriterion> {
    value
        .split(',')
        .filter_map(|c| match c.trim() {
            "Behavior-Changing" | "BehaviorChanging" => Some(WriteGateCriterion::BehaviorChanging),
            "Decision-Rationale" | "DecisionRationale" => {
                Some(WriteGateCriterion::DecisionRationale)
            }
            "Stable-Fact" | "StableFact" => Some(WriteGateCriterion::StableFact),
            "Explicit-Request" | "ExplicitRequest" => Some(WriteGateCriterion::ExplicitRequest),
            _ => None,
        })
        .collect()
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| {
            // Try parsing without timezone
            chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%dT%H:%M:%SZ")
                .ok()
                .map(|ndt| ndt.and_utc())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_learning() -> CompoundLearning {
        CompoundLearning::new(
            LearningCategory::Pitfall,
            "Avoid N+1 queries in UserDashboard",
            "The dashboard was loading users then iterating to load posts separately. Use eager loading instead.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["performance".to_string(), "database".to_string()],
            "test-session-123",
        )
        .with_ticket_id("T001")
        .with_context_files(vec!["src/dashboard.rs".to_string()])
    }

    // Sanitization tests

    #[test]
    fn test_sanitize_summary_single_line() {
        let input = "Line 1\nLine 2\nLine 3";
        assert_eq!(sanitize_summary(input), "Line 1");
    }

    #[test]
    fn test_sanitize_summary_escapes_hash() {
        let input = "Use # for headings";
        assert_eq!(sanitize_summary(input), "Use \\# for headings");
    }

    #[test]
    fn test_sanitize_summary_escapes_pipe() {
        let input = "A | B | C";
        assert_eq!(sanitize_summary(input), "A \\| B \\| C");
    }

    #[test]
    fn test_sanitize_detail_balanced_fences() {
        let input = "```rust\ncode\n```";
        assert_eq!(sanitize_detail(input), input);
    }

    #[test]
    fn test_sanitize_detail_unbalanced_fences() {
        let input = "```rust\ncode";
        assert_eq!(sanitize_detail(input), "```rust\ncode\n```");
    }

    #[test]
    fn test_sanitize_tag_alphanumeric() {
        assert_eq!(sanitize_tag("rust-lang"), "rust-lang");
        assert_eq!(sanitize_tag("Rust_Lang!"), "rustlang");
        assert_eq!(sanitize_tag("UPPER"), "upper");
    }

    // Write tests

    #[test]
    fn test_write_creates_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let learning = sample_learning();
        let result = backend.write(&learning).unwrap();

        assert!(result.success);
        assert!(path.exists());
    }

    #[test]
    fn test_write_ephemeral_discards() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let mut learning = sample_learning();
        learning.scope = LearningScope::Ephemeral;

        let result = backend.write(&learning).unwrap();

        assert!(result.success);
        assert_eq!(result.location, "ephemeral");
        assert!(!path.exists());
    }

    #[test]
    fn test_write_personal_scope() {
        let temp = TempDir::new().unwrap();
        let project_path = temp.path().join(".grove").join("learnings.md");
        let personal_path = temp.path().join("personal-learnings.md");
        let backend = MarkdownBackend::with_paths(&project_path, &personal_path);

        let mut learning = sample_learning();
        learning.scope = LearningScope::Personal;

        let result = backend.write(&learning).unwrap();

        assert!(result.success);
        assert!(!project_path.exists());
        assert!(personal_path.exists());
    }

    #[test]
    fn test_write_appends() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let learning1 = sample_learning();
        let mut learning2 = sample_learning();
        learning2.summary = "Second learning".to_string();

        backend.write(&learning1).unwrap();
        backend.write(&learning2).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("Avoid N+1 queries"));
        assert!(content.contains("Second learning"));
    }

    // Parse tests

    #[test]
    fn test_parse_roundtrip() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let learning = sample_learning();
        backend.write(&learning).unwrap();

        let parsed = backend.parse_learnings().unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, learning.id);
        assert_eq!(parsed[0].summary, learning.summary);
        assert_eq!(parsed[0].category, learning.category);
        assert_eq!(parsed[0].scope, learning.scope);
        assert_eq!(parsed[0].status, LearningStatus::Active);
    }

    #[test]
    fn test_parse_empty_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let parsed = backend.parse_learnings().unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_parse_multiple_learnings() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        for i in 0..3 {
            let mut learning = sample_learning();
            learning.summary = format!("Learning {}", i);
            backend.write(&learning).unwrap();
        }

        let parsed = backend.parse_learnings().unwrap();
        assert_eq!(parsed.len(), 3);
    }

    // Search tests

    #[test]
    fn test_search_by_tag() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let learning = sample_learning();
        backend.write(&learning).unwrap();

        let query = SearchQuery::with_tags(vec!["performance".to_string()]);
        let results = backend.search(&query, &SearchFilters::default()).unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].relevance >= scores::TAG_EXACT);
    }

    #[test]
    fn test_search_by_partial_tag() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let learning = sample_learning();
        backend.write(&learning).unwrap();

        let query = SearchQuery::with_tags(vec!["perf".to_string()]);
        let results = backend.search(&query, &SearchFilters::default()).unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].relevance >= scores::TAG_PARTIAL);
    }

    #[test]
    fn test_search_by_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let learning = sample_learning();
        backend.write(&learning).unwrap();

        let query = SearchQuery::with_files(vec!["src/dashboard.rs".to_string()]);
        let results = backend.search(&query, &SearchFilters::default()).unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].relevance >= scores::FILE_OVERLAP);
    }

    #[test]
    fn test_search_by_keyword() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let learning = sample_learning();
        backend.write(&learning).unwrap();

        let query = SearchQuery::with_keywords(vec!["eager loading".to_string()]);
        let results = backend.search(&query, &SearchFilters::default()).unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].relevance >= scores::KEYWORD);
    }

    #[test]
    fn test_search_no_match() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let learning = sample_learning();
        backend.write(&learning).unwrap();

        let query = SearchQuery::with_tags(vec!["nonexistent".to_string()]);
        let results = backend.search(&query, &SearchFilters::default()).unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn test_search_respects_status_filter() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let learning = sample_learning();
        backend.write(&learning).unwrap();
        backend.archive(&learning.id).unwrap();

        // Default filter only returns active
        let query = SearchQuery::new();
        let results = backend.search(&query, &SearchFilters::default()).unwrap();
        assert!(results.is_empty());

        // All filter returns archived too
        let results = backend.search(&query, &SearchFilters::all()).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_max_results() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        for i in 0..5 {
            let mut learning = sample_learning();
            learning.summary = format!("Learning {}", i);
            backend.write(&learning).unwrap();
        }

        let query = SearchQuery::new();
        let filters = SearchFilters::default().max_results(2);
        let results = backend.search(&query, &filters).unwrap();

        assert_eq!(results.len(), 2);
    }

    // Archive tests

    #[test]
    fn test_archive_learning() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let learning = sample_learning();
        backend.write(&learning).unwrap();
        backend.archive(&learning.id).unwrap();

        let parsed = backend.parse_learnings().unwrap();
        assert_eq!(parsed[0].status, LearningStatus::Archived);
    }

    #[test]
    fn test_restore_learning() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let learning = sample_learning();
        backend.write(&learning).unwrap();
        backend.archive(&learning.id).unwrap();
        backend.restore(&learning.id).unwrap();

        let parsed = backend.parse_learnings().unwrap();
        assert_eq!(parsed[0].status, LearningStatus::Active);
    }

    #[test]
    fn test_archive_nonexistent() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let result = backend.archive("nonexistent");
        assert!(result.is_err());
    }

    // Ping test

    #[test]
    fn test_ping_success() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        assert!(backend.ping());
    }

    // File overlap tests

    #[test]
    fn test_files_overlap_exact() {
        assert!(files_overlap("src/lib.rs", "src/lib.rs"));
    }

    #[test]
    fn test_files_overlap_filename() {
        assert!(files_overlap("src/lib.rs", "lib.rs"));
    }

    #[test]
    fn test_files_overlap_suffix() {
        assert!(files_overlap("project/src/lib.rs", "src/lib.rs"));
    }

    #[test]
    fn test_files_no_overlap() {
        assert!(!files_overlap("src/lib.rs", "src/main.rs"));
    }

    // Combined query test

    #[test]
    fn test_search_combined_query() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let learning = sample_learning();
        backend.write(&learning).unwrap();

        let query = SearchQuery::new()
            .tags(vec!["performance".to_string()])
            .files(vec!["dashboard.rs".to_string()])
            .keywords(vec!["N+1".to_string()]);

        let results = backend.search(&query, &SearchFilters::default()).unwrap();

        assert_eq!(results.len(), 1);
        // Should have high relevance due to multiple matches
        assert!(results[0].relevance > 0.5);
    }

    #[test]
    fn test_name() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("learnings.md");
        let backend = MarkdownBackend::new(&path);
        assert_eq!(backend.name(), "markdown");
    }

    // next_id tests

    #[test]
    fn test_next_id_starts_at_000_for_empty_backend() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        let id = backend.next_id();

        // Should be cl_YYYYMMDD_000
        assert!(id.starts_with("cl_"));
        assert!(id.ends_with("_000"));
    }

    #[test]
    fn test_next_id_increments_after_write() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".grove").join("learnings.md");
        let backend = MarkdownBackend::new(&path);

        // Write a learning
        let learning = sample_learning();
        backend.write(&learning).unwrap();

        // Next ID should be _001
        let id = backend.next_id();
        assert!(id.ends_with("_001"), "Expected _001, got {}", id);
    }

    #[test]
    fn test_next_id_finds_highest_counter_in_file() {
        let temp = TempDir::new().unwrap();
        let grove_dir = temp.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();
        let path = grove_dir.join("learnings.md");

        // Create a learnings file with some existing entries
        let today = chrono::Utc::now().format("%Y%m%d").to_string();
        let content = format!(
            r#"# Grove Learnings

## cl_{}_002

**Category:** Pattern
**Summary:** First learning
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #test
**Session:** test
**Criteria:** Behavior-Changing
**Created:** 2026-02-10T10:00:00Z

Detail text.

---

## cl_{}_007

**Category:** Pattern
**Summary:** Second learning
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #test
**Session:** test
**Criteria:** Behavior-Changing
**Created:** 2026-02-10T11:00:00Z

Detail text.

---
"#,
            today, today
        );
        fs::write(&path, content).unwrap();

        let backend = MarkdownBackend::new(&path);

        // Next ID should be _008 (highest was 007)
        let id = backend.next_id();
        assert!(id.ends_with("_008"), "Expected _008, got {}", id);
    }
}
