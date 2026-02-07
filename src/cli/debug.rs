//! Debug command for Grove.
//!
//! Full session state dump for development and troubleshooting.
//! This is a testing-only escape hatch and may expose internal state.

use serde::{Deserialize, Serialize};

use crate::core::SessionState;
use crate::error::Result;
use crate::storage::SessionStore;

/// Options for the debug command.
#[derive(Debug, Clone, Default)]
pub struct DebugOptions {
    /// Output as JSON (always true for debug).
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Set the gate status (testing escape hatch).
    pub set_gate: Option<String>,
}

/// Output format for the debug command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugOutput {
    /// Whether the command was successful.
    pub success: bool,
    /// The session state (full dump).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionState>,
    /// Whether the gate was modified.
    pub gate_modified: bool,
    /// Error message if command failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DebugOutput {
    /// Create a successful output.
    pub fn success(session: SessionState, gate_modified: bool) -> Self {
        Self {
            success: true,
            session: Some(session),
            gate_modified,
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            session: None,
            gate_modified: false,
            error: Some(error.into()),
        }
    }
}

/// The debug command implementation.
pub struct DebugCommand<S: SessionStore> {
    store: S,
}

impl<S: SessionStore> DebugCommand<S> {
    /// Create a new debug command.
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Run the debug command.
    pub fn run(&self, session_id: &str, options: &DebugOptions) -> DebugOutput {
        // Load session
        let session = match self.load_session(session_id) {
            Ok(Some(s)) => s,
            Ok(None) => return DebugOutput::failure(format!("Session not found: {}", session_id)),
            Err(e) => return DebugOutput::failure(format!("Failed to load session: {}", e)),
        };

        // Handle --set-gate if provided
        if let Some(gate_status) = &options.set_gate {
            return self.handle_set_gate(session, gate_status);
        }

        DebugOutput::success(session, false)
    }

    /// Load a session from the store.
    fn load_session(&self, session_id: &str) -> Result<Option<SessionState>> {
        self.store.get(session_id)
    }

    /// Handle the --set-gate escape hatch.
    fn handle_set_gate(&self, mut session: SessionState, gate_status: &str) -> DebugOutput {
        use crate::core::GateStatus;

        let status = match gate_status.to_lowercase().as_str() {
            "idle" => GateStatus::Idle,
            "active" => GateStatus::Active,
            "pending" => GateStatus::Pending,
            "blocked" => GateStatus::Blocked,
            "reflected" => GateStatus::Reflected,
            "skipped" => GateStatus::Skipped,
            _ => {
                return DebugOutput::failure(format!(
                    "Invalid gate status: {}. Valid values: idle, active, pending, blocked, reflected, skipped",
                    gate_status
                ))
            }
        };

        session.gate.status = status;
        session.touch();

        if let Err(e) = self.store.put(&session) {
            return DebugOutput::failure(format!("Failed to save session: {}", e));
        }

        DebugOutput::success(session, true)
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &DebugOutput, options: &DebugOptions) -> String {
        if options.quiet {
            return String::new();
        }

        // Debug always outputs JSON for full state dump
        if options.json || output.session.is_some() {
            serde_json::to_string_pretty(output).unwrap_or_else(|_| "{}".to_string())
        } else {
            self.format_human_readable(output)
        }
    }

