//! Session and gate state types for Grove.
//!
//! These types represent the runtime state of a Grove session, including
//! the gate state machine, ticket context, and trace events.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Main session container.
///
/// Represents a Claude Code session with its associated gate state,
/// ticket context, and trace events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionState {
    /// Unique session identifier (UUID v4 from Claude Code).
    pub id: String,
    /// Current working directory.
    pub cwd: String,
    /// Path to the transcript file.
    pub transcript_path: String,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// When the session was last updated.
    pub updated_at: DateTime<Utc>,
    /// Gate state for this session.
    pub gate: GateState,
    /// Detected ticket context, if any.
    pub ticket: Option<TicketContext>,
    /// Trace events for debugging.
    pub trace: Vec<TraceEvent>,
}

impl SessionState {
    /// Create a new session with the given ID.
    pub fn new(
        id: impl Into<String>,
        cwd: impl Into<String>,
        transcript_path: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: id.into(),
            cwd: cwd.into(),
            transcript_path: transcript_path.into(),
            created_at: now,
            updated_at: now,
            gate: GateState::default(),
            ticket: None,
            trace: Vec::new(),
        }
    }

    /// Add a trace event to the session.
    pub fn add_trace(&mut self, event_type: EventType, details: Option<String>) {
        self.trace.push(TraceEvent::new(event_type, details));
        self.updated_at = Utc::now();
    }

    /// Update the session's updated_at timestamp.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }
}

/// Gate tracking state.
///
/// Tracks the current gate status, block count for circuit breaker,
/// and associated reflection/skip data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateState {
    /// Current gate status.
    pub status: GateStatus,
    /// Number of times the gate has blocked exit.
    pub block_count: u32,
    /// Whether the circuit breaker has tripped.
    pub circuit_breaker_tripped: bool,
    /// Session ID of the last block (for reset logic).
    pub last_blocked_session_id: Option<String>,
    /// When the gate last blocked (for cooldown calculation).
    pub last_blocked_at: Option<DateTime<Utc>>,
    /// Reflection result, if any.
    pub reflection: Option<ReflectionResult>,
    /// Skip decision, if any.
    pub skip: Option<SkipDecision>,
    /// Observations from subagents.
    pub subagent_observations: Vec<SubagentObservation>,
    /// Learnings injected at session start.
    pub injected_learnings: Vec<InjectedLearning>,
    /// Pending ticket close intent (pre-confirmation).
    pub ticket_close_intent: Option<TicketCloseIntent>,
    /// Cached diff size from git (lines changed).
    pub cached_diff_size: Option<u32>,
    /// Detected ticket context (when gate is Active).
    pub ticket: Option<TicketContext>,
}

impl Default for GateState {
    fn default() -> Self {
        Self {
            status: GateStatus::Idle,
            block_count: 0,
            circuit_breaker_tripped: false,
            last_blocked_session_id: None,
            last_blocked_at: None,
            reflection: None,
            skip: None,
            subagent_observations: Vec::new(),
            injected_learnings: Vec::new(),
            ticket_close_intent: None,
            cached_diff_size: None,
            ticket: None,
        }
    }
}

/// Gate status enum.
///
/// Represents the current state of the reflection gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    /// No reflection needed.
    #[default]
    Idle,
    /// Ticket in progress.
    Active,
    /// Reflection required (ticket closed or session mode).
    Pending,
    /// Exit blocked until reflection.
    Blocked,
    /// Learnings captured.
    Reflected,
    /// Skip logged to stats.
    Skipped,
}

impl GateStatus {
    /// Check if the gate is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, GateStatus::Reflected | GateStatus::Skipped)
    }

    /// Check if the gate requires reflection before exit.
    pub fn requires_reflection(&self) -> bool {
        matches!(self, GateStatus::Pending | GateStatus::Blocked)
    }
}

/// Detected ticket context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TicketContext {
    /// Ticket identifier.
    pub ticket_id: String,
    /// Source ticketing system (tissue, beads, tasks, session).
    pub source: String,
    /// Ticket title.
    pub title: String,
    /// Optional ticket description.
    pub description: Option<String>,
    /// When the ticket was detected.
    pub detected_at: DateTime<Utc>,
}

impl TicketContext {
    /// Create a new ticket context.
    pub fn new(
        ticket_id: impl Into<String>,
        source: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            ticket_id: ticket_id.into(),
            source: source.into(),
            title: title.into(),
            description: None,
            detected_at: Utc::now(),
        }
    }

    /// Create a ticket context with description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// Pending ticket close intent.
