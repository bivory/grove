//! Gate state machine for Grove.
//!
//! The gate enforces structured reflection at ticket boundaries. It tracks
//! session state and blocks exit when reflection is required.

use chrono::Utc;

use crate::config::Config;
use crate::core::state::{
    GateState, GateStatus, ReflectionResult, SkipDecider, SkipDecision, TicketCloseIntent,
    TicketContext,
};
use crate::error::{GroveError, Result};

/// Gate state machine.
///
/// Manages transitions between gate states and enforces the reflection
/// protocol. All state mutations go through this struct.
#[derive(Debug)]
pub struct Gate<'a> {
    /// The gate state being managed.
    state: &'a mut GateState,
    /// Configuration for gate behavior.
    config: &'a Config,
    /// Current session ID.
    session_id: String,
}

impl<'a> Gate<'a> {
    /// Create a new gate manager.
    pub fn new(
        state: &'a mut GateState,
        config: &'a Config,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            state,
            config,
            session_id: session_id.into(),
        }
    }

    /// Get the current gate status.
    pub fn status(&self) -> GateStatus {
        self.state.status
    }

    /// Check if the gate is in a terminal state (Reflected or Skipped).
    pub fn is_terminal(&self) -> bool {
        self.state.status.is_terminal()
    }

    /// Check if the gate requires reflection before exit.
    pub fn requires_reflection(&self) -> bool {
        self.state.status.requires_reflection()
    }

    // =========================================================================
    // Transitions
    // =========================================================================

    /// Transition: Idle → Active (ticket detected)
    ///
    /// Called when a ticket is detected via discovery.
    pub fn detect_ticket(&mut self, ticket: TicketContext) -> Result<()> {
        if self.state.status != GateStatus::Idle {
            return Err(GroveError::invalid_state(format!(
                "Cannot detect ticket in {} state",
                self.status_name()
            )));
        }

        self.state.ticket = Some(ticket);
        self.state.status = GateStatus::Active;
        Ok(())
    }

    /// Transition: Idle → Pending (session mode, non-trivial diff)
    ///
    /// Called when stop hook fires in session mode with non-trivial changes.
    pub fn enable_session_gate(&mut self, diff_size: u32) -> Result<()> {
        if self.state.status != GateStatus::Idle {
            return Err(GroveError::invalid_state(format!(
                "Cannot enable session gate in {} state",
                self.status_name()
            )));
        }

        self.state.cached_diff_size = Some(diff_size);
        self.state.status = GateStatus::Pending;
        Ok(())
    }

    /// Transition: Active → Pending (ticket closed)
    ///
    /// Called when a ticket close is confirmed via PostToolUse.
    pub fn confirm_ticket_close(&mut self) -> Result<()> {
        if self.state.status != GateStatus::Active {
            return Err(GroveError::invalid_state(format!(
                "Cannot confirm ticket close in {} state",
                self.status_name()
            )));
        }

        self.state.ticket_close_intent = None;
        self.state.status = GateStatus::Pending;
        Ok(())
    }

    /// Transition: Active → Idle (session ends without close)
    ///
    /// Called when session ends without completing the ticket.
    pub fn abandon_ticket(&mut self) -> Result<()> {
        if self.state.status != GateStatus::Active {
            return Err(GroveError::invalid_state(format!(
                "Cannot abandon ticket in {} state",
                self.status_name()
            )));
        }

        self.state.ticket_close_intent = None;
        self.state.ticket = None;
        self.state.status = GateStatus::Idle;
        Ok(())
    }

    /// Transition: Pending → Blocked (stop hook fires)
    ///
    /// Called when stop hook fires while reflection is pending.
    /// Returns whether the circuit breaker tripped (forcing approval).
    pub fn block(&mut self) -> Result<bool> {
        if self.state.status != GateStatus::Pending && self.state.status != GateStatus::Blocked {
            return Err(GroveError::invalid_state(format!(
                "Cannot block in {} state",
                self.status_name()
            )));
        }

        // If already blocked, don't increment counter again (prevents inflation
        // from rapid stop hook invocations)
        if self.state.status == GateStatus::Blocked {
            // Already blocked, just return current circuit breaker status
            return Ok(self.state.circuit_breaker_tripped);
        }

        // Check if circuit breaker should reset
        if self.should_reset_circuit_breaker() {
            self.reset_circuit_breaker();
        }

        // Increment block count (only on Pending → Blocked transition)
        self.state.block_count += 1;
        self.state.last_blocked_session_id = Some(self.session_id.clone());
        self.state.last_blocked_at = Some(Utc::now());

        // Check if circuit breaker should trip
        if self.state.block_count >= self.config.circuit_breaker.max_blocks {
            self.state.circuit_breaker_tripped = true;
            self.state.status = GateStatus::Idle;
            return Ok(true); // Circuit breaker tripped
        }

        self.state.status = GateStatus::Blocked;
        Ok(false)
    }

    /// Transition: Pending/Blocked → Skipped
    ///
    /// Called when reflection is skipped (auto or manual).
    pub fn skip(&mut self, reason: impl Into<String>, decider: SkipDecider) -> Result<()> {
        if !self.state.status.requires_reflection() {
            return Err(GroveError::invalid_state(format!(
                "Cannot skip in {} state",
                self.status_name()
            )));
        }

        let mut skip = SkipDecision::new(reason, decider);
        if let Some(lines) = self.state.cached_diff_size {
            skip = skip.with_lines_changed(lines);
        }

        self.state.skip = Some(skip);
        self.state.status = GateStatus::Skipped;
        self.reset_circuit_breaker();
        Ok(())
    }

    /// Transition: Pending/Blocked → Reflected (reflection completes)
    ///
    /// Called when structured reflection is successfully completed.
    /// Allows proactive reflection from Pending state (before stop hook fires).
    pub fn complete_reflection(&mut self, result: ReflectionResult) -> Result<()> {
        if !self.state.status.requires_reflection() {
            return Err(GroveError::invalid_state(format!(
                "Cannot complete reflection in {} state",
                self.status_name()
            )));
        }

        self.state.reflection = Some(result);
        self.state.status = GateStatus::Reflected;
        self.reset_circuit_breaker();
        Ok(())
    }

    /// Record a ticket close intent (PreToolUse).
    ///
    /// Does not change state - just records the intent for confirmation.
    pub fn record_close_intent(&mut self, intent: TicketCloseIntent) {
        self.state.ticket_close_intent = Some(intent);
    }

    /// Reset from a terminal state to Idle for a new ticket.
    ///
    /// Called when a new ticket close is detected after a previous reflection/skip.
    /// This allows multiple ticket closures in the same session to each trigger reflection.
    pub fn reset_for_new_ticket(&mut self) -> Result<()> {
        if !self.state.status.is_terminal() {
            return Err(GroveError::invalid_state(format!(
                "Cannot reset for new ticket in {} state (only from terminal states)",
                self.status_name()
            )));
        }

        // Clear previous reflection/skip data
        self.state.reflection = None;
        self.state.skip = None;
        self.state.ticket = None;
        self.state.ticket_close_intent = None;
        self.state.status = GateStatus::Idle;

        Ok(())
    }

    /// Clear the ticket close intent (e.g., on failure).
    pub fn clear_close_intent(&mut self) {
        self.state.ticket_close_intent = None;
    }

    /// Check if there's a pending ticket close intent.
    pub fn has_close_intent(&self) -> bool {
        self.state.ticket_close_intent.is_some()
    }

    /// Get the current ticket context, if any.
    pub fn ticket(&self) -> Option<&TicketContext> {
        self.state.ticket.as_ref()
    }

    // =========================================================================
    // Auto-skip evaluation
    // =========================================================================

    /// Evaluate whether auto-skip should apply.
    ///
    /// Returns Some(reason) if auto-skip should apply, None otherwise.
    pub fn evaluate_auto_skip(&self, diff_size: Option<u32>) -> Option<String> {
        if !self.config.gate.auto_skip.enabled {
            return None;
        }

        let threshold = self.config.gate.auto_skip.line_threshold;
        let decider = self.config.gate.auto_skip.decider.as_str();

        // "never" decider prevents all auto-skips
        if decider == "never" {
            return None;
        }

        match (diff_size, decider) {
            (Some(lines), _) if lines < threshold => Some(format!(
                "auto: {} lines changed (threshold: {})",
                lines, threshold
            )),
            // When diff is unavailable, we can't determine if it's a small change
            // Don't auto-skip for either decider - require explicit decision
            (None, _) => None,
            _ => None,
        }
    }

    // =========================================================================
    // Circuit breaker
    // =========================================================================

    /// Check if the circuit breaker should reset.
    fn should_reset_circuit_breaker(&self) -> bool {
        // Condition 1: Different session ID
        if let Some(ref last_session) = self.state.last_blocked_session_id {
            if last_session != &self.session_id {
                return true;
            }
        }

        // Condition 2: Cooldown elapsed
        if let Some(last_blocked) = self.state.last_blocked_at {
            let elapsed = Utc::now().signed_duration_since(last_blocked);
            if elapsed.num_seconds() >= self.config.circuit_breaker.cooldown_seconds as i64 {
                return true;
            }
        }

        false
    }

    /// Reset the circuit breaker state.
    fn reset_circuit_breaker(&mut self) {
        self.state.block_count = 0;
        self.state.circuit_breaker_tripped = false;
        self.state.last_blocked_at = None;
        self.state.last_blocked_session_id = None;
        // Clear all circuit breaker state for a fresh start
    }

    /// Get a human-readable name for the current status.
    fn status_name(&self) -> &'static str {
        match self.state.status {
            GateStatus::Idle => "Idle",
            GateStatus::Active => "Active",
            GateStatus::Pending => "Pending",
            GateStatus::Blocked => "Blocked",
            GateStatus::Reflected => "Reflected",
            GateStatus::Skipped => "Skipped",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn default_config() -> Config {
        Config::default()
    }

    fn config_with_max_blocks(max_blocks: u32) -> Config {
        let mut config = Config::default();
        config.circuit_breaker.max_blocks = max_blocks;
        config
    }

    fn config_with_cooldown(seconds: u32) -> Config {
        let mut config = Config::default();
        config.circuit_breaker.cooldown_seconds = seconds;
        config
    }

    fn config_with_auto_skip(enabled: bool, threshold: u32, decider: &str) -> Config {
        let mut config = Config::default();
        config.gate.auto_skip.enabled = enabled;
        config.gate.auto_skip.line_threshold = threshold;
        config.gate.auto_skip.decider = decider.to_string();
        config
    }

    // =========================================================================
    // Basic transitions
    // =========================================================================

    #[test]
    fn test_initial_state_is_idle() {
        let state = GateState::default();
        assert_eq!(state.status, GateStatus::Idle);
    }

    #[test]
    fn test_detect_ticket_idle_to_active() {
        let mut state = GateState::default();
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let ticket = TicketContext::new("TICKET-1", "tissue", "Test ticket");
        gate.detect_ticket(ticket).unwrap();

        assert_eq!(gate.status(), GateStatus::Active);
        assert!(gate.ticket().is_some());
        assert_eq!(gate.ticket().unwrap().ticket_id, "TICKET-1");
    }

    #[test]
    fn test_detect_ticket_fails_when_not_idle() {
        let mut state = GateState {
            status: GateStatus::Active,
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let ticket = TicketContext::new("TICKET-1", "tissue", "Test ticket");
        let result = gate.detect_ticket(ticket);

        assert!(result.is_err());
    }

    #[test]
    fn test_enable_session_gate_idle_to_pending() {
        let mut state = GateState::default();
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        gate.enable_session_gate(10).unwrap();

        assert_eq!(gate.status(), GateStatus::Pending);
        assert_eq!(state.cached_diff_size, Some(10));
    }

    #[test]
    fn test_confirm_ticket_close_active_to_pending() {
        let mut state = GateState {
            status: GateStatus::Active,
            ticket_close_intent: Some(TicketCloseIntent::new("TICKET-1", "tissue status closed")),
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        gate.confirm_ticket_close().unwrap();

        assert_eq!(gate.status(), GateStatus::Pending);
        assert!(state.ticket_close_intent.is_none());
    }

    #[test]
    fn test_abandon_ticket_active_to_idle() {
        let mut state = GateState {
            status: GateStatus::Active,
            ticket: Some(TicketContext::new("TICKET-1", "tissue", "Test")),
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        gate.abandon_ticket().unwrap();

        assert_eq!(gate.status(), GateStatus::Idle);
        assert!(gate.ticket().is_none()); // Ticket should be cleared
    }

    #[test]
    fn test_block_pending_to_blocked() {
        let mut state = GateState {
            status: GateStatus::Pending,
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let tripped = gate.block().unwrap();

        assert!(!tripped);
        assert_eq!(gate.status(), GateStatus::Blocked);
        assert_eq!(state.block_count, 1);
        assert!(state.last_blocked_at.is_some());
        assert_eq!(state.last_blocked_session_id, Some("session-1".to_string()));
    }

    #[test]
    fn test_skip_pending_to_skipped() {
        let mut state = GateState {
            status: GateStatus::Pending,
            cached_diff_size: Some(5),
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        gate.skip("trivial change", SkipDecider::AutoThreshold)
            .unwrap();

        assert_eq!(gate.status(), GateStatus::Skipped);
        assert!(state.skip.is_some());
        let skip = state.skip.as_ref().unwrap();
        assert_eq!(skip.reason, "trivial change");
        assert_eq!(skip.decider, SkipDecider::AutoThreshold);
        assert_eq!(skip.lines_changed, Some(5));
    }

    #[test]
    fn test_skip_blocked_to_skipped() {
        let mut state = GateState {
            status: GateStatus::Blocked,
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        gate.skip("user requested", SkipDecider::User).unwrap();

        assert_eq!(gate.status(), GateStatus::Skipped);
    }

    #[test]
    fn test_complete_reflection_blocked_to_reflected() {
        let mut state = GateState {
            status: GateStatus::Blocked,
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let result = ReflectionResult::new(vec!["l1".to_string()], 3, 1);
        gate.complete_reflection(result).unwrap();

        assert_eq!(gate.status(), GateStatus::Reflected);
        assert!(state.reflection.is_some());
    }

    // =========================================================================
    // Circuit breaker tests
    // =========================================================================

    #[test]
    fn test_circuit_breaker_trips_at_max_blocks() {
        let mut state = GateState::default();
        let config = config_with_max_blocks(3);

        // First block doesn't trip
        state.status = GateStatus::Pending;
        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            assert!(!gate.block().unwrap());
        }

        // Second block doesn't trip
        state.status = GateStatus::Pending;
        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            assert!(!gate.block().unwrap());
        }

        // Third block trips
        state.status = GateStatus::Pending;
        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            assert!(gate.block().unwrap());
            assert_eq!(gate.status(), GateStatus::Idle);
        }
        assert!(state.circuit_breaker_tripped);
    }

    #[test]
    fn test_circuit_breaker_resets_on_different_session() {
        let mut state = GateState {
            status: GateStatus::Pending,
            block_count: 2,
            last_blocked_session_id: Some("session-1".to_string()),
            last_blocked_at: Some(Utc::now()),
            ..Default::default()
        };
        let config = config_with_max_blocks(3);

        // New session should reset the counter
        let mut gate = Gate::new(&mut state, &config, "session-2");
        let tripped = gate.block().unwrap();

        assert!(!tripped);
        assert_eq!(state.block_count, 1); // Counter was reset before incrementing
    }

    #[test]
    fn test_circuit_breaker_resets_on_cooldown() {
        // Set last_blocked_at to past the cooldown
        let mut state = GateState {
            status: GateStatus::Pending,
            block_count: 2,
            last_blocked_session_id: Some("session-1".to_string()),
            last_blocked_at: Some(Utc::now() - Duration::seconds(400)),
            ..Default::default()
        };
        let config = config_with_cooldown(300);

        // Same session but cooldown elapsed
        let mut gate = Gate::new(&mut state, &config, "session-1");
        let tripped = gate.block().unwrap();

        assert!(!tripped);
        assert_eq!(state.block_count, 1); // Counter was reset
    }

    #[test]
    fn test_circuit_breaker_does_not_reset_within_cooldown() {
        // Set last_blocked_at to within cooldown
        let mut state = GateState {
            status: GateStatus::Pending,
            block_count: 2,
            last_blocked_session_id: Some("session-1".to_string()),
            last_blocked_at: Some(Utc::now() - Duration::seconds(100)),
            ..Default::default()
        };
        let config = config_with_cooldown(300);

        // Same session, within cooldown - should trip on next block
        let mut gate = Gate::new(&mut state, &config, "session-1");
        let tripped = gate.block().unwrap();

        assert!(tripped);
        assert_eq!(state.block_count, 3);
    }

    #[test]
    fn test_circuit_breaker_resets_on_reflection() {
        let mut state = GateState {
            status: GateStatus::Blocked,
            block_count: 2,
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let result = ReflectionResult::new(vec![], 0, 0);
        gate.complete_reflection(result).unwrap();

        assert_eq!(state.block_count, 0);
        assert!(!state.circuit_breaker_tripped);
    }

    #[test]
    fn test_circuit_breaker_resets_on_skip() {
        let mut state = GateState {
            status: GateStatus::Blocked,
            block_count: 2,
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        gate.skip("user decided", SkipDecider::User).unwrap();

        assert_eq!(state.block_count, 0);
        assert!(!state.circuit_breaker_tripped);
    }

    // =========================================================================
    // Auto-skip evaluation
    // =========================================================================

    #[test]
    fn test_auto_skip_under_threshold() {
        let config = config_with_auto_skip(true, 10, "agent");
        let mut state = GateState::default();
        let gate = Gate::new(&mut state, &config, "session-1");

        let reason = gate.evaluate_auto_skip(Some(5));
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("5 lines"));
    }

    #[test]
    fn test_auto_skip_at_threshold() {
        let config = config_with_auto_skip(true, 10, "agent");
        let mut state = GateState::default();
        let gate = Gate::new(&mut state, &config, "session-1");

        let reason = gate.evaluate_auto_skip(Some(10));
        assert!(reason.is_none());
    }

    #[test]
    fn test_auto_skip_over_threshold() {
        let config = config_with_auto_skip(true, 10, "agent");
        let mut state = GateState::default();
        let gate = Gate::new(&mut state, &config, "session-1");

        let reason = gate.evaluate_auto_skip(Some(15));
        assert!(reason.is_none());
    }

    #[test]
    fn test_auto_skip_disabled() {
        let config = config_with_auto_skip(false, 10, "agent");
        let mut state = GateState::default();
        let gate = Gate::new(&mut state, &config, "session-1");

        let reason = gate.evaluate_auto_skip(Some(5));
        assert!(reason.is_none());
    }

    #[test]
    fn test_auto_skip_no_diff_agent_decides() {
        let config = config_with_auto_skip(true, 10, "agent");
        let mut state = GateState::default();
        let gate = Gate::new(&mut state, &config, "session-1");

        let reason = gate.evaluate_auto_skip(None);
        assert!(reason.is_none()); // Agent decides
    }

    #[test]
    fn test_auto_skip_no_diff_always() {
        // When diff is unavailable, we can't determine if it's a small change
        // Don't auto-skip - require explicit decision (fail-safe, not fail-open)
        let config = config_with_auto_skip(true, 10, "always");
        let mut state = GateState::default();
        let gate = Gate::new(&mut state, &config, "session-1");

        let reason = gate.evaluate_auto_skip(None);
        assert!(
            reason.is_none(),
            "Should not auto-skip when diff is unavailable - require explicit decision"
        );
    }

    #[test]
    fn test_auto_skip_decider_never() {
        let config = config_with_auto_skip(true, 10, "never");
        let mut state = GateState::default();
        let gate = Gate::new(&mut state, &config, "session-1");

        // decider="never" prevents auto-skip even when under threshold
        let reason = gate.evaluate_auto_skip(Some(5));
        assert!(reason.is_none());
    }

    // =========================================================================
    // Intent tracking
    // =========================================================================

    #[test]
    fn test_record_and_clear_close_intent() {
        let mut state = GateState::default();
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        assert!(!gate.has_close_intent());

        let intent = TicketCloseIntent::new("TICKET-1", "close command");
        gate.record_close_intent(intent);

        assert!(gate.has_close_intent());

        gate.clear_close_intent();

        assert!(!gate.has_close_intent());
    }

    // =========================================================================
    // Invalid transitions
    // =========================================================================

    #[test]
    fn test_enable_session_gate_fails_when_active() {
        let mut state = GateState {
            status: GateStatus::Active,
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let result = gate.enable_session_gate(10);
        assert!(result.is_err());
    }

    #[test]
    fn test_confirm_close_fails_when_idle() {
        let mut state = GateState::default();
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let result = gate.confirm_ticket_close();
        assert!(result.is_err());
    }

    #[test]
    fn test_abandon_fails_when_idle() {
        let mut state = GateState::default();
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let result = gate.abandon_ticket();
        assert!(result.is_err());
    }

    #[test]
    fn test_block_fails_when_idle() {
        let mut state = GateState::default();
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let result = gate.block();
        assert!(result.is_err());
    }

    #[test]
    fn test_reblock_does_not_increment_counter() {
        let mut state = GateState {
            status: GateStatus::Blocked,
            block_count: 1,
            ..Default::default()
        };
        let config = config_with_max_blocks(3);

        // First re-block should not increment counter
        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            let tripped = gate.block().unwrap();
            assert!(!tripped);
            assert_eq!(gate.status(), GateStatus::Blocked);
        }
        assert_eq!(state.block_count, 1); // Counter should still be 1

        // Multiple re-blocks should not increment counter
        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            let tripped = gate.block().unwrap();
            assert!(!tripped);
        }
        assert_eq!(state.block_count, 1); // Counter should still be 1

        // Circuit breaker should not trip from re-blocking
        assert!(!state.circuit_breaker_tripped);
    }

    #[test]
    fn test_reblock_returns_circuit_breaker_status() {
        let mut state = GateState {
            status: GateStatus::Blocked,
            block_count: 2,
            circuit_breaker_tripped: true, // Simulate previously tripped
            ..Default::default()
        };
        let config = config_with_max_blocks(3);

        let mut gate = Gate::new(&mut state, &config, "session-1");
        // Re-blocking should return true because circuit breaker was tripped
        let tripped = gate.block().unwrap();
        assert!(tripped);
    }

    #[test]
    fn test_skip_fails_when_idle() {
        let mut state = GateState::default();
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let result = gate.skip("reason", SkipDecider::User);
        assert!(result.is_err());
    }

    #[test]
    fn test_complete_reflection_succeeds_from_pending() {
        let mut state = GateState {
            status: GateStatus::Pending,
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let result = ReflectionResult::new(vec![], 0, 0);
        // Proactive reflection from Pending should now succeed
        let res = gate.complete_reflection(result);
        assert!(res.is_ok());
        assert_eq!(gate.status(), GateStatus::Reflected);
    }

    #[test]
    fn test_complete_reflection_fails_when_idle() {
        let mut state = GateState {
            status: GateStatus::Idle,
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let result = ReflectionResult::new(vec![], 0, 0);
        let err = gate.complete_reflection(result);
        assert!(err.is_err());
    }

    // =========================================================================
    // Helper methods
    // =========================================================================

    #[test]
    fn test_is_terminal() {
        let mut state = GateState::default();
        let config = default_config();

        state.status = GateStatus::Idle;
        let gate = Gate::new(&mut state, &config, "s");
        assert!(!gate.is_terminal());

        state.status = GateStatus::Active;
        let gate = Gate::new(&mut state, &config, "s");
        assert!(!gate.is_terminal());

        state.status = GateStatus::Pending;
        let gate = Gate::new(&mut state, &config, "s");
        assert!(!gate.is_terminal());

        state.status = GateStatus::Blocked;
        let gate = Gate::new(&mut state, &config, "s");
        assert!(!gate.is_terminal());

        state.status = GateStatus::Reflected;
        let gate = Gate::new(&mut state, &config, "s");
        assert!(gate.is_terminal());

        state.status = GateStatus::Skipped;
        let gate = Gate::new(&mut state, &config, "s");
        assert!(gate.is_terminal());
    }

    #[test]
    fn test_requires_reflection() {
        let mut state = GateState::default();
        let config = default_config();

        state.status = GateStatus::Idle;
        let gate = Gate::new(&mut state, &config, "s");
        assert!(!gate.requires_reflection());

        state.status = GateStatus::Pending;
        let gate = Gate::new(&mut state, &config, "s");
        assert!(gate.requires_reflection());

        state.status = GateStatus::Blocked;
        let gate = Gate::new(&mut state, &config, "s");
        assert!(gate.requires_reflection());
    }

    // =========================================================================
    // Full flow tests
    // =========================================================================

    #[test]
    fn test_full_ticket_flow_with_reflection() {
        let mut state = GateState::default();
        let config = default_config();

        // Start session
        let mut gate = Gate::new(&mut state, &config, "session-1");
        assert_eq!(gate.status(), GateStatus::Idle);

        // Detect ticket
        let ticket = TicketContext::new("TICKET-1", "tissue", "Fix bug");
        gate.detect_ticket(ticket).unwrap();
        assert_eq!(gate.status(), GateStatus::Active);

        // Record close intent
        let intent = TicketCloseIntent::new("TICKET-1", "tissue status TICKET-1 closed");
        gate.record_close_intent(intent);
        assert!(gate.has_close_intent());

        // Confirm close
        gate.confirm_ticket_close().unwrap();
        assert_eq!(gate.status(), GateStatus::Pending);

        // Stop hook fires - block
        let tripped = gate.block().unwrap();
        assert!(!tripped);
        assert_eq!(gate.status(), GateStatus::Blocked);

        // Complete reflection
        let result = ReflectionResult::new(vec!["learning-1".to_string()], 5, 1);
        gate.complete_reflection(result).unwrap();
        assert_eq!(gate.status(), GateStatus::Reflected);
        assert!(gate.is_terminal());
    }

    #[test]
    fn test_session_mode_flow_with_skip() {
        let mut state = GateState::default();
        let config = config_with_auto_skip(true, 10, "agent");

        let mut gate = Gate::new(&mut state, &config, "session-1");
        assert_eq!(gate.status(), GateStatus::Idle);

        // Session mode with small diff
        gate.enable_session_gate(5).unwrap();
        assert_eq!(gate.status(), GateStatus::Pending);

        // Check auto-skip
        let reason = gate.evaluate_auto_skip(Some(5));
        assert!(reason.is_some());

        // Skip
        gate.skip(reason.unwrap(), SkipDecider::AutoThreshold)
            .unwrap();
        assert_eq!(gate.status(), GateStatus::Skipped);
        assert!(gate.is_terminal());
    }

    #[test]
    fn test_circuit_breaker_flow() {
        let mut state = GateState::default();
        let config = config_with_max_blocks(2);

        // First block
        state.status = GateStatus::Pending;
        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            let tripped = gate.block().unwrap();
            assert!(!tripped);
        }
        assert_eq!(state.block_count, 1);

        // Second block - trips
        state.status = GateStatus::Pending;
        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            let tripped = gate.block().unwrap();
            assert!(tripped);
            assert_eq!(gate.status(), GateStatus::Idle);
        }
        assert!(state.circuit_breaker_tripped);
    }

    // =========================================================================
    // Property-based tests
    // =========================================================================

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        fn arb_gate_status() -> impl Strategy<Value = GateStatus> {
            prop_oneof![
                Just(GateStatus::Idle),
                Just(GateStatus::Active),
                Just(GateStatus::Pending),
                Just(GateStatus::Blocked),
                Just(GateStatus::Reflected),
                Just(GateStatus::Skipped),
            ]
        }

        proptest! {
            // Property: Terminal states are never reflection-requiring
            #[test]
            fn prop_terminal_never_requires_reflection(status in arb_gate_status()) {
                if status.is_terminal() {
                    prop_assert!(!status.requires_reflection());
                }
            }

            // Property: Only Pending and Blocked require reflection
            #[test]
            fn prop_only_pending_blocked_require_reflection(status in arb_gate_status()) {
                let requires = status.requires_reflection();
                let expected = matches!(status, GateStatus::Pending | GateStatus::Blocked);
                prop_assert_eq!(requires, expected);
            }

            // Property: Block count is monotonically increasing until reset
            #[test]
            fn prop_block_increments_on_block(
                initial_count in 0u32..10,
                max_blocks in 3u32..20,
            ) {
                let mut state = GateState {
                    status: GateStatus::Pending,
                    block_count: initial_count,
                    ..Default::default()
                };
                let mut config = Config::default();
                config.circuit_breaker.max_blocks = max_blocks;

                let mut gate = Gate::new(&mut state, &config, "session-1");
                let tripped = gate.block().unwrap();

                if initial_count + 1 >= max_blocks {
                    // Circuit breaker trips
                    prop_assert!(tripped);
                    prop_assert!(state.circuit_breaker_tripped);
                } else {
                    // Counter incremented
                    prop_assert!(!tripped);
                    prop_assert_eq!(state.block_count, initial_count + 1);
                }
            }

            // Property: Skip always transitions to Skipped from valid states
            #[test]
            fn prop_skip_from_valid_states_succeeds(status in arb_gate_status()) {
                let valid_for_skip = matches!(status, GateStatus::Pending | GateStatus::Blocked);
                let mut state = GateState {
                    status,
                    ..Default::default()
                };
                let config = Config::default();
                let mut gate = Gate::new(&mut state, &config, "session-1");

                let result = gate.skip("test reason", SkipDecider::User);
                prop_assert_eq!(result.is_ok(), valid_for_skip);
                if result.is_ok() {
                    prop_assert_eq!(state.status, GateStatus::Skipped);
                }
            }

            // Property: Reflection always transitions to Reflected from Blocked
            #[test]
            fn prop_reflection_from_blocked_succeeds(
                candidates in 0u32..100,
                accepted in 0u32..100,
            ) {
                let mut state = GateState {
                    status: GateStatus::Blocked,
                    block_count: 5,
                    ..Default::default()
                };
                let config = Config::default();
                let mut gate = Gate::new(&mut state, &config, "session-1");

                let result = ReflectionResult::new(vec![], candidates, accepted);
                gate.complete_reflection(result).unwrap();

                prop_assert_eq!(state.status, GateStatus::Reflected);
                prop_assert_eq!(state.block_count, 0); // Reset on reflection
            }

            // Property: detect_ticket only succeeds from Idle
            #[test]
            fn prop_detect_ticket_only_from_idle(status in arb_gate_status()) {
                let mut state = GateState {
                    status,
                    ..Default::default()
                };
                let config = Config::default();
                let mut gate = Gate::new(&mut state, &config, "session-1");

                let ticket = TicketContext::new("T-1", "tissue", "Test");
                let result = gate.detect_ticket(ticket);

                prop_assert_eq!(result.is_ok(), status == GateStatus::Idle);
            }
        }
    }

    // =========================================================================
    // Reset for new ticket tests
    // =========================================================================

    #[test]
    fn test_reset_for_new_ticket_from_reflected() {
        let mut state = GateState {
            status: GateStatus::Reflected,
            reflection: Some(ReflectionResult::new(vec!["l1".to_string()], 5, 1)),
            ticket: Some(TicketContext::new("OLD-1", "tissue", "Old ticket")),
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        gate.reset_for_new_ticket().unwrap();

        assert_eq!(gate.status(), GateStatus::Idle);
        assert!(state.reflection.is_none());
        assert!(state.ticket.is_none());
    }

    #[test]
    fn test_reset_for_new_ticket_from_skipped() {
        let mut state = GateState {
            status: GateStatus::Skipped,
            skip: Some(SkipDecision::new("trivial", SkipDecider::User)),
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        gate.reset_for_new_ticket().unwrap();

        assert_eq!(gate.status(), GateStatus::Idle);
        assert!(state.skip.is_none());
    }

    #[test]
    fn test_reset_for_new_ticket_fails_from_idle() {
        let mut state = GateState::default();
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let result = gate.reset_for_new_ticket();
        assert!(result.is_err());
    }

    #[test]
    fn test_reset_for_new_ticket_fails_from_pending() {
        let mut state = GateState {
            status: GateStatus::Pending,
            ..Default::default()
        };
        let config = default_config();
        let mut gate = Gate::new(&mut state, &config, "session-1");

        let result = gate.reset_for_new_ticket();
        assert!(result.is_err());
    }

    #[test]
    fn test_full_multi_ticket_flow() {
        let mut state = GateState::default();
        let config = default_config();

        // First ticket: Idle -> Active -> Pending -> Blocked -> Reflected
        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            let ticket1 = TicketContext::new("TICKET-1", "tissue", "First ticket");
            gate.detect_ticket(ticket1).unwrap();
            assert_eq!(gate.status(), GateStatus::Active);
        }

        state.ticket_close_intent = Some(TicketCloseIntent::new(
            "TICKET-1",
            "tissue status TICKET-1 closed",
        ));
        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            gate.confirm_ticket_close().unwrap();
            assert_eq!(gate.status(), GateStatus::Pending);
        }

        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            gate.block().unwrap();
            assert_eq!(gate.status(), GateStatus::Blocked);
        }

        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            let result = ReflectionResult::new(vec!["l1".to_string()], 3, 1);
            gate.complete_reflection(result).unwrap();
            assert_eq!(gate.status(), GateStatus::Reflected);
        }

        // Second ticket: Reset -> Idle -> Active -> Pending -> Blocked -> Reflected
        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            gate.reset_for_new_ticket().unwrap();
            assert_eq!(gate.status(), GateStatus::Idle);

            let ticket2 = TicketContext::new("TICKET-2", "tissue", "Second ticket");
            gate.detect_ticket(ticket2).unwrap();
            assert_eq!(gate.status(), GateStatus::Active);
        }

        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            gate.confirm_ticket_close().unwrap();
            assert_eq!(gate.status(), GateStatus::Pending);
        }

        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            gate.block().unwrap();
            assert_eq!(gate.status(), GateStatus::Blocked);
        }

        {
            let mut gate = Gate::new(&mut state, &config, "session-1");
            let result = ReflectionResult::new(vec!["l2".to_string()], 2, 1);
            gate.complete_reflection(result).unwrap();
            assert_eq!(gate.status(), GateStatus::Reflected);
        }
    }
}
