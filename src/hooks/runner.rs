//! Hook runner for Grove.
//!
//! This module implements the hook dispatch and individual hook handlers.
//! Hooks integrate with Claude Code at key points in the session lifecycle.

use std::io::{self, Read};
use std::path::Path;

use crate::config::{project_stats_log_path, Config};
use crate::core::gate::Gate;
use crate::core::state::{
    EventType, GateStatus, SessionState, SkipDecider, TicketCloseIntent, TicketContext,
};
use crate::discovery::{detect_backends, detect_ticketing_system, match_close_command};
use crate::error::{GroveError, Result};
use crate::hooks::input::{
    parse_input, HookInput, PostToolUseInput, PreToolUseInput, SessionEndInput,
};
use crate::hooks::output::{PreToolUseOutput, SessionEndOutput, SessionStartOutput, StopOutput};
use crate::stats::StatsLogger;
use crate::storage::SessionStore;

/// Hook type enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookType {
    /// Session start hook.
    SessionStart,
    /// Pre-tool-use hook.
    PreToolUse,
    /// Post-tool-use hook.
    PostToolUse,
    /// Stop hook.
    Stop,
    /// Session end hook.
    SessionEnd,
}

impl HookType {
    /// Parse hook type from string.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "session-start" | "sessionstart" | "session_start" => Some(Self::SessionStart),
            "pre-tool-use" | "pretooluse" | "pre_tool_use" => Some(Self::PreToolUse),
            "post-tool-use" | "posttooluse" | "post_tool_use" => Some(Self::PostToolUse),
            "stop" => Some(Self::Stop),
            "session-end" | "sessionend" | "session_end" => Some(Self::SessionEnd),
            _ => None,
        }
    }
}

/// Hook runner context.
pub struct HookRunner<S: SessionStore> {
    /// Session storage.
    store: S,
    /// Configuration.
    config: Config,
}

impl<S: SessionStore> HookRunner<S> {
    /// Create a new hook runner.
    pub fn new(store: S, config: Config) -> Self {
        Self { store, config }
    }

    /// Run a hook with input from stdin.
    pub fn run(&self, hook_type: HookType) -> Result<String> {
        let input = read_stdin()?;
        self.run_with_input(hook_type, &input)
    }

    /// Run a hook with provided input.
    pub fn run_with_input(&self, hook_type: HookType, input: &str) -> Result<String> {
        match hook_type {
            HookType::SessionStart => self.handle_session_start(input),
            HookType::PreToolUse => self.handle_pre_tool_use(input),
            HookType::PostToolUse => self.handle_post_tool_use(input),
            HookType::Stop => self.handle_stop(input),
            HookType::SessionEnd => self.handle_session_end(input),
        }
    }

    // =========================================================================
    // Session Start Handler
    // =========================================================================

    /// Handle the session-start hook.
    ///
    /// 1. Parse input and create/load session
    /// 2. Discover ticketing system
    /// 3. Discover memory backends
    /// 4. Search for relevant learnings
    /// 5. Record surfaced events and return context
    fn handle_session_start(&self, input: &str) -> Result<String> {
        let hook_input: HookInput = parse_input(input)?;
        let cwd = Path::new(&hook_input.cwd);

        // Create or load session
        let mut session = self.get_or_create_session(&hook_input)?;
        session.add_trace(EventType::SessionStart, None);

        // Discover ticketing system
        let ticketing_info = detect_ticketing_system(cwd, Some(&self.config));
        session.add_trace(
            EventType::TicketDetected,
            Some(format!("system: {}", ticketing_info.system)),
        );

        // Discover memory backends
        let backends = detect_backends(cwd, Some(&self.config));
        if let Some(primary) = backends.first() {
            session.add_trace(
                EventType::BackendDetected,
                Some(format!("backend: {}", primary.backend_type)),
            );
        }

        // Search for relevant learnings and inject context
        let mut additional_context: Option<String> = None;
        let injected_count = session.gate.injected_learnings.len();

        // For now, we just record that we would inject learnings
        // Full implementation requires backend search which is Stage 2
        if injected_count > 0 {
            session.add_trace(
                EventType::LearningsInjected,
                Some(format!("count: {}", injected_count)),
            );
        }

        // Decay check is handled separately via grove maintain (Stage 2)
        // We don't run it here to keep session-start fast

        // Save session state
        let _ = self.store.put(&session);

        // Build output
        let output = if let Some(context) = additional_context.take() {
            SessionStartOutput::with_context(context)
        } else {
            SessionStartOutput::empty()
        };

        crate::hooks::output::to_json(&output)
    }