///
/// Recorded in PreToolUse when a ticket close command is detected,
/// confirmed in PostToolUse when the command succeeds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TicketCloseIntent {
    /// The ticket ID being closed.
    pub ticket_id: String,
    /// The command that will close the ticket.
    pub command: String,
    /// When the intent was recorded.
    pub recorded_at: DateTime<Utc>,
}

impl TicketCloseIntent {
    /// Create a new ticket close intent.
    pub fn new(ticket_id: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            ticket_id: ticket_id.into(),
            command: command.into(),
            recorded_at: Utc::now(),
        }
    }
}

/// Observation from a subagent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubagentObservation {
    /// The observation note.
    pub note: String,
    /// When the observation was recorded.
    pub timestamp: DateTime<Utc>,
}

impl SubagentObservation {
    /// Create a new subagent observation.
    pub fn new(note: impl Into<String>) -> Self {
        Self {
            note: note.into(),
            timestamp: Utc::now(),
        }
    }
}

/// Circuit breaker state.
///
/// Tracks state for the circuit breaker that prevents infinite blocking.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CircuitBreakerState {
    /// Number of consecutive blocks.
    pub block_count: u32,
    /// Whether the breaker has tripped.
    pub tripped: bool,
    /// Session ID of the last block.
    pub last_blocked_session_id: Option<String>,
    /// When the last block occurred.
    pub last_blocked_at: Option<DateTime<Utc>>,
}

impl CircuitBreakerState {
    /// Record a block and return whether the breaker should trip.
    pub fn record_block(&mut self, session_id: &str, max_blocks: u32) -> bool {
        self.block_count += 1;
        self.last_blocked_session_id = Some(session_id.to_string());
        self.last_blocked_at = Some(Utc::now());

        if self.block_count >= max_blocks {
            self.tripped = true;
        }

        self.tripped
    }

    /// Check if the breaker should reset.
    ///
    /// Reset conditions:
    /// 1. Cooldown elapsed since last block
    /// 2. Different session_id from last blocked session
    /// 3. Successful reflection completes (handled externally)
    pub fn should_reset(&self, current_session_id: &str, cooldown_seconds: u32) -> bool {
        // Different session
        if let Some(ref last_session) = self.last_blocked_session_id {
            if last_session != current_session_id {
                return true;
            }
        }

        // Cooldown elapsed
        if let Some(last_blocked) = self.last_blocked_at {
            let elapsed = Utc::now().signed_duration_since(last_blocked);
            if elapsed.num_seconds() >= cooldown_seconds as i64 {
                return true;
            }
        }

        false
    }

    /// Reset the circuit breaker.
    pub fn reset(&mut self) {
        self.block_count = 0;
        self.tripped = false;
        // Keep last_blocked_session_id and last_blocked_at for reference
    }
}

/// Individual trace event for debugging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraceEvent {
    /// Type of event.
    pub event_type: EventType,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Optional details.
    pub details: Option<String>,
}

impl TraceEvent {
    /// Create a new trace event.
    pub fn new(event_type: EventType, details: Option<String>) -> Self {
        Self {
            event_type,
            timestamp: Utc::now(),
            details,
        }
    }
}

/// Event type enum for trace events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    /// Session started.
    SessionStart,
    /// Ticket detected via discovery.
    TicketDetected,
    /// Backend detected via discovery.
    BackendDetected,
    /// Learnings injected at session start.
    LearningsInjected,
    /// Ticket close command detected (PreToolUse).
    TicketCloseDetected,
    /// Ticket close confirmed (PostToolUse success).
    TicketClosed,
    /// Ticket close failed (PostToolUse failure).
    TicketCloseFailed,
    /// Stop hook called.
    StopHookCalled,
    /// Gate blocked exit.
    GateBlocked,
    /// Reflection completed.
    ReflectionComplete,
    /// Skip decision made.
    Skip,
    /// Circuit breaker tripped.
    CircuitBreakerTripped,
    /// Session ending.
    SessionEnd,
    /// Subagent observation recorded.
    ObservationRecorded,
    /// Learning referenced during session.
    LearningReferenced,
    /// Learning dismissed (not used).
    LearningDismissed,
    /// Gate status changed.
    GateStatusChanged,
}

