//! Observe command for Grove.
//!
//! Appends a subagent observation to the current session.
//! Observations are notes from subagents that may inform the reflection.

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::core::{EventType, SessionState, SubagentObservation};
use crate::error::{FailOpen, Result};
use crate::storage::SessionStore;

/// Options for the observe command.
#[derive(Debug, Clone, Default)]
pub struct ObserveOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
}

/// Input for the observe command (JSON from stdin).
#[derive(Debug, Clone, Deserialize)]
pub struct ObserveInput {
    /// Session ID for this observation.
    pub session_id: String,
    /// The observation note.
    pub note: String,
}

/// Output format for the observe command.
#[derive(Debug, Clone, Serialize)]
pub struct ObserveOutput {
    /// Whether the observation was recorded.
    pub success: bool,
    /// The recorded note (truncated for display).
    pub note: String,
    /// Total observations in session.
    pub observation_count: usize,
    /// Error message if observation failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ObserveOutput {
    /// Create a successful output.
    pub fn success(note: impl Into<String>, observation_count: usize) -> Self {
        Self {
            success: true,
            note: note.into(),
            observation_count,
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            note: String::new(),
            observation_count: 0,
            error: Some(error.into()),
        }
    }
}

/// The observe command implementation.
pub struct ObserveCommand<S: SessionStore> {
    store: S,
    #[allow(dead_code)]
    config: Config,
}

impl<S: SessionStore> ObserveCommand<S> {
    /// Create a new observe command.
    pub fn new(store: S, config: Config) -> Self {
        Self { store, config }
    }

    /// Run the observe command with the given note.
    pub fn run(&self, session_id: &str, note: &str, _options: &ObserveOptions) -> ObserveOutput {
        // Validate note is not empty
        let trimmed_note = note.trim();
        if trimmed_note.is_empty() {
            return ObserveOutput::failure("Observation note cannot be empty");
        }

        // Load session (fail-open: create temporary if not found)
        let session_result: Result<Option<SessionState>> = self.store.get(session_id);
        let mut session = session_result
            .fail_open_with(
                "loading session",
                Some(SessionState::new(session_id, ".", ".")),
            )
            .unwrap_or_else(|| SessionState::new(session_id, ".", "."));

        // Create observation
        let observation = SubagentObservation::new(trimmed_note);

        // Add to session
        session.gate.subagent_observations.push(observation);
        let observation_count = session.gate.subagent_observations.len();

        // Add trace event
        session.add_trace(
            EventType::ObservationRecorded,
            Some(truncate(trimmed_note, 100)),
        );

        // Save session (fail-open)
        self.store.put(&session).fail_open_default("saving session");

        ObserveOutput::success(trimmed_note, observation_count)
    }

    /// Run the observe command with JSON input.
    pub fn run_with_input(&self, input: &ObserveInput, options: &ObserveOptions) -> ObserveOutput {
        self.run(&input.session_id, &input.note, options)
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &ObserveOutput, options: &ObserveOptions) -> String {
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
    fn format_human_readable(&self, output: &ObserveOutput) -> String {
        if output.success {
            format!(
                "Observation recorded ({} total).\nNote: {}\n",
                output.observation_count,
                truncate(&output.note, 80)
            )
        } else {
            format!(
                "Observation failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            )
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemorySessionStore;
    use std::sync::Arc;

    fn setup() -> Arc<MemorySessionStore> {
        Arc::new(MemorySessionStore::new())
    }

    #[test]
    fn test_observe_output_success() {
        let output = ObserveOutput::success("test note", 1);

        assert!(output.success);
        assert_eq!(output.note, "test note");
        assert_eq!(output.observation_count, 1);
        assert!(output.error.is_none());
    }

    #[test]
    fn test_observe_output_failure() {
        let output = ObserveOutput::failure("test error");

        assert!(!output.success);
        assert!(output.note.is_empty());
        assert_eq!(output.observation_count, 0);
        assert_eq!(output.error, Some("test error".to_string()));
    }

    #[test]
    fn test_observe_basic() {
        let store = setup();
        let config = Config::default();

        // Create initial session
        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = ObserveCommand::new(Arc::clone(&store), config);
        let options = ObserveOptions::default();

        let output = cmd.run(
            "test-session",
            "Found a pattern in auth middleware",
            &options,
        );

        assert!(output.success);
        assert_eq!(output.note, "Found a pattern in auth middleware");
        assert_eq!(output.observation_count, 1);

        // Check session was updated
        let updated = store.get("test-session").unwrap().unwrap();
        assert_eq!(updated.gate.subagent_observations.len(), 1);
        assert_eq!(
            updated.gate.subagent_observations[0].note,
            "Found a pattern in auth middleware"
        );
    }

    #[test]
    fn test_observe_multiple() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = ObserveCommand::new(Arc::clone(&store), config);
        let options = ObserveOptions::default();

        let output1 = cmd.run("test-session", "First observation", &options);
        assert!(output1.success);
        assert_eq!(output1.observation_count, 1);

        let output2 = cmd.run("test-session", "Second observation", &options);
        assert!(output2.success);
        assert_eq!(output2.observation_count, 2);

        let output3 = cmd.run("test-session", "Third observation", &options);
        assert!(output3.success);
        assert_eq!(output3.observation_count, 3);

        // Check all observations
        let updated = store.get("test-session").unwrap().unwrap();
        assert_eq!(updated.gate.subagent_observations.len(), 3);
        assert_eq!(
            updated.gate.subagent_observations[0].note,
            "First observation"
        );
        assert_eq!(
            updated.gate.subagent_observations[1].note,
            "Second observation"
        );
        assert_eq!(
            updated.gate.subagent_observations[2].note,
            "Third observation"
        );
    }

    #[test]
    fn test_observe_fails_with_empty_note() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = ObserveCommand::new(store, config);
        let options = ObserveOptions::default();

        let output = cmd.run("test-session", "", &options);

        assert!(!output.success);
        assert!(output.error.unwrap().contains("cannot be empty"));
    }

    #[test]
    fn test_observe_fails_with_whitespace_only_note() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = ObserveCommand::new(store, config);
        let options = ObserveOptions::default();

        let output = cmd.run("test-session", "   \n\t  ", &options);

        assert!(!output.success);
        assert!(output.error.unwrap().contains("cannot be empty"));
    }

    #[test]
    fn test_observe_trims_whitespace() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = ObserveCommand::new(Arc::clone(&store), config);
        let options = ObserveOptions::default();

        let output = cmd.run("test-session", "  trimmed note  ", &options);

        assert!(output.success);
        assert_eq!(output.note, "trimmed note");

        let updated = store.get("test-session").unwrap().unwrap();
        assert_eq!(updated.gate.subagent_observations[0].note, "trimmed note");
    }