    // =========================================================================
    // Pre-Tool-Use Handler
    // =========================================================================

    /// Handle the pre-tool-use hook.
    ///
    /// 1. Match tool against ticket close patterns
    /// 2. If match, record intent in session state
    /// 3. Always allow the tool to proceed
    fn handle_pre_tool_use(&self, input: &str) -> Result<String> {
        let hook_input: PreToolUseInput = parse_input(input)?;

        // Load session (fail-open if not found)
        let session_result = self.store.get(&hook_input.common.session_id);
        let mut session = match session_result {
            Ok(Some(s)) => s,
            _ => {
                // Fail-open: allow tool if session not found
                let output = PreToolUseOutput::allow();
                return crate::hooks::output::to_json(&output);
            }
        };

        // Check for ticket close command
        let command = hook_input
            .tool_input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if let Some(close_pattern) = match_close_command(&hook_input.tool_name, command) {
            // Extract ticket ID from command (simplified extraction)
            let ticket_id = extract_ticket_id(command).unwrap_or_else(|| "unknown".to_string());

            // Record intent
            let intent = TicketCloseIntent::new(&ticket_id, command);
            let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);
            gate.record_close_intent(intent);

            session.add_trace(
                EventType::TicketCloseDetected,
                Some(format!(
                    "pattern: {:?}, ticket: {}",
                    close_pattern, ticket_id
                )),
            );

            // Save session
            let _ = self.store.put(&session);
        }

