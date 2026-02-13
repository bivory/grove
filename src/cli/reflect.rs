//! Reflect command for Grove.
//!
//! Reads reflection output from stdin, validates against schema and write gate,
//! checks for near-duplicates, writes to backend, logs stats, and updates gate state.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::backends::{MemoryBackend, SearchFilters, SearchQuery};
use crate::config::{project_stats_log_path, Config};
use crate::core::{
    validate_with_duplicates, CandidateLearning, EventType, GateStatus, ReflectionResult,
    RejectedCandidate, SessionState,
};
use crate::error::{FailOpen, Result};
use crate::stats::StatsLogger;
use crate::storage::SessionStore;

/// Options for the reflect command.
#[derive(Debug, Clone, Default)]
pub struct ReflectOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Session ID to use (defaults to current session from context).
    pub session_id: Option<String>,
}

/// A reference to a learning that was used during the session.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LearningReference {
    /// The learning ID that was referenced.
    pub id: String,
    /// Optional description of how the learning was applied.
    #[serde(default)]
    pub how: Option<String>,
}

/// Input format for reflection (JSON from stdin).
#[derive(Debug, Clone, Deserialize)]
pub struct ReflectInput {
    /// Session ID for this reflection.
    pub session_id: String,
    /// Candidate learnings produced by Claude.
    pub candidates: Vec<CandidateLearning>,
    /// Explicit list of learnings that were used during this session.
    /// Takes precedence over pattern-based detection.
    #[serde(default)]
    pub learnings_used: Option<Vec<LearningReference>>,
    /// Free-form reflection notes that may mention applied learnings.
    #[serde(default)]
    pub reflection_notes: Option<String>,
}

