//! Ref command for Grove.
//!
//! Records that surfaced learnings were referenced (used) during a session,
//! enabling the scoring feedback loop via reference_boost.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::{project_stats_log_path, Config};
use crate::core::{EventType, SessionState};
use crate::error::{FailOpen, Result};
use crate::stats::StatsLogger;
use crate::storage::SessionStore;

/// Options for the ref command.
#[derive(Debug, Clone, Default)]
pub struct RefOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// How the learning was used (optional context for trace).
    pub how: Option<String>,
}

/// Output format for the ref command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefOutput {
    /// Whether the ref was successful.
    pub success: bool,
    /// Number of learnings referenced.
    pub referenced_count: usize,
    /// The learning IDs that were referenced.
    pub learning_ids: Vec<String>,
    /// Error message if ref failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RefOutput {
    /// Create a successful output.
    pub fn success(learning_ids: Vec<String>) -> Self {
        Self {
            success: true,
            referenced_count: learning_ids.len(),
            learning_ids,
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            referenced_count: 0,
            learning_ids: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// The ref command implementation.
pub struct RefCommand<S: SessionStore> {
    store: S,
    #[allow(dead_code)]
    config: Config,
}

impl<S: SessionStore> RefCommand<S> {
    /// Create a new ref command.
    pub fn new(store: S, config: Config) -> Self {
        Self { store, config }
    }

    /// Run the ref command with the given learning IDs.
    pub fn run(
        &self,
        session_id: &str,
        learning_ids: &[String],
        options: &RefOptions,
    ) -> RefOutput {
        // Validate learning_ids is non-empty
        if learning_ids.is_empty() {
            return RefOutput::failure("At least one learning ID is required");
        }

        // Load session (fail-open: create temporary if not found)
        let session_result: Result<Option<SessionState>> = self.store.get(session_id);
        let mut session = session_result
            .fail_open_with(
                "loading session",
                Some(SessionState::new_fallback(session_id)),
            )
            .unwrap_or_else(|| SessionState::new_fallback(session_id));

        // Create stats logger
        let stats_path = project_stats_log_path(Path::new(&session.cwd));
        let stats_logger = StatsLogger::new(&stats_path);

        // Get ticket_id from session
        let ticket_id = session.gate.ticket.as_ref().map(|t| t.ticket_id.clone());

        // Process each learning ID
        for id in learning_ids {
            // Append referenced stats event (fail-open)
            stats_logger
                .append_referenced(id, session_id, ticket_id.clone())
                .fail_open_default("logging referenced stats");

            // Mark injected learning as referenced (best-effort)
            if let Some(injected) = session
                .gate
                .injected_learnings
                .iter_mut()
                .find(|il| il.learning_id == *id)
            {
                injected.mark_referenced();
            }

            // Add trace event with optional how context
            let detail = match &options.how {
                Some(how) => format!("learning: {} ({})", id, how),
                None => format!("learning: {}", id),
            };
            session.add_trace(EventType::LearningReferenced, Some(detail));
        }

        // Save session (fail-open)
        self.store.put(&session).fail_open_default("saving session");

        RefOutput::success(learning_ids.to_vec())
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &RefOutput, options: &RefOptions) -> String {
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
    fn format_human_readable(&self, output: &RefOutput) -> String {
        if output.success {
            format!(
                "Referenced {} learning(s): {}\n",
                output.referenced_count,
                output.learning_ids.join(", ")
            )
        } else {
            format!(
                "Ref failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{InjectedLearning, InjectionOutcome};
    use crate::storage::MemorySessionStore;
    use std::sync::Arc;

    fn setup() -> Arc<MemorySessionStore> {
        Arc::new(MemorySessionStore::new())
    }

    #[test]
    fn test_ref_output_success() {
        let output = RefOutput::success(vec!["cl_001".to_string(), "cl_002".to_string()]);

        assert!(output.success);
        assert_eq!(output.referenced_count, 2);
        assert_eq!(output.learning_ids, vec!["cl_001", "cl_002"]);
        assert!(output.error.is_none());
    }

    #[test]
    fn test_ref_output_failure() {
        let output = RefOutput::failure("test error");

        assert!(!output.success);
        assert_eq!(output.referenced_count, 0);
        assert!(output.learning_ids.is_empty());
        assert_eq!(output.error, Some("test error".to_string()));
    }

    #[test]
    fn test_ref_basic() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = RefCommand::new(Arc::clone(&store), config);
        let options = RefOptions::default();

        let output = cmd.run("test-session", &["cl_001".to_string()], &options);

        assert!(output.success);
        assert_eq!(output.referenced_count, 1);
        assert_eq!(output.learning_ids, vec!["cl_001"]);
    }

    #[test]
    fn test_ref_multiple() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = RefCommand::new(Arc::clone(&store), config);
        let options = RefOptions::default();

        let ids = vec![
            "cl_001".to_string(),
            "cl_002".to_string(),
            "cl_003".to_string(),
        ];
        let output = cmd.run("test-session", &ids, &options);

        assert!(output.success);
        assert_eq!(output.referenced_count, 3);
        assert_eq!(output.learning_ids, ids);
    }

    #[test]
    fn test_ref_rejects_empty() {
        let store = setup();
        let config = Config::default();
        let cmd = RefCommand::new(store, config);
        let options = RefOptions::default();

        let output = cmd.run("test-session", &[], &options);

        assert!(!output.success);
        assert!(output.error.as_ref().unwrap().contains("required"));
    }

    #[test]
    fn test_ref_creates_session_if_not_found() {
        let store = setup();
        let config = Config::default();

        // Don't create session first
        let cmd = RefCommand::new(Arc::clone(&store), config);
        let options = RefOptions::default();

        let output = cmd.run("new-session", &["cl_001".to_string()], &options);

        // Should succeed with fail-open
        assert!(output.success);
    }

    #[test]
    fn test_ref_marks_injected_learning() {
        let store = setup();
        let config = Config::default();

        let mut session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        session
            .gate
            .injected_learnings
            .push(InjectedLearning::new("cl_001", 0.85));
        session
            .gate
            .injected_learnings
            .push(InjectedLearning::new("cl_002", 0.70));
        store.put(&session).unwrap();

        let cmd = RefCommand::new(Arc::clone(&store), config);
        let options = RefOptions::default();

        let output = cmd.run("test-session", &["cl_001".to_string()], &options);
        assert!(output.success);

        // Check that the injected learning was marked as referenced
        let updated = store.get("test-session").unwrap().unwrap();
        let il = updated
            .gate
            .injected_learnings
            .iter()
            .find(|il| il.learning_id == "cl_001")
            .unwrap();
        assert_eq!(il.outcome, InjectionOutcome::Referenced);

        // cl_002 should still be pending
        let il2 = updated
            .gate
            .injected_learnings
            .iter()
            .find(|il| il.learning_id == "cl_002")
            .unwrap();
        assert_eq!(il2.outcome, InjectionOutcome::Pending);
    }

    #[test]
    fn test_ref_adds_trace_event() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = RefCommand::new(Arc::clone(&store), config);
        let options = RefOptions::default();

        cmd.run(
            "test-session",
            &["cl_001".to_string(), "cl_002".to_string()],
            &options,
        );

        let updated = store.get("test-session").unwrap().unwrap();
        let ref_events: Vec<_> = updated
            .trace
            .iter()
            .filter(|t| t.event_type == EventType::LearningReferenced)
            .collect();

        assert_eq!(ref_events.len(), 2);
        assert!(ref_events[0].details.as_ref().unwrap().contains("cl_001"));
        assert!(ref_events[1].details.as_ref().unwrap().contains("cl_002"));
    }

    #[test]
    fn test_format_output_json() {
        let store = setup();
        let config = Config::default();
        let cmd = RefCommand::new(store, config);

        let output = RefOutput::success(vec!["cl_001".to_string()]);
        let options = RefOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"referenced_count\": 1"));
        assert!(formatted.contains("cl_001"));
    }

    #[test]
    fn test_format_output_quiet() {
        let store = setup();
        let config = Config::default();
        let cmd = RefCommand::new(store, config);

        let output = RefOutput::success(vec!["cl_001".to_string()]);
        let options = RefOptions {
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
        let cmd = RefCommand::new(store, config);

        let output = RefOutput::success(vec!["cl_001".to_string(), "cl_002".to_string()]);
        let options = RefOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Referenced 2 learning(s)"));
        assert!(formatted.contains("cl_001"));
        assert!(formatted.contains("cl_002"));
    }

    #[test]
    fn test_ref_how_included_in_trace() {
        let store = setup();
        let config = Config::default();

        let session = SessionState::new("test-session", "/tmp", "/tmp/transcript.json");
        store.put(&session).unwrap();

        let cmd = RefCommand::new(Arc::clone(&store), config);
        let options = RefOptions {
            how: Some("followed auth ordering pattern".to_string()),
            ..Default::default()
        };

        cmd.run("test-session", &["cl_001".to_string()], &options);

        let updated = store.get("test-session").unwrap().unwrap();
        let ref_events: Vec<_> = updated
            .trace
            .iter()
            .filter(|t| t.event_type == EventType::LearningReferenced)
            .collect();

        assert_eq!(ref_events.len(), 1);
        let detail = ref_events[0].details.as_ref().unwrap();
        assert!(detail.contains("cl_001"));
        assert!(
            detail.contains("followed auth ordering pattern"),
            "Trace should include how context, got: {}",
            detail
        );
    }
}