        // Always allow the tool to proceed
        let output = PreToolUseOutput::allow();
        crate::hooks::output::to_json(&output)
    }

    // =========================================================================
    // Post-Tool-Use Handler
    // =========================================================================

    /// Handle the post-tool-use hook.
    ///
    /// 1. Check if a ticket close intent was recorded
    /// 2. If successful, transition gate to Pending
    /// 3. If failed, clear the intent
    fn handle_post_tool_use(&self, input: &str) -> Result<String> {
        let hook_input: PostToolUseInput = parse_input(input)?;

        // Load session (fail-open if not found)
        let session_result = self.store.get(&hook_input.common.session_id);
        let mut session = match session_result {
            Ok(Some(s)) => s,
            _ => {
                // Fail-open: empty output if session not found
                let output = crate::hooks::output::PostToolUseOutput::empty();
                return crate::hooks::output::to_json(&output);
            }
        };

        // Check if there's a pending ticket close intent
        if session.gate.ticket_close_intent.is_some() {
            // Check if the command succeeded by looking at the response
            // A simple heuristic: if response contains "error" or exit code is non-zero
            let success = !hook_input.tool_response.to_lowercase().contains("error")
                && !hook_input.tool_response.contains("exit code");

            // Capture the current status before borrowing gate mutably
            let current_status = session.gate.status;

            if success {
                // Transition gate based on current state
                if current_status == GateStatus::Active {
                    let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);
                    let _ = gate.confirm_ticket_close();
                    session.add_trace(EventType::TicketClosed, None);
                } else if current_status == GateStatus::Idle {
                    // If in Idle, we need to first detect a ticket context
                    // Extract intent before borrowing gate
                    let intent = session.gate.ticket_close_intent.take();
                    if let Some(intent) = intent {
                        let ticket =
                            TicketContext::new(&intent.ticket_id, "detected", "Ticket closed");
                        let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);
                        let _ = gate.detect_ticket(ticket);
                        let _ = gate.confirm_ticket_close();
                        session.add_trace(EventType::TicketClosed, None);
                    }
                }
            } else {
                let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);
                gate.clear_close_intent();
                session.add_trace(EventType::TicketCloseFailed, None);
            }

            // Save session
            let _ = self.store.put(&session);
        }

        let output = crate::hooks::output::PostToolUseOutput::empty();
        crate::hooks::output::to_json(&output)
    }

    // =========================================================================
    // Stop Handler
    // =========================================================================

    /// Handle the stop hook.
    ///
    /// 1. If gate is terminal (Reflected/Skipped), approve
    /// 2. If gate is Idle, check auto-skip conditions
    /// 3. If gate requires reflection, block with instructions
    /// 4. Apply circuit breaker if needed
    fn handle_stop(&self, input: &str) -> Result<String> {
        let hook_input: HookInput = parse_input(input)?;

        // Load session (fail-open if not found)
        let session_result = self.store.get(&hook_input.session_id);
        let mut session = match session_result {
            Ok(Some(s)) => s,
            _ => {
                // Fail-open: approve if session not found
                let output = StopOutput::approve();
                return crate::hooks::output::to_json(&output);
            }
        };

        session.add_trace(EventType::StopHookCalled, None);

        // Check terminal states first
        if session.gate.status.is_terminal() {
            let _ = self.store.put(&session);
            let output = StopOutput::approve();
            return crate::hooks::output::to_json(&output);
        }

        // Handle Idle state (session mode)
        if session.gate.status == GateStatus::Idle {
            // Check auto-skip conditions
            let diff_size = session.gate.cached_diff_size;
            let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);

            if let Some(reason) = gate.evaluate_auto_skip(diff_size) {
                let _ = gate.skip(&reason, SkipDecider::AutoThreshold);
                session.add_trace(EventType::Skip, Some(reason));
                let _ = self.store.put(&session);
                let output = StopOutput::approve();
                return crate::hooks::output::to_json(&output);
            }

            // No auto-skip, allow exit in Idle state
            let _ = self.store.put(&session);
            let output = StopOutput::approve();
            return crate::hooks::output::to_json(&output);
        }

        // Handle Pending/Blocked states
        if session.gate.status.requires_reflection() {
            let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);

            // Try to block
            match gate.block() {
                Ok(circuit_breaker_tripped) => {
                    if circuit_breaker_tripped {
                        session.add_trace(EventType::CircuitBreakerTripped, None);
                        let _ = self.store.put(&session);
                        let output = StopOutput::approve_with_message(
                            "Circuit breaker tripped. Reflection skipped.",
                        );
                        return crate::hooks::output::to_json(&output);
                    }

                    session.add_trace(EventType::GateBlocked, None);
                    let _ = self.store.put(&session);

                    let message = "Reflection required. Run `grove reflect` to capture learnings or `grove skip <reason>` to skip.";
                    let output = StopOutput::block_with_message(message);
                    return crate::hooks::output::to_json(&output);
                }
                Err(_) => {
                    // Fail-open on error
                    let _ = self.store.put(&session);
                    let output = StopOutput::approve();
                    return crate::hooks::output::to_json(&output);
                }
            }
        }

        // Default: approve
        let _ = self.store.put(&session);
        let output = StopOutput::approve();
        crate::hooks::output::to_json(&output)
    }

    // =========================================================================
    // Session End Handler
    // =========================================================================

    /// Handle the session-end hook.
    ///
    /// 1. Load session state
    /// 2. Log dismissed events for unreferenced learnings
    /// 3. Always allow termination
    fn handle_session_end(&self, input: &str) -> Result<String> {
        let hook_input: SessionEndInput = parse_input(input)?;
        let cwd = Path::new(&hook_input.common.cwd);

        // Load session (fail-open if not found)
        let session_result = self.store.get(&hook_input.common.session_id);
        let mut session = match session_result {
            Ok(Some(s)) => s,
            _ => {
                // Fail-open: empty output if session not found
                let output = SessionEndOutput::empty();
                return crate::hooks::output::to_json(&output);
            }
        };

        // Log dismissed events for unreferenced learnings
        let stats_path = project_stats_log_path(cwd);
        let logger = StatsLogger::new(&stats_path);

        for learning in &session.gate.injected_learnings {
            if learning.outcome == crate::core::state::InjectionOutcome::Pending {
                // Learning was surfaced but not referenced - mark as dismissed
                let _ = logger.append_dismissed(&learning.learning_id, &session.id);
            }
        }

        session.add_trace(
            EventType::SessionEnd,
            Some(format!("reason: {:?}", hook_input.reason)),
        );

        // Save final session state
        let _ = self.store.put(&session);

        let output = SessionEndOutput::empty();
        crate::hooks::output::to_json(&output)
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    /// Get or create a session based on hook input.
    fn get_or_create_session(&self, input: &HookInput) -> Result<SessionState> {
        // Try to load existing session
        if let Ok(Some(session)) = self.store.get(&input.session_id) {
            return Ok(session);
        }

        // Create new session
        let session = SessionState::new(
            &input.session_id,
            input.cwd.to_string_lossy(),
            input.transcript_path.to_string_lossy(),
        );

        Ok(session)
    }
}