/// Output format for the reflect command.
#[derive(Debug, Clone, Serialize)]
pub struct ReflectOutput {
    /// Whether the reflection was successful.
    pub success: bool,
    /// Number of candidates submitted.
    pub candidates_submitted: usize,
    /// Number of learnings accepted and written.
    pub learnings_accepted: usize,
    /// IDs of accepted learnings.
    pub learning_ids: Vec<String>,
    /// Candidates that were rejected with reasons.
    pub rejected: Vec<RejectionInfo>,
    /// Error message if reflection failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Information about a rejected candidate.
#[derive(Debug, Clone, Serialize)]
pub struct RejectionInfo {
    /// Summary of the rejected learning.
    pub summary: String,
    /// Reason for rejection.
    pub reason: String,
    /// Stage at which rejection occurred.
    pub stage: String,
}

impl From<&RejectedCandidate> for RejectionInfo {
    fn from(rejected: &RejectedCandidate) -> Self {
        Self {
            summary: rejected.summary.clone(),
            reason: rejected.rejection_reason.clone(),
            stage: rejected.stage.to_string(),
        }
    }
}

impl ReflectOutput {
    /// Create a successful output.
    ///
    /// Note: `learnings_accepted` must equal `learning_ids.len()` as both represent
    /// the count of learnings that were successfully written. This invariant is
    /// enforced in debug builds.
    pub fn success(
        candidates_submitted: usize,
        learnings_accepted: usize,
        learning_ids: Vec<String>,
        rejected: Vec<RejectedCandidate>,
    ) -> Self {
        debug_assert_eq!(
            learnings_accepted,
            learning_ids.len(),
            "learnings_accepted ({}) must equal learning_ids.len() ({})",
            learnings_accepted,
            learning_ids.len()
        );
        Self {
            success: true,
            candidates_submitted,
            learnings_accepted,
            learning_ids,
            rejected: rejected.iter().map(RejectionInfo::from).collect(),
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            candidates_submitted: 0,
            learnings_accepted: 0,
            learning_ids: Vec::new(),
            rejected: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// The reflect command implementation.
pub struct ReflectCommand<S: SessionStore, B: MemoryBackend> {
    store: S,
    backend: B,
    #[allow(dead_code)]
    config: Config,
}

impl<S: SessionStore, B: MemoryBackend> ReflectCommand<S, B> {
    /// Create a new reflect command.
    pub fn new(store: S, backend: B, config: Config) -> Self {
        Self {
            store,
            backend,
            config,
        }
    }

    /// Run the reflect command, reading input from stdin.
    pub fn run(&self, options: &ReflectOptions) -> ReflectOutput {
        // Read input from stdin
        let input = match self.read_stdin() {
            Ok(input) => input,
            Err(e) => return ReflectOutput::failure(format!("Failed to read stdin: {}", e)),
        };

        self.run_with_input(&input, options)
    }

    /// Run the reflect command with provided input.
    pub fn run_with_input(&self, input: &ReflectInput, options: &ReflectOptions) -> ReflectOutput {
        let session_id = options
            .session_id
            .clone()
            .unwrap_or_else(|| input.session_id.clone());

        // Load session (fail-open: create temporary if not found)
        let session_result: Result<Option<SessionState>> = self.store.get(&session_id);
        let mut session = session_result
            .fail_open_with(
                "loading session",
                Some(SessionState::new_fallback(&session_id)),
            )
            .unwrap_or_else(|| SessionState::new_fallback(&session_id));

        // Get existing learnings for duplicate check
        let existing = self
            .backend
            .search(&SearchQuery::new(), &SearchFilters::active_only())
            .fail_open_default("searching existing learnings")
            .into_iter()
            .map(|r| r.learning)
            .collect::<Vec<_>>();

        // Validate candidates (schema + write gate + duplicate check)
        let (mut valid_learnings, rejected) =
            validate_with_duplicates(input.candidates.clone(), &session_id, &existing);

        let candidates_submitted = input.candidates.len();
        let learnings_accepted = valid_learnings.len();

        // Assign unique IDs from backend atomically (prevents race within batch)
        let ids = self.backend.next_ids(valid_learnings.len());
        for (learning, id) in valid_learnings.iter_mut().zip(ids) {
            learning.id = id;
        }

        // Write valid learnings to backend
        let mut learning_ids = Vec::new();
        let mut categories = Vec::new();

        for learning in &valid_learnings {
            let write_result = self.backend.write(learning).fail_open_with(
                "writing learning",
                crate::backends::WriteResult::failure(&learning.id, "backend write failed"),
            );

            if write_result.success {
                learning_ids.push(learning.id.clone());
                categories.push(learning.category);
            }
        }

        // Log reflection stats event
        let stats_path = project_stats_log_path(Path::new(&session.cwd));
        let stats_logger = StatsLogger::new(&stats_path);

        let ticket_id = session.gate.ticket.as_ref().map(|t| t.ticket_id.clone());

        stats_logger
            .append_reflection(
                &session_id,
                candidates_submitted as u32,
                learning_ids.len() as u32,
                categories,
                ticket_id.clone(),
                self.backend.name(),
            )
            .fail_open_default("logging reflection stats");

        // Detect and log referenced learnings
        let injected_ids: Vec<String> = session
            .gate
            .injected_learnings
            .iter()
            .map(|il| il.learning_id.clone())
            .collect();

        let referenced_ids = detect_referenced_learnings(input, &injected_ids);

        for ref_id in &referenced_ids {
            // Log referenced event
            stats_logger
                .append_referenced(ref_id, &session_id, ticket_id.clone())
                .fail_open_default("logging referenced event");

            // Mark learning as referenced in session state
            if let Some(il) = session
                .gate
                .injected_learnings
                .iter_mut()
                .find(|il| il.learning_id == *ref_id)
            {
                il.mark_referenced();
            }

            // Add trace event
            session.add_trace(
                EventType::LearningReferenced,
                Some(format!("Learning {} was referenced", ref_id)),
            );
        }

        // Update session state
        session.gate.reflection = Some(ReflectionResult::with_rejected(
            learning_ids.clone(),
            rejected.clone(),
            candidates_submitted as u32,
            learning_ids.len() as u32,
        ));
        session.gate.status = GateStatus::Reflected;
        session.add_trace(
            EventType::ReflectionComplete,
            Some(format!(
                "accepted {}/{} candidates (validated: {}, written: {})",
                learning_ids.len(),
                candidates_submitted,
                learnings_accepted,
                learning_ids.len()
            )),
        );

        // Save session (fail-open)
        self.store.put(&session).fail_open_default("saving session");

        // learnings_accepted should be the number actually written, not just validated
        ReflectOutput::success(
            candidates_submitted,
            learning_ids.len(),
            learning_ids,
            rejected,
        )
    }

    /// Read reflection input from stdin.
    fn read_stdin(&self) -> Result<ReflectInput> {
        use std::io::Read;

        let stdin = io::stdin();
        let mut input = String::new();

        stdin
            .lock()
            .read_to_string(&mut input)
            .map_err(|e| crate::error::GroveError::storage("stdin", e))?;

        if input.trim().is_empty() {
            return Err(crate::error::GroveError::serde(
                "No input provided on stdin".to_string(),
            ));
        }

        serde_json::from_str(&input)
            .map_err(|e| crate::error::GroveError::serde(format!("Invalid JSON input: {}", e)))
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &ReflectOutput, options: &ReflectOptions) -> String {
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
    fn format_human_readable(&self, output: &ReflectOutput) -> String {
        let mut result = String::new();

        if output.success {
            result.push_str(&format!(
                "Reflection complete: {}/{} learnings accepted\n",
                output.learnings_accepted, output.candidates_submitted
            ));

            if !output.learning_ids.is_empty() {
                result.push_str("\nAccepted:\n");
                for id in &output.learning_ids {
                    result.push_str(&format!("  - {}\n", id));
                }
            }

            if !output.rejected.is_empty() {
                result.push_str("\nRejected:\n");
                for r in &output.rejected {
                    result.push_str(&format!(
                        "  - [{}] {}: {}\n",
                        r.stage,
                        truncate(&r.summary, 40),
                        r.reason
                    ));
                }
            }
        } else {
            result.push_str(&format!(
                "Reflection failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            ));
        }

        result
    }
}

/// Truncate a string with ellipsis, handling Unicode correctly.
///
/// This function counts characters (not bytes) to avoid panicking on
/// multi-byte UTF-8 sequences like emojis or non-ASCII text.
fn truncate(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else {
        let truncate_at = max_len.saturating_sub(3);
        let truncated: String = s.chars().take(truncate_at).collect();
        format!("{}...", truncated)
    }
}

/// Detect learning references in reflection input.
///
/// Uses a three-tier detection strategy:
/// 1. Explicit `learnings_used` field (highest priority)
/// 2. Generous pattern matching for phrases like "applied learning", "used learning", etc.
/// 3. Bare learning ID mentions as fallback
///
/// Returns a deduplicated list of referenced learning IDs.
pub fn detect_referenced_learnings(input: &ReflectInput, injected_ids: &[String]) -> Vec<String> {
    use std::collections::HashSet;

    let mut referenced: HashSet<String> = HashSet::new();

    // 1. Explicit learnings_used field takes precedence
    if let Some(refs) = &input.learnings_used {
        for r in refs {
            if injected_ids.contains(&r.id) {
                referenced.insert(r.id.clone());
            }
        }
    }

    // Build text corpus for pattern matching
    let mut text_corpus = String::new();

    // Add reflection notes
    if let Some(notes) = &input.reflection_notes {
        text_corpus.push_str(notes);
        text_corpus.push('\n');
    }

    // Add candidate details (might mention applied learnings)
    for candidate in &input.candidates {
        text_corpus.push_str(&candidate.detail);
        text_corpus.push('\n');
    }

    let text_lower = text_corpus.to_lowercase();

    // 2. Generous pattern matching for each injected learning ID
    for id in injected_ids {
        if referenced.contains(id) {
            continue; // Already found via explicit field
        }

        let id_lower = id.to_lowercase();

        // Patterns to match (case-insensitive):
        // - "applied learning cl_001"
        // - "used learning cl_001"
        // - "referenced cl_001"
        // - "leveraged learning cl_001"
        // - "based on cl_001"
        // - "following cl_001"
        // - "following cl_001's guidance"
        let patterns = [
            format!("applied learning {}", id_lower),
            format!("applied {}", id_lower),
            format!("used learning {}", id_lower),
            format!("used {}", id_lower),
            format!("referenced learning {}", id_lower),
            format!("referenced {}", id_lower),
            format!("leveraged learning {}", id_lower),
            format!("leveraged {}", id_lower),
            format!("based on {}", id_lower),
            format!("following {}", id_lower),
        ];

        for pattern in &patterns {
            if text_lower.contains(pattern) {
                referenced.insert(id.clone());
                break;
            }
        }
    }

    // 3. Bare ID mention fallback
    for id in injected_ids {
        if referenced.contains(id) {
            continue;
        }

        // Check if the ID appears anywhere in the text (case-insensitive)
        if text_lower.contains(&id.to_lowercase()) {
            referenced.insert(id.clone());
        }
    }

    referenced.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::MarkdownBackend;
    use crate::storage::MemorySessionStore;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Arc<MemorySessionStore>, MarkdownBackend) {
        let temp = TempDir::new().unwrap();
        let learnings_path = temp.path().join(".grove").join("learnings.md");
        std::fs::create_dir_all(learnings_path.parent().unwrap()).unwrap();

        let store = Arc::new(MemorySessionStore::new());
        let backend = MarkdownBackend::new(&learnings_path);

        (temp, store, backend)
    }

    fn valid_candidate() -> CandidateLearning {
        CandidateLearning {
            category: "pattern".to_string(),
            summary: "Use async/await for I/O operations".to_string(),
            detail: "When performing I/O operations, always use async/await to avoid blocking. This improves responsiveness significantly.".to_string(),
            scope: "project".to_string(),
            confidence: "high".to_string(),
            criteria_met: vec!["behavior_changing".to_string()],
            tags: vec!["async".to_string(), "io".to_string()],
            context_files: None,
        }
    }

    #[test]
    fn test_reflect_output_success() {
        // Note: learnings_accepted must match learning_ids.len()
        let output = ReflectOutput::success(5, 2, vec!["L1".to_string(), "L2".to_string()], vec![]);

        assert!(output.success);
        assert_eq!(output.candidates_submitted, 5);
        assert_eq!(output.learnings_accepted, 2);
        assert_eq!(output.learning_ids.len(), 2);
        assert!(output.rejected.is_empty());
        assert!(output.error.is_none());
    }

    #[test]
    fn test_reflect_output_failure() {
        let output = ReflectOutput::failure("test error");

        assert!(!output.success);
        assert_eq!(output.candidates_submitted, 0);
        assert_eq!(output.learnings_accepted, 0);
        assert!(output.learning_ids.is_empty());
        assert_eq!(output.error, Some("test error".to_string()));
    }

    #[test]
    fn test_reflect_output_invariant_learnings_accepted_equals_ids_len() {
        // This test documents the invariant: learnings_accepted must equal learning_ids.len()
        // Both represent the count of learnings that were successfully written to backend.

        // Empty case
        let output = ReflectOutput::success(0, 0, vec![], vec![]);
        assert_eq!(output.learnings_accepted, output.learning_ids.len());

        // Some accepted, some rejected (candidates_submitted > learnings_accepted)
        let output = ReflectOutput::success(5, 2, vec!["L1".to_string(), "L2".to_string()], vec![]);
        assert_eq!(output.learnings_accepted, output.learning_ids.len());

        // All accepted
        let output = ReflectOutput::success(
            3,
            3,
            vec!["L1".to_string(), "L2".to_string(), "L3".to_string()],
            vec![],
        );
        assert_eq!(output.learnings_accepted, output.learning_ids.len());
    }

    #[test]
    fn test_reflect_with_valid_candidates() {
        let (_temp, store, backend) = setup();
        let config = Config::default();
        let cmd = ReflectCommand::new(store, backend, config);

        let input = ReflectInput {
            session_id: "test-session".to_string(),
            candidates: vec![valid_candidate()],
            learnings_used: None,
            reflection_notes: None,
        };

        let options = ReflectOptions::default();
        let output = cmd.run_with_input(&input, &options);

        assert!(output.success);
        assert_eq!(output.candidates_submitted, 1);
        assert_eq!(output.learnings_accepted, 1);
        assert_eq!(output.learning_ids.len(), 1);
        assert!(output.rejected.is_empty());
    }

    #[test]
    fn test_reflect_with_invalid_candidate() {
        let (_temp, store, backend) = setup();
        let config = Config::default();
        let cmd = ReflectCommand::new(store, backend, config);

        let mut invalid = valid_candidate();
        invalid.category = "invalid_category".to_string();

        let input = ReflectInput {
            session_id: "test-session".to_string(),
            candidates: vec![invalid],
            learnings_used: None,
            reflection_notes: None,
        };

        let options = ReflectOptions::default();
        let output = cmd.run_with_input(&input, &options);

        assert!(output.success);
        assert_eq!(output.candidates_submitted, 1);
        assert_eq!(output.learnings_accepted, 0);
        assert!(output.learning_ids.is_empty());
        assert_eq!(output.rejected.len(), 1);
        assert_eq!(output.rejected[0].stage, "schema");
    }

    #[test]
    fn test_reflect_with_mixed_candidates() {
        let (_temp, store, backend) = setup();
        let config = Config::default();
        let cmd = ReflectCommand::new(store, backend, config);

        let mut invalid = valid_candidate();
        invalid.category = "invalid_category".to_string();
        invalid.summary = "Invalid candidate summary text".to_string();

        let input = ReflectInput {
            session_id: "test-session".to_string(),
            candidates: vec![valid_candidate(), invalid],
            learnings_used: None,
            reflection_notes: None,
        };

        let options = ReflectOptions::default();
        let output = cmd.run_with_input(&input, &options);

        assert!(output.success);
        assert_eq!(output.candidates_submitted, 2);
        assert_eq!(output.learnings_accepted, 1);
        assert_eq!(output.rejected.len(), 1);
    }

    #[test]
    fn test_reflect_updates_session_state() {
        let (_temp, store, backend) = setup();
        let config = Config::default();

        // Create initial session
        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = ReflectCommand::new(Arc::clone(&store), backend, config);

        let input = ReflectInput {
            session_id: "test-session".to_string(),
            candidates: vec![valid_candidate()],
            learnings_used: None,
            reflection_notes: None,
        };

        let options = ReflectOptions::default();
        let output = cmd.run_with_input(&input, &options);

        assert!(output.success);

        // Check session was updated
        let updated = store.get("test-session").unwrap().unwrap();
        assert_eq!(updated.gate.status, GateStatus::Reflected);
        assert!(updated.gate.reflection.is_some());

        let reflection = updated.gate.reflection.unwrap();
        assert_eq!(reflection.learning_ids.len(), 1);
        assert_eq!(reflection.candidates_produced, 1);
        assert_eq!(reflection.candidates_accepted, 1);
    }

    #[test]
    fn test_reflect_detects_duplicates() {
        let (temp, store, backend) = setup();
        let config = Config::default();

        // First, write a learning to the backend
        let cmd = ReflectCommand::new(Arc::clone(&store), backend.clone(), config.clone());

        let input = ReflectInput {
            session_id: "session-1".to_string(),
            candidates: vec![valid_candidate()],
            learnings_used: None,
            reflection_notes: None,
        };

        let options = ReflectOptions::default();
        let output = cmd.run_with_input(&input, &options);
        assert!(output.success);
        assert_eq!(output.learnings_accepted, 1);

        // Create new backend from same path to reload learnings
        let learnings_path = temp.path().join(".grove").join("learnings.md");
        let backend2 = MarkdownBackend::new(&learnings_path);
        let cmd2 = ReflectCommand::new(store, backend2, config);

        // Now try to add the same learning again
        let input2 = ReflectInput {
            session_id: "session-2".to_string(),
            candidates: vec![valid_candidate()],
            learnings_used: None,
            reflection_notes: None,
        };

        let output2 = cmd2.run_with_input(&input2, &options);

        assert!(output2.success);
        assert_eq!(output2.candidates_submitted, 1);
        assert_eq!(output2.learnings_accepted, 0);
        assert_eq!(output2.rejected.len(), 1);
        assert_eq!(output2.rejected[0].stage, "duplicate");
    }

    #[test]
    fn test_reflect_with_empty_candidates() {
        let (_temp, store, backend) = setup();
        let config = Config::default();
        let cmd = ReflectCommand::new(store, backend, config);

        let input = ReflectInput {
            session_id: "test-session".to_string(),
            candidates: vec![],
            learnings_used: None,
            reflection_notes: None,
        };

        let options = ReflectOptions::default();
        let output = cmd.run_with_input(&input, &options);

        assert!(output.success);
        assert_eq!(output.candidates_submitted, 0);
        assert_eq!(output.learnings_accepted, 0);
        assert!(output.learning_ids.is_empty());
        assert!(output.rejected.is_empty());
    }

    #[test]
    fn test_format_output_json() {
        let (_temp, store, backend) = setup();
        let config = Config::default();
        let cmd = ReflectCommand::new(store, backend, config);

        let output = ReflectOutput::success(2, 1, vec!["L1".to_string()], vec![]);
        let options = ReflectOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"learnings_accepted\": 1"));
    }

    #[test]
    fn test_format_output_quiet() {
        let (_temp, store, backend) = setup();
        let config = Config::default();
        let cmd = ReflectCommand::new(store, backend, config);

        let output = ReflectOutput::success(2, 1, vec!["L1".to_string()], vec![]);
        let options = ReflectOptions {
            quiet: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_output_human_readable() {
        let (_temp, store, backend) = setup();
        let config = Config::default();
        let cmd = ReflectCommand::new(store, backend, config);

        let output = ReflectOutput::success(2, 1, vec!["L1".to_string()], vec![]);
        let options = ReflectOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Reflection complete: 1/2 learnings accepted"));
        assert!(formatted.contains("L1"));
    }

    #[test]
    fn test_truncate_ascii() {
        // String shorter than max_len
        assert_eq!(truncate("short", 10), "short");
        // String exactly at max_len
        assert_eq!(truncate("exactly10!", 10), "exactly10!");
        // String longer than max_len
        assert_eq!(truncate("this is a very long string", 10), "this is...");
        // Edge case: max_len of 3 leaves no room for content
        assert_eq!(truncate("hello", 3), "...");
        // Edge case: max_len of 4 leaves room for 1 char
        assert_eq!(truncate("hello", 4), "h...");
        // Empty string
        assert_eq!(truncate("", 10), "");
    }

    #[test]
    fn test_truncate_unicode_no_panic() {
        // Japanese text (3 bytes per char in UTF-8)
        let japanese = "日本語テスト";
        assert_eq!(japanese.chars().count(), 6);
        // Should not panic - truncate at 5 chars
        let result = truncate(japanese, 5);
        assert_eq!(result, "日本...");

        // Emoji (4 bytes per char in UTF-8)
        let emoji = "🎉🎊🎁🎈🎂";
        assert_eq!(emoji.chars().count(), 5);
        // Truncate at 4 chars
        let result = truncate(emoji, 4);
        assert_eq!(result, "🎉...");

        // Mixed ASCII and Unicode
        let mixed = "Hello 世界!";
        assert_eq!(mixed.chars().count(), 9);
        let result = truncate(mixed, 8);
        assert_eq!(result, "Hello...");
    }

    #[test]
    fn test_truncate_unicode_boundary() {
        // This string has multi-byte chars that would panic with byte slicing
        // 日 = 3 bytes, so at byte position 7 we'd be mid-character
        let text = "ab日本語cd";
        assert_eq!(text.len(), 13); // 2 + 9 + 2 bytes
        assert_eq!(text.chars().count(), 7); // 7 characters

        // Truncate at 6 chars - should work without panic
        let result = truncate(text, 6);
        assert_eq!(result, "ab日...");

        // Verify old byte-based logic would have panicked
        // (This is documentation of the bug we fixed)
        // &text[..6.saturating_sub(3)] = &text[..3] = "ab" + partial 日 = PANIC
    }

    #[test]
    fn test_truncate_combining_characters() {
        // é as e + combining accent (2 code points, 1 grapheme)
        let combining = "cafe\u{0301}"; // café with combining accent
                                        // .chars().count() = 5 (c, a, f, e, combining_accent)
        assert_eq!(combining.chars().count(), 5);

        // At max_len=5, no truncation (5 <= 5)
        assert_eq!(truncate(combining, 5), "cafe\u{0301}");

        // At max_len=4, truncate to 1 char + "..."
        // Note: .chars().count() counts code points, not graphemes
        // This splits the combining character from 'e', which is suboptimal
        // but acceptable - perfect grapheme handling requires unicode-segmentation
        let result = truncate(combining, 4);
        assert_eq!(result, "c...");
    }

    #[test]
    fn test_rejection_info_from_rejected_candidate() {
        use crate::core::{RejectedCandidate, ValidationStage};

        let rejected =
            RejectedCandidate::new("test summary", "invalid category", ValidationStage::Schema);
        let info = RejectionInfo::from(&rejected);

        assert_eq!(info.summary, "test summary");
        assert_eq!(info.reason, "invalid category");
        assert_eq!(info.stage, "schema");
    }

    #[test]
    fn test_detect_referenced_learnings_explicit_field() {
        let input = ReflectInput {
            session_id: "test".to_string(),
            candidates: vec![],
            learnings_used: Some(vec![LearningReference {
                id: "cl_001".to_string(),
                how: Some("Used for retry logic".to_string()),
            }]),
            reflection_notes: None,
        };

        let injected = vec!["cl_001".to_string(), "cl_002".to_string()];
        let referenced = detect_referenced_learnings(&input, &injected);

        assert_eq!(referenced.len(), 1);
        assert!(referenced.contains(&"cl_001".to_string()));
    }

    #[test]
    fn test_detect_referenced_learnings_pattern_matching() {
        let input = ReflectInput {
            session_id: "test".to_string(),
            candidates: vec![],
            learnings_used: None,
            reflection_notes: Some("I applied learning cl_001 for the API calls and used learning cl_002 for error handling.".to_string()),
        };

        let injected = vec![
            "cl_001".to_string(),
            "cl_002".to_string(),
            "cl_003".to_string(),
        ];
        let referenced = detect_referenced_learnings(&input, &injected);

        assert_eq!(referenced.len(), 2);
        assert!(referenced.contains(&"cl_001".to_string()));
        assert!(referenced.contains(&"cl_002".to_string()));
        assert!(!referenced.contains(&"cl_003".to_string()));
    }

    #[test]
    fn test_detect_referenced_learnings_bare_id() {
        let input = ReflectInput {
            session_id: "test".to_string(),
            candidates: vec![],
            learnings_used: None,
            reflection_notes: Some("The fix was based on cl_001 approach.".to_string()),
        };

        let injected = vec!["cl_001".to_string()];
        let referenced = detect_referenced_learnings(&input, &injected);

        assert_eq!(referenced.len(), 1);
        assert!(referenced.contains(&"cl_001".to_string()));
    }

    #[test]
    fn test_detect_referenced_learnings_in_candidate_detail() {
        let mut candidate = valid_candidate();
        candidate.detail = "Following cl_001's guidance, I implemented retry logic.".to_string();

        let input = ReflectInput {
            session_id: "test".to_string(),
            candidates: vec![candidate],
            learnings_used: None,
            reflection_notes: None,
        };

        let injected = vec!["cl_001".to_string()];
        let referenced = detect_referenced_learnings(&input, &injected);

        assert_eq!(referenced.len(), 1);
        assert!(referenced.contains(&"cl_001".to_string()));
    }

    #[test]
    fn test_detect_referenced_learnings_case_insensitive() {
        let input = ReflectInput {
            session_id: "test".to_string(),
            candidates: vec![],
            learnings_used: None,
            reflection_notes: Some("Applied Learning CL_001 for the fix.".to_string()),
        };

        let injected = vec!["cl_001".to_string()];
        let referenced = detect_referenced_learnings(&input, &injected);

        assert_eq!(referenced.len(), 1);
    }

    #[test]
    fn test_detect_referenced_learnings_no_false_positives() {
        let input = ReflectInput {
            session_id: "test".to_string(),
            candidates: vec![],
            learnings_used: None,
            reflection_notes: Some("I did not use any previous learnings.".to_string()),
        };

        let injected = vec!["cl_001".to_string(), "cl_002".to_string()];
        let referenced = detect_referenced_learnings(&input, &injected);

        assert!(referenced.is_empty());
    }

    #[test]
    fn test_detect_referenced_learnings_ignores_non_injected() {
        let input = ReflectInput {
            session_id: "test".to_string(),
            candidates: vec![],
            learnings_used: Some(vec![LearningReference {
                id: "cl_999".to_string(), // Not in injected list
                how: None,
            }]),
            reflection_notes: None,
        };

        let injected = vec!["cl_001".to_string()];
        let referenced = detect_referenced_learnings(&input, &injected);

        assert!(referenced.is_empty());
    }
}