/// Reflection result from a completed reflection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReflectionResult {
    /// IDs of learnings that were written.
    pub learning_ids: Vec<String>,
    /// Candidates that were rejected during validation.
    pub rejected_candidates: Vec<RejectedCandidate>,
    /// Number of candidates produced.
    pub candidates_produced: u32,
    /// Number of candidates accepted.
    pub candidates_accepted: u32,
    /// When the reflection completed.
    pub completed_at: DateTime<Utc>,
}

impl ReflectionResult {
    /// Create a new reflection result.
    pub fn new(
        learning_ids: Vec<String>,
        candidates_produced: u32,
        candidates_accepted: u32,
    ) -> Self {
        Self {
            learning_ids,
            rejected_candidates: Vec::new(),
            candidates_produced,
            candidates_accepted,
            completed_at: Utc::now(),
        }
    }

    /// Create a reflection result with rejected candidates.
    pub fn with_rejected(
        learning_ids: Vec<String>,
        rejected_candidates: Vec<RejectedCandidate>,
        candidates_produced: u32,
        candidates_accepted: u32,
    ) -> Self {
        Self {
            learning_ids,
            rejected_candidates,
            candidates_produced,
            candidates_accepted,
            completed_at: Utc::now(),
        }
    }
}

/// Re-export RejectedCandidate for convenience.
pub use crate::core::reflect::RejectedCandidate;

/// Skip decision when reflection is skipped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkipDecision {
    /// Reason for skipping.
    pub reason: String,
    /// Who decided to skip.
    pub decider: SkipDecider,
    /// Lines changed (if available).
    pub lines_changed: Option<u32>,
    /// When the skip was decided.
    pub timestamp: DateTime<Utc>,
}

impl SkipDecision {
    /// Create a new skip decision.
    pub fn new(reason: impl Into<String>, decider: SkipDecider) -> Self {
        Self {
            reason: reason.into(),
            decider,
            lines_changed: None,
            timestamp: Utc::now(),
        }
    }

    /// Create a skip decision with lines changed.
    pub fn with_lines_changed(mut self, lines: u32) -> Self {
        self.lines_changed = Some(lines);
        self
    }
}

/// Who decided to skip reflection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipDecider {
    /// Agent decided to skip.
    Agent,
    /// User decided to skip.
    User,
    /// Auto-threshold triggered skip.
    AutoThreshold,
}

/// Tracks a learning that was injected at session start.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InjectedLearning {
    /// ID of the learning.
    pub learning_id: String,
    /// Relevance score when injected.
    pub score: f64,
    /// What happened to the learning.
    pub outcome: InjectionOutcome,
}

impl InjectedLearning {
    /// Create a new injected learning (pending outcome).
    pub fn new(learning_id: impl Into<String>, score: f64) -> Self {
        Self {
            learning_id: learning_id.into(),
            score,
            outcome: InjectionOutcome::Pending,
        }
    }

    /// Mark as referenced.
    pub fn mark_referenced(&mut self) {
        self.outcome = InjectionOutcome::Referenced;
    }

    /// Mark as dismissed.
    pub fn mark_dismissed(&mut self) {
        self.outcome = InjectionOutcome::Dismissed;
    }

    /// Mark as corrected.
    pub fn mark_corrected(&mut self) {
        self.outcome = InjectionOutcome::Corrected;
    }
}

