//! Trace command for Grove.
//!
//! Shows trace events for a session.

use serde::{Deserialize, Serialize};

use crate::core::{SessionState, TraceEvent};
use crate::error::Result;
use crate::storage::SessionStore;

/// Options for the trace command.
#[derive(Debug, Clone, Default)]
pub struct TraceOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Maximum number of events to show.
    pub limit: Option<usize>,
    /// Filter by event type.
    pub event_type: Option<String>,
}

/// Output format for the trace command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceOutput {
    /// Whether the command was successful.
    pub success: bool,
    /// Session ID.
    pub session_id: String,
    /// Number of trace events.
    pub count: usize,
    /// Total events (before filtering/limiting).
    pub total: usize,
    /// Trace events.
    pub events: Vec<TraceEventInfo>,
    /// Error message if command failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Simplified trace event info for output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEventInfo {
    /// Event timestamp.
    pub timestamp: String,
    /// Event type.
    pub event_type: String,
    /// Event details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

impl From<&TraceEvent> for TraceEventInfo {
    fn from(event: &TraceEvent) -> Self {
        Self {
            timestamp: event.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
            event_type: format!("{:?}", event.event_type),
            details: event.details.clone(),
        }
    }
}

impl TraceOutput {
    /// Create a successful output.
    pub fn success(
        session_id: impl Into<String>,
        events: Vec<TraceEventInfo>,
        total: usize,
    ) -> Self {
        let count = events.len();
        Self {
            success: true,
            session_id: session_id.into(),
            count,
            total,
            events,
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(session_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            success: false,
            session_id: session_id.into(),
            count: 0,
            total: 0,
            events: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// The trace command implementation.
pub struct TraceCommand<S: SessionStore> {
    store: S,
}

impl<S: SessionStore> TraceCommand<S> {
    /// Create a new trace command.
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Run the trace command.
    pub fn run(&self, session_id: &str, options: &TraceOptions) -> TraceOutput {
        // Load session
        let session = match self.load_session(session_id) {
            Ok(Some(s)) => s,
            Ok(None) => {
                return TraceOutput::failure(
                    session_id,
                    format!("Session not found: {}", session_id),
                )
            }
            Err(e) => {
                return TraceOutput::failure(session_id, format!("Failed to load session: {}", e))
            }
        };

        let total = session.trace.len();

        // Convert to info format
        let mut events: Vec<TraceEventInfo> =
            session.trace.iter().map(TraceEventInfo::from).collect();

        // Filter by event type if specified
        if let Some(filter_type) = &options.event_type {
            events.retain(|e| {
                e.event_type
                    .to_lowercase()
                    .contains(&filter_type.to_lowercase())
            });
        }

        // Apply limit
        if let Some(limit) = options.limit {
            events.truncate(limit);
        }

        TraceOutput::success(session_id, events, total)
    }

    /// Load a session from the store.
    fn load_session(&self, session_id: &str) -> Result<Option<SessionState>> {
        self.store.get(session_id)
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &TraceOutput, options: &TraceOptions) -> String {
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
    fn format_human_readable(&self, output: &TraceOutput) -> String {
        if !output.success {
            return format!(
                "Trace failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            );
        }

        if output.events.is_empty() {
            return format!("No trace events for session: {}\n", output.session_id);
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "Trace events for session: {} ({}/{})\n",
            output.session_id, output.count, output.total
        ));

        for event in &output.events {
            let details = event
                .details
                .as_ref()
                .map(|d| format!(" - {}", d))
                .unwrap_or_default();

            lines.push(format!(
                "[{}] {}{}",
                event.timestamp, event.event_type, details
            ));
        }

        lines.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::EventType;
    use crate::storage::MemorySessionStore;

    fn create_test_store() -> MemorySessionStore {
        MemorySessionStore::new()
    }

    fn create_test_session(id: &str) -> SessionState {
        SessionState::new(id, "/tmp", "/tmp/transcript.json")
    }

    #[test]
    fn test_trace_output_success() {
        let events = vec![TraceEventInfo {
            timestamp: "2026-01-01 12:00:00".to_string(),
            event_type: "SessionStart".to_string(),
            details: Some("test".to_string()),
        }];
        let output = TraceOutput::success("test-1", events, 1);

        assert!(output.success);
        assert_eq!(output.session_id, "test-1");
        assert_eq!(output.count, 1);
        assert_eq!(output.total, 1);
        assert!(output.error.is_none());
    }

    #[test]
    fn test_trace_output_failure() {
        let output = TraceOutput::failure("test-1", "test error");

        assert!(!output.success);
        assert_eq!(output.session_id, "test-1");
        assert_eq!(output.count, 0);
        assert_eq!(output.error, Some("test error".to_string()));
    }

    #[test]
    fn test_trace_session_not_found() {
        let store = create_test_store();
        let cmd = TraceCommand::new(store);
        let options = TraceOptions::default();

        let output = cmd.run("nonexistent", &options);

        assert!(!output.success);
        assert!(output.error.unwrap().contains("not found"));
    }

    #[test]
    fn test_trace_empty_session() {
        let store = create_test_store();
        let session = create_test_session("test-1");
        store.put(&session).unwrap();

        let cmd = TraceCommand::new(store);
        let options = TraceOptions::default();

        let output = cmd.run("test-1", &options);

        assert!(output.success);
        assert_eq!(output.count, 0);
        assert_eq!(output.total, 0);
    }

    #[test]
    fn test_trace_with_events() {
        let store = create_test_store();
        let mut session = create_test_session("test-1");
        session.add_trace(EventType::SessionStart, Some("Started".to_string()));
        session.add_trace(EventType::GateBlocked, Some("Exit blocked".to_string()));
        store.put(&session).unwrap();

        let cmd = TraceCommand::new(store);
        let options = TraceOptions::default();

        let output = cmd.run("test-1", &options);

        assert!(output.success);
        assert_eq!(output.count, 2);
        assert_eq!(output.total, 2);
        assert!(output.events[0].event_type.contains("SessionStart"));
    }

    #[test]
    fn test_trace_with_limit() {
        let store = create_test_store();
        let mut session = create_test_session("test-1");
        for i in 0..5 {
            session.add_trace(EventType::TicketDetected, Some(format!("Event {}", i)));
        }
        store.put(&session).unwrap();

        let cmd = TraceCommand::new(store);
        let options = TraceOptions {
            limit: Some(2),
            ..Default::default()
        };

        let output = cmd.run("test-1", &options);

        assert!(output.success);
        assert_eq!(output.count, 2);
        assert_eq!(output.total, 5);
    }

    #[test]
    fn test_trace_with_filter() {
        let store = create_test_store();
        let mut session = create_test_session("test-1");
        session.add_trace(EventType::SessionStart, None);
        session.add_trace(EventType::GateBlocked, None);
        session.add_trace(EventType::GateBlocked, None);
        session.add_trace(EventType::SessionEnd, None);
        store.put(&session).unwrap();

        let cmd = TraceCommand::new(store);
        let options = TraceOptions {
            event_type: Some("GateBlocked".to_string()),
            ..Default::default()
        };

        let output = cmd.run("test-1", &options);

        assert!(output.success);
        assert_eq!(output.count, 2);
        assert_eq!(output.total, 4);
        assert!(output
            .events
            .iter()
            .all(|e| e.event_type.contains("GateBlocked")));
    }

    #[test]
    fn test_format_output_json() {
        let store = create_test_store();
        let cmd = TraceCommand::new(store);

        let events = vec![TraceEventInfo {
            timestamp: "2026-01-01 12:00:00".to_string(),
            event_type: "SessionStart".to_string(),
            details: None,
        }];
        let output = TraceOutput::success("test-1", events, 1);
        let options = TraceOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"session_id\": \"test-1\""));
    }

    #[test]
    fn test_format_output_quiet() {
        let store = create_test_store();
        let cmd = TraceCommand::new(store);

        let output = TraceOutput::success("test-1", vec![], 0);
        let options = TraceOptions {
            quiet: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_output_human_readable() {
        let store = create_test_store();
        let cmd = TraceCommand::new(store);

        let events = vec![
            TraceEventInfo {
                timestamp: "2026-01-01 12:00:00".to_string(),
                event_type: "SessionStart".to_string(),
                details: Some("Started session".to_string()),
            },
            TraceEventInfo {
                timestamp: "2026-01-01 12:01:00".to_string(),
                event_type: "GateBlocked".to_string(),
                details: Some("Exit blocked".to_string()),
            },
        ];
        let output = TraceOutput::success("test-1", events, 2);
        let options = TraceOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Trace events for session: test-1"));
        assert!(formatted.contains("SessionStart"));
        assert!(formatted.contains("Started session"));
        assert!(formatted.contains("GateBlocked"));
    }

    #[test]
    fn test_format_output_empty() {
        let store = create_test_store();
        let cmd = TraceCommand::new(store);

        let output = TraceOutput::success("test-1", vec![], 0);
        let options = TraceOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("No trace events"));
    }

    #[test]
    fn test_trace_event_info_from_event() {
        let event = TraceEvent::new(EventType::SessionStart, Some("Test".to_string()));
        let info = TraceEventInfo::from(&event);

        assert!(info.event_type.contains("SessionStart"));
        assert_eq!(info.details, Some("Test".to_string()));
    }

    #[test]
    fn test_trace_filter_case_insensitive() {
        let store = create_test_store();
        let mut session = create_test_session("test-1");
        session.add_trace(EventType::SessionStart, None);
        session.add_trace(EventType::GateBlocked, None);
        store.put(&session).unwrap();

        let cmd = TraceCommand::new(store);
        let options = TraceOptions {
            event_type: Some("gateblocked".to_string()),
            ..Default::default()
        };

        let output = cmd.run("test-1", &options);

        assert!(output.success);
        assert_eq!(output.count, 1);
    }
}
