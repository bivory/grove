//! Sessions command for Grove.
//!
//! Lists recent sessions with their IDs and gate status, useful for finding
//! session IDs to pass to `grove debug` and `grove trace`.

use serde::{Deserialize, Serialize};

use crate::core::SessionState;
use crate::error::Result;
use crate::storage::SessionStore;

/// Options for the sessions command.
#[derive(Debug, Clone, Default)]
pub struct SessionsOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Maximum number of sessions to show.
    pub limit: usize,
}

/// Summary of a single session for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Session ID.
    pub id: String,
    /// Gate status.
    pub gate_status: String,
    /// Project directory.
    pub project_dir: String,
    /// Last updated timestamp (ISO 8601).
    pub updated_at: String,
    /// Active ticket ID if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket_id: Option<String>,
}

impl From<&SessionState> for SessionSummary {
    fn from(session: &SessionState) -> Self {
        Self {
            id: session.id.clone(),
            gate_status: format!("{:?}", session.gate.status),
            project_dir: session.cwd.clone(),
            updated_at: session.updated_at.to_rfc3339(),
            ticket_id: session.gate.ticket.as_ref().map(|t| t.ticket_id.clone()),
        }
    }
}

/// Output format for the sessions command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsOutput {
    /// Whether the command was successful.
    pub success: bool,
    /// List of session summaries.
    pub sessions: Vec<SessionSummary>,
    /// Total count of sessions returned.
    pub count: usize,
    /// Error message if command failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SessionsOutput {
    /// Create a successful output.
    pub fn success(sessions: Vec<SessionSummary>) -> Self {
        let count = sessions.len();
        Self {
            success: true,
            sessions,
            count,
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            sessions: vec![],
            count: 0,
            error: Some(error.into()),
        }
    }

    /// Format as human-readable text.
    pub fn format_text(&self) -> String {
        if !self.success {
            return format!(
                "Sessions failed: {}",
                self.error.as_deref().unwrap_or("unknown error")
            );
        }

        if self.sessions.is_empty() {
            return "No sessions found.".to_string();
        }

        let mut lines = vec![format!("Sessions ({} found):", self.count)];
        lines.push(String::new());

        // Header
        lines.push(format!(
            "{:<36}  {:<10}  {:<20}  {}",
            "ID", "STATUS", "UPDATED", "TICKET"
        ));
        lines.push("-".repeat(90));

        for session in &self.sessions {
            let ticket = session.ticket_id.as_deref().unwrap_or("-");
            // Truncate timestamp to just date and time (YYYY-MM-DDTHH:MM:SS)
            // Use char-based truncation for UTF-8 safety even though RFC 3339 is ASCII-only
            let updated: String = session.updated_at.chars().take(19).collect();
            lines.push(format!(
                "{:<36}  {:<10}  {:<20}  {}",
                session.id, session.gate_status, updated, ticket
            ));
        }

        lines.join("\n")
    }
}

/// The sessions command implementation.
pub struct SessionsCommand<S: SessionStore> {
    store: S,
}

impl<S: SessionStore> SessionsCommand<S> {
    /// Create a new sessions command.
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Run the sessions command.
    pub fn run(&self, options: &SessionsOptions) -> SessionsOutput {
        match self.list_sessions(options.limit) {
            Ok(sessions) => {
                let summaries: Vec<SessionSummary> =
                    sessions.iter().map(SessionSummary::from).collect();
                SessionsOutput::success(summaries)
            }
            Err(e) => SessionsOutput::failure(format!("Failed to list sessions: {}", e)),
        }
    }

    /// List sessions from the store.
    fn list_sessions(&self, limit: usize) -> Result<Vec<SessionState>> {
        self.store.list(limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::GateStatus;
    use crate::storage::MemorySessionStore;
    use std::sync::Arc;

    fn create_test_store() -> Arc<MemorySessionStore> {
        Arc::new(MemorySessionStore::new())
    }

    #[test]
    fn test_sessions_empty() {
        let store = create_test_store();
        let cmd = SessionsCommand::new(store);
        let options = SessionsOptions {
            limit: 10,
            ..Default::default()
        };

        let output = cmd.run(&options);
        assert!(output.success);
        assert_eq!(output.count, 0);
        assert!(output.sessions.is_empty());
    }

    #[test]
    fn test_sessions_with_data() {
        let store = create_test_store();

        // Add some sessions
        let session1 = SessionState::new("session-1", "/project/a", "/transcript1.json");
        let session2 = SessionState::new("session-2", "/project/b", "/transcript2.json");
        store.put(&session1).unwrap();
        store.put(&session2).unwrap();

        let cmd = SessionsCommand::new(store);
        let options = SessionsOptions {
            limit: 10,
            ..Default::default()
        };

        let output = cmd.run(&options);
        assert!(output.success);
        assert_eq!(output.count, 2);
    }

    #[test]
    fn test_sessions_respects_limit() {
        let store = create_test_store();

        // Add 5 sessions
        for i in 0..5 {
            let session =
                SessionState::new(format!("session-{}", i), "/project", "/transcript.json");
            store.put(&session).unwrap();
        }

        let cmd = SessionsCommand::new(store);
        let options = SessionsOptions {
            limit: 3,
            ..Default::default()
        };

        let output = cmd.run(&options);
        assert!(output.success);
        assert_eq!(output.count, 3);
    }

    #[test]
    fn test_sessions_output_format_text() {
        let summaries = vec![SessionSummary {
            id: "abc-123".to_string(),
            gate_status: "Idle".to_string(),
            project_dir: "/project".to_string(),
            updated_at: "2024-01-15T10:30:00Z".to_string(),
            ticket_id: Some("TICKET-1".to_string()),
        }];

        let output = SessionsOutput::success(summaries);
        let text = output.format_text();

        assert!(text.contains("abc-123"));
        assert!(text.contains("Idle"));
        assert!(text.contains("TICKET-1"));
    }

    #[test]
    fn test_sessions_output_empty() {
        let output = SessionsOutput::success(vec![]);
        let text = output.format_text();
        assert!(text.contains("No sessions found"));
    }

    #[test]
    fn test_sessions_output_failure() {
        let output = SessionsOutput::failure("Test error");
        let text = output.format_text();
        assert!(text.contains("Test error"));
    }

    #[test]
    fn test_session_summary_from_session_state() {
        let mut session = SessionState::new("test-id", "/project", "/transcript.json");
        session.gate.status = GateStatus::Blocked;

        let summary = SessionSummary::from(&session);
        assert_eq!(summary.id, "test-id");
        assert_eq!(summary.gate_status, "Blocked");
        assert_eq!(summary.project_dir, "/project");
        assert!(summary.ticket_id.is_none());
    }
}