    #[test]
    fn test_observe_creates_session_if_not_found() {
        let store = setup();
        let config = Config::default();

        // Don't create session first
        let cmd = ObserveCommand::new(Arc::clone(&store), config);
        let options = ObserveOptions::default();

        let output = cmd.run("new-session", "observation for new session", &options);

        // Should succeed with fail-open
        assert!(output.success);
        assert_eq!(output.observation_count, 1);
    }

    #[test]
    fn test_observe_with_input() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = ObserveCommand::new(Arc::clone(&store), config);

        let input = ObserveInput {
            session_id: "test-session".to_string(),
            note: "from json input".to_string(),
        };

        let options = ObserveOptions::default();
        let output = cmd.run_with_input(&input, &options);

        assert!(output.success);
        assert_eq!(output.note, "from json input");
    }

    #[test]
    fn test_format_output_json() {
        let store = setup();
        let config = Config::default();
        let cmd = ObserveCommand::new(store, config);

        let output = ObserveOutput::success("test note", 1);
        let options = ObserveOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"note\": \"test note\""));
    }

    #[test]
    fn test_format_output_quiet() {
        let store = setup();
        let config = Config::default();
        let cmd = ObserveCommand::new(store, config);

        let output = ObserveOutput::success("test note", 1);
        let options = ObserveOptions {
            quiet: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_output_human_readable() {
        let store = setup();
        let config = Config::default();
        let cmd = ObserveCommand::new(store, config);

        let output = ObserveOutput::success("test observation note", 3);
        let options = ObserveOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Observation recorded (3 total)"));
        assert!(formatted.contains("test observation note"));
    }

    #[test]
    fn test_observe_adds_trace_event() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = ObserveCommand::new(Arc::clone(&store), config);
        let options = ObserveOptions::default();

        cmd.run("test-session", "trace test observation", &options);

        let updated = store.get("test-session").unwrap().unwrap();
        let obs_events: Vec<_> = updated
            .trace
            .iter()
            .filter(|t| t.event_type == EventType::ObservationRecorded)
            .collect();

        assert_eq!(obs_events.len(), 1);
        assert!(obs_events[0]
            .details
            .as_ref()
            .unwrap()
            .contains("trace test observation"));
    }

    #[test]
    fn test_truncate_ascii() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this is a very long string", 10), "this is...");
        assert_eq!(truncate("exactly10!", 10), "exactly10!");
    }

    #[test]
    fn test_truncate_unicode() {
        // Japanese text (3 bytes per char) - should not panic
        let japanese = "日本語テスト";
        assert_eq!(japanese.chars().count(), 6);
        assert_eq!(truncate(japanese, 5), "日本...");

        // Emoji (4 bytes per char) - should not panic
        let emoji = "🎉🎊🎁🎈🎂";
        assert_eq!(truncate(emoji, 4), "🎉...");

        // Mixed ASCII and multi-byte
        let mixed = "Hello 世界!";
        assert_eq!(truncate(mixed, 8), "Hello...");
    }

    #[test]
    fn test_observe_with_long_note() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = ObserveCommand::new(Arc::clone(&store), config);
        let options = ObserveOptions::default();

        let long_note = "x".repeat(1000);
        let output = cmd.run("test-session", &long_note, &options);

        assert!(output.success);
        assert_eq!(output.note.len(), 1000);

        // Check full note is stored
        let updated = store.get("test-session").unwrap().unwrap();
        assert_eq!(updated.gate.subagent_observations[0].note.len(), 1000);
    }
}