/// Outcome of an injected learning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InjectionOutcome {
    /// Not yet determined.
    #[default]
    Pending,
    /// Learning was referenced during the session.
    Referenced,
    /// Learning was not used.
    Dismissed,
    /// Learning was corrected (superseded).
    Corrected,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_state_new() {
        let session = SessionState::new("test-id", "/tmp/project", "/tmp/transcript.json");

        assert_eq!(session.id, "test-id");
        assert_eq!(session.cwd, "/tmp/project");
        assert_eq!(session.transcript_path, "/tmp/transcript.json");
        assert_eq!(session.gate.status, GateStatus::Idle);
        assert!(session.ticket.is_none());
        assert!(session.trace.is_empty());
    }

    #[test]
    fn test_session_state_add_trace() {
        let mut session = SessionState::new("test-id", "/tmp", "/tmp/t.json");
        let old_updated = session.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(10));
        session.add_trace(EventType::SessionStart, Some("started".to_string()));

        assert_eq!(session.trace.len(), 1);
        assert_eq!(session.trace[0].event_type, EventType::SessionStart);
        assert!(session.updated_at > old_updated);
    }

    #[test]
    fn test_gate_state_default() {
        let gate = GateState::default();

        assert_eq!(gate.status, GateStatus::Idle);
        assert_eq!(gate.block_count, 0);
        assert!(!gate.circuit_breaker_tripped);
        assert!(gate.last_blocked_session_id.is_none());
        assert!(gate.reflection.is_none());
        assert!(gate.skip.is_none());
        assert!(gate.subagent_observations.is_empty());
        assert!(gate.injected_learnings.is_empty());
    }

    #[test]
    fn test_gate_status_is_terminal() {
        assert!(!GateStatus::Idle.is_terminal());
        assert!(!GateStatus::Active.is_terminal());
        assert!(!GateStatus::Pending.is_terminal());
        assert!(!GateStatus::Blocked.is_terminal());
        assert!(GateStatus::Reflected.is_terminal());
        assert!(GateStatus::Skipped.is_terminal());
    }

    #[test]
    fn test_gate_status_requires_reflection() {
        assert!(!GateStatus::Idle.requires_reflection());
        assert!(!GateStatus::Active.requires_reflection());
        assert!(GateStatus::Pending.requires_reflection());
        assert!(GateStatus::Blocked.requires_reflection());
        assert!(!GateStatus::Reflected.requires_reflection());
        assert!(!GateStatus::Skipped.requires_reflection());
    }

    #[test]
    fn test_ticket_context_new() {
        let ticket = TicketContext::new("TICKET-123", "tissue", "Fix the bug");

        assert_eq!(ticket.ticket_id, "TICKET-123");
        assert_eq!(ticket.source, "tissue");
        assert_eq!(ticket.title, "Fix the bug");
        assert!(ticket.description.is_none());
    }

    #[test]
    fn test_ticket_context_with_description() {
        let ticket = TicketContext::new("TICKET-123", "tissue", "Fix the bug")
            .with_description("A detailed description");

        assert_eq!(
            ticket.description,
            Some("A detailed description".to_string())
        );
    }

    #[test]
    fn test_ticket_close_intent() {
        let intent = TicketCloseIntent::new("TICKET-123", "tissue status TICKET-123 closed");

        assert_eq!(intent.ticket_id, "TICKET-123");
        assert_eq!(intent.command, "tissue status TICKET-123 closed");
    }

    #[test]
    fn test_subagent_observation() {
        let obs = SubagentObservation::new("Found a pattern in auth middleware");

        assert_eq!(obs.note, "Found a pattern in auth middleware");
    }

    #[test]
    fn test_circuit_breaker_record_block() {
        let mut cb = CircuitBreakerState::default();

        // First two blocks shouldn't trip (max_blocks = 3)
        assert!(!cb.record_block("session-1", 3));
        assert_eq!(cb.block_count, 1);
        assert!(!cb.tripped);

        assert!(!cb.record_block("session-1", 3));
        assert_eq!(cb.block_count, 2);
        assert!(!cb.tripped);

        // Third block should trip
        assert!(cb.record_block("session-1", 3));
        assert_eq!(cb.block_count, 3);
        assert!(cb.tripped);
    }

    #[test]
    fn test_circuit_breaker_should_reset_different_session() {
        let mut cb = CircuitBreakerState::default();
        cb.record_block("session-1", 3);

        // Different session should trigger reset
        assert!(cb.should_reset("session-2", 300));

        // Same session should not
        assert!(!cb.should_reset("session-1", 300));
    }

    #[test]
    fn test_circuit_breaker_reset() {
        let mut cb = CircuitBreakerState::default();
        cb.record_block("session-1", 3);
        cb.record_block("session-1", 3);
        cb.record_block("session-1", 3);

        assert!(cb.tripped);
        assert_eq!(cb.block_count, 3);

        cb.reset();

        assert!(!cb.tripped);
        assert_eq!(cb.block_count, 0);
        // last_blocked_session_id and last_blocked_at are preserved
        assert_eq!(cb.last_blocked_session_id, Some("session-1".to_string()));
    }

    #[test]
    fn test_trace_event() {
        let event = TraceEvent::new(EventType::SessionStart, Some("test".to_string()));

        assert_eq!(event.event_type, EventType::SessionStart);
        assert_eq!(event.details, Some("test".to_string()));
    }

    #[test]
    fn test_reflection_result() {
        let result = ReflectionResult::new(vec!["l1".to_string(), "l2".to_string()], 5, 2);

        assert_eq!(result.learning_ids.len(), 2);
        assert_eq!(result.candidates_produced, 5);
        assert_eq!(result.candidates_accepted, 2);
    }

    #[test]
    fn test_skip_decision() {
        let skip =
            SkipDecision::new("trivial change", SkipDecider::AutoThreshold).with_lines_changed(3);

        assert_eq!(skip.reason, "trivial change");
        assert_eq!(skip.decider, SkipDecider::AutoThreshold);
        assert_eq!(skip.lines_changed, Some(3));
    }

    #[test]
    fn test_injected_learning() {
        let mut learning = InjectedLearning::new("learning-1", 0.85);

        assert_eq!(learning.learning_id, "learning-1");
        assert!((learning.score - 0.85).abs() < f64::EPSILON);
        assert_eq!(learning.outcome, InjectionOutcome::Pending);

        learning.mark_referenced();
        assert_eq!(learning.outcome, InjectionOutcome::Referenced);
    }

    #[test]
    fn test_session_state_serialization() {
        let session = SessionState::new("test-id", "/tmp", "/tmp/t.json");

        let json = serde_json::to_string(&session).unwrap();
        let deserialized: SessionState = serde_json::from_str(&json).unwrap();

        assert_eq!(session.id, deserialized.id);
        assert_eq!(session.cwd, deserialized.cwd);
        assert_eq!(session.gate.status, deserialized.gate.status);
    }

    #[test]
    fn test_gate_status_serialization() {
        let statuses = vec![
            GateStatus::Idle,
            GateStatus::Active,
            GateStatus::Pending,
            GateStatus::Blocked,
            GateStatus::Reflected,
            GateStatus::Skipped,
        ];

        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let deserialized: GateStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, deserialized);
        }
    }

    #[test]
    fn test_event_type_serialization() {
        let events = vec![
            EventType::SessionStart,
            EventType::TicketDetected,
            EventType::GateBlocked,
            EventType::ReflectionComplete,
        ];

        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let deserialized: EventType = serde_json::from_str(&json).unwrap();
            assert_eq!(event, deserialized);
        }
    }

    #[test]
    fn test_skip_decider_serialization() {
        let deciders = vec![
            SkipDecider::Agent,
            SkipDecider::User,
            SkipDecider::AutoThreshold,
        ];

        for decider in deciders {
            let json = serde_json::to_string(&decider).unwrap();
            let deserialized: SkipDecider = serde_json::from_str(&json).unwrap();
            assert_eq!(decider, deserialized);
        }
    }

    #[test]
    fn test_injection_outcome_serialization() {
        let outcomes = vec![
            InjectionOutcome::Pending,
            InjectionOutcome::Referenced,
            InjectionOutcome::Dismissed,
            InjectionOutcome::Corrected,
        ];

        for outcome in outcomes {
            let json = serde_json::to_string(&outcome).unwrap();
            let deserialized: InjectionOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(outcome, deserialized);
        }
    }

    #[test]
    fn test_full_session_roundtrip() {
        let mut session = SessionState::new("test-session", "/project", "/transcript.json");
        session.ticket = Some(
            TicketContext::new("TICKET-1", "tissue", "Test ticket").with_description("Description"),
        );
        session.gate.status = GateStatus::Blocked;
        session.gate.block_count = 2;
        session
            .gate
            .injected_learnings
            .push(InjectedLearning::new("l1", 0.9));
        session
            .gate
            .subagent_observations
            .push(SubagentObservation::new("obs"));
        session.add_trace(EventType::SessionStart, None);
        session.add_trace(EventType::TicketDetected, Some("TICKET-1".to_string()));

        let json = serde_json::to_string_pretty(&session).unwrap();
        let deserialized: SessionState = serde_json::from_str(&json).unwrap();

        assert_eq!(session.id, deserialized.id);
        assert_eq!(session.gate.status, deserialized.gate.status);
        assert_eq!(session.gate.block_count, deserialized.gate.block_count);
        assert_eq!(
            session.gate.injected_learnings.len(),
            deserialized.gate.injected_learnings.len()
        );
        assert_eq!(
            session.gate.subagent_observations.len(),
            deserialized.gate.subagent_observations.len()
        );
        assert_eq!(session.trace.len(), deserialized.trace.len());
        assert!(deserialized.ticket.is_some());
    }
}
