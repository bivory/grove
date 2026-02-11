//! Skip command for Grove.
//!
//! Records a skip decision and sets the gate to Skipped status,
//! allowing session exit without reflection.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::{project_stats_log_path, Config};
use crate::core::{EventType, GateStatus, SessionState, SkipDecider, SkipDecision};
use crate::error::{FailOpen, Result};
use crate::stats::StatsLogger;
use crate::storage::SessionStore;

/// Options for the skip command.
#[derive(Debug, Clone, Default)]
pub struct SkipOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Who decided to skip (default: User).
    pub decider: Option<SkipDecider>,
    /// Number of lines changed (for stats tracking).
    pub lines_changed: Option<u32>,
}

/// Input for the skip command.
#[derive(Debug, Clone, Deserialize)]
pub struct SkipInput {
    /// Session ID for this skip.
    pub session_id: String,
    /// Reason for skipping reflection.
    pub reason: String,
    /// Who decided to skip.
    #[serde(default)]
    pub decider: Option<SkipDecider>,
    /// Lines changed in the session.
    #[serde(default)]
    pub lines_changed: Option<u32>,
}

/// Output format for the skip command.
#[derive(Debug, Clone, Serialize)]
pub struct SkipOutput {
    /// Whether the skip was successful.
    pub success: bool,
    /// The reason for skipping.
    pub reason: String,
    /// Who decided to skip.
    pub decider: String,
    /// Error message if skip failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SkipOutput {
    /// Create a successful output.
    pub fn success(reason: impl Into<String>, decider: SkipDecider) -> Self {
        Self {
            success: true,
            reason: reason.into(),
            decider: format!("{:?}", decider).to_lowercase(),
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            reason: String::new(),
            decider: String::new(),
            error: Some(error.into()),
        }
    }
}

/// The skip command implementation.
pub struct SkipCommand<S: SessionStore> {
    store: S,
    #[allow(dead_code)]
    config: Config,
}

impl<S: SessionStore> SkipCommand<S> {
    /// Create a new skip command.
    pub fn new(store: S, config: Config) -> Self {
        Self { store, config }
    }

    /// Run the skip command with the given reason.
    pub fn run(&self, session_id: &str, reason: &str, options: &SkipOptions) -> SkipOutput {
        let decider = options.decider.unwrap_or(SkipDecider::User);
        let lines_changed = options.lines_changed.unwrap_or(0);

        // Load session (fail-open: create temporary if not found)
        let session_result: Result<Option<SessionState>> = self.store.get(session_id);
        let mut session = session_result
            .fail_open_with(
                "loading session",
                Some(SessionState::new(session_id, ".", ".")),
            )
            .unwrap_or_else(|| SessionState::new(session_id, ".", "."));

        // Check if already in terminal state
        if session.gate.status.is_terminal() {
            return SkipOutput::failure(format!(
                "Gate already in terminal state: {:?}",
                session.gate.status
            ));
        }

        // Create skip decision
        let skip = SkipDecision::new(reason, decider).with_lines_changed(lines_changed);

        // Log skip stats event
        let stats_path = project_stats_log_path(Path::new(&session.cwd));
        let stats_logger = StatsLogger::new(&stats_path);

        let ticket_id = session.gate.ticket.as_ref().map(|t| t.ticket_id.clone());

        stats_logger
            .append_skip(session_id, reason, decider, lines_changed, ticket_id)
            .fail_open_default("logging skip stats");

        // Update session state
        session.gate.skip = Some(skip);
        session.gate.status = GateStatus::Skipped;
        session.add_trace(EventType::Skip, Some(format!("{:?}: {}", decider, reason)));

        // Save session (fail-open)
        self.store.put(&session).fail_open_default("saving session");

        SkipOutput::success(reason, decider)
    }