/// Read input from stdin.
fn read_stdin() -> Result<String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| GroveError::storage("stdin", e))?;
    Ok(input)
}

/// Extract ticket ID from a close command.
///
/// Handles patterns like:
/// - `tissue status grove-123 closed` -> `grove-123`
/// - `beads close issue-456` -> `issue-456`
fn extract_ticket_id(command: &str) -> Option<String> {
    let parts: Vec<&str> = command.split_whitespace().collect();

    // tissue status <id> closed
    if parts.len() >= 4 && parts[0] == "tissue" && parts[1] == "status" {
        return Some(parts[2].to_string());
    }

    // beads close <id> or beads complete <id>
    if parts.len() >= 3 && parts[0] == "beads" && (parts[1] == "close" || parts[1] == "complete") {
        return Some(parts[2].to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::output::StopDecision;
    use crate::storage::MemorySessionStore;

    fn test_runner() -> HookRunner<MemorySessionStore> {
        HookRunner::new(MemorySessionStore::new(), Config::default())
    }

    // HookType tests

    #[test]
    fn test_hook_type_parse() {
        assert_eq!(
            HookType::parse("session-start"),
            Some(HookType::SessionStart)
        );
        assert_eq!(
            HookType::parse("sessionstart"),
            Some(HookType::SessionStart)
        );
        assert_eq!(
            HookType::parse("session_start"),
            Some(HookType::SessionStart)
        );
        assert_eq!(HookType::parse("pre-tool-use"), Some(HookType::PreToolUse));
        assert_eq!(
            HookType::parse("post-tool-use"),
            Some(HookType::PostToolUse)
        );
        assert_eq!(HookType::parse("stop"), Some(HookType::Stop));
        assert_eq!(HookType::parse("session-end"), Some(HookType::SessionEnd));
        assert_eq!(HookType::parse("unknown"), None);
    }

    // extract_ticket_id tests

    #[test]
    fn test_extract_ticket_id_tissue() {
        assert_eq!(
            extract_ticket_id("tissue status grove-123 closed"),
            Some("grove-123".to_string())
        );
    }

    #[test]
    fn test_extract_ticket_id_beads_close() {
        assert_eq!(
            extract_ticket_id("beads close issue-456"),
            Some("issue-456".to_string())
        );
    }

    #[test]
    fn test_extract_ticket_id_beads_complete() {
        assert_eq!(
            extract_ticket_id("beads complete task-789"),
            Some("task-789".to_string())
        );
    }

    #[test]
    fn test_extract_ticket_id_no_match() {
        assert_eq!(extract_ticket_id("git status"), None);
    }

    // Session-start handler tests

    #[test]
    fn test_session_start_creates_session() {
        let runner = test_runner();
        let input = r#"{
            "session_id": "test-session",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;

        let result = runner.run_with_input(HookType::SessionStart, input);
        assert!(result.is_ok());

        // Verify session was created
        let session = runner.store.get("test-session").unwrap();
        assert!(session.is_some());
    }

    #[test]
    fn test_session_start_adds_trace_events() {
        let runner = test_runner();
        let input = r#"{
            "session_id": "trace-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;

        runner
            .run_with_input(HookType::SessionStart, input)
            .unwrap();

        let session = runner.store.get("trace-test").unwrap().unwrap();
        assert!(!session.trace.is_empty());
        assert!(session
            .trace
            .iter()
            .any(|t| t.event_type == EventType::SessionStart));
    }

    // Pre-tool-use handler tests

    #[test]
    fn test_pre_tool_use_allows_non_close_commands() {
        let runner = test_runner();

        // First create a session
        let start_input = r#"{
            "session_id": "pre-tool-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // Test non-close command
        let input = r#"{
            "session_id": "pre-tool-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "git status"}
        }"#;

        let result = runner.run_with_input(HookType::PreToolUse, input);
        assert!(result.is_ok());

        let output: PreToolUseOutput = serde_json::from_str(&result.unwrap()).unwrap();
        assert!(output.allow);
    }

    #[test]
    fn test_pre_tool_use_detects_ticket_close() {
        let runner = test_runner();

        // First create a session
        let start_input = r#"{
            "session_id": "close-detect-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // Test ticket close command
        let input = r#"{
            "session_id": "close-detect-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "tissue status grove-123 closed"}
        }"#;

        let result = runner.run_with_input(HookType::PreToolUse, input);
        assert!(result.is_ok());

        // Verify intent was recorded
        let session = runner.store.get("close-detect-test").unwrap().unwrap();
        assert!(session.gate.ticket_close_intent.is_some());
    }

    // Stop handler tests

    #[test]
    fn test_stop_approves_idle_session() {
        let runner = test_runner();

        // Create session
        let start_input = r#"{
            "session_id": "stop-idle-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // Test stop hook
        let stop_input = r#"{
            "session_id": "stop-idle-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;

        let result = runner.run_with_input(HookType::Stop, stop_input);
        assert!(result.is_ok());

        let output: StopOutput = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(output.decision, StopDecision::Approve);
    }

    #[test]
    fn test_stop_blocks_pending_session() {
        let runner = test_runner();

        // Create session and transition to Pending
        let start_input = r#"{
            "session_id": "stop-block-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // Manually set gate to Pending for testing
        let mut session = runner.store.get("stop-block-test").unwrap().unwrap();
        session.gate.status = GateStatus::Pending;
        runner.store.put(&session).unwrap();

        // Test stop hook
        let stop_input = r#"{
            "session_id": "stop-block-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;

        let result = runner.run_with_input(HookType::Stop, stop_input);
        assert!(result.is_ok());

        let output: StopOutput = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(output.decision, StopDecision::Block);
    }

    #[test]
    fn test_stop_approves_reflected_session() {
        let runner = test_runner();

        // Create session and set to Reflected
        let start_input = r#"{
            "session_id": "stop-reflected-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        let mut session = runner.store.get("stop-reflected-test").unwrap().unwrap();
        session.gate.status = GateStatus::Reflected;
        runner.store.put(&session).unwrap();

        // Test stop hook
        let stop_input = r#"{
            "session_id": "stop-reflected-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;

        let result = runner.run_with_input(HookType::Stop, stop_input);
        assert!(result.is_ok());

        let output: StopOutput = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(output.decision, StopDecision::Approve);
    }

    #[test]
    fn test_stop_fail_open_missing_session() {
        let runner = test_runner();

        // Test with nonexistent session
        let stop_input = r#"{
            "session_id": "nonexistent",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;

        let result = runner.run_with_input(HookType::Stop, stop_input);
        assert!(result.is_ok());

        let output: StopOutput = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(output.decision, StopDecision::Approve);
    }

    // Session-end handler tests

    #[test]
    fn test_session_end_logs_trace() {
        let runner = test_runner();

        // Create session
        let start_input = r#"{
            "session_id": "end-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // Test session end
        let end_input = r#"{
            "session_id": "end-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "reason": "user_exit"
        }"#;

        let result = runner.run_with_input(HookType::SessionEnd, end_input);
        assert!(result.is_ok());

        // Verify trace was added
        let session = runner.store.get("end-test").unwrap().unwrap();
        assert!(session
            .trace
            .iter()
            .any(|t| t.event_type == EventType::SessionEnd));
    }

    // Integration tests

    #[test]
    fn test_full_ticket_flow() {
        let runner = test_runner();

        // 1. Session start
        let start_input = r#"{
            "session_id": "full-flow-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 2. Pre-tool-use: ticket close detected
        let pre_input = r#"{
            "session_id": "full-flow-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "tissue status grove-123 closed"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input)
            .unwrap();

        // 3. Post-tool-use: close confirmed
        let post_input = r#"{
            "session_id": "full-flow-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "tissue status grove-123 closed"},
            "tool_response": "grove-123"
        }"#;
        runner
            .run_with_input(HookType::PostToolUse, post_input)
            .unwrap();

        // Verify gate is now Pending
        let session = runner.store.get("full-flow-test").unwrap().unwrap();
        assert_eq!(session.gate.status, GateStatus::Pending);

        // 4. Stop hook: should block
        let stop_input = r#"{
            "session_id": "full-flow-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        let result = runner.run_with_input(HookType::Stop, stop_input).unwrap();
        let output: StopOutput = serde_json::from_str(&result).unwrap();
        assert_eq!(output.decision, StopDecision::Block);
    }
}