    /// Format output as human-readable text.
    fn format_human_readable(&self, output: &DebugOutput) -> String {
        if !output.success {
            return format!(
                "Debug failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            );
        }

        // Fallback to JSON for session dump
        serde_json::to_string_pretty(output).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::GateStatus;
    use crate::storage::MemorySessionStore;

    fn create_test_store() -> MemorySessionStore {
        MemorySessionStore::new()
    }

    fn create_test_session(id: &str) -> SessionState {
        SessionState::new(id, "/tmp", "/tmp/transcript.json")
    }

    #[test]
    fn test_debug_output_success() {
        let session = create_test_session("test-1");
        let output = DebugOutput::success(session.clone(), false);

        assert!(output.success);
        assert!(output.session.is_some());
        assert!(!output.gate_modified);
        assert!(output.error.is_none());
    }

    #[test]
    fn test_debug_output_failure() {
        let output = DebugOutput::failure("test error");

        assert!(!output.success);
        assert!(output.session.is_none());
        assert!(!output.gate_modified);
        assert_eq!(output.error, Some("test error".to_string()));
    }

    #[test]
    fn test_debug_session_not_found() {
        let store = create_test_store();
        let cmd = DebugCommand::new(store);
        let options = DebugOptions::default();

        let output = cmd.run("nonexistent", &options);

        assert!(!output.success);
        assert!(output.error.unwrap().contains("not found"));
    }

    #[test]
    fn test_debug_dumps_session() {
        let store = create_test_store();
        let session = create_test_session("test-1");
        store.put(&session).unwrap();

        let cmd = DebugCommand::new(store);
        let options = DebugOptions::default();

        let output = cmd.run("test-1", &options);

        assert!(output.success);
        assert!(output.session.is_some());
        assert_eq!(output.session.unwrap().id, "test-1");
    }

    #[test]
    fn test_debug_set_gate_idle() {
        let store = create_test_store();
        let mut session = create_test_session("test-1");
        session.gate.status = GateStatus::Blocked;
        store.put(&session).unwrap();

        let cmd = DebugCommand::new(store);
        let options = DebugOptions {
            set_gate: Some("idle".to_string()),
            ..Default::default()
        };

        let output = cmd.run("test-1", &options);

        assert!(output.success);
        assert!(output.gate_modified);
        // The output contains the modified session
        assert_eq!(output.session.unwrap().gate.status, GateStatus::Idle);
    }

    #[test]
    fn test_debug_set_gate_all_statuses() {
        let statuses = [
            "idle",
            "active",
            "pending",
            "blocked",
            "reflected",
            "skipped",
        ];

        for status in statuses {
            let store = create_test_store();
            let session = create_test_session("test-1");
            store.put(&session).unwrap();

            let cmd = DebugCommand::new(store);
            let options = DebugOptions {
                set_gate: Some(status.to_string()),
                ..Default::default()
            };

            let output = cmd.run("test-1", &options);
            assert!(output.success, "Failed for status: {}", status);
            assert!(output.gate_modified);
        }
    }

    #[test]
    fn test_debug_set_gate_invalid() {
        let store = create_test_store();
        let session = create_test_session("test-1");
        store.put(&session).unwrap();

        let cmd = DebugCommand::new(store);
        let options = DebugOptions {
            set_gate: Some("invalid".to_string()),
            ..Default::default()
        };

        let output = cmd.run("test-1", &options);

        assert!(!output.success);
        assert!(output.error.unwrap().contains("Invalid gate status"));
    }

    #[test]
    fn test_format_output_json() {
        let store = create_test_store();
        let cmd = DebugCommand::new(store);

        let session = create_test_session("test-1");
        let output = DebugOutput::success(session, false);
        let options = DebugOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"id\": \"test-1\""));
    }

    #[test]
    fn test_format_output_quiet() {
        let store = create_test_store();
        let cmd = DebugCommand::new(store);

        let session = create_test_session("test-1");
        let output = DebugOutput::success(session, false);
        let options = DebugOptions {
            quiet: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_debug_outputs_json_by_default() {
        let store = create_test_store();
        let cmd = DebugCommand::new(store);

        let session = create_test_session("test-1");
        let output = DebugOutput::success(session, false);
        let options = DebugOptions::default();

        let formatted = cmd.format_output(&output, &options);
        // Debug always outputs JSON for full session dump
        assert!(formatted.contains("{"));
        assert!(formatted.contains("\"session\""));
    }

    #[test]
    fn test_debug_set_gate_case_insensitive() {
        let store = create_test_store();
        let session = create_test_session("test-1");
        store.put(&session).unwrap();

        let cmd = DebugCommand::new(store);
        let options = DebugOptions {
            set_gate: Some("IDLE".to_string()),
            ..Default::default()
        };

        let output = cmd.run("test-1", &options);

        assert!(output.success);
        assert_eq!(output.session.unwrap().gate.status, GateStatus::Idle);
    }
}