    /// Run the skip command with JSON input from stdin.
    pub fn run_with_input(&self, input: &SkipInput, options: &SkipOptions) -> SkipOutput {
        let decider = input
            .decider
            .or(options.decider)
            .unwrap_or(SkipDecider::User);
        let lines_changed = input.lines_changed.or(options.lines_changed).unwrap_or(0);

        let merged_options = SkipOptions {
            decider: Some(decider),
            lines_changed: Some(lines_changed),
            ..*options
        };

        self.run(&input.session_id, &input.reason, &merged_options)
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &SkipOutput, options: &SkipOptions) -> String {
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
    fn format_human_readable(&self, output: &SkipOutput) -> String {
        if output.success {
            format!(
                "Reflection skipped ({}).\nReason: {}\n",
                output.decider, output.reason
            )
        } else {
            format!(
                "Skip failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            )
        }
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
    fn test_skip_output_success() {
        let output = SkipOutput::success("too small", SkipDecider::Agent);

        assert!(output.success);
        assert_eq!(output.reason, "too small");
        assert_eq!(output.decider, "agent");
        assert!(output.error.is_none());
    }

    #[test]
    fn test_skip_output_failure() {
        let output = SkipOutput::failure("test error");

        assert!(!output.success);
        assert!(output.reason.is_empty());
        assert_eq!(output.error, Some("test error".to_string()));
    }

    #[test]
    fn test_skip_basic() {
        let store = setup();
        let config = Config::default();

        // Create initial session
        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = SkipCommand::new(Arc::clone(&store), config);
        let options = SkipOptions::default();

        let output = cmd.run("test-session", "trivial change", &options);

        assert!(output.success);
        assert_eq!(output.reason, "trivial change");
        assert_eq!(output.decider, "user"); // Default

        // Check session was updated
        let updated = store.get("test-session").unwrap().unwrap();
        assert_eq!(updated.gate.status, GateStatus::Skipped);
        assert!(updated.gate.skip.is_some());

        let skip = updated.gate.skip.unwrap();
        assert_eq!(skip.reason, "trivial change");
        assert_eq!(skip.decider, SkipDecider::User);
    }

    #[test]
    fn test_skip_with_agent_decider() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = SkipCommand::new(Arc::clone(&store), config);
        let options = SkipOptions {
            decider: Some(SkipDecider::Agent),
            lines_changed: Some(3),
            ..Default::default()
        };

        let output = cmd.run("test-session", "auto: 3 lines, version bump", &options);

        assert!(output.success);
        assert_eq!(output.decider, "agent");

        let updated = store.get("test-session").unwrap().unwrap();
        let skip = updated.gate.skip.unwrap();
        assert_eq!(skip.decider, SkipDecider::Agent);
        assert_eq!(skip.lines_changed, Some(3));
    }

    #[test]
    fn test_skip_with_auto_threshold() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = SkipCommand::new(Arc::clone(&store), config);
        let options = SkipOptions {
            decider: Some(SkipDecider::AutoThreshold),
            lines_changed: Some(2),
            ..Default::default()
        };

        let output = cmd.run("test-session", "below threshold", &options);

        assert!(output.success);
        assert_eq!(output.decider, "autothreshold");
    }

    #[test]
    fn test_skip_fails_if_already_reflected() {
        let store = setup();
        let config = Config::default();

        let mut session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        session.gate.status = GateStatus::Reflected;
        store.put(&session).unwrap();

        let cmd = SkipCommand::new(store, config);
        let options = SkipOptions::default();

        let output = cmd.run("test-session", "want to skip", &options);

        assert!(!output.success);
        assert!(output.error.unwrap().contains("terminal state"));
    }

    #[test]
    fn test_skip_fails_if_already_skipped() {
        let store = setup();
        let config = Config::default();

        let mut session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        session.gate.status = GateStatus::Skipped;
        store.put(&session).unwrap();

        let cmd = SkipCommand::new(store, config);
        let options = SkipOptions::default();

        let output = cmd.run("test-session", "skip again", &options);

        assert!(!output.success);
        assert!(output.error.unwrap().contains("terminal state"));
    }

    #[test]
    fn test_skip_creates_session_if_not_found() {
        let store = setup();
        let config = Config::default();

        // Don't create session first
        let cmd = SkipCommand::new(Arc::clone(&store), config);
        let options = SkipOptions::default();

        let output = cmd.run("new-session", "no session exists", &options);

        // Should succeed with fail-open
        assert!(output.success);
    }

    #[test]
    fn test_skip_with_input() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = SkipCommand::new(Arc::clone(&store), config);

        let input = SkipInput {
            session_id: "test-session".to_string(),
            reason: "from json input".to_string(),
            decider: Some(SkipDecider::Agent),
            lines_changed: Some(5),
        };

        let options = SkipOptions::default();
        let output = cmd.run_with_input(&input, &options);

        assert!(output.success);
        assert_eq!(output.reason, "from json input");
        assert_eq!(output.decider, "agent");

        let updated = store.get("test-session").unwrap().unwrap();
        let skip = updated.gate.skip.unwrap();
        assert_eq!(skip.lines_changed, Some(5));
    }

    #[test]
    fn test_format_output_json() {
        let store = setup();
        let config = Config::default();
        let cmd = SkipCommand::new(store, config);

        let output = SkipOutput::success("test reason", SkipDecider::User);
        let options = SkipOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"reason\": \"test reason\""));
    }

    #[test]
    fn test_format_output_quiet() {
        let store = setup();
        let config = Config::default();
        let cmd = SkipCommand::new(store, config);

        let output = SkipOutput::success("test reason", SkipDecider::User);
        let options = SkipOptions {
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
        let cmd = SkipCommand::new(store, config);

        let output = SkipOutput::success("trivial change", SkipDecider::Agent);
        let options = SkipOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Reflection skipped (agent)"));
        assert!(formatted.contains("trivial change"));
    }

    #[test]
    fn test_skip_adds_trace_event() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = SkipCommand::new(Arc::clone(&store), config);
        let options = SkipOptions::default();

        cmd.run("test-session", "trace test", &options);

        let updated = store.get("test-session").unwrap().unwrap();
        let skip_events: Vec<_> = updated
            .trace
            .iter()
            .filter(|t| t.event_type == EventType::Skip)
            .collect();

        assert_eq!(skip_events.len(), 1);
        assert!(skip_events[0]
            .details
            .as_ref()
            .unwrap()
            .contains("trace test"));
    }
}
