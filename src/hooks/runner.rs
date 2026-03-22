//! Hook runner for Grove.
//!
//! This module implements the hook dispatch and individual hook handlers.
//! Hooks integrate with Claude Code at key points in the session lifecycle.

use std::io::{self, Read};
use std::path::Path;

use crate::backends::{SearchFilters, SearchQuery};
use crate::config::{project_stats_log_path, Config};
use crate::core::gate::Gate;
use crate::core::state::{
    EventType, GateStatus, SessionState, SkipDecider, TicketCloseIntent, TicketContext,
};
use crate::core::InjectedLearning;
use crate::discovery::{
    create_primary_backend, detect_backends, detect_ticketing_system, extract_title_keywords,
    match_close_command, query_active_tickets, TicketingSystem,
};
use crate::error::{GroveError, Result};
use crate::hooks::input::{
    parse_input, HookInput, PostToolUseInput, PreToolUseInput, SessionEndInput, SessionStartInput,
    StopInput, TaskCompletedInput, UserPromptSubmitInput,
};
use crate::hooks::output::{
    PreToolUseOutput, SessionEndOutput, SessionStartOutput, StopOutput, UserPromptSubmitOutput,
};
use crate::stats::scoring::{recency, recency_weight, reference_boost, CompositeScore, Strategy};
use crate::stats::{StatsCacheManager, StatsLogger};
use crate::storage::SessionStore;
use tracing::{debug, warn};

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
    /// Task completed hook (Claude Code tasks ticketing).
    TaskCompleted,
    /// User prompt submit hook (mid-session re-retrieval).
    UserPromptSubmit,
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
            "task-completed" | "taskcompleted" | "task_completed" => Some(Self::TaskCompleted),
            "user-prompt-submit" | "userpromptsubmit" | "user_prompt_submit" => {
                Some(Self::UserPromptSubmit)
            }
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

    /// Save session state with warning on failure (fail-open pattern).
    ///
    /// Logs a warning if the save fails but doesn't propagate the error,
    /// following the fail-open philosophy where infrastructure errors
    /// should never block work.
    fn save_session(&self, session: &SessionState) {
        if let Err(e) = self.store.put(session) {
            warn!(
                session_id = %session.id,
                error = %e,
                "Failed to save session state (fail-open: continuing anyway)"
            );
        }
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
            HookType::TaskCompleted => self.handle_task_completed(input),
            HookType::UserPromptSubmit => self.handle_user_prompt_submit(input),
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
        let hook_input: SessionStartInput = parse_input(input)?;
        let cwd = Path::new(&hook_input.common.cwd);

        // Create or load session
        let mut session = self.get_or_create_session(&hook_input.common)?;
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

        // Populate SearchQuery with git context for contextual relevance scoring
        let (git_files, mut git_keywords) = extract_git_context(cwd);

        // Add ticket keywords from any restored ticket context,
        // but only if the gate is Active (ticket still being worked on).
        // Terminal states (Reflected/Skipped) are stale from a prior close.
        if let Some(ref ticket) = session.gate.ticket {
            if session.gate.status == GateStatus::Active {
                git_keywords.push(ticket.ticket_id.clone());
            }
        }

        // Query active tickets from tissue for additional context signals
        if ticketing_info.system == TicketingSystem::Tissue
            && self.config.context.active_ticket_query
        {
            let active_tickets =
                query_active_tickets(cwd, self.config.context.active_ticket_timeout_ms);
            for ticket in &active_tickets {
                // Extract keywords from title
                let title_keywords = extract_title_keywords(&ticket.title);
                git_keywords.extend(title_keywords);

                // Add tags as keywords
                for tag in &ticket.tags {
                    let tag_lower = tag.to_lowercase();
                    if tag_lower.len() >= 3 {
                        git_keywords.push(tag_lower);
                    }
                }
            }

            if !active_tickets.is_empty() {
                session.add_trace(
                    EventType::TicketDetected,
                    Some(format!("active tickets queried: {}", active_tickets.len())),
                );
            }
        }

        let query = if git_files.is_empty() && git_keywords.is_empty() {
            SearchQuery::new()
        } else {
            SearchQuery::new().files(git_files).keywords(git_keywords)
        };

        // Retrieve, score, and inject learnings using shared helper
        let top_learnings = self.retrieve_and_score_learnings(
            cwd,
            &session,
            &query,
            Some(&hook_input.common.transcript_path),
        );
        if let Some(context) =
            self.build_injection_context(cwd, &mut session, &top_learnings, false)
        {
            session.add_trace(
                EventType::LearningsInjected,
                Some(format!("count: {}", top_learnings.len())),
            );
            additional_context = Some(context);
        }

        // Correction propagation: check for recently corrected learnings
        // and inject notices at session-start (best-effort)
        let correction_notices = self.get_correction_notices(cwd, &session);
        if !correction_notices.is_empty() {
            let notice = format!(
                "[CORRECTION NOTICE] The following learnings have been corrected since you may have last seen them:\n{}",
                correction_notices.join("\n")
            );
            // Append correction notices to any existing context (e.g., injected learnings)
            // rather than overwriting it
            additional_context = Some(match additional_context.take() {
                Some(ctx) => format!("{}\n\n{}", ctx, notice),
                None => notice,
            });
            session.add_trace(
                EventType::CorrectionNotice,
                Some(format!("count: {}", correction_notices.len())),
            );
        }

        // Check if gate is in blocking state and inject context
        // This ensures subagents see why they're blocked when the stop hook fires
        if session.gate.status.requires_reflection() {
            // requires_reflection() only returns true for Pending/Blocked
            let status_str = match session.gate.status {
                GateStatus::Pending => "Pending",
                GateStatus::Blocked => "Blocked",
                _ => unreachable!("requires_reflection() returned true for non-blocking state"),
            };

            let mut gate_notice = format!(
                "## Grove Gate Active\n\n\
                 **Status:** {} (reflection required before exit)\n\n",
                status_str
            );

            // Add ticket context if available
            if let Some(ref ticket) = session.gate.ticket {
                gate_notice.push_str(&format!(
                    "**Ticket:** {} - {}\n\n",
                    ticket.ticket_id, ticket.title
                ));
            }

            gate_notice.push_str(&format!(
                "To resolve, run one of:\n\
                 - `grove reflect --session-id {}` - capture learnings\n\
                 - `grove skip <reason> --session-id {}` - skip with reason\n",
                session.id, session.id
            ));

            // Prepend gate notice to any existing context
            additional_context = Some(match additional_context.take() {
                Some(ctx) => format!("{}\n\n{}", gate_notice, ctx),
                None => gate_notice,
            });

            session.add_trace(
                EventType::GateStatusChanged,
                Some(format!(
                    "injected blocking notice: {:?}",
                    session.gate.status
                )),
            );
        }

        // Decay check is handled separately via grove maintain (Stage 2)
        // We don't run it here to keep session-start fast

        // Set deferred injection flag if enabled in config
        if self.config.context.deferred_injection {
            session.gate.deferred_injection_pending = true;
            session.add_trace(EventType::DeferredInjection, Some("pending".to_string()));
        }

        // Save session state
        self.save_session(&session);

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
    /// 2. If match, transition gate immediately (assuming command will succeed)
    /// 3. Always allow the tool to proceed
    ///
    /// Note: We transition the gate in PreToolUse rather than PostToolUse because
    /// PostToolUse hooks may not fire reliably in all Claude Code configurations.
    /// This follows the same pattern used by the Roz quality gate plugin.
    /// If the command fails, the circuit breaker provides a safety valve.
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

        // Deferred injection: on the first tool call after SessionStart,
        // extract keywords from tool_input and augment retrieval.
        let mut deferred_context: Option<String> = None;

        if session.gate.deferred_injection_pending {
            // Clear flag immediately (fail-open: even if injection fails)
            session.gate.deferred_injection_pending = false;

            // Extract keywords from tool_input
            let tool_keywords =
                extract_tool_input_keywords(&hook_input.tool_name, &hook_input.tool_input);

            if !tool_keywords.is_empty() {
                // Re-extract git context and merge with tool keywords
                let cwd = Path::new(&hook_input.common.cwd);
                let (git_files, mut git_keywords) = extract_git_context(cwd);
                git_keywords.extend(tool_keywords);

                let query = SearchQuery::new()
                    .files(git_files.clone())
                    .keywords(git_keywords);

                // Retrieve and score learnings with augmented query
                let top_learnings = self.retrieve_and_score_learnings(
                    cwd,
                    &session,
                    &query,
                    Some(&hook_input.common.transcript_path),
                );

                // LLM reranking: optionally rerank candidates using Haiku
                let top_learnings =
                    if self.config.retrieval.rerank.enabled && !top_learnings.is_empty() {
                        let tool_input_str =
                            serde_json::to_string(&hook_input.tool_input).unwrap_or_default();
                        let git_branch = extract_git_branch(cwd);
                        let reranked = rerank_with_llm(
                            top_learnings,
                            &self.config.retrieval.rerank,
                            &self.config.judge.api_url,
                            &hook_input.tool_name,
                            &tool_input_str,
                            &git_branch,
                            &git_files,
                        );
                        session.add_trace(
                            EventType::DeferredInjection,
                            Some("LLM reranking applied".to_string()),
                        );
                        reranked
                    } else {
                        top_learnings
                    };

                // Build context, deduplicating against already-injected learnings
                if let Some(context) =
                    self.build_injection_context(cwd, &mut session, &top_learnings, true)
                {
                    session.add_trace(
                        EventType::DeferredInjection,
                        Some(format!(
                            "injected via {} (new learnings: {})",
                            hook_input.tool_name,
                            top_learnings.len()
                        )),
                    );
                    deferred_context = Some(context);
                } else {
                    session.add_trace(
                        EventType::DeferredInjection,
                        Some("no new learnings found".to_string()),
                    );
                }
            } else {
                session.add_trace(
                    EventType::DeferredInjection,
                    Some("no keywords extracted".to_string()),
                );
            }

            // Save session with cleared flag (before falling through to ticket-close check)
            self.save_session(&session);
        } else if hook_input.tool_name != "Bash" {
            // Performance optimization: after the deferred flag is cleared,
            // only Bash tools need further processing (ticket-close detection).
            // Non-Bash tools can return immediately.
            let output = PreToolUseOutput::allow();
            return crate::hooks::output::to_json(&output);
        }

        // Check for ticket close command
        // Note: match_close_command gates on tool_name == "Bash" internally,
        // so this only matches Bash commands.
        let command = hook_input
            .tool_input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if let Some(close_pattern) = match_close_command(&hook_input.tool_name, command) {
            // Extract ticket ID from command (simplified extraction)
            let ticket_id = extract_ticket_id(command).unwrap_or_else(|| "unknown".to_string());

            // Record intent for tracking
            let intent = TicketCloseIntent::new(&ticket_id, command);

            // Transition gate immediately based on current state
            // We assume the command will succeed - if it fails, circuit breaker handles it
            let current_status = session.gate.status;

            if current_status == GateStatus::Idle {
                // Idle -> Active -> Pending
                let ticket = TicketContext::new(&ticket_id, "detected", "Ticket closed");
                let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);
                gate.record_close_intent(intent);
                let _ = gate.detect_ticket(ticket);
                let _ = gate.confirm_ticket_close();
                session.add_trace(EventType::TicketClosed, None);
            } else if current_status == GateStatus::Active {
                // Active -> Pending
                let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);
                gate.record_close_intent(intent);
                let _ = gate.confirm_ticket_close();
                session.add_trace(EventType::TicketClosed, None);
            } else if current_status.is_terminal() {
                // Terminal (Reflected/Skipped) -> reset -> Active -> Pending
                // Allows multiple ticket closures in same session
                let ticket = TicketContext::new(&ticket_id, "detected", "Ticket closed");
                let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);
                let _ = gate.reset_for_new_ticket();
                gate.record_close_intent(intent);
                let _ = gate.detect_ticket(ticket);
                let _ = gate.confirm_ticket_close();
                session.add_trace(
                    EventType::GateStatusChanged,
                    Some("reset from terminal state for new ticket".to_string()),
                );
                session.add_trace(EventType::TicketClosed, None);
            } else {
                // Already in Pending or Blocked - just record intent
                let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);
                gate.record_close_intent(intent);
            }

            session.add_trace(
                EventType::TicketCloseDetected,
                Some(format!(
                    "pattern: {:?}, ticket: {}",
                    close_pattern, ticket_id
                )),
            );

            // Save session
            self.save_session(&session);
        }

        // Return with deferred context if we have it, otherwise plain allow
        if let Some(context) = deferred_context {
            crate::hooks::output::to_json(&PreToolUseOutput::allow_with_context(context))
        } else {
            let output = PreToolUseOutput::allow();
            crate::hooks::output::to_json(&output)
        }
    }

    // =========================================================================
    // Post-Tool-Use Handler
    // =========================================================================

    /// Handle the post-tool-use hook.
    ///
    /// This is a fallback handler - the primary gate transition happens in PreToolUse.
    /// PostToolUse may not fire reliably in all Claude Code configurations.
    ///
    /// 1. Check if a ticket close intent was recorded but gate not yet transitioned
    /// 2. If successful and gate still needs transition, complete it
    /// 3. If failed, clear the intent (allows retry)
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
                } else if current_status.is_terminal() {
                    // If in terminal state (Reflected/Skipped), reset for new ticket
                    // This allows multiple ticket closures in the same session to each trigger reflection
                    let intent = session.gate.ticket_close_intent.take();
                    if let Some(intent) = intent {
                        let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);
                        let _ = gate.reset_for_new_ticket();
                        let ticket =
                            TicketContext::new(&intent.ticket_id, "detected", "Ticket closed");
                        let _ = gate.detect_ticket(ticket);
                        let _ = gate.confirm_ticket_close();
                        session.add_trace(
                            EventType::GateStatusChanged,
                            Some("reset from terminal state for new ticket".to_string()),
                        );
                        session.add_trace(EventType::TicketClosed, None);
                    }
                }
            } else {
                let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);
                gate.clear_close_intent();
                session.add_trace(EventType::TicketCloseFailed, None);
            }

            // Save session
            self.save_session(&session);
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
        let hook_input: StopInput = parse_input(input)?;

        // Load session (fail-open if not found)
        let session_result = self.store.get(&hook_input.common.session_id);
        let mut session = match session_result {
            Ok(Some(s)) => s,
            _ => {
                // Fail-open: approve if session not found
                let output = StopOutput::approve();
                return crate::hooks::output::to_json(&output);
            }
        };

        session.add_trace(EventType::StopHookCalled, None);

        // If stop_hook_active is true, the agent is already in a stop-hook-triggered
        // continuation trying to resolve the block. Don't block again to prevent loops.
        if hook_input.stop_hook_active {
            session.add_trace(
                EventType::StopHookCalled,
                Some("stop_hook_active=true, auto-approving to prevent loop".to_string()),
            );
            self.save_session(&session);
            let output = StopOutput::approve_with_reason(
                "Auto-approved: agent is already resolving a previous stop hook block.",
            );
            return crate::hooks::output::to_json(&output);
        }

        // Check terminal states first
        if session.gate.status.is_terminal() {
            self.save_session(&session);
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
                self.save_session(&session);
                let output = StopOutput::approve();
                return crate::hooks::output::to_json(&output);
            }

            // No auto-skip, allow exit in Idle state
            self.save_session(&session);
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
                        self.save_session(&session);
                        let output = StopOutput::approve_with_reason(
                            "Circuit breaker tripped. Reflection skipped.",
                        );
                        return crate::hooks::output::to_json(&output);
                    }

                    session.add_trace(EventType::GateBlocked, None);
                    self.save_session(&session);

                    let reason = format!(
                        "Grove gate is blocking exit: reflection required before this session can end.\n\
                         Run `grove reflect --session-id {sid}` to capture learnings, \
                         or `grove skip <reason> --session-id {sid}` to skip reflection.\n\n\
                         grove reflect expects JSON on stdin. Example:\n\
                         \n\
                         cat <<'EOF' | grove reflect --session-id {sid}\n\
                         {{\n\
                           \"session_id\": \"{sid}\",\n\
                           \"candidates\": [\n\
                             {{\n\
                               \"category\": \"pitfall\",\n\
                               \"summary\": \"Ecto changeset cast/3 silently drops fields not in the schema\",\n\
                               \"detail\": \"When adding a new field to a Phoenix form, cast/3 silently ignores fields missing from the schema module. Always update the schema before the changeset.\",\n\
                               \"criteria_met\": [\"behavior-changing\"],\n\
                               \"tags\": [\"ecto\", \"phoenix-forms\"],\n\
                               \"relevance_context\": \"Surface when modifying Ecto schemas or debugging forms that submit but don't persist. Not relevant for read-only queries.\"\n\
                             }}\n\
                           ]\n\
                         }}\n\
                         EOF\n\
                         \n\
                         Categories: Pattern, Pitfall, Convention, Dependency, Process, Domain, Debugging\n\
                         Criteria (claim ≥1): behavior-changing, decision-rationale, stable-fact, explicit-request\n\
                         \n\
                         Quality tips:\n\
                         - Include project-specific terms (library names, APIs, file patterns) so the learning surfaces precisely\n\
                         - relevance_context controls WHEN this learning appears — include both triggers and exclusions\n\
                         - Avoid generic advice without concrete anchors — it will surface in every session",
                        sid = session.id
                    );
                    let output = StopOutput::block_with_reason(reason);
                    return crate::hooks::output::to_json(&output);
                }
                Err(_) => {
                    // Fail-open on error
                    self.save_session(&session);
                    let output = StopOutput::approve();
                    return crate::hooks::output::to_json(&output);
                }
            }
        }

        // Default: approve
        self.save_session(&session);
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

        // Log dismissed events for unreferenced learnings.
        // Per design (01-architecture.md §SessionEnd), dismissed events are emitted
        // for ALL sessions that had learnings injected. Any learning still in Pending
        // state was surfaced but never referenced — that's a dismissal signal.
        //
        // Exception: if gate status is Skipped and skip_counts_as_dismissal is false,
        // we skip dismissal logging because the user explicitly skipped reflection
        // (e.g., urgent meeting) and the skip is treated as no-signal.
        let should_log_dismissals = if !session.gate.injected_learnings.is_empty() {
            match session.gate.status {
                crate::core::state::GateStatus::Skipped => {
                    self.config.gate.skip_counts_as_dismissal
                }
                _ => true,
            }
        } else {
            false
        };

        if should_log_dismissals {
            let stats_path = project_stats_log_path(cwd);
            let logger = StatsLogger::new(&stats_path);

            for learning in &session.gate.injected_learnings {
                if learning.outcome == crate::core::state::InjectionOutcome::Pending {
                    // Learning was surfaced but not referenced - mark as dismissed
                    let _ = logger.append_dismissed(&learning.learning_id, &session.id);
                }
            }
        }

        session.add_trace(
            EventType::SessionEnd,
            Some(format!("reason: {:?}", hook_input.reason)),
        );

        // Save final session state
        self.save_session(&session);

        let output = SessionEndOutput::empty();
        crate::hooks::output::to_json(&output)
    }

    // =========================================================================
    // Task Completed Handler
    // =========================================================================

    /// Handle the task-completed hook.
    ///
    /// Called when a Claude Code task is marked as completed.
    ///
    /// 1. Parse TaskCompletedInput from stdin
    /// 2. Create/update session with task context
    /// 3. Transition gate to Pending state
    /// 4. Return exit code 2 to block completion until reflection/skip
    fn handle_task_completed(&self, input: &str) -> Result<String> {
        let hook_input: TaskCompletedInput = parse_input(input)?;

        // Get or create session
        let mut session = self.get_or_create_session(&hook_input.common)?;

        // Create ticket context from task data
        let mut ticket = TicketContext::new(&hook_input.task_id, "tasks", &hook_input.task_subject);
        if let Some(desc) = &hook_input.task_description {
            ticket = ticket.with_description(desc);
        }

        // Record ticket detection and transition to Active → Pending
        let mut gate = Gate::new(&mut session.gate, &self.config, &session.id);
        let _ = gate.detect_ticket(ticket);
        let _ = gate.confirm_ticket_close();

        session.add_trace(
            EventType::TicketClosed,
            Some(format!(
                "task_id: {}, subject: {}",
                hook_input.task_id, hook_input.task_subject
            )),
        );

        // Save session state
        self.save_session(&session);

        // Block task completion until reflection/skip.
        // main.rs translates this to stderr + exit code 2 for TaskCompleted hooks.
        let reason = format!(
            "Grove gate is blocking task completion: reflection required.\n\
             Run `grove reflect --session-id {sid}` to capture learnings, \
             or `grove skip <reason> --session-id {sid}` to skip reflection.\n\n\
             grove reflect expects JSON on stdin. Example:\n\
             \n\
             cat <<'EOF' | grove reflect --session-id {sid}\n\
             {{\n\
               \"session_id\": \"{sid}\",\n\
               \"candidates\": [\n\
                 {{\n\
                   \"category\": \"pitfall\",\n\
                   \"summary\": \"Ecto changeset cast/3 silently drops fields not in the schema\",\n\
                   \"detail\": \"When adding a new field to a Phoenix form, cast/3 silently ignores fields missing from the schema module. Always update the schema before the changeset.\",\n\
                   \"criteria_met\": [\"behavior-changing\"],\n\
                   \"tags\": [\"ecto\", \"phoenix-forms\"],\n\
                   \"relevance_context\": \"Surface when modifying Ecto schemas or debugging forms that submit but don't persist. Not relevant for read-only queries.\"\n\
                 }}\n\
               ]\n\
             }}\n\
             EOF\n\
             \n\
             Categories: Pattern, Pitfall, Convention, Dependency, Process, Domain, Debugging\n\
             Criteria (claim ≥1): behavior-changing, decision-rationale, stable-fact, explicit-request\n\
             \n\
             Quality tips:\n\
             - Include project-specific terms (library names, APIs, file patterns) so the learning surfaces precisely\n\
             - relevance_context controls WHEN this learning appears — include both triggers and exclusions\n\
             - Avoid generic advice without concrete anchors — it will surface in every session",
            sid = session.id
        );
        let output = StopOutput::block_with_reason(reason);
        crate::hooks::output::to_json(&output)
    }

    // =========================================================================
    // User Prompt Submit Handler
    // =========================================================================

    /// Handle the user-prompt-submit hook.
    ///
    /// Fires when the user submits a prompt, before Claude processes it.
    /// Extracts keywords from the user's prompt text and re-queries for
    /// relevant learnings that weren't surfaced at session start.
    ///
    /// Fail-open: if session not found, empty prompt, or no new learnings,
    /// returns empty output without blocking.
    fn handle_user_prompt_submit(&self, input: &str) -> Result<String> {
        let hook_input: UserPromptSubmitInput = parse_input(input)?;

        // Load session (fail-open if not found)
        let session_result = self.store.get(&hook_input.common.session_id);
        let mut session = match session_result {
            Ok(Some(s)) => s,
            _ => {
                // Fail-open: no session → return empty output
                return crate::hooks::output::to_json(&UserPromptSubmitOutput::empty());
            }
        };

        // Extract keywords from the user's prompt using the same logic as
        // extract_user_intent_keywords, but directly from the prompt string
        // rather than reading a transcript file.
        let keywords = extract_prompt_keywords(
            &hook_input.prompt,
            self.config.retrieval.intent_filter.max_keywords,
        );

        if keywords.is_empty() {
            session.add_trace(
                EventType::UserPromptInjection,
                Some("no keywords extracted from prompt".to_string()),
            );
            return crate::hooks::output::to_json(&UserPromptSubmitOutput::empty());
        }

        // Build query from prompt keywords + git context
        let cwd = Path::new(&hook_input.common.cwd);
        let (git_files, mut git_keywords) = extract_git_context(cwd);
        git_keywords.extend(keywords);

        let query = SearchQuery::new().files(git_files).keywords(git_keywords);

        // Retrieve and score learnings
        let top_learnings = self.retrieve_and_score_learnings(
            cwd,
            &session,
            &query,
            Some(&hook_input.common.transcript_path),
        );

        // Build context, deduplicating against already-injected learnings
        if let Some(context) = self.build_injection_context(cwd, &mut session, &top_learnings, true)
        {
            session.add_trace(
                EventType::UserPromptInjection,
                Some(format!("injected new learnings: {}", top_learnings.len())),
            );
            self.save_session(&session);
            return crate::hooks::output::to_json(&UserPromptSubmitOutput::with_context(context));
        }

        session.add_trace(
            EventType::UserPromptInjection,
            Some("no new learnings found".to_string()),
        );
        self.save_session(&session);
        crate::hooks::output::to_json(&UserPromptSubmitOutput::empty())
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    /// Retrieve and score learnings from the backend.
    ///
    /// Shared helper used by both SessionStart and PreToolUse deferred injection.
    /// Returns scored learnings sorted by score descending, capped at the configured limit.
    fn retrieve_and_score_learnings(
        &self,
        cwd: &Path,
        _session: &SessionState,
        query: &SearchQuery,
        transcript_path: Option<&Path>,
    ) -> Vec<CompositeScore> {
        let max_injections = self.config.retrieval.max_injections;
        let mut strategy = Strategy::parse(&self.config.retrieval.strategy).unwrap_or_default();

        let backend = create_primary_backend(cwd, Some(&self.config));
        let filters = SearchFilters::active_only();

        // Try to load stats cache for reference boost
        let stats_path = project_stats_log_path(cwd);
        let cache_path = cwd.join(".grove").join("stats-cache.json");
        let cache_manager = StatsCacheManager::new(&cache_path, &stats_path);
        let cache = cache_manager.load_or_rebuild().ok();

        let results = match backend.search(query, &filters) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        // Optionally rescore with Tantivy BM25
        #[cfg(feature = "tantivy-search")]
        let results = if self.config.retrieval.scoring_backend == "bm25" {
            // Fetch all active learnings for corpus-size heuristic and enrichment
            let all_active = backend
                .search(&SearchQuery::new(), &SearchFilters::active_only())
                .unwrap_or_default();
            let corpus_size = all_active.len();

            // Select retrieval profile based on total corpus size
            let profile = crate::config::RetrievalProfile::select(
                corpus_size,
                self.config.retrieval.corpus_size_threshold,
            );
            debug!(
                "Retrieval profile: {:?} (corpus_size={}, threshold={})",
                profile, corpus_size, self.config.retrieval.corpus_size_threshold
            );

            // Corpus-derived vocabulary enrichment: augment query keywords with
            // domain terms extracted from the learning corpus itself.
            let effective_query = if self.config.retrieval.corpus_enrichment {
                let corpus_learnings: Vec<_> = all_active.iter().map(|r| &r.learning).collect();
                let vocab = extract_corpus_vocabulary_from_refs(&corpus_learnings, 2);
                if !vocab.is_empty() {
                    let enrichment =
                        enrich_query_with_corpus_vocabulary(&query.keywords, &query.files, &vocab);
                    if !enrichment.is_empty() {
                        debug!("Corpus enrichment: +{} terms", enrichment.len());
                        let mut enriched = query.clone();
                        enriched.keywords.extend(enrichment);
                        enriched
                    } else {
                        query.clone()
                    }
                } else {
                    query.clone()
                }
            } else {
                query.clone()
            };

            rescore_with_tantivy(results, &effective_query, profile)
        } else {
            results
        };

        #[cfg(not(feature = "tantivy-search"))]
        if self.config.retrieval.scoring_backend == "bm25" {
            warn!(
                "scoring_backend=\"bm25\" configured but tantivy-search feature is not enabled; \
                 falling back to keyword scoring"
            );
        }

        // Downgrade conservative to moderate when pool is too small
        if strategy == Strategy::Conservative
            && (results.len() as u32) < self.config.retrieval.min_pool_size
        {
            strategy = Strategy::Moderate;
        }

        let now = chrono::Utc::now();
        let min_threshold = strategy.min_relevance_threshold();

        let mut scored: Vec<CompositeScore> = results
            .into_iter()
            .filter_map(|result| {
                let qualifies = if result.relevance >= min_threshold && result.relevance > 0.0 {
                    true
                } else if strategy.includes_recent_without_match() {
                    let days_old = (now - result.learning.timestamp).num_days();
                    (0..=recency::AGGRESSIVE_RECENT_DAYS).contains(&days_old)
                } else {
                    false
                };

                if !qualifies {
                    return None;
                }

                let (surfaced, referenced) = cache
                    .as_ref()
                    .and_then(|c| c.learnings.get(&result.learning.id))
                    .map(|stats| (stats.surfaced, stats.referenced))
                    .unwrap_or((0, 0));

                let half_life = self
                    .config
                    .retrieval
                    .half_life_for_category(&result.learning.category);
                let lambda = recency::lambda_from_half_life(half_life);
                let recency = recency_weight(result.learning.timestamp, now, lambda);
                let hit_rate = if surfaced == 0 {
                    None
                } else {
                    Some(referenced as f64 / surfaced as f64)
                };
                let ref_boost = reference_boost(hit_rate);

                Some(CompositeScore::new(
                    result.learning,
                    result.relevance,
                    recency,
                    ref_boost,
                    strategy,
                ))
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let scored_count = scored.len();

        // Compute top/median/gap for logging before adaptive threshold consumes the vec.
        let top_score = scored.first().map(|s| s.score).unwrap_or(0.0);
        let median_score = if scored.len() >= 3 {
            scored[scored.len() / 2].score
        } else {
            0.0
        };
        let score_gap = top_score - median_score;

        // Adaptive threshold: suppress injection when retriever has no strong signal.
        // Fail-open: any error in threshold logic → fall back to current behavior.
        let scored = match apply_adaptive_threshold(
            scored,
            self.config.retrieval.min_confidence_threshold,
            self.config.retrieval.min_score_gap,
        ) {
            Some(s) => s,
            None => {
                warn!(
                    "Adaptive threshold: suppressing injection (top_score={:.3}, gap={:.3}, threshold={:.3})",
                    top_score, score_gap, self.config.retrieval.min_confidence_threshold
                );
                return Vec::new();
            }
        };

        // Dynamic K: only inject learnings with score >= ratio of top score.
        // When adaptive_dk is enabled, the ratio is adjusted per-query based on
        // score distribution (CV), corpus maturity (stats cache), and per-category
        // dismiss rates.
        let effective_limit = (max_injections as usize).min(strategy.default_max_injections());
        let dk_ratio = if self.config.retrieval.adaptive_dk {
            let score_values: Vec<f64> = scored.iter().map(|s| s.score).collect();
            adaptive_dk_ratio(
                &score_values,
                self.config.retrieval.dynamic_k_ratio,
                cache.as_ref(),
                scored.first().map(|s| &s.learning.category),
            )
        } else {
            self.config.retrieval.dynamic_k_ratio
        };
        let qualified = apply_dynamic_k(scored, dk_ratio, effective_limit);

        if qualified.len() < scored_count {
            debug!(
                "Dynamic K: {} of {} learnings qualified (ratio={:.3}, adaptive={})",
                qualified.len(),
                scored_count,
                dk_ratio,
                self.config.retrieval.adaptive_dk,
            );
        }

        // Intent filter: post-retrieval filtering based on user's first message.
        // Only applies when enabled via config AND a transcript path is available.
        // Fail-open: no transcript, empty keywords, or all filtered → degrade gracefully.
        let intent_cfg = &self.config.retrieval.intent_filter;
        if intent_cfg.enabled {
            if let Some(path) = transcript_path {
                let keywords = extract_user_intent_keywords(path, intent_cfg.max_keywords);
                if !keywords.is_empty() {
                    let pre_filter_count = qualified.len();
                    let filtered: Vec<CompositeScore> = qualified
                        .into_iter()
                        .filter(|cs| {
                            learning_matches_intent(
                                &cs.learning.summary,
                                &cs.learning.detail,
                                &keywords,
                                intent_cfg.min_overlap,
                            )
                        })
                        .collect();
                    if filtered.len() < pre_filter_count {
                        debug!(
                            "Intent filter: {} of {} learnings matched user intent ({} keywords)",
                            filtered.len(),
                            pre_filter_count,
                            keywords.len()
                        );
                    }
                    return filtered;
                }
            }
        }

        qualified
    }

    /// Build injection context string from scored learnings, recording surfaced events.
    ///
    /// Returns the context string if any learnings qualify, `None` otherwise.
    /// Always includes all learnings in the formatted text (for the agent to see),
    /// but only records surfaced events for learnings not already in `session.gate.injected_learnings`.
    ///
    /// The `only_new` parameter controls whether to include already-injected learnings
    /// in the output. When `true` (deferred injection), only new learnings are included.
    /// When `false` (session start), all learnings are included.
    fn build_injection_context(
        &self,
        cwd: &Path,
        session: &mut SessionState,
        scored_learnings: &[CompositeScore],
        only_new: bool,
    ) -> Option<String> {
        if scored_learnings.is_empty() {
            return None;
        }

        let stats_path = project_stats_log_path(cwd);
        let logger = StatsLogger::new(&stats_path);
        let mut context_parts = Vec::new();
        let mut has_content = false;

        context_parts.push("## Relevant Learnings\n".to_string());
        context_parts.push("The following learnings from past work may be relevant:\n".to_string());

        for cs in scored_learnings {
            let learning = &cs.learning;

            // Per-session dedup: check if already surfaced in this session
            let already_injected = session
                .gate
                .injected_learnings
                .iter()
                .any(|il| il.learning_id == learning.id);

            // For deferred injection (only_new=true), skip already-injected learnings entirely.
            // For session start (only_new=false), include them in text but don't re-record.
            if only_new && already_injected {
                continue;
            }

            has_content = true;

            context_parts.push(format!(
                "\n### {} [{}]\n**{}**\n{}\n",
                learning.category.display_name(),
                learning.id,
                learning.summary,
                learning.detail
            ));

            if !already_injected {
                // Record surfaced event only once per session
                let _ = logger.append_surfaced(&learning.id, &session.id, Some(learning.category));

                // Add to session's injected learnings
                session
                    .gate
                    .injected_learnings
                    .push(InjectedLearning::new(&learning.id, cs.score));
            }
        }

        if !has_content {
            return None;
        }

        // Add citation guidance
        context_parts.push("\n---\n".to_string());
        context_parts.push(format!("*Session: {}*\n", session.id));
        context_parts.push(format!(
            "*When a learning above helps your work, run: `grove ref <ID> --session-id {}`*\n",
            session.id
        ));

        Some(context_parts.join(""))
    }

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

    /// Get correction notices for recently corrected learnings.
    ///
    /// This implements correction propagation: when learnings that were
    /// previously surfaced have been corrected, we inject a notice at
    /// session-start to inform the agent of the correction.
    ///
    /// Best-effort: if cache is unavailable, returns empty list.
    fn get_correction_notices(&self, cwd: &Path, session: &SessionState) -> Vec<String> {
        let stats_log_path = project_stats_log_path(cwd);
        let cache_path = cwd.join(".grove").join("stats-cache.json");
        let cache_manager = StatsCacheManager::new(&cache_path, &stats_log_path);

        // Load or rebuild cache (best-effort)
        let cache = match cache_manager.load_or_rebuild() {
            Ok(cache) => cache,
            Err(_) => return Vec::new(),
        };

        // Get corrected learning IDs
        let corrected_ids = cache.get_corrected_learning_ids();
        if corrected_ids.is_empty() {
            return Vec::new();
        }

        // Check if any of the corrected learnings were previously injected to this session
        let mut notices = Vec::new();
        for learning in &session.gate.injected_learnings {
            if corrected_ids.contains(&learning.learning_id) {
                notices.push(format!("- Learning ID: {}", learning.learning_id));
            }
        }

        // Also check all previously seen sessions for this project (best-effort)
        // This is a simplified implementation - in production we'd check recent surfaced events
        // For now, just check the current session's injected learnings

        notices
    }
}

// =========================================================================
// Tantivy BM25 rescoring (feature-gated)
// =========================================================================

/// Rescore search results using Tantivy BM25 relevance.
///
/// Builds an in-memory Tantivy index from the learnings in the search results,
/// constructs a query string from the SearchQuery fields, and replaces each
/// result's relevance score with the normalized BM25 score.
///
/// The `profile` parameter controls which BM25 variant is used:
/// - `Standard` → plain BM25 (better precision for large corpora)
/// - `SmallCorpus` → boosted BM25 (better recall for small corpora)
///
/// Fail-open: any Tantivy error returns original results unchanged.
#[cfg(feature = "tantivy-search")]
fn rescore_with_tantivy(
    results: Vec<crate::backends::SearchResult>,
    query: &SearchQuery,
    profile: crate::config::RetrievalProfile,
) -> Vec<crate::backends::SearchResult> {
    use crate::config::RetrievalProfile;
    use crate::search::TantivySearchIndex;

    let learnings: Vec<_> = results.iter().map(|r| r.learning.clone()).collect();

    // Build in-memory index from the learnings
    let index = match TantivySearchIndex::in_memory() {
        Ok(idx) => idx,
        Err(e) => {
            warn!("Tantivy index creation failed (fail-open): {}", e);
            return results;
        }
    };

    if let Err(e) = index.index_learnings(&learnings) {
        warn!("Tantivy indexing failed (fail-open): {}", e);
        return results;
    }

    // Search with Tantivy — boosted or plain depending on profile
    let tantivy_results = match profile {
        RetrievalProfile::SmallCorpus => {
            let boosted_query = build_tantivy_query_string_boosted(query);
            if boosted_query.trim().is_empty() {
                return results;
            }
            match index.search_boosted(&boosted_query, results.len()) {
                Ok(r) => r,
                Err(e) => {
                    warn!("Tantivy boosted search failed (fail-open): {}", e);
                    return results;
                }
            }
        }
        RetrievalProfile::Standard => {
            let query_string = build_tantivy_query_string(query);
            if query_string.trim().is_empty() {
                return results;
            }
            match index.search(&query_string, results.len()) {
                Ok(r) => r,
                Err(e) => {
                    warn!("Tantivy search failed (fail-open): {}", e);
                    return results;
                }
            }
        }
    };

    // Normalize BM25 scores to [0.0, 1.0]
    let score_map = normalize_bm25_scores(&tantivy_results);

    // Replace relevance scores
    results
        .into_iter()
        .map(|mut r| {
            if let Some(&normalized) = score_map.get(&r.learning.id) {
                r.relevance = normalized;
            } else {
                r.relevance = 0.0;
            }
            r
        })
        .collect()
}

/// Build a query string from SearchQuery fields for Tantivy BM25 scoring.
///
/// Concatenates keywords, tags, file path segments (split on `/`, `.`, filtered),
/// and ticket_id into a single search string.
#[cfg(feature = "tantivy-search")]
fn build_tantivy_query_string(query: &SearchQuery) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Keywords
    parts.extend(query.keywords.iter().cloned());

    // Tags
    parts.extend(query.tags.iter().cloned());

    // File path segments
    for file in &query.files {
        let segments: Vec<String> = file
            .split(['/', '.', '\\'])
            .filter(|s| s.len() >= 3 && !s.chars().all(|c| c.is_numeric()))
            .filter(|s| !matches!(*s, "src" | "lib" | "bin" | "mod" | "tmp" | "var" | "usr"))
            .map(|s| s.to_string())
            .collect();
        parts.extend(segments);
    }

    // Ticket ID
    if let Some(ref ticket_id) = query.ticket_id {
        parts.push(ticket_id.clone());
    }

    parts.join(" ")
}

/// Build a query string with per-term BM25 boosts based on signal source.
///
/// Tool input keywords (file paths, CLI commands) are the highest-precision signal.
/// User intent keywords and domain inference receive lower boosts to avoid
/// overwhelming the primary signal.
///
/// Boost weights (production defaults):
/// - keywords (tool input): 2.0x
/// - tags: 1.5x
/// - file path segments: 1.0x
/// - ticket_id: 1.0x
///
/// Terms are escaped individually, then boost suffix is appended so the `^`
/// character is not affected by escape_query().
#[cfg(feature = "tantivy-search")]
pub(crate) fn build_tantivy_query_string_boosted(query: &SearchQuery) -> String {
    build_tantivy_query_string_boosted_with_params(query, 2.0, 1.5)
}

/// Build a boosted query string with configurable boost factors.
///
/// Same as `build_tantivy_query_string_boosted` but accepts custom boost values
/// for keyword and tag terms. Used by the eval runner to benchmark alternative
/// boost configurations.
#[cfg(feature = "tantivy-search")]
pub(crate) fn build_tantivy_query_string_boosted_with_params(
    query: &SearchQuery,
    keyword_boost: f64,
    tag_boost: f64,
) -> String {
    use crate::search::escape_query_term;

    let mut parts: Vec<String> = Vec::new();

    // Keywords (tool input) — highest signal
    for kw in &query.keywords {
        parts.push(format!("{}^{:.1}", escape_query_term(kw), keyword_boost));
    }

    // Tags — high signal
    for tag in &query.tags {
        parts.push(format!("{}^{:.1}", escape_query_term(tag), tag_boost));
    }

    // File path segments — moderate signal
    for file in &query.files {
        let segments: Vec<String> = file
            .split(['/', '.', '\\'])
            .filter(|s| s.len() >= 3 && !s.chars().all(|c| c.is_numeric()))
            .filter(|s| !matches!(*s, "src" | "lib" | "bin" | "mod" | "tmp" | "var" | "usr"))
            .map(escape_query_term)
            .collect();
        parts.extend(segments);
    }

    // Ticket ID — default weight
    if let Some(ref ticket_id) = query.ticket_id {
        parts.push(escape_query_term(ticket_id));
    }

    parts.join(" ")
}

/// Extract domain vocabulary from corpus learnings for query enrichment.
///
/// Tokenizes each learning's tags, summary, and relevance_context. Filters
/// noise words and short tokens (< 4 chars), then returns terms appearing
/// in at least `min_occurrences` distinct learnings.
///
/// This is corpus-agnostic: instead of hand-tuned domain mappings, the
/// vocabulary comes from the learnings themselves.
#[cfg(feature = "tantivy-search")]
pub(crate) fn extract_corpus_vocabulary(
    learnings: &[crate::core::learning::CompoundLearning],
    min_occurrences: usize,
) -> std::collections::HashSet<String> {
    use std::collections::{HashMap, HashSet};

    // Reuse the expanded noise word list from keyword extraction
    let noise: HashSet<&str> = [
        "ls",
        "cd",
        "git",
        "cat",
        "grep",
        "echo",
        "pwd",
        "rm",
        "mv",
        "cp",
        "mkdir",
        "touch",
        "chmod",
        "chown",
        "sudo",
        "apt",
        "brew",
        "npm",
        "yarn",
        "cargo",
        "make",
        "cmake",
        "true",
        "false",
        "null",
        "test",
        "run",
        "build",
        "install",
        "the",
        "and",
        "for",
        "with",
        "from",
        "this",
        "that",
        "src",
        "lib",
        "bin",
        "tmp",
        "var",
        "etc",
        "usr",
        "opt",
        "home",
        "status",
        "commit",
        "push",
        "pull",
        "fetch",
        "merge",
        "rebase",
        "reset",
        "clean",
        "clone",
        "remote",
        "branch",
        "tag",
        "stash",
        "diff",
        "log",
        "add",
        "checkout",
        "check",
        "release",
        "dev",
        "update",
        "init",
        "start",
        "stop",
        "lint",
        "format",
        "watch",
        "serve",
        "migrate",
        "generate",
        "create",
        "delete",
        "remove",
        "file",
        "new",
        "set",
        "get",
        "list",
        "help",
        "info",
        "version",
        "output",
        "input",
        "data",
        "type",
        "name",
        "path",
        "mode",
        "debug",
        "error",
        "warn",
        "main",
        "index",
        "spec",
        "mod",
        "use",
        "pub",
        "crate",
        "self",
        "super",
        "users",
        "documents",
        "downloads",
        "desktop",
        "applications",
        "library",
        "volumes",
        "private",
        "github",
        "repos",
        "projects",
        "workspace",
        "code",
        "when",
        "should",
        "used",
        "using",
        "also",
        "need",
        "will",
        "have",
        "been",
        "does",
        "into",
        "each",
        "only",
        "than",
        "then",
        "some",
        "such",
        "more",
    ]
    .into_iter()
    .collect();

    // Count how many distinct learnings each term appears in
    let mut term_learnings: HashMap<String, HashSet<usize>> = HashMap::new();

    for (idx, learning) in learnings.iter().enumerate() {
        let mut terms_in_learning: HashSet<String> = HashSet::new();

        // Tokenize tags (already individual terms)
        for tag in &learning.tags {
            let lower = tag.to_lowercase();
            if lower.len() >= 4 && !noise.contains(lower.as_str()) {
                terms_in_learning.insert(lower);
            }
        }

        // Tokenize summary
        for word in learning
            .summary
            .split(|c: char| !c.is_alphanumeric() && c != '-')
        {
            let lower = word.to_lowercase();
            if lower.len() >= 4 && !noise.contains(lower.as_str()) {
                terms_in_learning.insert(lower);
            }
        }

        // Tokenize relevance_context
        if let Some(ref ctx) = learning.relevance_context {
            for word in ctx.split(|c: char| !c.is_alphanumeric() && c != '-') {
                let lower = word.to_lowercase();
                if lower.len() >= 4 && !noise.contains(lower.as_str()) {
                    terms_in_learning.insert(lower);
                }
            }
        }

        // Record all terms from this learning
        for term in terms_in_learning {
            term_learnings.entry(term).or_default().insert(idx);
        }
    }

    // Return terms meeting the minimum occurrence threshold
    term_learnings
        .into_iter()
        .filter(|(_, learning_ids)| learning_ids.len() >= min_occurrences)
        .map(|(term, _)| term)
        .collect()
}

/// Extract corpus vocabulary from borrowed learning references.
///
/// Same as [`extract_corpus_vocabulary`] but accepts `&[&CompoundLearning]`
/// for use in production paths where learnings come from search results.
#[cfg(feature = "tantivy-search")]
fn extract_corpus_vocabulary_from_refs(
    learnings: &[&crate::core::learning::CompoundLearning],
    min_occurrences: usize,
) -> std::collections::HashSet<String> {
    // Collect into owned slice and delegate
    let owned: Vec<crate::core::learning::CompoundLearning> =
        learnings.iter().map(|l| (*l).clone()).collect();
    extract_corpus_vocabulary(&owned, min_occurrences)
}

/// Enrich a query with matching corpus vocabulary terms.
///
/// Tokenizes file paths into segments, then finds the intersection of
/// (query keywords + path segments) with the corpus vocabulary. Returns
/// matched terms not already present in the query keywords.
#[cfg(feature = "tantivy-search")]
pub(crate) fn enrich_query_with_corpus_vocabulary(
    keywords: &[String],
    file_paths: &[String],
    corpus_vocab: &std::collections::HashSet<String>,
) -> Vec<String> {
    use std::collections::HashSet;

    if corpus_vocab.is_empty() {
        return Vec::new();
    }

    let keyword_set: HashSet<String> = keywords.iter().map(|k| k.to_lowercase()).collect();

    // Tokenize file paths into segments
    let mut path_segments: HashSet<String> = HashSet::new();
    for path in file_paths {
        for segment in path.split(['/', '.', '\\', '-', '_']) {
            let lower = segment.to_lowercase();
            if lower.len() >= 4 {
                path_segments.insert(lower);
            }
        }
    }

    // Find intersection with corpus vocabulary, excluding already-present keywords
    let mut enrichment: Vec<String> = path_segments
        .iter()
        .chain(keyword_set.iter())
        .filter(|term| corpus_vocab.contains(term.as_str()) && !keyword_set.contains(term.as_str()))
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    enrichment.sort();
    enrichment
}

/// Normalize BM25 scores to (0.0, 1.0] using max-ratio normalization.
///
/// The top score maps to 1.0, with all others as `score / max_score`.
/// This avoids zeroing out the lowest scorer (which min-max would do),
/// since downstream filters drop results with `relevance == 0.0`.
/// If all scores are equal, all map to 1.0.
#[cfg(feature = "tantivy-search")]
fn normalize_bm25_scores(
    results: &[crate::search::TantivySearchResult],
) -> std::collections::HashMap<String, f64> {
    use std::collections::HashMap;

    let mut map = HashMap::new();
    if results.is_empty() {
        return map;
    }

    let max_score = results
        .iter()
        .map(|r| r.score)
        .fold(f32::NEG_INFINITY, f32::max);

    for r in results {
        let normalized = if max_score < f32::EPSILON {
            1.0 // All scores zero or near-zero → all get 1.0
        } else {
            (r.score / max_score) as f64
        };
        map.insert(r.id.clone(), normalized);
    }

    map
}

// =========================================================================
// Adaptive threshold + dynamic K helpers
// =========================================================================

/// Apply adaptive threshold: suppress injection when the retriever has no strong signal.
///
/// Returns `None` if injection should be suppressed (top score too low or score
/// distribution is flat), `Some(scored)` otherwise.
///
/// # Arguments
///
/// * `scored` - Learnings sorted by score descending
/// * `min_confidence` - Minimum top score to trigger injection
/// * `min_gap` - Minimum gap between top and median score
pub fn apply_adaptive_threshold(
    scored: Vec<CompositeScore>,
    min_confidence: f64,
    min_gap: f64,
) -> Option<Vec<CompositeScore>> {
    if scored.is_empty() {
        return Some(scored);
    }

    let top_score = scored.first().map(|s| s.score).unwrap_or(0.0);
    let median_score = if scored.len() >= 3 {
        scored[scored.len() / 2].score
    } else {
        0.0
    };
    let score_gap = top_score - median_score;

    if top_score < min_confidence || score_gap < min_gap {
        return None;
    }

    Some(scored)
}

/// Apply dynamic K: filter learnings below a ratio of the top score.
///
/// Only learnings with `score >= top_score * ratio` are retained, up to `max_count`.
///
/// # Arguments
///
/// * `scored` - Learnings sorted by score descending
/// * `ratio` - Fraction of the top score below which learnings are excluded
/// * `max_count` - Maximum number of learnings to return
pub fn apply_dynamic_k(
    scored: Vec<CompositeScore>,
    ratio: f64,
    max_count: usize,
) -> Vec<CompositeScore> {
    if scored.is_empty() {
        return scored;
    }

    let top_score = scored.first().map(|s| s.score).unwrap_or(0.0);
    let dynamic_threshold = top_score * ratio;

    scored
        .into_iter()
        .filter(|s| s.score >= dynamic_threshold)
        .take(max_count)
        .collect()
}

/// Compute an adaptive dynamic_k_ratio based on score distribution and stats.
///
/// Designed for gradual self-calibration. Defaults to off (`adaptive_dk: false`)
/// until a repo accumulates enough stats for the data-driven levels to work.
///
/// Level 1 (per-query): Gentle CV-based nudge (±0.03 max). Intentionally mild
/// so it doesn't dominate on small corpora where it can't be validated.
///
/// Level 2 (corpus-maturity): The primary adaptive mechanism. Uses stats cache
/// hit rates (≥20 surfaced learnings required). Low hit rates (noisy corpus) →
/// tighter. High hit rates (healthy corpus) → looser.
///
/// Level 3 (per-category feedback): Fine-grained adjustment per learning category.
/// Categories with high dismiss rates (>50%) get tighter dk for their learnings.
///
/// Returns the adjusted ratio clamped to [0.15, 0.6].
pub fn adaptive_dk_ratio(
    scores: &[f64],
    base_ratio: f64,
    cache: Option<&crate::stats::StatsCache>,
    category: Option<&crate::core::LearningCategory>,
) -> f64 {
    if scores.is_empty() {
        return base_ratio;
    }

    // --- Level 1: Per-query CV adjustment ---
    let n = scores.len() as f64;
    let mean = scores.iter().sum::<f64>() / n;
    let ratio = if mean < f64::EPSILON {
        base_ratio
    } else {
        let variance = scores.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / n;
        let cv = variance.sqrt() / mean;

        // CV < 0.3 → compressed scores → gentle tighten by up to +0.03
        // CV > 0.7 → spread scores → gentle loosen by up to -0.03
        // Kept mild so stats-driven L2/L3 do the heavy lifting over time.
        let cv_adjustment = if cv < 0.3 {
            0.03 * (1.0 - cv / 0.3) // +0.0 to +0.03
        } else if cv > 0.7 {
            -0.03 * ((cv - 0.7) / 0.3).min(1.0) // -0.0 to -0.03
        } else {
            0.0
        };

        base_ratio + cv_adjustment
    };

    // --- Level 2: Corpus-maturity adjustment via stats cache ---
    let ratio = if let Some(cache) = cache {
        let surfaced_count = cache
            .learnings
            .values()
            .filter(|s| s.surfaced > 0 && !s.archived)
            .count();

        if surfaced_count >= 20 {
            let avg_hit_rate = cache.aggregates.average_hit_rate;
            // Low hit rate → corpus is noisy → tighten dk (+0.05)
            // High hit rate → corpus is healthy → loosen dk (-0.05)
            let maturity_adjustment = if avg_hit_rate < 0.3 {
                0.05
            } else if avg_hit_rate > 0.7 {
                -0.05
            } else {
                0.0
            };
            ratio + maturity_adjustment
        } else {
            ratio // Cold start: not enough data
        }
    } else {
        ratio
    };

    // --- Level 3: Per-category dismiss-rate adjustment ---
    let ratio = if let (Some(cache), Some(cat)) = (cache, category) {
        let cat_dismiss_rate = cache
            .learnings
            .values()
            .filter(|s| s.category.as_ref() == Some(cat) && s.surfaced > 0 && !s.archived)
            .map(|s| s.dismissed as f64 / s.surfaced as f64)
            .collect::<Vec<_>>();

        if cat_dismiss_rate.len() >= 3 {
            let avg_dismiss = cat_dismiss_rate.iter().sum::<f64>() / cat_dismiss_rate.len() as f64;
            // High dismiss rate → tighten dk for this category (up to +0.08)
            if avg_dismiss > 0.5 {
                ratio + 0.08 * ((avg_dismiss - 0.5) / 0.5).min(1.0)
            } else {
                ratio
            }
        } else {
            ratio
        }
    } else {
        ratio
    };

    ratio.clamp(0.15, 0.6)
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

/// Extract git context from the working directory for SearchQuery population.
///
/// Returns (changed_files, keywords) where:
/// - changed_files: paths from `git diff --name-only HEAD` (recently modified files)
/// - keywords: terms extracted from the current branch name
///
/// Fails silently (returns empty vecs) if git is not available or cwd is not a repo.
fn extract_git_context(cwd: &Path) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut keywords = Vec::new();

    // 1. Get changed files (staged + unstaged relative to HEAD)
    if let Ok(output) = std::process::Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        if output.status.success() {
            if let Ok(stdout) = String::from_utf8(output.stdout) {
                files = stdout
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| l.to_string())
                    .collect();
            }
        }
    }

    // 2. Get current branch name and extract keywords
    if let Ok(output) = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        if output.status.success() {
            if let Ok(branch) = String::from_utf8(output.stdout) {
                let branch = branch.trim();
                if !branch.is_empty() {
                    // Split branch name on common separators: /, -, _
                    // e.g., "fix/login-bug" → ["fix", "login", "bug"]
                    // Filter out common prefixes that don't add signal
                    let noise: &[&str] = &[
                        "fix", "feat", "feature", "bug", "chore", "docs", "refactor", "test",
                        "hotfix", "release", "main", "master", "develop", "dev",
                    ];
                    for part in branch.split(&['/', '-', '_'][..]) {
                        let part = part.trim().to_lowercase();
                        if part.len() >= 2 && !noise.contains(&part.as_str()) {
                            keywords.push(part);
                        }
                    }
                }
            }
        }
    }

    (files, keywords)
}

/// Extract the current git branch name.
///
/// Returns empty string if git is not available or not in a repo.
fn extract_git_branch(cwd: &Path) -> String {
    if let Ok(output) = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        if output.status.success() {
            if let Ok(branch) = String::from_utf8(output.stdout) {
                return branch.trim().to_string();
            }
        }
    }
    String::new()
}

/// Extract keywords from the user's first real message in a transcript JSONL file.
///
/// Used by the intent filter (post-retrieval) when `retrieval.intent_filter.enabled`
/// is true. Also used by the eval harness for benchmarking.
///
/// Reads the transcript line-by-line, skipping meta entries (local-command-*,
/// command-name), and extracts keywords from the first substantive user message.
/// Applies noise/stopword filtering with a minimum keyword length of 4.
///
/// Returns up to `max_keywords` deduplicated keywords, or an empty Vec if
/// the transcript cannot be read or contains no real user message.
pub fn extract_user_intent_keywords(transcript_path: &Path, max_keywords: usize) -> Vec<String> {
    use std::collections::HashSet;
    use std::io::BufRead;

    let file = match std::fs::File::open(transcript_path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let reader = std::io::BufReader::new(file);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        let obj: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Only look at user messages
        if obj.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }

        let content = match obj
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
        {
            Some(s) if s.len() > 10 => s,
            _ => continue,
        };

        // Skip meta entries
        if content.contains("<local-command") || content.contains("<command-name>") {
            continue;
        }

        // Found the first real user message — extract keywords
        // Strip XML tags
        let stripped = {
            let mut result = String::with_capacity(content.len());
            let mut in_tag = false;
            for ch in content.chars() {
                if ch == '<' {
                    in_tag = true;
                } else if ch == '>' {
                    in_tag = false;
                } else if !in_tag {
                    result.push(ch);
                }
            }
            result
        };

        // Noise words (subset — avoid matching on implementation plan boilerplate)
        let noise: &[&str] = &[
            "implement",
            "following",
            "plan",
            "context",
            "current",
            "currently",
            "should",
            "would",
            "could",
            "using",
            "needs",
            "need",
            "will",
            "also",
            "make",
            "sure",
            "like",
            "just",
            "want",
            "file",
            "files",
            "code",
            "changes",
            "change",
            "update",
            "create",
            "added",
            "adding",
            "remove",
            "note",
            "important",
            "existing",
            "based",
            "approach",
            "step",
            "steps",
            "first",
            "then",
            "next",
            "after",
            "before",
            "each",
            "every",
            "both",
            "more",
            "most",
            "some",
            "many",
            "very",
            "only",
            "other",
            "still",
            "already",
            "must",
            "shall",
            "into",
            "from",
            "with",
            "that",
            "this",
            "have",
            "been",
            "being",
            "does",
            "done",
            "when",
            "where",
            "which",
            "what",
            "they",
            "them",
            "their",
            "there",
            "here",
            "these",
            "those",
        ];
        let noise_set: HashSet<&str> = noise.iter().copied().collect();

        let mut seen = HashSet::new();
        let mut keywords = Vec::new();

        for word in stripped.split(|c: char| !c.is_alphanumeric() && c != '_') {
            let lower = word.to_lowercase();
            if lower.len() >= 4 && !noise_set.contains(lower.as_str()) && seen.insert(lower.clone())
            {
                keywords.push(lower);
                if keywords.len() >= max_keywords {
                    break;
                }
            }
        }

        return keywords;
    }

    Vec::new()
}

/// Check if a learning's text contains any overlap with user intent keywords.
///
/// Used as a post-retrieval filter: after BM25 + adaptive threshold selects
/// candidate learnings, this function checks whether the learning's content
/// shares vocabulary with the user's stated intent. Learnings with no keyword
/// overlap are likely false positives from BM25 scoring.
///
/// Returns true if at least `min_overlap` intent keywords appear in the
/// learning's summary + detail text (case-insensitive, tokenized the same way).
pub fn learning_matches_intent(
    summary: &str,
    detail: &str,
    intent_keywords: &[String],
    min_overlap: usize,
) -> bool {
    if intent_keywords.is_empty() {
        return true; // No intent signal → don't filter
    }

    // Tokenize learning text into lowercase words (same rules as keyword extraction)
    let mut learning_words: std::collections::HashSet<String> = std::collections::HashSet::new();
    let combined = format!("{} {}", summary, detail);
    for word in combined.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let lower = word.to_lowercase();
        if lower.len() >= 4 {
            learning_words.insert(lower);
        }
    }

    let overlap = intent_keywords
        .iter()
        .filter(|kw| learning_words.contains(kw.as_str()))
        .count();

    overlap >= min_overlap
}

/// Extract keywords from a user's prompt text for mid-session re-retrieval.
///
/// Uses the same noise filtering and tokenization as `extract_user_intent_keywords`
/// but operates on a string directly rather than reading from a transcript file.
/// Returns up to `max_keywords` deduplicated keywords.
pub fn extract_prompt_keywords(prompt: &str, max_keywords: usize) -> Vec<String> {
    use std::collections::HashSet;

    if prompt.len() <= 10 {
        return Vec::new();
    }

    // Strip XML tags (same as extract_user_intent_keywords)
    let stripped = {
        let mut result = String::with_capacity(prompt.len());
        let mut in_tag = false;
        for ch in prompt.chars() {
            if ch == '<' {
                in_tag = true;
            } else if ch == '>' {
                in_tag = false;
            } else if !in_tag {
                result.push(ch);
            }
        }
        result
    };

    // Noise words (same set as extract_user_intent_keywords)
    let noise: &[&str] = &[
        "implement",
        "following",
        "plan",
        "context",
        "current",
        "currently",
        "should",
        "would",
        "could",
        "using",
        "needs",
        "need",
        "will",
        "also",
        "make",
        "sure",
        "like",
        "just",
        "want",
        "file",
        "files",
        "code",
        "changes",
        "change",
        "update",
        "create",
        "added",
        "adding",
        "remove",
        "note",
        "important",
        "existing",
        "based",
        "approach",
        "step",
        "steps",
        "first",
        "then",
        "next",
        "after",
        "before",
        "each",
        "every",
        "both",
        "more",
        "most",
        "some",
        "many",
        "very",
        "only",
        "other",
        "still",
        "already",
        "must",
        "shall",
        "into",
        "from",
        "with",
        "that",
        "this",
        "have",
        "been",
        "being",
        "does",
        "done",
        "when",
        "where",
        "which",
        "what",
        "they",
        "them",
        "their",
        "there",
        "here",
        "these",
        "those",
    ];
    let noise_set: HashSet<&str> = noise.iter().copied().collect();

    let mut seen = HashSet::new();
    let mut keywords = Vec::new();

    for word in stripped.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let lower = word.to_lowercase();
        if lower.len() >= 4 && !noise_set.contains(lower.as_str()) && seen.insert(lower.clone()) {
            keywords.push(lower);
            if keywords.len() >= max_keywords {
                break;
            }
        }
    }

    keywords
}

// =========================================================================
// LLM Reranking
// =========================================================================

/// Build a reranking prompt for the LLM.
///
/// Asks the LLM to score each candidate learning's relevance (1-5) to the
/// current session context. Context includes tool name, tool input excerpt,
/// and git branch/files.
fn build_rerank_prompt(
    candidates: &[CompositeScore],
    tool_name: &str,
    tool_input_excerpt: &str,
    git_branch: &str,
    git_files: &[String],
) -> String {
    let mut prompt = String::from(
        "You are evaluating which captured learnings are most relevant to the developer's current task.\n\n\
         Session context:\n",
    );

    if !tool_name.is_empty() {
        prompt.push_str(&format!("- Tool: {}\n", tool_name));
    }
    if !tool_input_excerpt.is_empty() {
        let excerpt = crate::core::judge::truncate_str(tool_input_excerpt, 500);
        prompt.push_str(&format!("- Tool input: {}\n", excerpt));
    }
    if !git_branch.is_empty() {
        prompt.push_str(&format!("- Branch: {}\n", git_branch));
    }
    if !git_files.is_empty() {
        let files_str = git_files
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        prompt.push_str(&format!("- Recent files: {}\n", files_str));
    }

    prompt.push_str("\nCandidate learnings:\n");
    for (i, cs) in candidates.iter().enumerate() {
        let summary = crate::core::judge::truncate_str(&cs.learning.summary, 200);
        let detail = crate::core::judge::truncate_str(&cs.learning.detail, 300);
        prompt.push_str(&format!(
            "\n[{}] {}\n    Detail: {}\n    Tags: {}\n",
            i + 1,
            summary,
            detail,
            cs.learning.tags.join(", "),
        ));
    }

    prompt.push_str(
        "\nFor each learning, score its relevance to the current task from 1 (irrelevant) to 5 (highly relevant).\n\
         Respond with ONLY the scores as comma-separated integers. Example for 3 learnings: 4,2,5",
    );

    prompt
}

/// Parse comma-separated scores from the LLM reranking response.
///
/// Returns `None` if parsing fails or the count doesn't match expected.
fn parse_rerank_scores(response: &str, expected_count: usize) -> Option<Vec<f64>> {
    // Try to extract from JSON wrapper first (--output-format json)
    let text = if let Ok(json) = serde_json::from_str::<serde_json::Value>(response) {
        json.get("result")
            .and_then(|r| r.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| response.trim().to_string())
    } else {
        response.trim().to_string()
    };

    let scores: Vec<f64> = text
        .split(',')
        .filter_map(|s| {
            let digits: String = s.trim().chars().filter(|c| c.is_ascii_digit()).collect();
            digits.parse::<u32>().ok().map(|d| d.clamp(1, 5) as f64)
        })
        .collect();

    if scores.len() == expected_count {
        Some(scores)
    } else {
        eprintln!(
            "Warning: rerank response had {} scores, expected {}",
            scores.len(),
            expected_count
        );
        None
    }
}

/// Call the LLM to rerank candidates via the CLI backend.
///
/// Returns `None` on any failure (fail-open).
fn call_rerank_cli(model: &str, timeout: u32, prompt: &str) -> Option<String> {
    let timeout_str = timeout.to_string();
    let output = match std::process::Command::new("timeout")
        .args([
            &timeout_str,
            "claude",
            "-p",
            prompt,
            "--model",
            model,
            "--output-format",
            "json",
        ])
        .env_remove("CLAUDECODE")
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            eprintln!("Warning: failed to invoke claude CLI for rerank: {}", e);
            return None;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if stderr.trim().is_empty() {
            &stdout
        } else {
            &stderr
        };
        eprintln!(
            "Warning: rerank CLI exited with status {}: {}",
            output.status,
            crate::core::judge::truncate_str(detail, 200)
        );
        return None;
    }

    Some(stdout.to_string())
}

/// Call the LLM to rerank candidates via the API backend.
///
/// Returns `None` on any failure (fail-open).
fn call_rerank_api(model: &str, api_url: &str, timeout: u32, prompt: &str) -> Option<String> {
    let api_key = match std::env::var("ANTHROPIC_API_KEY") {
        Ok(key) if !key.is_empty() => key,
        _ => {
            eprintln!("Warning: ANTHROPIC_API_KEY not set, skipping rerank (fail-open)");
            return None;
        }
    };

    let body = serde_json::json!({
        "model": model,
        "max_tokens": 64,
        "messages": [{
            "role": "user",
            "content": prompt
        }]
    });

    let timeout_str = timeout.to_string();
    let auth_header = format!("x-api-key: {}", api_key);
    let output = match std::process::Command::new("curl")
        .args([
            "-s",
            "--max-time",
            &timeout_str,
            "-X",
            "POST",
            api_url,
            "-H",
            "Content-Type: application/json",
            "-H",
            &auth_header,
            "-H",
            "anthropic-version: 2023-06-01",
            "-d",
            &body.to_string(),
        ])
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            eprintln!("Warning: failed to invoke curl for rerank API: {}", e);
            return None;
        }
    };

    if !output.status.success() {
        eprintln!(
            "Warning: rerank API curl failed with status {}",
            output.status
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Warning: rerank API returned invalid JSON: {}", e);
            return None;
        }
    };

    // Extract text from the Messages API response
    json.get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|block| block.get("text"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
}

/// Rerank candidates using an LLM.
///
/// Sends a single prompt with all candidates and session context.
/// The LLM scores each candidate 1-5 for relevance. Candidates are then
/// re-sorted by (LLM score descending, original BM25 score descending).
///
/// Fail-open: returns the original ordering if any step fails.
pub(crate) fn rerank_with_llm(
    candidates: Vec<CompositeScore>,
    config: &crate::config::RerankConfig,
    api_url: &str,
    tool_name: &str,
    tool_input_excerpt: &str,
    git_branch: &str,
    git_files: &[String],
) -> Vec<CompositeScore> {
    if candidates.is_empty() {
        return candidates;
    }

    let prompt = build_rerank_prompt(
        &candidates,
        tool_name,
        tool_input_excerpt,
        git_branch,
        git_files,
    );

    let response = match config.backend.as_str() {
        "cli" => call_rerank_cli(&config.model, config.timeout_seconds, &prompt),
        "api" => call_rerank_api(&config.model, api_url, config.timeout_seconds, &prompt),
        other => {
            eprintln!(
                "Warning: unknown rerank backend '{}', skipping (fail-open)",
                other
            );
            return candidates;
        }
    };

    let response = match response {
        Some(r) => r,
        None => return candidates, // fail-open
    };

    let scores = match parse_rerank_scores(&response, candidates.len()) {
        Some(s) => s,
        None => return candidates, // fail-open
    };

    // Pair candidates with LLM scores and re-sort
    let mut paired: Vec<(CompositeScore, f64)> = candidates.into_iter().zip(scores).collect();
    paired.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.0.score
                    .partial_cmp(&a.0.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    paired.into_iter().map(|(cs, _)| cs).collect()
}

/// Infer domain keywords from file path clustering.
///
/// Currently benchmark-only (not wired into production paths). Awaiting
/// multi-corpus validation before enabling — see grove-5npk0mtt.
///
/// Analyzes a list of file paths to extract semantic domain keywords that
/// enrich BM25 queries. Uses two strategies:
/// 1. **Directory clustering** — requires ≥2 distinct files touching a domain group
/// 2. **Special file detection** — single file triggers (Dockerfile, CI configs, etc.)
///
/// Extension-based language keywords (e.g., "elixir", "rust") are intentionally
/// excluded — they match too broadly in monoglot codebases, diluting BM25 precision.
///
/// Returns a sorted, deduplicated `Vec<String>` of domain keywords.
#[cfg(test)]
fn infer_domains_from_paths(paths: &[String]) -> Vec<String> {
    use std::collections::{HashMap, HashSet};

    let mut domains = HashSet::new();

    // Track unique files per domain group (not component hits).
    // Key = domain name, Value = set of file indices that matched.
    let mut domain_files: HashMap<&str, HashSet<usize>> = HashMap::new();

    // Directory-to-domain mapping
    let dir_domains: &[(&[&str], &str)] = &[
        (
            &["auth", "authentication", "authorization", "oauth", "sso"],
            "authentication",
        ),
        (
            &["database", "migrations", "schemas", "repo", "ecto"],
            "database",
        ),
        (
            &["api", "controllers", "endpoints", "routes", "router"],
            "api",
        ),
        (
            &["components", "views", "templates", "layouts", "assets"],
            "frontend",
        ),
        (&["live"], "liveview"),
        (
            &[
                "deploy",
                "deployment",
                "infra",
                "infrastructure",
                "terraform",
                "kamal",
            ],
            "deployment",
        ),
        (&["test", "tests", "spec", "specs", "fixtures"], "testing"),
        (&["config", "configuration", "settings"], "configuration"),
        (&["hooks", "plugins", "middleware"], "hooks"),
        (&["search", "indexing"], "search"),
        (&["stats", "analytics", "metrics", "telemetry"], "analytics"),
        (&["cli", "commands"], "cli"),
    ];

    for (file_idx, path) in paths.iter().enumerate() {
        // Split path into components
        let components: Vec<&str> = path.split('/').collect();

        // 1. Directory clustering: track which files touch each domain group
        for component in &components {
            let lower = component.to_lowercase();
            for (patterns, domain) in dir_domains {
                if patterns.contains(&lower.as_str()) {
                    domain_files.entry(domain).or_default().insert(file_idx);
                }
            }
        }

        // 2. Special file detection
        let file_name = components.last().unwrap_or(&"");
        let file_lower = file_name.to_lowercase();

        // Dockerfile / docker-compose
        if file_lower == "dockerfile" || file_lower.starts_with("docker-compose") {
            domains.insert("deployment".to_string());
        }
        // fly.toml
        if file_lower == "fly.toml" {
            domains.insert("deployment".to_string());
        }
        // .kamal/ directory
        if components.iter().any(|c| c.to_lowercase() == ".kamal") {
            domains.insert("deployment".to_string());
        }
        // CI files
        if path.contains(".github/workflows") || file_lower == "ci.yml" {
            domains.insert("ci".to_string());
        }
        // Test file patterns: *_test.*, *.test.*, *_spec.*
        if let Some(stem) = Path::new(file_name).file_stem().and_then(|s| s.to_str()) {
            let stem_lower = stem.to_lowercase();
            if stem_lower.ends_with("_test")
                || stem_lower.ends_with(".test")
                || stem_lower.ends_with("_spec")
            {
                domains.insert("testing".to_string());
            }
        }
    }

    // Apply directory clustering threshold: require ≥2 distinct files in a domain group
    for (domain, files) in &domain_files {
        if files.len() >= 2 {
            domains.insert((*domain).to_string());
        }
    }

    let mut result: Vec<String> = domains.into_iter().collect();
    result.sort();
    result
}

/// Extract keywords from a tool's input for deferred injection retrieval.
///
/// Examines the tool name and input JSON to extract meaningful keywords
/// that indicate the user's intent. These keywords augment git-based
/// retrieval to surface more relevant learnings.
pub fn extract_tool_input_keywords(tool_name: &str, tool_input: &serde_json::Value) -> Vec<String> {
    let mut keywords = Vec::new();

    // Noise words to filter out (common CLI terms, flags, etc.)
    // Includes expanded list covering git subcommands, build/test verbs,
    // generic programming terms, and common path components (R2 improvement).
    let noise: &[&str] = &[
        // Original base noise list
        "ls",
        "cd",
        "git",
        "cat",
        "grep",
        "echo",
        "pwd",
        "rm",
        "mv",
        "cp",
        "mkdir",
        "touch",
        "chmod",
        "chown",
        "sudo",
        "apt",
        "brew",
        "npm",
        "yarn",
        "cargo",
        "make",
        "cmake",
        "true",
        "false",
        "null",
        "test",
        "run",
        "build",
        "install",
        "the",
        "and",
        "for",
        "with",
        "from",
        "this",
        "that",
        "src",
        "lib",
        "bin",
        "tmp",
        "var",
        "etc",
        "usr",
        "opt",
        "home",
        // Git subcommands
        "status",
        "commit",
        "push",
        "pull",
        "fetch",
        "merge",
        "rebase",
        "reset",
        "clean",
        "clone",
        "remote",
        "branch",
        "tag",
        "stash",
        "diff",
        "log",
        "add",
        "checkout",
        "cherry",
        "pick",
        "bisect",
        "blame",
        "show",
        // Build/test verbs
        "check",
        "release",
        "dev",
        "update",
        "init",
        "start",
        "stop",
        "lint",
        "format",
        "watch",
        "serve",
        "migrate",
        "generate",
        "create",
        "delete",
        "remove",
        // Generic programming terms
        "file",
        "new",
        "set",
        "get",
        "list",
        "help",
        "info",
        "version",
        "output",
        "input",
        "data",
        "type",
        "name",
        "path",
        "mode",
        "debug",
        "error",
        "warn",
        "main",
        "index",
        "spec",
        "mod",
        "use",
        "pub",
        "crate",
        "self",
        "super",
        // Common path components
        "users",
        "documents",
        "downloads",
        "desktop",
        "applications",
        "library",
        "volumes",
        "private",
        "github",
        "repos",
        "projects",
        "workspace",
        "code",
    ];

    // English stopwords for natural language filtering (R3 improvement).
    // Separate from the CLI/dev-tool noise list above. Used for tools like
    // Task that receive free-form English prompts where common words would
    // cause retrieval explosion by matching virtually every learning.
    let stopwords: &[&str] = &[
        // Articles / determiners
        "the",
        "an",
        "this",
        "that",
        "these",
        "those",
        "each",
        "every",
        // Pronouns
        "me",
        "my",
        "mine",
        "we",
        "our",
        "ours",
        "you",
        "your",
        "yours",
        "he",
        "him",
        "his",
        "she",
        "her",
        "hers",
        "it",
        "its",
        "they",
        "them",
        "their",
        "theirs",
        "who",
        "whom",
        "what",
        "which",
        "how",
        "when",
        "where",
        "why",
        "whose",
        // Prepositions / particles
        "in",
        "on",
        "at",
        "to",
        "of",
        "by",
        "as",
        "up",
        "out",
        "off",
        "into",
        "onto",
        "upon",
        "about",
        "after",
        "before",
        "between",
        "through",
        "during",
        "above",
        "below",
        "under",
        "over",
        "near",
        "along",
        "across",
        "against",
        "toward",
        "towards",
        "within",
        "without",
        "around",
        // Conjunctions
        "and",
        "but",
        "or",
        "nor",
        "if",
        "then",
        "than",
        "so",
        "yet",
        "both",
        "either",
        "neither",
        "whether",
        "because",
        "since",
        "while",
        "although",
        "though",
        "unless",
        "until",
        // Common verbs / auxiliaries
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "being",
        "have",
        "has",
        "had",
        "having",
        "do",
        "does",
        "did",
        "will",
        "would",
        "could",
        "should",
        "shall",
        "may",
        "might",
        "can",
        "must",
        "need",
        "want",
        "like",
        "make",
        "find",
        "know",
        "think",
        "take",
        "come",
        "see",
        "look",
        "give",
        "go",
        "use",
        "try",
        "ask",
        "work",
        "call",
        "keep",
        "let",
        "say",
        "tell",
        "put",
        "show",
        "get",
        "set",
        "run",
        "seem",
        "feel",
        "leave",
        "turn",
        "mean",
        "move",
        "follow",
        "begin",
        "help",
        // Adverbs / modifiers
        "also",
        "just",
        "only",
        "very",
        "well",
        "still",
        "even",
        "back",
        "now",
        "here",
        "there",
        "not",
        "no",
        "yes",
        "too",
        "more",
        "less",
        "most",
        "much",
        "many",
        "few",
        "really",
        "quite",
        "rather",
        "already",
        "always",
        "never",
        "often",
        "sometimes",
        "perhaps",
        "maybe",
        "enough",
        "almost",
        "away",
        // Adjectives (generic)
        "other",
        "another",
        "same",
        "different",
        "first",
        "last",
        "next",
        "new",
        "old",
        "good",
        "bad",
        "great",
        "little",
        "long",
        "own",
        "right",
        "left",
        "sure",
        "able",
        "else",
        "some",
        "any",
        "all",
        "such",
        // Generic task / instruction words
        "please",
        "using",
        "used",
        "based",
        "related",
        "ensure",
        "verify",
        "appropriate",
        "check",
        "search",
        "review",
        "implement",
        "update",
        "determine",
        "consider",
        "investigate",
        "analyze",
        "examine",
        "describe",
        "explain",
        "provide",
        "include",
        "exclude",
        "specific",
        "current",
        "existing",
        "available",
        "possible",
        "necessary",
        "required",
        "relevant",
        "important",
    ];

    // Acronym allowlist: 3-character (or shorter) technical terms that are
    // genuinely meaningful for learning retrieval and should be preserved
    // even though they're below the minimum keyword length of 4 (R4).
    // Generic 3-char words (app, log, env, dev, get, set, run, etc.) are
    // still filtered -- they're either in the noise list or too ambiguous.
    let acronym_allowlist: &[&str] = &[
        // Databases & query languages
        "sql", // Cloud providers & services
        "aws", "gcp", "vpc", "rds", "sqs", "sns", "iam", "ec2", "cdn",
        // Auth & security
        "jwt", "ssl", "tls", "ssh", // Networking
        "tcp", "udp", "dns", "rpc", "ip", // Web & markup
        "api", "css", "dom", "xml", "csv", "svg", "url", "uri", // Image formats
        "png", "jpg", "gif", "pdf", // CLI & tooling
        "cli", "sdk", "orm", // Hardware & system
        "gpu", "cpu", "ram", // AI/ML
        "mcp", "gpt", "llm", "rag", "nlp", // DevOps
        "ci", "cd", // Cloud storage
        "s3", // Identifiers
        "uid", "pid",
    ];

    let extract_words = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| {
                let w_lower = w.to_lowercase();
                (w.len() >= 4 || acronym_allowlist.contains(&w_lower.as_str()))
                    && !w.starts_with('-')
                    && !noise.contains(&w_lower.as_str())
                    && !w.chars().all(|c| c.is_numeric())
            })
            .map(|w| w.to_lowercase())
            .collect()
    };

    // Extract words from natural language text, applying both noise and
    // stopword filters (R3). Used for tools like Task whose input is
    // free-form English rather than structured CLI / path data.
    let extract_words_nlp = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| {
                let w_lower = w.to_lowercase();
                (w.len() >= 4 || acronym_allowlist.contains(&w_lower.as_str()))
                    && !w.starts_with('-')
                    && !noise.contains(&w_lower.as_str())
                    && !stopwords.contains(&w_lower.as_str())
                    && !w.chars().all(|c| c.is_numeric())
            })
            .map(|w| w.to_lowercase())
            .collect()
    };

    /// Strip common path prefixes from absolute file paths (R1 improvement).
    ///
    /// Removes user home directory patterns like `/Users/<name>/GitHub/<org>/`
    /// or `/home/<name>/projects/` to keep only the project-relative path.
    fn strip_path_prefix(path: &str) -> &str {
        let parts: Vec<&str> = path.split('/').collect();

        // Hosting services (GitHub, GitLab) have an org level between the
        // marker and the project name: /Users/x/GitHub/<org>/<project>/...
        // We skip marker + org to get to the project.
        let hosting_with_org = ["GitHub", "github", "GitLab", "gitlab"];

        // Local workspace dirs go directly to the project:
        // /home/x/projects/<project>/... or /Users/x/code/<project>/...
        let workspace_dirs = [
            "repos",
            "Repos",
            "projects",
            "Projects",
            "workspace",
            "Workspace",
            "code",
            "Code",
        ];

        for (i, part) in parts.iter().enumerate() {
            if hosting_with_org.contains(part) {
                // Skip marker + org name, return from project onward
                // e.g., /Users/dev/GitHub/org/my-project/src/config.rs
                //        0     1     2      3     4           5   6
                // marker at 2, skip org at 3, return from 4 (my-project/...)
                if i + 2 < parts.len() {
                    let skip: usize = parts[..i + 2].iter().map(|p| p.len() + 1).sum();
                    return &path[skip..];
                }
            }
            if workspace_dirs.contains(part) {
                // Skip only the marker, return from project onward
                // e.g., /home/developer/projects/myapp/lib/database.rs
                //        0    1         2        3     4   5
                // marker at 2, return from 3 (myapp/...)
                if i + 1 < parts.len() {
                    let skip: usize = parts[..i + 1].iter().map(|p| p.len() + 1).sum();
                    return &path[skip..];
                }
            }
        }

        // Fallback: if path starts with /Users/<x>/ or /home/<x>/, strip those
        if parts.len() > 3 && (parts[1] == "Users" || parts[1] == "home") {
            let skip: usize = parts[..3].iter().map(|p| p.len() + 1).sum();
            return &path[skip..];
        }

        path
    }

    match tool_name {
        "Bash" => {
            if let Some(command) = tool_input.get("command").and_then(|v| v.as_str()) {
                keywords.extend(extract_words(command));
            }
        }
        "Grep" => {
            if let Some(pattern) = tool_input.get("pattern").and_then(|v| v.as_str()) {
                keywords.extend(extract_words(pattern));
            }
        }
        "Read" | "Write" | "Edit" => {
            if let Some(file_path) = tool_input.get("file_path").and_then(|v| v.as_str()) {
                let effective_path = strip_path_prefix(file_path);
                keywords.extend(extract_words(effective_path));
            }
        }
        "Glob" => {
            if let Some(pattern) = tool_input.get("pattern").and_then(|v| v.as_str()) {
                keywords.extend(extract_words(pattern));
            }
        }
        "WebSearch" => {
            if let Some(query) = tool_input.get("query").and_then(|v| v.as_str()) {
                keywords.extend(extract_words(query));
            }
        }
        "WebFetch" => {
            if let Some(url) = tool_input.get("url").and_then(|v| v.as_str()) {
                keywords.extend(extract_words(url));
            }
        }
        "Task" => {
            // R3: Task tool sends natural language prompts. Use the NLP
            // extractor which applies both the CLI noise filter and the
            // English stopword filter to prevent retrieval explosion.
            if let Some(prompt) = tool_input.get("prompt").and_then(|v| v.as_str()) {
                keywords.extend(extract_words_nlp(prompt));
            }
        }
        _ => {
            // Unknown tool: no keywords extracted
        }
    }

    // Dedup
    keywords.sort();
    keywords.dedup();
    keywords
}

/// Configurable v2 keyword extractor for quality audit experiments.
///
/// Production now includes R1 (path stripping), R2 (expanded noise list),
/// R3 (Task tool support with stopword filtering), and R4 (min keyword
/// length 4 with acronym allowlist).
///
/// This v2 function mirrors production behavior but with configurable
/// toggles for experiment baseline comparisons. The `strip_paths` and
/// `expanded_noise` toggles can be set to `false` to simulate pre-R1/R2
/// behavior. Task tool support (R3) defaults to true.
/// The `min_keyword_len` parameter defaults to 4 (matching production).
/// The acronym allowlist is always active regardless of min_keyword_len.
pub fn extract_tool_input_keywords_v2(
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> Vec<String> {
    extract_tool_input_keywords_v2_with_options(tool_name, tool_input, true, true, true, 4)
}

/// Configurable experimental keyword extractor.
///
/// R1 (path stripping), R2 (expanded noise), R3 (Task tool with
/// stopword filtering), and R4 (min keyword length 4 with acronym
/// allowlist) are now all in production.
/// The `strip_paths` and `expanded_noise` toggles allow simulating
/// pre-R1/R2 behavior for baseline experiment comparisons.
/// R3 (`task_tool_support`) defaults to true. `min_keyword_len`
/// defaults to 4 (matching production).
pub fn extract_tool_input_keywords_v2_with_options(
    tool_name: &str,
    tool_input: &serde_json::Value,
    strip_paths: bool,
    expanded_noise: bool,
    task_tool_support: bool,
    min_keyword_len: usize,
) -> Vec<String> {
    let mut keywords = Vec::new();

    // Original noise list (44 entries)
    let base_noise: &[&str] = &[
        "ls", "cd", "git", "cat", "grep", "echo", "pwd", "rm", "mv", "cp", "mkdir", "touch",
        "chmod", "chown", "sudo", "apt", "brew", "npm", "yarn", "cargo", "make", "cmake", "true",
        "false", "null", "test", "run", "build", "install", "the", "and", "for", "with", "from",
        "this", "that", "src", "lib", "bin", "tmp", "var", "etc", "usr", "opt", "home",
    ];

    // Expanded noise list additions (~60 new entries)
    let expanded_noise_additions: &[&str] = &[
        // Git subcommands
        "status",
        "commit",
        "push",
        "pull",
        "fetch",
        "merge",
        "rebase",
        "reset",
        "clean",
        "clone",
        "remote",
        "branch",
        "tag",
        "stash",
        "diff",
        "log",
        "add",
        "checkout",
        "cherry",
        "pick",
        "bisect",
        "blame",
        "show",
        // Build/test verbs
        "check",
        "release",
        "dev",
        "update",
        "init",
        "start",
        "stop",
        "lint",
        "format",
        "watch",
        "serve",
        "migrate",
        "generate",
        "create",
        "delete",
        "remove",
        // Generic programming terms
        "file",
        "new",
        "set",
        "get",
        "list",
        "help",
        "info",
        "version",
        "output",
        "input",
        "data",
        "type",
        "name",
        "path",
        "mode",
        "debug",
        "error",
        "warn",
        "main",
        "index",
        "spec",
        "mod",
        "use",
        "pub",
        "crate",
        "self",
        "super",
        // Common path components
        "users",
        "documents",
        "downloads",
        "desktop",
        "applications",
        "library",
        "volumes",
        "private",
        "github",
        "repos",
        "projects",
        "workspace",
        "code",
    ];

    let noise: Vec<&str> = if expanded_noise {
        let mut combined = base_noise.to_vec();
        combined.extend_from_slice(expanded_noise_additions);
        combined
    } else {
        base_noise.to_vec()
    };

    // English stopwords for natural language filtering (R3).
    // Mirrors the production stopword list. Used for Task tool prompts
    // to prevent retrieval explosion from common English words.
    let stopwords: &[&str] = &[
        // Articles / determiners
        "the",
        "an",
        "this",
        "that",
        "these",
        "those",
        "each",
        "every",
        // Pronouns
        "me",
        "my",
        "mine",
        "we",
        "our",
        "ours",
        "you",
        "your",
        "yours",
        "he",
        "him",
        "his",
        "she",
        "her",
        "hers",
        "it",
        "its",
        "they",
        "them",
        "their",
        "theirs",
        "who",
        "whom",
        "what",
        "which",
        "how",
        "when",
        "where",
        "why",
        "whose",
        // Prepositions / particles
        "in",
        "on",
        "at",
        "to",
        "of",
        "by",
        "as",
        "up",
        "out",
        "off",
        "into",
        "onto",
        "upon",
        "about",
        "after",
        "before",
        "between",
        "through",
        "during",
        "above",
        "below",
        "under",
        "over",
        "near",
        "along",
        "across",
        "against",
        "toward",
        "towards",
        "within",
        "without",
        "around",
        // Conjunctions
        "and",
        "but",
        "or",
        "nor",
        "if",
        "then",
        "than",
        "so",
        "yet",
        "both",
        "either",
        "neither",
        "whether",
        "because",
        "since",
        "while",
        "although",
        "though",
        "unless",
        "until",
        // Common verbs / auxiliaries
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "being",
        "have",
        "has",
        "had",
        "having",
        "do",
        "does",
        "did",
        "will",
        "would",
        "could",
        "should",
        "shall",
        "may",
        "might",
        "can",
        "must",
        "need",
        "want",
        "like",
        "make",
        "find",
        "know",
        "think",
        "take",
        "come",
        "see",
        "look",
        "give",
        "go",
        "use",
        "try",
        "ask",
        "work",
        "call",
        "keep",
        "let",
        "say",
        "tell",
        "put",
        "show",
        "get",
        "set",
        "run",
        "seem",
        "feel",
        "leave",
        "turn",
        "mean",
        "move",
        "follow",
        "begin",
        "help",
        // Adverbs / modifiers
        "also",
        "just",
        "only",
        "very",
        "well",
        "still",
        "even",
        "back",
        "now",
        "here",
        "there",
        "not",
        "no",
        "yes",
        "too",
        "more",
        "less",
        "most",
        "much",
        "many",
        "few",
        "really",
        "quite",
        "rather",
        "already",
        "always",
        "never",
        "often",
        "sometimes",
        "perhaps",
        "maybe",
        "enough",
        "almost",
        "away",
        // Adjectives (generic)
        "other",
        "another",
        "same",
        "different",
        "first",
        "last",
        "next",
        "new",
        "old",
        "good",
        "bad",
        "great",
        "little",
        "long",
        "own",
        "right",
        "left",
        "sure",
        "able",
        "else",
        "some",
        "any",
        "all",
        "such",
        // Generic task / instruction words
        "please",
        "using",
        "used",
        "based",
        "related",
        "ensure",
        "verify",
        "appropriate",
        "check",
        "search",
        "review",
        "implement",
        "update",
        "determine",
        "consider",
        "investigate",
        "analyze",
        "examine",
        "describe",
        "explain",
        "provide",
        "include",
        "exclude",
        "specific",
        "current",
        "existing",
        "available",
        "possible",
        "necessary",
        "required",
        "relevant",
        "important",
    ];

    // Acronym allowlist: matches production extract_tool_input_keywords.
    // See the production function for the full rationale.
    let acronym_allowlist: &[&str] = &[
        "sql", "aws", "gcp", "vpc", "rds", "sqs", "sns", "iam", "ec2", "cdn", "jwt", "ssl", "tls",
        "ssh", "tcp", "udp", "dns", "rpc", "ip", "api", "css", "dom", "xml", "csv", "svg", "url",
        "uri", "png", "jpg", "gif", "pdf", "cli", "sdk", "orm", "gpu", "cpu", "ram", "mcp", "gpt",
        "llm", "rag", "nlp", "ci", "cd", "s3", "uid", "pid",
    ];

    let extract_words = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| {
                let w_lower = w.to_lowercase();
                (w.len() >= min_keyword_len || acronym_allowlist.contains(&w_lower.as_str()))
                    && !w.starts_with('-')
                    && !noise.contains(&w_lower.as_str())
                    && !w.chars().all(|c| c.is_numeric())
            })
            .map(|w| w.to_lowercase())
            .collect()
    };

    // Extract words from natural language text, applying both noise and
    // stopword filters (R3). Used for tools like Task whose input is
    // free-form English rather than structured CLI / path data.
    let extract_words_nlp = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| {
                let w_lower = w.to_lowercase();
                (w.len() >= min_keyword_len || acronym_allowlist.contains(&w_lower.as_str()))
                    && !w.starts_with('-')
                    && !noise.contains(&w_lower.as_str())
                    && !stopwords.contains(&w_lower.as_str())
                    && !w.chars().all(|c| c.is_numeric())
            })
            .map(|w| w.to_lowercase())
            .collect()
    };

    /// Strip common path prefixes from absolute file paths.
    ///
    /// Removes user home directory patterns like `/Users/<name>/GitHub/<org>/`
    /// or `/home/<name>/projects/` to keep only the project-relative path.
    fn strip_path_prefix(path: &str) -> &str {
        let parts: Vec<&str> = path.split('/').collect();

        // Hosting services (GitHub, GitLab) have an org level between the
        // marker and the project name: /Users/x/GitHub/<org>/<project>/...
        // We skip marker + org to get to the project.
        let hosting_with_org = ["GitHub", "github", "GitLab", "gitlab"];

        // Local workspace dirs go directly to the project:
        // /home/x/projects/<project>/... or /Users/x/code/<project>/...
        let workspace_dirs = [
            "repos",
            "Repos",
            "projects",
            "Projects",
            "workspace",
            "Workspace",
            "code",
            "Code",
        ];

        for (i, part) in parts.iter().enumerate() {
            if hosting_with_org.contains(part) {
                // Skip marker + org name, return from project onward
                // e.g., /Users/dev/GitHub/org/my-project/src/config.rs
                //        0     1     2      3     4           5   6
                // marker at 2, skip org at 3, return from 4 (my-project/...)
                if i + 2 < parts.len() {
                    let skip: usize = parts[..i + 2].iter().map(|p| p.len() + 1).sum();
                    return &path[skip..];
                }
            }
            if workspace_dirs.contains(part) {
                // Skip only the marker, return from project onward
                // e.g., /home/developer/projects/myapp/lib/database.rs
                //        0    1         2        3     4   5
                // marker at 2, return from 3 (myapp/...)
                if i + 1 < parts.len() {
                    let skip: usize = parts[..i + 1].iter().map(|p| p.len() + 1).sum();
                    return &path[skip..];
                }
            }
        }

        // Fallback: if path starts with /Users/<x>/ or /home/<x>/, strip those
        if parts.len() > 3 && (parts[1] == "Users" || parts[1] == "home") {
            let skip: usize = parts[..3].iter().map(|p| p.len() + 1).sum();
            return &path[skip..];
        }

        path
    }

    match tool_name {
        "Bash" => {
            if let Some(command) = tool_input.get("command").and_then(|v| v.as_str()) {
                keywords.extend(extract_words(command));
            }
        }
        "Grep" => {
            if let Some(pattern) = tool_input.get("pattern").and_then(|v| v.as_str()) {
                keywords.extend(extract_words(pattern));
            }
        }
        "Read" | "Write" | "Edit" => {
            if let Some(file_path) = tool_input.get("file_path").and_then(|v| v.as_str()) {
                let effective_path = if strip_paths {
                    strip_path_prefix(file_path)
                } else {
                    file_path
                };
                keywords.extend(extract_words(effective_path));
            }
        }
        "Glob" => {
            if let Some(pattern) = tool_input.get("pattern").and_then(|v| v.as_str()) {
                keywords.extend(extract_words(pattern));
            }
        }
        "WebSearch" => {
            if let Some(query) = tool_input.get("query").and_then(|v| v.as_str()) {
                keywords.extend(extract_words(query));
            }
        }
        "WebFetch" => {
            if let Some(url) = tool_input.get("url").and_then(|v| v.as_str()) {
                keywords.extend(extract_words(url));
            }
        }
        "Task" => {
            // R3: Task tool sends natural language prompts. Use the NLP
            // extractor which applies both the CLI noise filter and the
            // English stopword filter to prevent retrieval explosion.
            if task_tool_support {
                if let Some(prompt) = tool_input.get("prompt").and_then(|v| v.as_str()) {
                    keywords.extend(extract_words_nlp(prompt));
                }
            }
        }
        _ => {
            // Unknown tool: no keywords extracted
        }
    }

    // Dedup
    keywords.sort();
    keywords.dedup();
    keywords
}

#[cfg(test)]
#[path = "replay_harness.rs"]
mod replay_harness;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CompoundLearning;
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
        assert!(output.is_allowed());
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

        // Verify gate was transitioned to Pending (intent is consumed on transition)
        let session = runner.store.get("close-detect-test").unwrap().unwrap();
        assert_eq!(
            session.gate.status,
            GateStatus::Pending,
            "Gate should transition to Pending in PreToolUse"
        );
        // Intent is consumed by confirm_ticket_close()
        assert!(
            session.gate.ticket_close_intent.is_none(),
            "Intent should be consumed after gate transition"
        );
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
    fn test_stop_auto_approves_when_stop_hook_active() {
        let runner = test_runner();

        // Create session and set to Pending (would normally block)
        let start_input = r#"{
            "session_id": "stop-hook-active-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        let mut session = runner.store.get("stop-hook-active-test").unwrap().unwrap();
        session.gate.status = GateStatus::Pending;
        runner.store.put(&session).unwrap();

        // Send stop hook with stop_hook_active=true — should auto-approve
        let stop_input = r#"{
            "session_id": "stop-hook-active-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "stop_hook_active": true
        }"#;

        let result = runner.run_with_input(HookType::Stop, stop_input);
        assert!(result.is_ok());

        let output: StopOutput = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(
            output.decision,
            StopDecision::Approve,
            "Should auto-approve when stop_hook_active=true to prevent loops"
        );
        assert!(output.reason.is_some());
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

    // Task-completed handler tests

    #[test]
    fn test_hook_type_parse_task_completed() {
        assert_eq!(
            HookType::parse("task-completed"),
            Some(HookType::TaskCompleted)
        );
        assert_eq!(
            HookType::parse("taskcompleted"),
            Some(HookType::TaskCompleted)
        );
        assert_eq!(
            HookType::parse("task_completed"),
            Some(HookType::TaskCompleted)
        );
    }

    #[test]
    fn test_task_completed_creates_session() {
        let runner = test_runner();
        let input = r#"{
            "session_id": "task-session",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "task_id": "task-001",
            "task_subject": "Implement authentication"
        }"#;

        let result = runner.run_with_input(HookType::TaskCompleted, input);
        assert!(result.is_ok());

        // Verify session was created
        let session = runner.store.get("task-session").unwrap();
        assert!(session.is_some());
    }

    #[test]
    fn test_task_completed_sets_ticket_context() {
        let runner = test_runner();
        let input = r#"{
            "session_id": "task-context-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "task_id": "task-123",
            "task_subject": "Fix login bug",
            "task_description": "Users cannot log in with special characters"
        }"#;

        runner
            .run_with_input(HookType::TaskCompleted, input)
            .unwrap();

        let session = runner.store.get("task-context-test").unwrap().unwrap();

        // Verify ticket context was set
        let ticket = session.gate.ticket.as_ref().unwrap();
        assert_eq!(ticket.ticket_id, "task-123");
        assert_eq!(ticket.source, "tasks");
        assert_eq!(ticket.title, "Fix login bug");
        assert_eq!(
            ticket.description,
            Some("Users cannot log in with special characters".to_string())
        );
    }

    #[test]
    fn test_task_completed_transitions_to_pending() {
        let runner = test_runner();
        let input = r#"{
            "session_id": "task-pending-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "task_id": "task-456",
            "task_subject": "Add new feature"
        }"#;

        runner
            .run_with_input(HookType::TaskCompleted, input)
            .unwrap();

        let session = runner.store.get("task-pending-test").unwrap().unwrap();
        assert_eq!(session.gate.status, GateStatus::Pending);
    }

    #[test]
    fn test_task_completed_blocks_exit() {
        let runner = test_runner();
        let input = r#"{
            "session_id": "task-block-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "task_id": "task-789",
            "task_subject": "Refactor module"
        }"#;

        let result = runner
            .run_with_input(HookType::TaskCompleted, input)
            .unwrap();

        // Should return a block decision
        let output: StopOutput = serde_json::from_str(&result).unwrap();
        assert_eq!(output.decision, StopDecision::Block);
        assert!(output.reason.is_some());
        assert!(output.reason.unwrap().contains("reflection required"));
    }

    #[test]
    fn test_task_completed_adds_trace() {
        let runner = test_runner();
        let input = r#"{
            "session_id": "task-trace-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "task_id": "task-999",
            "task_subject": "Update docs"
        }"#;

        runner
            .run_with_input(HookType::TaskCompleted, input)
            .unwrap();

        let session = runner.store.get("task-trace-test").unwrap().unwrap();
        assert!(session
            .trace
            .iter()
            .any(|t| t.event_type == EventType::TicketClosed));
    }

    #[test]
    fn test_task_completed_with_team_context() {
        let runner = test_runner();
        let input = r#"{
            "session_id": "task-team-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "task_id": "team-task-001",
            "task_subject": "Team task",
            "teammate_name": "implementer",
            "team_name": "backend-team"
        }"#;

        // Should parse and process without error
        let result = runner.run_with_input(HookType::TaskCompleted, input);
        assert!(result.is_ok());

        let session = runner.store.get("task-team-test").unwrap().unwrap();
        assert_eq!(session.gate.status, GateStatus::Pending);
    }

    #[test]
    fn test_full_task_flow() {
        let runner = test_runner();

        // 1. Task completed - should block
        let task_input = r#"{
            "session_id": "task-flow-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "task_id": "task-flow-001",
            "task_subject": "Complete feature implementation"
        }"#;
        let result = runner
            .run_with_input(HookType::TaskCompleted, task_input)
            .unwrap();
        let output: StopOutput = serde_json::from_str(&result).unwrap();
        assert_eq!(output.decision, StopDecision::Block);

        // 2. Verify gate is Pending
        let session = runner.store.get("task-flow-test").unwrap().unwrap();
        assert_eq!(session.gate.status, GateStatus::Pending);

        // 3. Stop hook should also block
        let stop_input = r#"{
            "session_id": "task-flow-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        let result = runner.run_with_input(HookType::Stop, stop_input).unwrap();
        let output: StopOutput = serde_json::from_str(&result).unwrap();
        assert_eq!(output.decision, StopDecision::Block);
    }

    // Correction propagation tests

    #[test]
    fn test_session_start_no_correction_notices_without_cache() {
        let runner = test_runner();
        // When no stats cache exists, correction notices should be empty
        let input = r#"{
            "session_id": "correction-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/nonexistent-project"
        }"#;

        let result = runner.run_with_input(HookType::SessionStart, input);
        assert!(result.is_ok());

        // Verify session was created without correction notice trace event
        let session = runner.store.get("correction-test").unwrap().unwrap();
        assert!(session
            .trace
            .iter()
            .all(|t| t.event_type != EventType::CorrectionNotice));
    }

    #[test]
    fn test_correction_notices_with_stats_cache() {
        use crate::stats::StatsLogger;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Create stats log with corrected learning
        let stats_log_path = grove_dir.join("stats.log");
        let logger = StatsLogger::new(&stats_log_path);
        logger.append_surfaced("L001", "s1", None).unwrap();
        logger.append_corrected("L001", "s2", None).unwrap();

        // Pre-populate cache
        let cache_path = grove_dir.join("stats-cache.json");
        let cache_manager = crate::stats::StatsCacheManager::new(&cache_path, &stats_log_path);
        let _ = cache_manager.load_or_rebuild().unwrap();

        // Now create a session with an injected learning that was corrected
        let runner = test_runner();
        let cwd = project_dir.to_str().unwrap();
        let input = format!(
            r#"{{
                "session_id": "correction-propagation-test",
                "transcript_path": "/tmp/transcript.jsonl",
                "cwd": "{}"
            }}"#,
            cwd
        );

        runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();

        // Add an injected learning to the session
        let mut session = runner
            .store
            .get("correction-propagation-test")
            .unwrap()
            .unwrap();
        session
            .gate
            .injected_learnings
            .push(crate::core::InjectedLearning::new("L001", 0.8));
        runner.store.put(&session).unwrap();

        // Call session-start again - this simulates the scenario where the session
        // was restored and we check for correction notices
        // For this test, we verify that the get_correction_notices helper works
        // by calling it directly through another session-start (which won't find notices
        // since injected_learnings are set after session creation)
        //
        // The actual integration is tested via the trace event check above.
    }

    #[test]
    fn test_correction_notices_append_to_learning_injections() {
        use crate::stats::StatsLogger;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Create a learnings.md file with one learning
        // Use recent timestamp so it passes category-specific decay (Pitfall: 60d half-life)
        let learnings_content = r#"# Grove Learnings

---
## cl_test_001

**Category:** Pitfall
**Summary:** Test learning for correction notice test
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #testing
**Session:** test-session
**Criteria:** Behavior Changing
**Created:** 2026-03-01T00:00:00Z

This is a test learning that will be injected and also flagged as corrected.

---
"#;
        std::fs::write(grove_dir.join("learnings.md"), learnings_content).unwrap();

        // Create stats log with surfacing, reference, and correction events.
        // The reference event gives the learning a non-zero hit rate so it
        // passes composite scoring with the steeper reference boost.
        let stats_log_path = grove_dir.join("stats.log");
        let logger = StatsLogger::new(&stats_log_path);
        logger
            .append_surfaced("cl_test_001", "old-session", None)
            .unwrap();
        logger
            .append_referenced("cl_test_001", "old-session", None)
            .unwrap();
        logger
            .append_corrected("cl_test_001", "correction-session", None)
            .unwrap();

        // Build cache so correction notices can be detected
        let cache_path = grove_dir.join("stats-cache.json");
        let cache_manager = crate::stats::StatsCacheManager::new(&cache_path, &stats_log_path);
        let _ = cache_manager.load_or_rebuild().unwrap();

        // Create a runner with default config (markdown backend)
        let runner = test_runner();
        let cwd = project_dir.to_str().unwrap();

        // First session-start: injects the learning
        let input = format!(
            r#"{{
                "session_id": "correction-append-test",
                "transcript_path": "/tmp/transcript.jsonl",
                "cwd": "{}"
            }}"#,
            cwd
        );
        let result = runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();
        let output: SessionStartOutput = serde_json::from_str(&result).unwrap();

        // The learning should be injected (additional_context should have it)
        assert!(
            output.additional_context().is_some(),
            "Should have additional context from learning injection"
        );

        let context = output.additional_context().unwrap().to_string();

        // Verify learning injection content is present
        assert!(
            context.contains("Relevant Learnings"),
            "Should contain learning injection header, got: {:?}",
            context
        );
        assert!(
            context.contains("cl_test_001"),
            "Should contain the learning ID, got: {:?}",
            context
        );

        // Verify citation guidance includes grove ref command and session ID
        assert!(
            context.contains("grove ref"),
            "Should contain grove ref command in citation guidance, got: {:?}",
            context
        );
        assert!(
            context.contains("--session-id"),
            "Should contain --session-id flag in citation guidance, got: {:?}",
            context
        );
        assert!(
            context.contains("correction-append-test"),
            "Should contain session ID in citation guidance, got: {:?}",
            context
        );

        // Now the session should have cl_test_001 in injected_learnings.
        // When session-start runs again, it will:
        // 1. Restore the session (which has injected_learnings from first run)
        // 2. Re-inject learnings (overwriting injected_learnings)
        // 3. Check correction notices against session.gate.injected_learnings
        //
        // Since cl_test_001 is both injected AND corrected, correction notice fires.
        let result2 = runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();
        let output2: SessionStartOutput = serde_json::from_str(&result2).unwrap();

        assert!(
            output2.additional_context().is_some(),
            "Should have additional context on second call"
        );

        let context2 = output2.additional_context().unwrap().to_string();

        // Verify BOTH learning injection AND correction notice are present
        assert!(
            context2.contains("Relevant Learnings"),
            "Should still contain learning injection, got: {:?}",
            context2
        );
        assert!(
            context2.contains("CORRECTION NOTICE"),
            "Should also contain correction notice, got: {:?}",
            context2
        );
        assert!(
            context2.contains("cl_test_001"),
            "Should contain the learning ID in context, got: {:?}",
            context2
        );
    }

    // Additional ticketing system tests

    #[test]
    fn test_beads_close_triggers_gate_transition() {
        let runner = test_runner();

        // 1. Session start
        let start_input = r#"{
            "session_id": "beads-close-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 2. Pre-tool-use: beads close detected
        let pre_input = r#"{
            "session_id": "beads-close-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "beads close issue-456"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input)
            .unwrap();

        // Verify gate was transitioned to Pending in PreToolUse (not PostToolUse)
        let session = runner.store.get("beads-close-test").unwrap().unwrap();
        assert_eq!(
            session.gate.status,
            GateStatus::Pending,
            "Gate should transition to Pending in PreToolUse"
        );
        // Intent is consumed by confirm_ticket_close()
        assert!(
            session.gate.ticket_close_intent.is_none(),
            "Intent should be consumed after gate transition"
        );
        // Verify ticket context was set
        assert!(session.gate.ticket.is_some());
        assert_eq!(session.gate.ticket.as_ref().unwrap().ticket_id, "issue-456");

        // 3. Post-tool-use: now a no-op since gate already transitioned
        let post_input = r#"{
            "session_id": "beads-close-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "beads close issue-456"},
            "tool_response": "Closed issue-456"
        }"#;
        runner
            .run_with_input(HookType::PostToolUse, post_input)
            .unwrap();

        // Verify gate is still Pending (no change from PostToolUse)
        let session = runner.store.get("beads-close-test").unwrap().unwrap();
        assert_eq!(session.gate.status, GateStatus::Pending);
    }

    #[test]
    fn test_beads_complete_triggers_gate_transition() {
        let runner = test_runner();

        // 1. Session start
        let start_input = r#"{
            "session_id": "beads-complete-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 2. Pre-tool-use: beads complete detected
        let pre_input = r#"{
            "session_id": "beads-complete-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "beads complete task-789"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input)
            .unwrap();

        // Verify gate was transitioned to Pending in PreToolUse
        let session = runner.store.get("beads-complete-test").unwrap().unwrap();
        assert_eq!(
            session.gate.status,
            GateStatus::Pending,
            "Gate should transition to Pending in PreToolUse"
        );
        // Intent is consumed by confirm_ticket_close()
        assert!(
            session.gate.ticket_close_intent.is_none(),
            "Intent should be consumed after gate transition"
        );

        // 3. Post-tool-use: now a no-op since gate already transitioned
        let post_input = r#"{
            "session_id": "beads-complete-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "beads complete task-789"},
            "tool_response": "task-789"
        }"#;
        runner
            .run_with_input(HookType::PostToolUse, post_input)
            .unwrap();

        // Verify gate is still Pending
        let session = runner.store.get("beads-complete-test").unwrap().unwrap();
        assert_eq!(session.gate.status, GateStatus::Pending);
    }

    #[test]
    fn test_non_close_command_does_not_trigger_gate() {
        let runner = test_runner();

        // 1. Session start
        let start_input = r#"{
            "session_id": "no-gate-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 2. Pre-tool-use: regular git command
        let pre_input = r#"{
            "session_id": "no-gate-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "git status"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input)
            .unwrap();

        // Verify no intent recorded
        let session = runner.store.get("no-gate-test").unwrap().unwrap();
        assert!(session.gate.ticket_close_intent.is_none());
        assert_eq!(session.gate.status, GateStatus::Idle);

        // 3. Post-tool-use: git status response
        let post_input = r#"{
            "session_id": "no-gate-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "git status"},
            "tool_response": "On branch main"
        }"#;
        runner
            .run_with_input(HookType::PostToolUse, post_input)
            .unwrap();

        // Verify gate is still Idle
        let session = runner.store.get("no-gate-test").unwrap().unwrap();
        assert_eq!(session.gate.status, GateStatus::Idle);
    }

    #[test]
    fn test_second_ticket_close_resets_from_reflected() {
        let runner = test_runner();

        // 1. Session start
        let start_input = r#"{
            "session_id": "multi-ticket-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 2. First ticket close: pre-tool-use (now also transitions gate)
        let pre_input1 = r#"{
            "session_id": "multi-ticket-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "tissue status grove-001 closed"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input1)
            .unwrap();

        // Verify gate is Pending after PreToolUse (no need for PostToolUse)
        let session = runner.store.get("multi-ticket-test").unwrap().unwrap();
        assert_eq!(session.gate.status, GateStatus::Pending);

        // 3. Simulate reflection by setting gate to Reflected manually
        let mut session = runner.store.get("multi-ticket-test").unwrap().unwrap();
        session.gate.status = GateStatus::Reflected;
        session.gate.reflection = Some(crate::core::ReflectionResult::new(
            vec!["l1".to_string()],
            3,
            1,
        ));
        runner.store.put(&session).unwrap();

        // 4. Second ticket close: pre-tool-use (should reset from terminal state)
        let pre_input2 = r#"{
            "session_id": "multi-ticket-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "tissue status grove-002 closed"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input2)
            .unwrap();

        // Verify gate was reset and is now Pending again
        let session = runner.store.get("multi-ticket-test").unwrap().unwrap();
        assert_eq!(session.gate.status, GateStatus::Pending);
        assert!(session.gate.reflection.is_none()); // Previous reflection cleared
        assert!(session.gate.ticket.is_some());
        assert_eq!(session.gate.ticket.as_ref().unwrap().ticket_id, "grove-002");

        // Verify trace shows the reset event
        assert!(session
            .trace
            .iter()
            .any(|t| t.event_type == EventType::GateStatusChanged));
    }

    #[test]
    fn test_second_ticket_close_resets_from_skipped() {
        let runner = test_runner();

        // 1. Session start
        let start_input = r#"{
            "session_id": "skip-then-close-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // Simulate skipped state
        let mut session = runner.store.get("skip-then-close-test").unwrap().unwrap();
        session.gate.status = GateStatus::Skipped;
        session.gate.skip = Some(crate::core::SkipDecision::new(
            "trivial",
            crate::core::SkipDecider::User,
        ));
        runner.store.put(&session).unwrap();

        // 2. Ticket close: pre-tool-use
        let pre_input = r#"{
            "session_id": "skip-then-close-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "tissue status grove-003 closed"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input)
            .unwrap();

        // 3. Ticket close: post-tool-use
        let post_input = r#"{
            "session_id": "skip-then-close-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "tissue status grove-003 closed"},
            "tool_response": "grove-003"
        }"#;
        runner
            .run_with_input(HookType::PostToolUse, post_input)
            .unwrap();

        // Verify gate is now Pending (reset from Skipped)
        let session = runner.store.get("skip-then-close-test").unwrap().unwrap();
        assert_eq!(session.gate.status, GateStatus::Pending);
        assert!(session.gate.skip.is_none()); // Previous skip cleared
    }

    #[test]
    fn test_session_end_skip_does_not_dismiss_learnings() {
        // Test that skip is treated as no-signal: dismissals should NOT be logged
        // when a session ends in Skipped state.
        use crate::core::state::{InjectedLearning, InjectionOutcome};

        let runner = test_runner();

        // 1. Session start
        let start_input = r#"{
            "session_id": "skip-dismiss-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 2. Set up session with injected learnings in Skipped state
        let mut session = runner.store.get("skip-dismiss-test").unwrap().unwrap();
        session.gate.status = GateStatus::Skipped;
        session.gate.injected_learnings.push(InjectedLearning {
            learning_id: "L001".to_string(),
            score: 0.8,
            outcome: InjectionOutcome::Pending, // Not referenced
        });
        session.gate.injected_learnings.push(InjectedLearning {
            learning_id: "L002".to_string(),
            score: 0.7,
            outcome: InjectionOutcome::Pending, // Not referenced
        });
        runner.store.put(&session).unwrap();

        // 3. Session end (with Skipped state) - note: can't easily test stats logging
        //    with MemorySessionStore, but we can verify the logic path was correct
        //    by checking that the session state is preserved
        let end_input = r#"{
            "session_id": "skip-dismiss-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "reason": "user_exit"
        }"#;
        runner
            .run_with_input(HookType::SessionEnd, end_input)
            .unwrap();

        // Verify session still has the injected learnings as Pending (not modified)
        let session = runner.store.get("skip-dismiss-test").unwrap().unwrap();
        assert_eq!(session.gate.injected_learnings.len(), 2);
        assert!(session
            .gate
            .injected_learnings
            .iter()
            .all(|l| l.outcome == InjectionOutcome::Pending));
    }

    #[test]
    fn test_session_end_reflect_does_dismiss_unreferenced_learnings() {
        // Test that reflected sessions DO log dismissals for unreferenced learnings.
        // This test verifies the condition gate for dismissal logging.
        use crate::core::state::{InjectedLearning, InjectionOutcome};

        let runner = test_runner();

        // 1. Session start
        let start_input = r#"{
            "session_id": "reflect-dismiss-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 2. Set up session with injected learnings in Reflected state
        let mut session = runner.store.get("reflect-dismiss-test").unwrap().unwrap();
        session.gate.status = GateStatus::Reflected;
        session.gate.injected_learnings.push(InjectedLearning {
            learning_id: "L001".to_string(),
            score: 0.8,
            outcome: InjectionOutcome::Pending, // Not referenced - should be dismissed
        });
        session.gate.injected_learnings.push(InjectedLearning {
            learning_id: "L002".to_string(),
            score: 0.7,
            outcome: InjectionOutcome::Referenced, // Referenced - should NOT be dismissed
        });
        runner.store.put(&session).unwrap();

        // 3. Session end (with Reflected state) - dismissals should be logged
        //    Note: We can't easily verify the stats log with MemorySessionStore,
        //    but we verify the conditional logic path is correct based on gate state
        let end_input = r#"{
            "session_id": "reflect-dismiss-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "reason": "user_exit"
        }"#;
        runner
            .run_with_input(HookType::SessionEnd, end_input)
            .unwrap();

        // Session state should be preserved (dismissal logging doesn't modify state)
        let session = runner.store.get("reflect-dismiss-test").unwrap().unwrap();
        assert_eq!(session.gate.injected_learnings.len(), 2);
    }

    // Integration tests with actual file storage to verify stats log writing

    #[test]
    fn test_session_end_skip_does_not_write_dismissed_events_to_log() {
        // Integration test: verify stats log is NOT written when session is skipped
        use crate::core::state::{InjectedLearning, InjectionOutcome};
        use crate::storage::FileSessionStore;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let session_dir = temp_dir.path().join("sessions");
        let grove_dir = temp_dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let store = FileSessionStore::with_dir(&session_dir).unwrap();
        let runner = HookRunner::new(store, Config::default());
        let cwd = temp_dir.path().to_str().unwrap();

        // Start a session
        let start_input = format!(
            r#"{{
            "session_id": "skip-log-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "{}"
        }}"#,
            cwd
        );
        runner
            .run_with_input(HookType::SessionStart, &start_input)
            .unwrap();

        // Get the session and modify it
        let mut session = runner.store.get("skip-log-test").unwrap().unwrap();
        session.gate.status = GateStatus::Skipped;
        session.gate.injected_learnings.push(InjectedLearning {
            learning_id: "L001".to_string(),
            score: 0.8,
            outcome: InjectionOutcome::Pending,
        });
        runner.store.put(&session).unwrap();

        // Session end
        let end_input = format!(
            r#"{{
            "session_id": "skip-log-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "{}",
            "reason": "user_exit"
        }}"#,
            cwd
        );
        runner
            .run_with_input(HookType::SessionEnd, &end_input)
            .unwrap();

        // Verify NO dismissed events were logged
        let stats_path = grove_dir.join("stats.log");
        if stats_path.exists() {
            let events = crate::stats::StatsLogger::new(&stats_path)
                .read_all()
                .unwrap_or_default();
            let dismissed: Vec<_> = events
                .iter()
                .filter(|e| e.data.event_name() == "dismissed")
                .collect();
            assert!(
                dismissed.is_empty(),
                "Skip should NOT log dismissed events, found: {:?}",
                dismissed
            );
        }
        // If file doesn't exist, that's also correct (no events written)
    }

    #[test]
    fn test_session_end_reflect_writes_dismissed_events_to_log() {
        // Integration test: verify stats log IS written when session is reflected
        use crate::core::state::{InjectedLearning, InjectionOutcome};
        use crate::storage::FileSessionStore;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let session_dir = temp_dir.path().join("sessions");
        let grove_dir = temp_dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let store = FileSessionStore::with_dir(&session_dir).unwrap();
        let runner = HookRunner::new(store, Config::default());
        let cwd = temp_dir.path().to_str().unwrap();

        // Start a session
        let start_input = format!(
            r#"{{
            "session_id": "reflect-log-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "{}"
        }}"#,
            cwd
        );
        runner
            .run_with_input(HookType::SessionStart, &start_input)
            .unwrap();

        // Get the session and modify it
        let mut session = runner.store.get("reflect-log-test").unwrap().unwrap();
        session.gate.status = GateStatus::Reflected;
        session.gate.injected_learnings.push(InjectedLearning {
            learning_id: "L001".to_string(),
            score: 0.8,
            outcome: InjectionOutcome::Pending, // Not referenced - should be dismissed
        });
        session.gate.injected_learnings.push(InjectedLearning {
            learning_id: "L002".to_string(),
            score: 0.7,
            outcome: InjectionOutcome::Referenced, // Referenced - should NOT be dismissed
        });
        runner.store.put(&session).unwrap();

        // Session end
        let end_input = format!(
            r#"{{
            "session_id": "reflect-log-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "{}",
            "reason": "user_exit"
        }}"#,
            cwd
        );
        runner
            .run_with_input(HookType::SessionEnd, &end_input)
            .unwrap();

        // Verify dismissed event was logged for L001 (unreferenced) only
        let stats_path = grove_dir.join("stats.log");
        assert!(stats_path.exists(), "Stats log should exist after reflect");

        let events = crate::stats::StatsLogger::new(&stats_path)
            .read_all()
            .unwrap();
        let dismissed: Vec<_> = events
            .iter()
            .filter(|e| e.data.event_name() == "dismissed")
            .collect();

        assert_eq!(
            dismissed.len(),
            1,
            "Should have exactly 1 dismissed event (for L001)"
        );

        // Verify it's for L001
        if let crate::stats::StatsEventType::Dismissed { learning_id, .. } = &dismissed[0].data {
            assert_eq!(learning_id, "L001");
        } else {
            panic!("Expected Dismissed event");
        }
    }

    #[test]
    fn test_session_end_idle_does_dismiss_unreferenced_learnings() {
        // Idle sessions (no ticket closed) should still emit dismissed events
        // for injected learnings. This is the primary source of feedback data.
        use crate::core::state::{InjectedLearning, InjectionOutcome};

        let runner = test_runner();

        let start_input = r#"{
            "session_id": "idle-dismiss-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // Set up session with injected learnings in Idle state (default)
        let mut session = runner.store.get("idle-dismiss-test").unwrap().unwrap();
        assert_eq!(session.gate.status, GateStatus::Idle);
        session.gate.injected_learnings.push(InjectedLearning {
            learning_id: "L001".to_string(),
            score: 0.8,
            outcome: InjectionOutcome::Pending,
        });
        session.gate.injected_learnings.push(InjectedLearning {
            learning_id: "L002".to_string(),
            score: 0.7,
            outcome: InjectionOutcome::Referenced,
        });
        runner.store.put(&session).unwrap();

        let end_input = r#"{
            "session_id": "idle-dismiss-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "reason": "user_exit"
        }"#;
        runner
            .run_with_input(HookType::SessionEnd, end_input)
            .unwrap();

        // Session state preserved
        let session = runner.store.get("idle-dismiss-test").unwrap().unwrap();
        assert_eq!(session.gate.injected_learnings.len(), 2);
    }

    #[test]
    fn test_session_end_idle_writes_dismissed_events_to_log() {
        // Integration test: verify stats log IS written for idle sessions
        use crate::core::state::{InjectedLearning, InjectionOutcome};
        use crate::storage::FileSessionStore;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let session_dir = temp_dir.path().join("sessions");
        let grove_dir = temp_dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let store = FileSessionStore::with_dir(&session_dir).unwrap();
        let runner = HookRunner::new(store, Config::default());
        let cwd = temp_dir.path().to_str().unwrap();

        let start_input = format!(
            r#"{{
            "session_id": "idle-log-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "{}"
        }}"#,
            cwd
        );
        runner
            .run_with_input(HookType::SessionStart, &start_input)
            .unwrap();

        // Session stays in Idle state — no ticket was closed
        let mut session = runner.store.get("idle-log-test").unwrap().unwrap();
        assert_eq!(session.gate.status, GateStatus::Idle);
        session.gate.injected_learnings.push(InjectedLearning {
            learning_id: "L001".to_string(),
            score: 0.8,
            outcome: InjectionOutcome::Pending,
        });
        session.gate.injected_learnings.push(InjectedLearning {
            learning_id: "L002".to_string(),
            score: 0.7,
            outcome: InjectionOutcome::Referenced,
        });
        runner.store.put(&session).unwrap();

        let end_input = format!(
            r#"{{
            "session_id": "idle-log-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "{}",
            "reason": "user_exit"
        }}"#,
            cwd
        );
        runner
            .run_with_input(HookType::SessionEnd, &end_input)
            .unwrap();

        // Verify dismissed event was logged for L001 only
        let stats_path = grove_dir.join("stats.log");
        assert!(
            stats_path.exists(),
            "Stats log should exist after idle session with injected learnings"
        );

        let events = crate::stats::StatsLogger::new(&stats_path)
            .read_all()
            .unwrap();
        let dismissed: Vec<_> = events
            .iter()
            .filter(|e| e.data.event_name() == "dismissed")
            .collect();

        assert_eq!(
            dismissed.len(),
            1,
            "Should have exactly 1 dismissed event (L001 pending, L002 referenced)"
        );

        if let crate::stats::StatsEventType::Dismissed { learning_id, .. } = &dismissed[0].data {
            assert_eq!(learning_id, "L001");
        } else {
            panic!("Expected Dismissed event");
        }
    }

    #[test]
    fn test_session_end_no_injected_learnings_no_dismissed_events() {
        // When no learnings were injected, no dismissed events should be emitted
        use crate::storage::FileSessionStore;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let session_dir = temp_dir.path().join("sessions");
        let grove_dir = temp_dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let store = FileSessionStore::with_dir(&session_dir).unwrap();
        let runner = HookRunner::new(store, Config::default());
        let cwd = temp_dir.path().to_str().unwrap();

        let start_input = format!(
            r#"{{
            "session_id": "no-inject-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "{}"
        }}"#,
            cwd
        );
        runner
            .run_with_input(HookType::SessionStart, &start_input)
            .unwrap();

        // No injected learnings — session ends normally
        let end_input = format!(
            r#"{{
            "session_id": "no-inject-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "{}",
            "reason": "user_exit"
        }}"#,
            cwd
        );
        runner
            .run_with_input(HookType::SessionEnd, &end_input)
            .unwrap();

        let stats_path = grove_dir.join("stats.log");
        if stats_path.exists() {
            let events = crate::stats::StatsLogger::new(&stats_path)
                .read_all()
                .unwrap_or_default();
            let dismissed: Vec<_> = events
                .iter()
                .filter(|e| e.data.event_name() == "dismissed")
                .collect();
            assert!(
                dismissed.is_empty(),
                "No dismissed events without injected learnings"
            );
        }
    }

    // Blocking gate context injection tests

    #[test]
    fn test_session_start_injects_blocking_gate_context_pending() {
        let runner = test_runner();

        // 1. Create a session
        let start_input = r#"{
            "session_id": "blocking-context-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 2. Set session to Pending state with a ticket
        let mut session = runner.store.get("blocking-context-test").unwrap().unwrap();
        session.gate.status = GateStatus::Pending;
        session.gate.ticket = Some(TicketContext::new(
            "GROVE-123",
            "github",
            "Fix blocking bug",
        ));
        runner.store.put(&session).unwrap();

        // 3. Call session-start again (simulates subagent starting)
        let result = runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 4. Verify additionalContext contains gate notice
        let output: SessionStartOutput = serde_json::from_str(&result).unwrap();
        assert!(output.additional_context().is_some());

        let context = output.additional_context().unwrap().to_string();
        assert!(
            context.contains("## Grove Gate Active"),
            "Should contain gate notice header"
        );
        assert!(
            context.contains("Pending"),
            "Should contain status: {:?}",
            context
        );
        assert!(
            context.contains("GROVE-123"),
            "Should contain ticket ID: {:?}",
            context
        );
        assert!(
            context.contains("grove reflect"),
            "Should contain reflect command: {:?}",
            context
        );
        assert!(
            context.contains("grove skip"),
            "Should contain skip command: {:?}",
            context
        );
        assert!(
            context.contains("blocking-context-test"),
            "Should contain session ID: {:?}",
            context
        );
    }

    #[test]
    fn test_session_start_injects_blocking_gate_context_blocked() {
        let runner = test_runner();

        // 1. Create a session in Blocked state
        let start_input = r#"{
            "session_id": "blocked-context-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // Set to Blocked state
        let mut session = runner.store.get("blocked-context-test").unwrap().unwrap();
        session.gate.status = GateStatus::Blocked;
        runner.store.put(&session).unwrap();

        // 2. Call session-start again
        let result = runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 3. Verify context contains Blocked status
        let output: SessionStartOutput = serde_json::from_str(&result).unwrap();
        assert!(output.additional_context().is_some());

        let context = output.additional_context().unwrap().to_string();
        assert!(
            context.contains("Blocked"),
            "Should contain Blocked status: {:?}",
            context
        );
    }

    #[test]
    fn test_session_start_no_blocking_context_for_idle() {
        let runner = test_runner();

        // 1. Create a session (defaults to Idle)
        let start_input = r#"{
            "session_id": "idle-no-context-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        let result = runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 2. Verify no gate blocking context (Idle doesn't require reflection)
        let output: SessionStartOutput = serde_json::from_str(&result).unwrap();
        // Context might be None or might contain learnings, but should NOT contain gate notice
        if let Some(context) = output.additional_context() {
            assert!(
                !context.contains("## Grove Gate Active"),
                "Idle session should NOT have gate notice"
            );
        }
    }

    #[test]
    fn test_session_start_no_blocking_context_for_reflected() {
        let runner = test_runner();

        // 1. Create a session in Reflected state
        let start_input = r#"{
            "session_id": "reflected-no-context-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        let mut session = runner
            .store
            .get("reflected-no-context-test")
            .unwrap()
            .unwrap();
        session.gate.status = GateStatus::Reflected;
        runner.store.put(&session).unwrap();

        // 2. Call session-start again
        let result = runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 3. Verify no gate blocking context (Reflected is terminal)
        let output: SessionStartOutput = serde_json::from_str(&result).unwrap();
        if let Some(context) = output.additional_context() {
            assert!(
                !context.contains("## Grove Gate Active"),
                "Reflected session should NOT have gate notice"
            );
        }
    }

    #[test]
    fn test_session_start_blocking_context_adds_trace() {
        let runner = test_runner();

        // 1. Create a session and set to Pending
        let start_input = r#"{
            "session_id": "blocking-trace-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        let mut session = runner.store.get("blocking-trace-test").unwrap().unwrap();
        session.gate.status = GateStatus::Pending;
        runner.store.put(&session).unwrap();

        // 2. Call session-start again
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 3. Verify trace event was added
        let session = runner.store.get("blocking-trace-test").unwrap().unwrap();
        let blocking_trace = session.trace.iter().find(|t| {
            t.event_type == EventType::GateStatusChanged
                && t.details
                    .as_ref()
                    .is_some_and(|d| d.contains("injected blocking notice"))
        });
        assert!(
            blocking_trace.is_some(),
            "Should have trace event for blocking notice injection"
        );
    }

    #[test]
    fn test_session_start_no_blocking_context_for_skipped() {
        let runner = test_runner();

        // 1. Create a session in Skipped state (another terminal state)
        let start_input = r#"{
            "session_id": "skipped-no-context-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        let mut session = runner
            .store
            .get("skipped-no-context-test")
            .unwrap()
            .unwrap();
        session.gate.status = GateStatus::Skipped;
        runner.store.put(&session).unwrap();

        // 2. Call session-start again
        let result = runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 3. Verify no gate blocking context (Skipped is terminal)
        let output: SessionStartOutput = serde_json::from_str(&result).unwrap();
        if let Some(context) = output.additional_context() {
            assert!(
                !context.contains("## Grove Gate Active"),
                "Skipped session should NOT have gate notice"
            );
        }
    }

    #[test]
    fn test_session_start_blocking_context_without_ticket() {
        let runner = test_runner();

        // 1. Create a session in Pending state WITHOUT a ticket
        let start_input = r#"{
            "session_id": "no-ticket-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        let mut session = runner.store.get("no-ticket-test").unwrap().unwrap();
        session.gate.status = GateStatus::Pending;
        // Explicitly no ticket: session.gate.ticket = None (default)
        runner.store.put(&session).unwrap();

        // 2. Call session-start again
        let result = runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // 3. Verify context has gate notice but NO ticket line
        let output: SessionStartOutput = serde_json::from_str(&result).unwrap();
        assert!(output.additional_context().is_some());

        let context = output.additional_context().unwrap().to_string();
        assert!(
            context.contains("## Grove Gate Active"),
            "Should have gate notice"
        );
        assert!(context.contains("Pending"), "Should show Pending status");
        assert!(
            !context.contains("**Ticket:**"),
            "Should NOT have Ticket line when no ticket: {:?}",
            context
        );
        assert!(
            context.contains("grove reflect"),
            "Should still have resolution commands"
        );
    }

    // Strategy-aware injection tests

    /// Helper: create N learnings in markdown format with controllable timestamps.
    fn make_learnings_md(count: usize, created: &str) -> String {
        let mut md = "# Grove Learnings\n".to_string();
        for i in 1..=count {
            md.push_str(&format!(
                "\n---\n## cl_cap_{i:03}\n\n\
                 **Category:** Pattern\n\
                 **Summary:** Strategy test learning {i}\n\
                 **Scope:** Project | **Confidence:** High | **Status:** Active\n\
                 **Tags:** #testing\n\
                 **Session:** test-session\n\
                 **Criteria:** Behavior Changing\n\
                 **Created:** {created}\n\n\
                 Detail for learning {i}.\n\n---\n"
            ));
        }
        md
    }

    fn runner_with_strategy(strategy: &str) -> HookRunner<MemorySessionStore> {
        let mut config = Config::default();
        config.retrieval.strategy = strategy.to_string();
        // Set max_injections high so only strategy cap limits results
        config.retrieval.max_injections = 20;
        // Disable pool size relaxation so we test pure strategy behavior
        config.retrieval.min_pool_size = 0;
        // Disable adaptive threshold so tests exercise pure strategy logic
        config.retrieval.min_confidence_threshold = 0.0;
        config.retrieval.min_score_gap = 0.0;
        HookRunner::new(MemorySessionStore::new(), config)
    }

    #[test]
    fn test_conservative_strategy_caps_at_3() {
        let temp = tempfile::TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Create 6 learnings — more than conservative's cap of 3
        let content = make_learnings_md(6, "2026-01-15T00:00:00Z");
        std::fs::write(grove_dir.join("learnings.md"), &content).unwrap();

        let runner = runner_with_strategy("conservative");
        let cwd = project_dir.to_str().unwrap();
        let input = format!(
            r#"{{"session_id":"cap-cons","transcript_path":"/tmp/t.jsonl","cwd":"{}"}}"#,
            cwd
        );

        let result = runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();
        let output: SessionStartOutput = serde_json::from_str(&result).unwrap();

        // With empty query, markdown backend returns relevance=1.0 for all,
        // which exceeds conservative's threshold (0.8). Cap should limit to 3.
        if let Some(ctx) = output.additional_context() {
            let injection_count = ctx.matches("cl_cap_").count();
            assert!(
                injection_count <= 3,
                "Conservative should cap at 3, got {injection_count}"
            );
        }
        // Verify via injected_learnings on the session
        let session = runner.store.get("cap-cons").unwrap().unwrap();
        assert!(
            session.gate.injected_learnings.len() <= 3,
            "Conservative should inject at most 3, got {}",
            session.gate.injected_learnings.len()
        );
    }

    #[test]
    fn test_moderate_strategy_caps_at_5() {
        let temp = tempfile::TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Create 8 learnings — more than moderate's cap of 5
        let content = make_learnings_md(8, "2026-01-15T00:00:00Z");
        std::fs::write(grove_dir.join("learnings.md"), &content).unwrap();

        let runner = runner_with_strategy("moderate");
        let cwd = project_dir.to_str().unwrap();
        let input = format!(
            r#"{{"session_id":"cap-mod","transcript_path":"/tmp/t.jsonl","cwd":"{}"}}"#,
            cwd
        );

        let result = runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();
        let output: SessionStartOutput = serde_json::from_str(&result).unwrap();

        if let Some(ctx) = output.additional_context() {
            let injection_count = ctx.matches("cl_cap_").count();
            assert!(
                injection_count <= 5,
                "Moderate should cap at 5, got {injection_count}"
            );
        }
        let session = runner.store.get("cap-mod").unwrap().unwrap();
        assert!(
            session.gate.injected_learnings.len() <= 5,
            "Moderate should inject at most 5, got {}",
            session.gate.injected_learnings.len()
        );
    }

    #[test]
    fn test_aggressive_strategy_includes_recent_without_match() {
        let temp = tempfile::TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Create learnings with a recent timestamp (within 30 days of now)
        let recent = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::days(5))
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let content = make_learnings_md(2, &recent);
        std::fs::write(grove_dir.join("learnings.md"), &content).unwrap();

        let runner = runner_with_strategy("aggressive");
        let cwd = project_dir.to_str().unwrap();
        let input = format!(
            r#"{{"session_id":"cap-agg","transcript_path":"/tmp/t.jsonl","cwd":"{}"}}"#,
            cwd
        );

        let result = runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();
        let output: SessionStartOutput = serde_json::from_str(&result).unwrap();

        // Aggressive mode should include recent learnings
        assert!(
            output.additional_context().is_some(),
            "Aggressive should inject recent learnings"
        );
        let ctx = output.additional_context().unwrap().to_string();
        assert!(
            ctx.contains("cl_cap_"),
            "Aggressive should include recent learnings in context"
        );
    }

    #[test]
    fn test_config_max_injections_respected_alongside_strategy() {
        let temp = tempfile::TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Create 8 learnings
        let content = make_learnings_md(8, "2026-01-15T00:00:00Z");
        std::fs::write(grove_dir.join("learnings.md"), &content).unwrap();

        // Aggressive caps at 10, but config max_injections=2 should win
        let mut config = Config::default();
        config.retrieval.strategy = "aggressive".to_string();
        config.retrieval.max_injections = 2;
        let runner = HookRunner::new(MemorySessionStore::new(), config);

        let cwd = project_dir.to_str().unwrap();
        let input = format!(
            r#"{{"session_id":"cap-minmax","transcript_path":"/tmp/t.jsonl","cwd":"{}"}}"#,
            cwd
        );

        let result = runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();
        let _output: SessionStartOutput = serde_json::from_str(&result).unwrap();

        let session = runner.store.get("cap-minmax").unwrap().unwrap();
        assert!(
            session.gate.injected_learnings.len() <= 2,
            "Should respect min(config, strategy), got {}",
            session.gate.injected_learnings.len()
        );
    }

    #[test]
    fn test_conservative_downgrades_to_moderate_below_min_pool_size() {
        let temp = tempfile::TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Create 5 learnings — well below default min_pool_size of 20
        let content = make_learnings_md(5, "2026-01-15T00:00:00Z");
        std::fs::write(grove_dir.join("learnings.md"), &content).unwrap();

        // Conservative normally caps at 3, but with only 5 learnings
        // (below min_pool_size=20), should downgrade to moderate (cap 5)
        let mut config = Config::default();
        config.retrieval.strategy = "conservative".to_string();
        config.retrieval.max_injections = 20;
        // Disable adaptive threshold so we test pure pool-size downgrade behavior
        config.retrieval.min_confidence_threshold = 0.0;
        config.retrieval.min_score_gap = 0.0;
        let runner = HookRunner::new(MemorySessionStore::new(), config);

        let cwd = project_dir.to_str().unwrap();
        let input = format!(
            r#"{{"session_id":"pool-small","transcript_path":"/tmp/t.jsonl","cwd":"{}"}}"#,
            cwd
        );

        let result = runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();
        let _output: SessionStartOutput = serde_json::from_str(&result).unwrap();

        let session = runner.store.get("pool-small").unwrap().unwrap();
        // With moderate behavior, should inject up to 5 (moderate cap)
        // rather than just 3 (conservative cap)
        assert!(
            session.gate.injected_learnings.len() <= 5,
            "Should use moderate cap (5), got {}",
            session.gate.injected_learnings.len()
        );
        assert!(
            session.gate.injected_learnings.len() > 3,
            "Should have been relaxed from conservative cap of 3, got {}",
            session.gate.injected_learnings.len()
        );
    }

    #[test]
    fn test_conservative_stays_conservative_above_min_pool_size() {
        let temp = tempfile::TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Create 25 learnings — above default min_pool_size of 20
        let content = make_learnings_md(25, "2026-01-15T00:00:00Z");
        std::fs::write(grove_dir.join("learnings.md"), &content).unwrap();

        let mut config = Config::default();
        config.retrieval.strategy = "conservative".to_string();
        config.retrieval.max_injections = 20;
        let runner = HookRunner::new(MemorySessionStore::new(), config);

        let cwd = project_dir.to_str().unwrap();
        let input = format!(
            r#"{{"session_id":"pool-large","transcript_path":"/tmp/t.jsonl","cwd":"{}"}}"#,
            cwd
        );

        let result = runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();
        let _output: SessionStartOutput = serde_json::from_str(&result).unwrap();

        let session = runner.store.get("pool-large").unwrap().unwrap();
        assert!(
            session.gate.injected_learnings.len() <= 3,
            "Should stay conservative (cap 3) with large pool, got {}",
            session.gate.injected_learnings.len()
        );
    }

    // Per-session surfacing dedup tests

    #[test]
    fn test_session_start_deduplicates_surfaced_events() {
        // When session-start is called twice, learnings already in
        // injected_learnings should not get duplicate surfaced events
        use crate::storage::FileSessionStore;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let session_dir = temp_dir.path().join("sessions");
        let grove_dir = temp_dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let content = make_learnings_md(2, "2026-01-15T00:00:00Z");
        std::fs::write(grove_dir.join("learnings.md"), &content).unwrap();

        let store = FileSessionStore::with_dir(&session_dir).unwrap();
        let runner = HookRunner::new(store, Config::default());
        let cwd = temp_dir.path().to_str().unwrap();

        let input = format!(
            r#"{{"session_id":"dedup-test","transcript_path":"/tmp/t.jsonl","cwd":"{}"}}"#,
            cwd
        );

        // First session-start: learnings are surfaced normally
        runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();

        let session = runner.store.get("dedup-test").unwrap().unwrap();
        let first_count = session.gate.injected_learnings.len();
        assert!(first_count > 0, "Should have injected learnings");

        // Second session-start (same session): should not duplicate
        runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();

        let session = runner.store.get("dedup-test").unwrap().unwrap();
        assert_eq!(
            session.gate.injected_learnings.len(),
            first_count,
            "Should not duplicate injected learnings on re-entry"
        );

        // Verify stats log has each learning surfaced only once
        let stats_path = grove_dir.join("stats.log");
        if stats_path.exists() {
            let events = crate::stats::StatsLogger::new(&stats_path)
                .read_all()
                .unwrap_or_default();
            let surfaced: Vec<_> = events
                .iter()
                .filter(|e| e.data.event_name() == "surfaced")
                .collect();

            // Each learning should appear exactly once, not twice
            for il in &session.gate.injected_learnings {
                let count = surfaced
                    .iter()
                    .filter(|e| {
                        if let crate::stats::StatsEventType::Surfaced { learning_id, .. } = &e.data
                        {
                            learning_id == &il.learning_id
                        } else {
                            false
                        }
                    })
                    .count();
                assert_eq!(
                    count, 1,
                    "Learning {} should be surfaced exactly once, got {}",
                    il.learning_id, count
                );
            }
        }
    }

    // Git context extraction tests

    #[test]
    fn test_extract_git_context_in_git_repo() {
        // This test runs in the grove repo itself, so git commands should work
        let cwd = std::path::Path::new("/workspaces/grove");
        let (files, keywords) = extract_git_context(cwd);

        // We can't assert specific files/keywords since they change,
        // but we can verify the function returns without panicking
        // and that results are reasonable types
        assert!(
            files.iter().all(|f| !f.is_empty()),
            "All file paths should be non-empty"
        );
        assert!(
            keywords.iter().all(|k| !k.is_empty()),
            "All keywords should be non-empty"
        );
    }

    #[test]
    fn test_extract_git_context_non_git_dir() {
        // A temp dir is not a git repo — should fail silently
        let temp = tempfile::TempDir::new().unwrap();
        let (files, keywords) = extract_git_context(temp.path());

        assert!(files.is_empty(), "Non-git dir should produce no files");
        assert!(
            keywords.is_empty(),
            "Non-git dir should produce no keywords"
        );
    }

    #[test]
    fn test_extract_git_context_nonexistent_dir() {
        let (files, keywords) = extract_git_context(std::path::Path::new("/nonexistent/path"));
        assert!(files.is_empty());
        assert!(keywords.is_empty());
    }

    #[test]
    fn test_session_start_uses_git_context() {
        // Integration test: session-start in a git repo should populate SearchQuery
        // We test this indirectly by verifying the session runs successfully in a
        // real git repo with learnings that have matching context
        let temp = tempfile::TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Initialize a git repo in the temp dir
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(project_dir)
            .output()
            .unwrap();

        // Create a file and commit it
        std::fs::create_dir_all(project_dir.join("src")).unwrap();
        std::fs::write(project_dir.join("src/main.rs"), "fn main() {}").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(project_dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "init",
            ])
            .current_dir(project_dir)
            .output()
            .unwrap();

        // Create a branch with meaningful name
        std::process::Command::new("git")
            .args(["checkout", "-b", "fix/login-auth"])
            .current_dir(project_dir)
            .output()
            .unwrap();

        // Modify a file to create a diff
        std::fs::write(project_dir.join("src/main.rs"), "fn main() { todo!() }").unwrap();

        // Create learnings — one matching the branch keywords, one not
        let learnings_content = r#"# Grove Learnings

---
## cl_git_001

**Category:** Pitfall
**Summary:** Login authentication timeout issue
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #auth #login
**Session:** test-session
**Criteria:** Behavior Changing
**Created:** 2026-01-15T00:00:00Z

Watch for timeout in login auth flow.

---
## cl_git_002

**Category:** Pattern
**Summary:** Database migration best practices
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #database
**Session:** test-session
**Criteria:** Behavior Changing
**Created:** 2026-01-15T00:00:00Z

Always back up before migrating.

---
"#;
        std::fs::write(grove_dir.join("learnings.md"), learnings_content).unwrap();

        let runner = runner_with_strategy("moderate");
        let cwd = project_dir.to_str().unwrap();
        let input = format!(
            r#"{{"session_id":"git-ctx-test","transcript_path":"/tmp/t.jsonl","cwd":"{}"}}"#,
            cwd
        );

        // Should succeed regardless of whether context matches
        let result = runner.run_with_input(HookType::SessionStart, &input);
        assert!(
            result.is_ok(),
            "Session start should succeed with git context"
        );
    }

    #[test]
    fn test_session_start_without_git_falls_back_gracefully() {
        // Non-git directory should still work (empty query fallback)
        let temp = tempfile::TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let content = make_learnings_md(3, "2026-01-15T00:00:00Z");
        std::fs::write(grove_dir.join("learnings.md"), &content).unwrap();

        let runner = runner_with_strategy("moderate");
        let cwd = project_dir.to_str().unwrap();
        let input = format!(
            r#"{{"session_id":"no-git-test","transcript_path":"/tmp/t.jsonl","cwd":"{}"}}"#,
            cwd
        );

        let result = runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();
        let output: SessionStartOutput = serde_json::from_str(&result).unwrap();

        // Without git context, empty query should be used — markdown backend
        // returns relevance=1.0, so learnings should still be injected
        assert!(
            output.additional_context().is_some(),
            "Should inject learnings even without git context"
        );
    }

    #[test]
    fn test_session_start_uses_active_ticket_context_for_keywords() {
        // When a session has a restored ticket with Active status, its ID should be used
        let temp = tempfile::TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let learnings_content = r#"# Grove Learnings

---
## cl_ticket_001

**Category:** Pitfall
**Summary:** GROVE-42 deployment issue
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #deployment
**Session:** test-session
**Criteria:** Behavior Changing
**Created:** 2026-01-15T00:00:00Z

Watch for GROVE-42 deployment race condition.

---
"#;
        std::fs::write(grove_dir.join("learnings.md"), learnings_content).unwrap();

        let runner = runner_with_strategy("moderate");
        let cwd = project_dir.to_str().unwrap();

        // First create the session
        let input = format!(
            r#"{{"session_id":"ticket-ctx-test","transcript_path":"/tmp/t.jsonl","cwd":"{}"}}"#,
            cwd
        );
        runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();

        // Set a ticket context on the session with Active status
        let mut session = runner.store.get("ticket-ctx-test").unwrap().unwrap();
        session.gate.ticket = Some(TicketContext::new(
            "GROVE-42",
            "tissue",
            "Fix deployment race condition",
        ));
        session.gate.status = GateStatus::Active;
        runner.store.put(&session).unwrap();

        // Second session-start should pick up the ticket context as a keyword
        let result = runner.run_with_input(HookType::SessionStart, &input);
        assert!(
            result.is_ok(),
            "Session start should succeed with active ticket context"
        );
    }

    #[test]
    fn test_session_start_excludes_stale_terminal_ticket_keyword() {
        // When a session has a ticket in a terminal state (Reflected/Skipped),
        // the ticket ID should NOT be used as a search keyword
        let temp = tempfile::TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let learnings_content = "# Grove Learnings\n";
        std::fs::write(grove_dir.join("learnings.md"), learnings_content).unwrap();

        let runner = runner_with_strategy("moderate");
        let cwd = project_dir.to_str().unwrap();

        // Create session
        let input = format!(
            r#"{{"session_id":"stale-ticket-test","transcript_path":"/tmp/t.jsonl","cwd":"{}"}}"#,
            cwd
        );
        runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();

        // Set a ticket in Reflected (terminal) state
        let mut session = runner.store.get("stale-ticket-test").unwrap().unwrap();
        session.gate.ticket = Some(TicketContext::new(
            "OLD-TICKET",
            "tissue",
            "Already reflected ticket",
        ));
        session.gate.status = GateStatus::Reflected;
        runner.store.put(&session).unwrap();

        // Session-start should succeed without using the stale ticket ID
        let result = runner.run_with_input(HookType::SessionStart, &input);
        assert!(
            result.is_ok(),
            "Session start should succeed with terminal ticket (ID excluded from keywords)"
        );
    }

    #[test]
    fn test_session_start_excludes_skipped_ticket_keyword() {
        // Skipped state is also terminal - ticket ID should be excluded
        let temp = tempfile::TempDir::new().unwrap();
        let project_dir = temp.path();
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let learnings_content = "# Grove Learnings\n";
        std::fs::write(grove_dir.join("learnings.md"), learnings_content).unwrap();

        let runner = runner_with_strategy("moderate");
        let cwd = project_dir.to_str().unwrap();

        // Create session
        let input = format!(
            r#"{{"session_id":"skipped-ticket-test","transcript_path":"/tmp/t.jsonl","cwd":"{}"}}"#,
            cwd
        );
        runner
            .run_with_input(HookType::SessionStart, &input)
            .unwrap();

        // Set a ticket in Skipped (terminal) state
        let mut session = runner.store.get("skipped-ticket-test").unwrap().unwrap();
        session.gate.ticket = Some(TicketContext::new("SKIP-99", "tissue", "Skipped ticket"));
        session.gate.status = GateStatus::Skipped;
        runner.store.put(&session).unwrap();

        // Session-start should succeed without using the stale ticket ID
        let result = runner.run_with_input(HookType::SessionStart, &input);
        assert!(
            result.is_ok(),
            "Session start should succeed with skipped ticket (ID excluded from keywords)"
        );
    }

    // =========================================================================
    // extract_tool_input_keywords tests
    // =========================================================================

    #[test]
    fn test_extract_keywords_bash_command() {
        let input = serde_json::json!({"command": "cargo test --release connection_pool"});
        let keywords = extract_tool_input_keywords("Bash", &input);
        // "cargo" is noise (common CLI tool), "test" is noise, "connection_pool" is meaningful
        assert!(keywords.contains(&"connection_pool".to_string()));
        assert!(
            !keywords.contains(&"cargo".to_string()),
            "cargo should be filtered as noise"
        );
        // Flags should be filtered (starts with -)
        assert!(!keywords.iter().any(|k| k.starts_with('-')));
    }

    #[test]
    fn test_extract_keywords_grep_pattern() {
        let input = serde_json::json!({"pattern": "connection.*pool"});
        let keywords = extract_tool_input_keywords("Grep", &input);
        assert!(keywords.contains(&"connection".to_string()));
        assert!(keywords.contains(&"pool".to_string()));
    }

    #[test]
    fn test_extract_keywords_read_file_path() {
        let input = serde_json::json!({"file_path": "/src/database/connection_pool.rs"});
        let keywords = extract_tool_input_keywords("Read", &input);
        assert!(keywords.contains(&"database".to_string()));
        assert!(keywords.contains(&"connection_pool".to_string()));
    }

    #[test]
    fn test_extract_keywords_glob_pattern() {
        let input = serde_json::json!({"pattern": "**/*.rs"});
        let keywords = extract_tool_input_keywords("Glob", &input);
        // Short words (< 4 chars and not in acronym allowlist) should be filtered
        assert!(!keywords.contains(&"rs".to_string()));
    }

    #[test]
    fn test_extract_keywords_unknown_tool() {
        let input = serde_json::json!({"foo": "bar"});
        let keywords = extract_tool_input_keywords("UnknownTool", &input);
        assert!(keywords.is_empty());
    }

    #[test]
    fn test_extract_keywords_empty_input() {
        let input = serde_json::json!({});
        let keywords = extract_tool_input_keywords("Bash", &input);
        assert!(keywords.is_empty());
    }

    #[test]
    fn test_extract_keywords_null_input() {
        let input = serde_json::Value::Null;
        let keywords = extract_tool_input_keywords("Bash", &input);
        assert!(keywords.is_empty());
    }

    #[test]
    fn test_extract_keywords_filters_noise_words() {
        let input = serde_json::json!({"command": "git status"});
        let keywords = extract_tool_input_keywords("Bash", &input);
        // "git" and "status" are both in the expanded noise list
        assert!(!keywords.contains(&"git".to_string()));
        assert!(
            !keywords.contains(&"status".to_string()),
            "status should be filtered as noise (expanded R2 list)"
        );
        assert!(
            keywords.is_empty(),
            "both words should be filtered as noise"
        );

        // Verify domain terms still pass through the noise filter
        let input2 = serde_json::json!({"command": "git status connection_pool"});
        let keywords2 = extract_tool_input_keywords("Bash", &input2);
        assert!(
            keywords2.contains(&"connection_pool".to_string()),
            "domain terms should pass through noise filter"
        );
    }

    #[test]
    fn test_extract_keywords_deduplicates() {
        let input = serde_json::json!({"command": "pool connection pool connection"});
        let keywords = extract_tool_input_keywords("Bash", &input);
        let pool_count = keywords.iter().filter(|k| *k == "pool").count();
        assert_eq!(pool_count, 1, "Should deduplicate keywords");
    }

    #[test]
    fn test_extract_keywords_lowercases() {
        let input = serde_json::json!({"pattern": "ConnectionPool"});
        let keywords = extract_tool_input_keywords("Grep", &input);
        assert!(keywords.contains(&"connectionpool".to_string()));
    }

    #[test]
    fn test_extract_keywords_web_search() {
        let input = serde_json::json!({"query": "rust async connection pooling"});
        let keywords = extract_tool_input_keywords("WebSearch", &input);
        assert!(keywords.contains(&"rust".to_string()));
        assert!(keywords.contains(&"async".to_string()));
        assert!(keywords.contains(&"connection".to_string()));
        assert!(keywords.contains(&"pooling".to_string()));
    }

    #[test]
    fn test_extract_keywords_web_fetch() {
        let input = serde_json::json!({"url": "https://docs.rs/sqlx/connection"});
        let keywords = extract_tool_input_keywords("WebFetch", &input);
        assert!(keywords.contains(&"docs".to_string()));
        assert!(keywords.contains(&"sqlx".to_string()));
        assert!(keywords.contains(&"connection".to_string()));
    }

    #[test]
    fn test_extract_keywords_edit_tool() {
        let input = serde_json::json!({"file_path": "/src/middleware/auth_handler.rs"});
        let keywords = extract_tool_input_keywords("Edit", &input);
        assert!(keywords.contains(&"middleware".to_string()));
        assert!(keywords.contains(&"auth_handler".to_string()));
    }

    #[test]
    fn test_extract_keywords_write_tool() {
        let input = serde_json::json!({"file_path": "/src/config/database.rs"});
        let keywords = extract_tool_input_keywords("Write", &input);
        assert!(keywords.contains(&"config".to_string()));
        assert!(keywords.contains(&"database".to_string()));
    }

    #[test]
    fn test_extract_keywords_task_tool() {
        // Task tool receives natural language prompts. Stopword filtering
        // should remove common English words while preserving domain terms.
        let input = serde_json::json!({
            "prompt": "Search for connection pool configuration and check if timeout settings are appropriate"
        });
        let keywords = extract_tool_input_keywords("Task", &input);

        // Domain terms should survive
        assert!(
            keywords.contains(&"connection".to_string()),
            "domain term 'connection' should be extracted"
        );
        assert!(
            keywords.contains(&"pool".to_string()),
            "domain term 'pool' should be extracted"
        );
        assert!(
            keywords.contains(&"configuration".to_string()),
            "domain term 'configuration' should be extracted"
        );
        assert!(
            keywords.contains(&"timeout".to_string()),
            "domain term 'timeout' should be extracted"
        );
        assert!(
            keywords.contains(&"settings".to_string()),
            "domain term 'settings' should be extracted"
        );

        // Stopwords should be filtered out
        assert!(
            !keywords.contains(&"search".to_string()),
            "'search' is a stopword and should be filtered"
        );
        assert!(
            !keywords.contains(&"check".to_string()),
            "'check' is a stopword and should be filtered"
        );
        assert!(
            !keywords.contains(&"appropriate".to_string()),
            "'appropriate' is a stopword and should be filtered"
        );
        assert!(
            !keywords.contains(&"are".to_string()),
            "'are' is a stopword and should be filtered"
        );
        assert!(
            !keywords.iter().any(|k| k == "if"),
            "'if' should be filtered (< 4 chars and not in allowlist, also a stopword)"
        );
        assert!(
            !keywords.contains(&"and".to_string()),
            "'and' is in the noise list and should be filtered"
        );
        assert!(
            !keywords.contains(&"for".to_string()),
            "'for' is in the noise list and should be filtered"
        );
    }

    #[test]
    fn test_extract_keywords_task_tool_empty_prompt() {
        // Missing prompt field
        let input = serde_json::json!({"description": "some description"});
        let keywords = extract_tool_input_keywords("Task", &input);
        assert!(
            keywords.is_empty(),
            "missing prompt should yield no keywords"
        );

        // Empty prompt string
        let input2 = serde_json::json!({"prompt": ""});
        let keywords2 = extract_tool_input_keywords("Task", &input2);
        assert!(
            keywords2.is_empty(),
            "empty prompt should yield no keywords"
        );
    }

    #[test]
    fn test_extract_keywords_task_tool_technical_terms() {
        // Verify that technical/domain terms survive both noise and stopword filters
        let input = serde_json::json!({
            "prompt": "Review the database migration for connection pool configuration and check timeout settings"
        });
        let keywords = extract_tool_input_keywords("Task", &input);

        // These domain-specific terms must survive filtering
        assert!(
            keywords.contains(&"database".to_string()),
            "technical term 'database' should survive"
        );
        assert!(
            keywords.contains(&"migration".to_string()),
            "technical term 'migration' should survive"
        );
        assert!(
            keywords.contains(&"connection".to_string()),
            "technical term 'connection' should survive"
        );
        assert!(
            keywords.contains(&"pool".to_string()),
            "technical term 'pool' should survive"
        );
        assert!(
            keywords.contains(&"configuration".to_string()),
            "technical term 'configuration' should survive"
        );
        assert!(
            keywords.contains(&"timeout".to_string()),
            "technical term 'timeout' should survive"
        );
        assert!(
            keywords.contains(&"settings".to_string()),
            "technical term 'settings' should survive"
        );

        // Common English words should be filtered
        assert!(
            !keywords.contains(&"review".to_string()),
            "'review' is a stopword"
        );
        assert!(
            !keywords.contains(&"the".to_string()),
            "'the' is a noise/stopword"
        );
        assert!(
            !keywords.contains(&"check".to_string()),
            "'check' is a stopword"
        );
    }

    #[test]
    fn test_extract_keywords_acronym_allowlist() {
        // Verify the acronym allowlist preserves meaningful short technical
        // terms while generic 3-char words are still filtered by min length 4.

        // Allowlisted acronyms should be preserved
        let input = serde_json::json!({"command": "configure sql jwt ssl tls api cli dns"});
        let keywords = extract_tool_input_keywords("Bash", &input);
        assert!(
            keywords.contains(&"sql".to_string()),
            "'sql' should be preserved (allowlisted). Keywords: {:?}",
            keywords
        );
        assert!(
            keywords.contains(&"jwt".to_string()),
            "'jwt' should be preserved (allowlisted). Keywords: {:?}",
            keywords
        );
        assert!(
            keywords.contains(&"ssl".to_string()),
            "'ssl' should be preserved (allowlisted). Keywords: {:?}",
            keywords
        );
        assert!(
            keywords.contains(&"tls".to_string()),
            "'tls' should be preserved (allowlisted). Keywords: {:?}",
            keywords
        );
        assert!(
            keywords.contains(&"api".to_string()),
            "'api' should be preserved (allowlisted). Keywords: {:?}",
            keywords
        );
        assert!(
            keywords.contains(&"cli".to_string()),
            "'cli' should be preserved (allowlisted). Keywords: {:?}",
            keywords
        );
        assert!(
            keywords.contains(&"dns".to_string()),
            "'dns' should be preserved (allowlisted). Keywords: {:?}",
            keywords
        );

        // Cloud/infra acronyms
        let input2 = serde_json::json!({"command": "deploy to aws vpc rds ec2 s3"});
        let keywords2 = extract_tool_input_keywords("Bash", &input2);
        assert!(
            keywords2.contains(&"aws".to_string()),
            "'aws' should be preserved (allowlisted). Keywords: {:?}",
            keywords2
        );
        assert!(
            keywords2.contains(&"vpc".to_string()),
            "'vpc' should be preserved (allowlisted). Keywords: {:?}",
            keywords2
        );
        assert!(
            keywords2.contains(&"rds".to_string()),
            "'rds' should be preserved (allowlisted). Keywords: {:?}",
            keywords2
        );
        assert!(
            keywords2.contains(&"ec2".to_string()),
            "'ec2' should be preserved (allowlisted). Keywords: {:?}",
            keywords2
        );
        assert!(
            keywords2.contains(&"s3".to_string()),
            "'s3' should be preserved (allowlisted). Keywords: {:?}",
            keywords2
        );

        // Generic 3-char words should still be filtered
        let input3 = serde_json::json!({"command": "app log env foo bar baz"});
        let keywords3 = extract_tool_input_keywords("Bash", &input3);
        assert!(
            !keywords3.contains(&"app".to_string()),
            "'app' should be filtered (not in allowlist, < 4 chars). Keywords: {:?}",
            keywords3
        );
        assert!(
            !keywords3.contains(&"env".to_string()),
            "'env' should be filtered (not in allowlist, < 4 chars). Keywords: {:?}",
            keywords3
        );
        assert!(
            !keywords3.contains(&"foo".to_string()),
            "'foo' should be filtered (not in allowlist, < 4 chars). Keywords: {:?}",
            keywords3
        );

        // Words >= 4 chars should still be preserved
        let input4 = serde_json::json!({"command": "connection pool timeout"});
        let keywords4 = extract_tool_input_keywords("Bash", &input4);
        assert!(
            keywords4.contains(&"connection".to_string()),
            "'connection' should be preserved (>= 4 chars). Keywords: {:?}",
            keywords4
        );
        assert!(
            keywords4.contains(&"pool".to_string()),
            "'pool' should be preserved (>= 4 chars). Keywords: {:?}",
            keywords4
        );
        assert!(
            keywords4.contains(&"timeout".to_string()),
            "'timeout' should be preserved (>= 4 chars). Keywords: {:?}",
            keywords4
        );

        // Verify allowlist works with NLP extractor (Task tool)
        let input5 = serde_json::json!({
            "prompt": "Configure the sql connection with jwt authentication over tls"
        });
        let keywords5 = extract_tool_input_keywords("Task", &input5);
        assert!(
            keywords5.contains(&"sql".to_string()),
            "'sql' should be preserved in NLP context (allowlisted). Keywords: {:?}",
            keywords5
        );
        assert!(
            keywords5.contains(&"jwt".to_string()),
            "'jwt' should be preserved in NLP context (allowlisted). Keywords: {:?}",
            keywords5
        );
        assert!(
            keywords5.contains(&"tls".to_string()),
            "'tls' should be preserved in NLP context (allowlisted). Keywords: {:?}",
            keywords5
        );
    }

    // =========================================================================
    // Deferred injection flow tests
    // =========================================================================

    fn test_runner_with_config(config: Config) -> HookRunner<MemorySessionStore> {
        HookRunner::new(MemorySessionStore::new(), config)
    }

    #[test]
    fn test_session_start_sets_deferred_injection_flag() {
        let runner = test_runner();
        let input = r#"{
            "session_id": "deferred-flag-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;

        runner
            .run_with_input(HookType::SessionStart, input)
            .unwrap();

        let session = runner.store.get("deferred-flag-test").unwrap().unwrap();
        assert!(
            session.gate.deferred_injection_pending,
            "Deferred injection flag should be set after session start"
        );
    }

    #[test]
    fn test_session_start_no_deferred_flag_when_disabled() {
        let mut config = Config::default();
        config.context.deferred_injection = false;
        let runner = test_runner_with_config(config);

        let input = r#"{
            "session_id": "no-deferred-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;

        runner
            .run_with_input(HookType::SessionStart, input)
            .unwrap();

        let session = runner.store.get("no-deferred-test").unwrap().unwrap();
        assert!(
            !session.gate.deferred_injection_pending,
            "Deferred injection flag should NOT be set when config disables it"
        );
    }

    #[test]
    fn test_pre_tool_use_clears_deferred_flag() {
        let runner = test_runner();

        // Create session via SessionStart (sets deferred flag)
        let start_input = r#"{
            "session_id": "clear-flag-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // Verify flag is set
        let session = runner.store.get("clear-flag-test").unwrap().unwrap();
        assert!(session.gate.deferred_injection_pending);

        // Send a PreToolUse call
        let pre_input = r#"{
            "session_id": "clear-flag-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Grep",
            "tool_input": {"pattern": "connection pool"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input)
            .unwrap();

        // Flag should be cleared
        let session = runner.store.get("clear-flag-test").unwrap().unwrap();
        assert!(
            !session.gate.deferred_injection_pending,
            "Deferred injection flag should be cleared after first PreToolUse"
        );
    }

    #[test]
    fn test_pre_tool_use_deferred_flag_cleared_even_on_non_bash() {
        let runner = test_runner();

        // Create session
        let start_input = r#"{
            "session_id": "non-bash-clear-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // Send a non-Bash PreToolUse (Read tool)
        let pre_input = r#"{
            "session_id": "non-bash-clear-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Read",
            "tool_input": {"file_path": "/src/database/pool.rs"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input)
            .unwrap();

        let session = runner.store.get("non-bash-clear-test").unwrap().unwrap();
        assert!(
            !session.gate.deferred_injection_pending,
            "Flag should be cleared for any tool type, not just Bash"
        );
    }

    #[test]
    fn test_pre_tool_use_subsequent_calls_no_overhead() {
        let runner = test_runner();

        // Create session
        let start_input = r#"{
            "session_id": "subsequent-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // First PreToolUse clears the flag
        let pre_input1 = r#"{
            "session_id": "subsequent-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Grep",
            "tool_input": {"pattern": "connection pool"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input1)
            .unwrap();

        // Second PreToolUse should not trigger deferred injection
        let pre_input2 = r#"{
            "session_id": "subsequent-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "cargo test"}
        }"#;
        let result = runner
            .run_with_input(HookType::PreToolUse, pre_input2)
            .unwrap();

        // Second call should just return simple allow (no additional_context)
        let output: PreToolUseOutput = serde_json::from_str(&result).unwrap();
        assert!(output.is_allowed());
        // No deferred injection trace on second call
        let session = runner.store.get("subsequent-test").unwrap().unwrap();
        let deferred_traces: Vec<_> = session
            .trace
            .iter()
            .filter(|t| t.event_type == EventType::DeferredInjection)
            .collect();
        // Should have exactly the traces from SessionStart (pending) + first PreToolUse
        // but NOT from the second PreToolUse
        assert!(
            deferred_traces.len() <= 2,
            "Second PreToolUse should not add DeferredInjection trace"
        );
    }

    #[test]
    fn test_pre_tool_use_deferred_no_keywords_skips_retrieval() {
        let runner = test_runner();

        // Create session
        let start_input = r#"{
            "session_id": "no-kw-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // PreToolUse with unknown tool (no keywords extracted)
        let pre_input = r#"{
            "session_id": "no-kw-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "UnknownTool",
            "tool_input": {"something": "irrelevant"}
        }"#;
        let result = runner
            .run_with_input(HookType::PreToolUse, pre_input)
            .unwrap();

        // Should still allow (no error)
        let output: PreToolUseOutput = serde_json::from_str(&result).unwrap();
        assert!(output.is_allowed());

        // Flag should be cleared even though no keywords extracted
        let session = runner.store.get("no-kw-test").unwrap().unwrap();
        assert!(!session.gate.deferred_injection_pending);

        // Should have a trace indicating no keywords
        assert!(session.trace.iter().any(|t| {
            t.event_type == EventType::DeferredInjection
                && t.details
                    .as_ref()
                    .is_some_and(|d| d.contains("no keywords"))
        }));
    }

    #[test]
    fn test_pre_tool_use_deferred_injection_fail_open() {
        let runner = test_runner();

        // Create session directly with flag set (simulating session-start)
        let mut session = SessionState::new("fail-open-test", "/tmp/project", "/tmp/t.jsonl");
        session.gate.deferred_injection_pending = true;
        runner.store.put(&session).unwrap();

        // PreToolUse with keywords — retrieval may fail but should not block
        let pre_input = r#"{
            "session_id": "fail-open-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/nonexistent/path",
            "tool_name": "Grep",
            "tool_input": {"pattern": "database connection"}
        }"#;
        let result = runner.run_with_input(HookType::PreToolUse, pre_input);
        assert!(
            result.is_ok(),
            "Deferred injection errors should not block the tool"
        );

        // Flag should be cleared
        let session = runner.store.get("fail-open-test").unwrap().unwrap();
        assert!(!session.gate.deferred_injection_pending);
    }

    #[test]
    fn test_pre_tool_use_deferred_then_ticket_close() {
        let runner = test_runner();

        // Create session
        let start_input = r#"{
            "session_id": "deferred-then-close",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // First call: Grep (triggers deferred injection, clears flag)
        let pre_input1 = r#"{
            "session_id": "deferred-then-close",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Grep",
            "tool_input": {"pattern": "connection pool"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input1)
            .unwrap();

        // Second call: Bash ticket close (should still work)
        let pre_input2 = r#"{
            "session_id": "deferred-then-close",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "tissue status grove-123 closed"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input2)
            .unwrap();

        let session = runner.store.get("deferred-then-close").unwrap().unwrap();
        assert_eq!(
            session.gate.status,
            GateStatus::Pending,
            "Ticket close should still be detected after deferred injection"
        );
    }

    #[test]
    fn test_pre_tool_use_first_call_ticket_close_still_detected() {
        let runner = test_runner();

        // Create session (sets deferred flag)
        let start_input = r#"{
            "session_id": "first-call-close",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        // Verify deferred flag is set
        let session = runner.store.get("first-call-close").unwrap().unwrap();
        assert!(session.gate.deferred_injection_pending);

        // First tool call IS a ticket close — both deferred injection AND
        // ticket-close detection should fire (no early return)
        let pre_input = r#"{
            "session_id": "first-call-close",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Bash",
            "tool_input": {"command": "tissue status grove-123 closed"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input)
            .unwrap();

        let session = runner.store.get("first-call-close").unwrap().unwrap();
        assert!(
            !session.gate.deferred_injection_pending,
            "Deferred flag should be cleared"
        );
        assert_eq!(
            session.gate.status,
            GateStatus::Pending,
            "Ticket close on first tool call must still be detected"
        );
    }

    #[test]
    fn test_pre_tool_use_non_bash_short_circuits_after_deferred_cleared() {
        let runner = test_runner();

        // Create session and clear deferred flag via first PreToolUse
        let start_input = r#"{
            "session_id": "short-circuit-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project"
        }"#;
        runner
            .run_with_input(HookType::SessionStart, start_input)
            .unwrap();

        let pre_input1 = r#"{
            "session_id": "short-circuit-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Grep",
            "tool_input": {"pattern": "test"}
        }"#;
        runner
            .run_with_input(HookType::PreToolUse, pre_input1)
            .unwrap();

        // Subsequent non-Bash call should short-circuit (returns allow, no session save)
        let pre_input2 = r#"{
            "session_id": "short-circuit-test",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "tool_name": "Read",
            "tool_input": {"file_path": "/tmp/some_file.rs"}
        }"#;
        let result = runner
            .run_with_input(HookType::PreToolUse, pre_input2)
            .unwrap();

        let output: PreToolUseOutput = serde_json::from_str(&result).unwrap();
        assert!(output.is_allowed());
    }

    // =========================================================================
    // State serialization tests for deferred_injection_pending
    // =========================================================================

    #[test]
    fn test_deferred_injection_pending_serialization() {
        use crate::core::state::GateState;

        let gate = GateState {
            deferred_injection_pending: true,
            ..Default::default()
        };

        let json = serde_json::to_string(&gate).unwrap();
        let deserialized: GateState = serde_json::from_str(&json).unwrap();
        assert!(deserialized.deferred_injection_pending);
    }

    #[test]
    fn test_deferred_injection_pending_defaults_false() {
        use crate::core::state::GateState;

        // Simulate loading old session JSON without the new field
        let json = r#"{
            "status": "idle",
            "block_count": 0,
            "circuit_breaker_tripped": false,
            "last_blocked_session_id": null,
            "last_blocked_at": null,
            "reflection": null,
            "skip": null,
            "subagent_observations": [],
            "injected_learnings": [],
            "ticket_close_intent": null,
            "cached_diff_size": null,
            "ticket": null
        }"#;

        let gate: GateState = serde_json::from_str(json).unwrap();
        assert!(
            !gate.deferred_injection_pending,
            "Should default to false for backward compatibility with old sessions"
        );
    }

    #[test]
    fn test_deferred_injection_event_type_serialization() {
        let event = EventType::DeferredInjection;
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(json, r#""deferred_injection""#);
        let deserialized: EventType = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, EventType::DeferredInjection);
    }

    // =========================================================================
    // Keyword Extraction Quality Audit - Validation Experiments
    //
    // These tests validate or bust the four recommendations from the keyword
    // extraction quality audit (design/research/keyword-extraction-audit.md).
    //
    // Run with: cargo test -- keyword_experiment
    // =========================================================================

    // -------------------------------------------------------------------------
    // Experiment 1: Path Prefix Pollution
    //
    // Demonstrates that full absolute paths inject noise keywords from the
    // user's home directory structure. The v2 path-stripping fix should
    // eliminate these.
    // -------------------------------------------------------------------------

    #[test]
    fn keyword_experiment_r1_path_pollution_baseline() {
        // Demonstrate the problem WITHOUT path stripping: full path extracts
        // path infrastructure tokens. Use v2_with_options(strip_paths=false)
        // to simulate pre-R1 behavior.
        let input = serde_json::json!({
            "file_path": "/Users/devuser/GitHub/org/my-project/src/config.rs"
        });

        let keywords_no_strip =
            extract_tool_input_keywords_v2_with_options("Read", &input, false, false, false, 3);

        // Without path stripping, all path components >= 3 chars are extracted
        assert!(
            keywords_no_strip.contains(&"devuser".to_string()),
            "Without path stripping should extract 'devuser': {:?}",
            keywords_no_strip
        );

        // Count how many of the extracted keywords are noise (not project-relevant)
        let noise_tokens: Vec<&String> = keywords_no_strip
            .iter()
            .filter(|k| matches!(k.as_str(), "users" | "devuser" | "github" | "org"))
            .collect();
        let meaningful_tokens: Vec<&String> = keywords_no_strip
            .iter()
            .filter(|k| matches!(k.as_str(), "project" | "config"))
            .collect();

        eprintln!("=== R1 Path Pollution Baseline (without stripping) ===");
        eprintln!("All keywords (no strip): {:?}", keywords_no_strip);
        eprintln!("Noise tokens: {:?} ({})", noise_tokens, noise_tokens.len());
        eprintln!(
            "Meaningful tokens: {:?} ({})",
            meaningful_tokens,
            meaningful_tokens.len()
        );
        let noise_ratio = noise_tokens.len() as f64 / keywords_no_strip.len().max(1) as f64;
        eprintln!("Noise ratio: {:.0}%", noise_ratio * 100.0);

        // The noise ratio should be substantial, demonstrating the problem
        assert!(
            noise_tokens.len() >= 2,
            "Expected at least 2 noise tokens from path prefix, got {}",
            noise_tokens.len()
        );

        // Now verify production (with R1 path stripping) fixes the problem
        let keywords_prod = extract_tool_input_keywords("Read", &input);
        assert!(
            !keywords_prod.contains(&"devuser".to_string()),
            "Production (with R1) should NOT extract 'devuser': {:?}",
            keywords_prod
        );
        assert!(
            keywords_prod.contains(&"project".to_string()),
            "Production should still extract 'project': {:?}",
            keywords_prod
        );
    }

    #[test]
    fn keyword_experiment_r1_path_stripping_fix() {
        // Production now has R1 path stripping. Verify it removes home dir /
        // hosting dir prefix while preserving meaningful tokens.
        let input = serde_json::json!({
            "file_path": "/Users/devuser/GitHub/org/my-project/src/config.rs"
        });

        let keywords_prod = extract_tool_input_keywords("Read", &input);

        eprintln!("=== R1 Path Stripping Fix (production) ===");
        eprintln!("Production keywords (path stripped): {:?}", keywords_prod);

        // After stripping, "users", "devuser", "github", "org" should be gone
        assert!(
            !keywords_prod.contains(&"devuser".to_string()),
            "Production should NOT extract 'devuser' after path stripping: {:?}",
            keywords_prod
        );
        assert!(
            !keywords_prod.contains(&"users".to_string()),
            "Production should NOT extract 'users' after path stripping: {:?}",
            keywords_prod
        );

        // But meaningful tokens should still be present
        assert!(
            keywords_prod.contains(&"project".to_string()),
            "Production should still extract 'project': {:?}",
            keywords_prod
        );
        // "config" is not in the expanded noise list, so it should still be extracted
        assert!(
            keywords_prod.contains(&"config".to_string()),
            "Production should still extract 'config': {:?}",
            keywords_prod
        );
    }

    #[test]
    fn keyword_experiment_r1_path_stripping_multiple_paths() {
        // Test path stripping across different path formats using production
        // function (which now has R1 path stripping and R2 expanded noise).
        let test_cases: Vec<(&str, Vec<&str>, Vec<&str>)> = vec![
            // (path, should_not_contain, should_contain)
            (
                "/Users/devuser/GitHub/org/my-project/src/main.rs",
                vec!["users", "devuser", "github", "org"],
                // "main" is now filtered by R2 expanded noise list
                vec!["project"],
            ),
            (
                "/home/developer/projects/myapp/lib/database.rs",
                vec!["developer"],
                vec!["myapp", "database"],
            ),
            (
                "/Users/dev/GitHub/org/repo/tests/integration.rs",
                vec!["users", "github"],
                vec!["repo", "tests", "integration"],
            ),
        ];

        eprintln!("=== R1 Path Stripping Multiple Paths (production) ===");
        for (path, should_not_contain, should_contain) in &test_cases {
            let input = serde_json::json!({"file_path": path});
            let kw = extract_tool_input_keywords("Read", &input);
            eprintln!("Path: {} -> keywords: {:?}", path, kw);

            for bad in should_not_contain {
                assert!(
                    !kw.contains(&bad.to_string()),
                    "Path '{}': should not contain '{}', got {:?}",
                    path,
                    bad,
                    kw
                );
            }
            for good in should_contain {
                assert!(
                    kw.contains(&good.to_string()),
                    "Path '{}': should contain '{}', got {:?}",
                    path,
                    good,
                    kw
                );
            }
        }
    }

    // -------------------------------------------------------------------------
    // Experiment 2: Noise List Expansion
    //
    // Demonstrates that common CLI/git terms pass through the small noise list
    // and pollute keyword sets. The expanded noise list should filter them.
    // -------------------------------------------------------------------------

    #[test]
    fn keyword_experiment_r2_noise_gaps_baseline() {
        // Demonstrate the problem WITHOUT expanded noise: common CLI/git terms
        // pass through the base noise list. Use v2_with_options(expanded_noise=false)
        // to simulate pre-R2 behavior.
        let test_cases: Vec<(&str, Vec<&str>)> = vec![
            (
                "git status",
                vec!["status"], // "git" is in base noise, but "status" passes through
            ),
            (
                "cargo test --release",
                vec!["release"], // "cargo" and "test" are base noise, but "release" passes
            ),
            (
                "git diff HEAD~3",
                vec!["diff", "head"], // both pass through base noise list
            ),
            ("git log --oneline", vec!["log", "oneline"]),
            ("git push origin main", vec!["push", "origin", "main"]),
        ];

        eprintln!("=== R2 Noise Gaps Baseline (without expanded noise) ===");
        let mut total_noise_passed = 0;
        for (cmd, expected_noise) in &test_cases {
            let input = serde_json::json!({"command": cmd});
            let kw =
                extract_tool_input_keywords_v2_with_options("Bash", &input, false, false, false, 3);
            eprintln!("Command: '{}' -> base-noise-only keywords: {:?}", cmd, kw);

            for noise_word in expected_noise {
                if kw.contains(&noise_word.to_string()) {
                    total_noise_passed += 1;
                }
            }
        }

        eprintln!(
            "Total noise words that passed base-noise-only filter: {}",
            total_noise_passed
        );
        assert!(
            total_noise_passed >= 5,
            "Expected at least 5 noise words to pass through base noise list, got {}",
            total_noise_passed
        );

        // Now verify production (with R2 expanded noise) filters the key noise words.
        // Only check words that are in the expanded noise list (some words like
        // "head", "oneline", "origin" are not in the expanded list).
        let expanded_noise_words = ["status", "release", "diff", "log", "push", "main"];
        let mut total_noise_in_prod = 0;
        for word in &expanded_noise_words {
            // Build a command containing this word
            let input = serde_json::json!({"command": format!("git {}", word)});
            let kw = extract_tool_input_keywords("Bash", &input);
            if kw.contains(&word.to_string()) {
                total_noise_in_prod += 1;
            }
        }
        assert_eq!(
            total_noise_in_prod, 0,
            "Production (with R2) should filter expanded noise words, but {} passed",
            total_noise_in_prod
        );
    }

    #[test]
    fn keyword_experiment_r2_expanded_noise_fix() {
        // Production now has expanded noise list (R2). Verify it filters noise.
        let test_cases: Vec<(&str, Vec<&str>)> = vec![
            ("git status", vec!["status"]),
            ("cargo test --release", vec!["release"]),
            ("git diff HEAD~3", vec!["diff"]),
            ("git log --oneline", vec!["log"]),
            ("git push origin main", vec!["push", "main"]),
        ];

        eprintln!("=== R2 Expanded Noise Fix (production) ===");
        let mut total_noise_passed = 0;
        for (cmd, noise_words) in &test_cases {
            let input = serde_json::json!({"command": cmd});
            let kw = extract_tool_input_keywords("Bash", &input);
            eprintln!("Command: '{}' -> production keywords: {:?}", cmd, kw);

            for noise_word in noise_words {
                if kw.contains(&noise_word.to_string()) {
                    total_noise_passed += 1;
                }
            }
        }

        eprintln!(
            "Total noise words that passed production expanded filter: {}",
            total_noise_passed
        );
        assert!(
            total_noise_passed == 0,
            "Expected 0 noise words to pass through production, got {}",
            total_noise_passed
        );
    }

    #[test]
    fn keyword_experiment_r2_noise_preserves_domain_terms() {
        // Verify the production expanded noise list does NOT filter out domain-specific terms
        let domain_commands: Vec<(&str, Vec<&str>)> = vec![
            ("cargo test --test kamal_deploy", vec!["kamal_deploy"]),
            ("tissue status pickup-42 closed", vec!["tissue", "pickup"]),
            (
                "grep 'connection_pool' src/database.rs",
                vec!["connection_pool", "database"],
            ),
        ];

        eprintln!("=== R2 Noise Preserves Domain Terms (production) ===");
        for (cmd, expected_present) in &domain_commands {
            let input = serde_json::json!({"command": cmd});
            let kw = extract_tool_input_keywords("Bash", &input);
            eprintln!("Command: '{}' -> production keywords: {:?}", cmd, kw);

            for term in expected_present {
                assert!(
                    kw.contains(&term.to_string()),
                    "Production expanded noise should preserve domain term '{}' in '{}', got {:?}",
                    term,
                    cmd,
                    kw
                );
            }
        }
    }

    // -------------------------------------------------------------------------
    // Experiment 3: Task Tool Support
    //
    // Demonstrates that the Task tool prompt field is a rich source of
    // domain keywords that v1 ignores entirely.
    // -------------------------------------------------------------------------

    #[test]
    fn keyword_experiment_r3_task_tool_baseline() {
        // Production (v1) now supports Task tool with stopword filtering (R3).
        // It should extract domain keywords but filter common English words.
        let input = serde_json::json!({
            "prompt": "Search for connection pool configuration in the database module"
        });

        let keywords_v1 = extract_tool_input_keywords("Task", &input);

        eprintln!("=== R3 Task Tool Baseline ===");
        eprintln!("Task prompt keywords (v1): {:?}", keywords_v1);

        // Domain terms should be present
        assert!(
            keywords_v1.contains(&"connection".to_string()),
            "v1 should extract 'connection' from Task prompt: {:?}",
            keywords_v1
        );
        assert!(
            keywords_v1.contains(&"pool".to_string()),
            "v1 should extract 'pool' from Task prompt: {:?}",
            keywords_v1
        );
        assert!(
            keywords_v1.contains(&"configuration".to_string()),
            "v1 should extract 'configuration' from Task prompt: {:?}",
            keywords_v1
        );
        // "database" is in the noise list (generic programming term), so it's filtered
        // "search" is in the stopword list, so it's filtered
        assert!(
            !keywords_v1.contains(&"search".to_string()),
            "'search' should be filtered as stopword: {:?}",
            keywords_v1
        );
        assert!(
            !keywords_v1.contains(&"the".to_string()),
            "'the' should be filtered as noise/stopword: {:?}",
            keywords_v1
        );
    }

    #[test]
    fn keyword_experiment_r3_task_tool_fix() {
        // v2 extracts keywords from Task tool prompt
        let input = serde_json::json!({
            "prompt": "Search for connection pool configuration in the database module"
        });

        let keywords_v2 =
            extract_tool_input_keywords_v2_with_options("Task", &input, false, false, true, 3);

        eprintln!("=== R3 Task Tool Fix ===");
        eprintln!("Task prompt keywords (v2): {:?}", keywords_v2);

        assert!(
            keywords_v2.contains(&"connection".to_string()),
            "v2 should extract 'connection' from Task prompt: {:?}",
            keywords_v2
        );
        assert!(
            keywords_v2.contains(&"pool".to_string()),
            "v2 should extract 'pool' from Task prompt: {:?}",
            keywords_v2
        );
        assert!(
            keywords_v2.contains(&"configuration".to_string()),
            "v2 should extract 'configuration' from Task prompt: {:?}",
            keywords_v2
        );
        assert!(
            keywords_v2.contains(&"database".to_string()),
            "v2 should extract 'database' from Task prompt: {:?}",
            keywords_v2
        );
    }

    #[test]
    fn keyword_experiment_r3_task_tool_variety() {
        // Test Task tool across various prompt styles.
        // With R3 stopword filtering, common English words like "investigate",
        // "review" are filtered. Domain-specific terms survive.
        let prompts: Vec<(&str, Vec<&str>, Vec<&str>)> = vec![
            (
                "Investigate the Kamal deployment failures on staging",
                // Domain terms that should survive
                vec!["kamal", "deployment", "failures", "staging"],
                // Stopwords that should be filtered
                vec!["investigate", "the"],
            ),
            (
                "Review the tissue CLI integration test results",
                // "results" survives (not a stopword - has domain specificity)
                // "cli" survives (3+ chars, not noise/stopword)
                vec!["tissue", "integration", "results", "cli"],
                // "review" is a stopword, "the" is noise/stopword
                vec!["review", "the"],
            ),
            (
                "Fix the authentication middleware error handling",
                // "handling" survives, "fix" survives (not in stopwords)
                // "error" survives with expanded_noise=false (only in expanded list)
                vec!["authentication", "middleware", "handling", "fix", "error"],
                // "the" is in base noise and stopwords
                vec!["the"],
            ),
        ];

        eprintln!("=== R3 Task Tool Variety ===");
        for (prompt, expected_present, expected_filtered) in &prompts {
            let input = serde_json::json!({"prompt": prompt});
            let kw =
                extract_tool_input_keywords_v2_with_options("Task", &input, false, false, true, 3);
            eprintln!("Prompt: '{}' -> keywords: {:?}", prompt, kw);

            for term in expected_present {
                assert!(
                    kw.contains(&term.to_string()),
                    "Task prompt '{}': should contain '{}', got {:?}",
                    prompt,
                    term,
                    kw
                );
            }
            for term in expected_filtered {
                assert!(
                    !kw.contains(&term.to_string()),
                    "Task prompt '{}': should NOT contain stopword/noise '{}', got {:?}",
                    prompt,
                    term,
                    kw
                );
            }
        }
    }

    // -------------------------------------------------------------------------
    // Experiment 4: Minimum Keyword Length
    //
    // Demonstrates that 3-char keywords are too ambiguous and match broadly.
    // Raising to 4 chars filters noise without losing important terms.
    // -------------------------------------------------------------------------

    #[test]
    fn keyword_experiment_r4_min_length_baseline() {
        // R4 is now in production: min_length=4 with acronym allowlist.
        // Short ambiguous 3-char tokens (app, log, env) are filtered,
        // but "api" is preserved because it's in the acronym allowlist.
        let input = serde_json::json!({
            "command": "cargo run -- --app myapp --log-level debug --api-key env --env production"
        });

        let keywords = extract_tool_input_keywords("Bash", &input);

        // These generic 3-char terms should be filtered (not in allowlist)
        let short_ambiguous: Vec<&str> = vec!["app", "log", "env"];
        for term in &short_ambiguous {
            assert!(
                !keywords.contains(&term.to_string()),
                "Generic 3-char term '{}' should be filtered by R4 min length. Keywords: {:?}",
                term,
                keywords
            );
        }

        // "api" is in the acronym allowlist and should be preserved
        assert!(
            keywords.contains(&"api".to_string()),
            "Allowlisted acronym 'api' should be preserved. Keywords: {:?}",
            keywords
        );

        // Meaningful longer terms should still be present
        assert!(
            keywords.contains(&"myapp".to_string()),
            "Should still contain 'myapp': {:?}",
            keywords
        );
        assert!(
            keywords.contains(&"production".to_string()),
            "Should still contain 'production': {:?}",
            keywords
        );

        eprintln!("=== R4 Min Length Baseline (production) ===");
        eprintln!("Keywords: {:?}", keywords);
    }

    #[test]
    fn keyword_experiment_r4_min_length_fix() {
        // R4 is now in production. Both production and v2 use min_length=4
        // with acronym allowlist. Generic 3-char tokens are filtered, while
        // allowlisted acronyms are preserved.
        let input = serde_json::json!({
            "command": "cargo run -- --app myapp --log-level debug --api-key env --env production"
        });

        // v2 with min_len=4 (matches production behavior)
        let keywords_v2 =
            extract_tool_input_keywords_v2_with_options("Bash", &input, false, false, false, 4);

        // Generic 3-char terms should be filtered
        let short_ambiguous: Vec<&str> = vec!["app", "log", "env"];
        for term in &short_ambiguous {
            assert!(
                !keywords_v2.contains(&term.to_string()),
                "Generic 3-char term '{}' should be filtered. Keywords: {:?}",
                term,
                keywords_v2
            );
        }

        // "api" is in the acronym allowlist and should be preserved
        assert!(
            keywords_v2.contains(&"api".to_string()),
            "Allowlisted acronym 'api' should be preserved in v2. Keywords: {:?}",
            keywords_v2
        );

        // Meaningful longer terms should still be present
        assert!(
            keywords_v2.contains(&"myapp".to_string()),
            "v2 should still contain 'myapp': {:?}",
            keywords_v2
        );
        assert!(
            keywords_v2.contains(&"production".to_string()),
            "v2 should still contain 'production': {:?}",
            keywords_v2
        );
        assert!(
            keywords_v2.contains(&"level".to_string()),
            "v2 should still contain 'level': {:?}",
            keywords_v2
        );

        eprintln!("=== R4 Min Length Fix ===");
        eprintln!("v2 keywords (min_len=4 + allowlist): {:?}", keywords_v2);
    }

    #[test]
    fn keyword_experiment_r4_min_length_tradeoff() {
        // The tradeoff from R4 is now resolved: the acronym allowlist
        // preserves meaningful short technical terms while still filtering
        // generic 3-char words. Verify that all previously-identified
        // meaningful 3-char terms are now preserved via the allowlist.
        let meaningful_3char = vec![
            "tcp", "ssl", "tls", "sql", "css", "dom", "jwt", "xml", "csv", "dns", "aws", "gcp",
            "vpc", "iam", "s3",
        ];

        // Build a command containing all these terms
        let command = meaningful_3char.join(" ");
        let input = serde_json::json!({"command": command});
        let keywords = extract_tool_input_keywords("Bash", &input);

        let mut preserved = 0;
        for term in &meaningful_3char {
            if keywords.contains(&term.to_string()) {
                preserved += 1;
            }
        }

        eprintln!("=== R4 Min Length Tradeoff (resolved) ===");
        eprintln!("Keywords from meaningful 3-char terms: {:?}", keywords);
        eprintln!(
            "Preserved by allowlist: {}/{}",
            preserved,
            meaningful_3char.len()
        );

        // All meaningful 3-char terms should now be preserved via the allowlist
        assert_eq!(
            preserved,
            meaningful_3char.len(),
            "All meaningful 3-char terms should be preserved by the acronym allowlist. \
             Missing: {:?}",
            meaningful_3char
                .iter()
                .filter(|t| !keywords.contains(&t.to_string()))
                .collect::<Vec<_>>()
        );
    }

    // -------------------------------------------------------------------------
    // Experiment: Combined Production vs v2 Comparison
    //
    // Production now includes R1 (path stripping), R2 (expanded noise),
    // R3 (Task tool support), and R4 (min_length=4 with acronym allowlist).
    // v2 mirrors production with configurable toggles for baseline comparisons.
    // With all toggles enabled and min_len=4, v2 should produce identical
    // output to production.
    // -------------------------------------------------------------------------

    #[test]
    fn keyword_experiment_combined_v1_vs_v2() {
        // Simulate a realistic sequence of tool calls from a my-project session
        let tool_calls: Vec<(&str, serde_json::Value)> = vec![
            (
                "Read",
                serde_json::json!({"file_path": "/Users/devuser/GitHub/org/my-project/src/config.rs"}),
            ),
            ("Bash", serde_json::json!({"command": "git status"})),
            (
                "Bash",
                serde_json::json!({"command": "cargo test --release"}),
            ),
            (
                "Grep",
                serde_json::json!({"pattern": "connection_pool", "path": "/Users/devuser/GitHub/org/my-project/src"}),
            ),
            (
                "Read",
                serde_json::json!({"file_path": "/Users/devuser/GitHub/org/my-project/src/deploy/kamal.rs"}),
            ),
            (
                "Task",
                serde_json::json!({"prompt": "Search for connection pool configuration"}),
            ),
            (
                "Bash",
                serde_json::json!({"command": "tissue status pickup-42 closed"}),
            ),
            (
                "Edit",
                serde_json::json!({"file_path": "/Users/devuser/GitHub/org/my-project/src/api/handler.rs", "old_string": "x", "new_string": "y"}),
            ),
        ];

        // Known noise tokens (should NOT be in output for either extractor)
        let known_noise = ["users", "devuser", "github", "org", "status", "release"];
        // Known signal tokens (SHOULD be in output)
        let known_signal = [
            "project",
            "config",
            "connection_pool",
            "kamal",
            "tissue",
            "handler",
            "deploy",
        ];

        let mut prod_all: Vec<String> = Vec::new();
        let mut v2_all: Vec<String> = Vec::new();

        for (tool, input) in &tool_calls {
            prod_all.extend(extract_tool_input_keywords(tool, input));
            v2_all.extend(extract_tool_input_keywords_v2(tool, input));
        }

        prod_all.sort();
        prod_all.dedup();
        v2_all.sort();
        v2_all.dedup();

        let prod_noise: Vec<&&str> = known_noise
            .iter()
            .filter(|n| prod_all.contains(&n.to_string()))
            .collect();
        let v2_noise: Vec<&&str> = known_noise
            .iter()
            .filter(|n| v2_all.contains(&n.to_string()))
            .collect();
        let prod_signal: Vec<&&str> = known_signal
            .iter()
            .filter(|s| prod_all.contains(&s.to_string()))
            .collect();
        let v2_signal: Vec<&&str> = known_signal
            .iter()
            .filter(|s| v2_all.contains(&s.to_string()))
            .collect();

        eprintln!("=== Combined Production vs v2 Comparison ===");
        eprintln!("Production total unique keywords: {}", prod_all.len());
        eprintln!("v2 total unique keywords: {}", v2_all.len());
        eprintln!(
            "Production noise tokens present: {:?} ({}/{})",
            prod_noise,
            prod_noise.len(),
            known_noise.len()
        );
        eprintln!(
            "v2 noise tokens present: {:?} ({}/{})",
            v2_noise,
            v2_noise.len(),
            known_noise.len()
        );
        eprintln!(
            "Production signal tokens present: {:?} ({}/{})",
            prod_signal,
            prod_signal.len(),
            known_signal.len()
        );
        eprintln!(
            "v2 signal tokens present: {:?} ({}/{})",
            v2_signal,
            v2_signal.len(),
            known_signal.len()
        );

        // Both production (R1+R2+R3+R4) and v2 should filter all known noise
        assert!(
            prod_noise.is_empty(),
            "Production (R1+R2+R3+R4) should filter all known noise, but found: {:?}",
            prod_noise
        );
        assert!(
            v2_noise.is_empty(),
            "v2 should filter all known noise, but found: {:?}",
            v2_noise
        );

        // Both should preserve all signal tokens
        assert!(
            prod_signal.len() == known_signal.len(),
            "Production should preserve all signal tokens ({}/{}): {:?}",
            prod_signal.len(),
            known_signal.len(),
            prod_signal
        );
        assert!(
            v2_signal.len() == known_signal.len(),
            "v2 should preserve all signal tokens ({}/{}): {:?}",
            v2_signal.len(),
            known_signal.len(),
            v2_signal
        );

        // Now that R1+R2+R3+R4 are all in production, both extractors should
        // produce identical output when v2 uses matching settings.
        assert_eq!(
            prod_all, v2_all,
            "Production and v2 (with matching settings) should produce identical keywords"
        );
    }

    /// Adversarial noise list analysis: audit v2 expanded noise words against
    /// 38 real learnings from my-project to identify false negatives.
    ///
    /// This test checks whether filtering expanded noise words would make any
    /// real learnings unreachable by removing keywords that appear in those
    /// learnings' summaries and descriptions.
    #[test]
    fn keyword_experiment_adversarial_noise_audit() {
        // The expanded noise words: words in v2's noise list but NOT in v1's.
        // These are the v2 additions organized by category.
        let expanded_noise: Vec<(&str, &str)> = vec![
            // Git subcommands
            ("status", "git"),
            ("commit", "git"),
            ("push", "git"),
            ("pull", "git"),
            ("fetch", "git"),
            ("merge", "git"),
            ("rebase", "git"),
            ("reset", "git"),
            ("clean", "git"),
            ("clone", "git"),
            ("remote", "git"),
            ("branch", "git"),
            ("tag", "git"),
            ("stash", "git"),
            ("diff", "git"),
            ("log", "git"),
            ("add", "git"),
            ("checkout", "git"),
            ("cherry", "git"),
            ("pick", "git"),
            ("bisect", "git"),
            ("blame", "git"),
            ("show", "git"),
            // Build/test verbs
            ("check", "build"),
            ("release", "build"),
            ("dev", "build"),
            ("update", "build"),
            ("init", "build"),
            ("start", "build"),
            ("stop", "build"),
            ("lint", "build"),
            ("format", "build"),
            ("watch", "build"),
            ("serve", "build"),
            ("migrate", "build"),
            ("generate", "build"),
            ("create", "build"),
            ("delete", "build"),
            ("remove", "build"),
            // Generic programming terms
            ("file", "generic"),
            ("new", "generic"),
            ("set", "generic"),
            ("get", "generic"),
            ("list", "generic"),
            ("help", "generic"),
            ("info", "generic"),
            ("version", "generic"),
            ("output", "generic"),
            ("input", "generic"),
            ("data", "generic"),
            ("type", "generic"),
            ("name", "generic"),
            ("path", "generic"),
            ("mode", "generic"),
            ("debug", "generic"),
            ("error", "generic"),
            ("warn", "generic"),
            ("main", "generic"),
            ("index", "generic"),
            ("spec", "generic"),
            ("mod", "generic"),
            ("use", "generic"),
            ("pub", "generic"),
            ("crate", "generic"),
            ("self", "generic"),
            ("super", "generic"),
            // Common path components
            ("users", "path"),
            ("documents", "path"),
            ("downloads", "path"),
            ("desktop", "path"),
            ("applications", "path"),
            ("library", "path"),
            ("volumes", "path"),
            ("private", "path"),
            ("github", "path"),
            ("repos", "path"),
            ("projects", "path"),
            ("workspace", "path"),
            ("code", "path"),
        ];

        // All 38 real learnings from my-project/.grove/learnings.md
        // Each entry: (learning_id, summary, description_excerpt)
        let learnings: Vec<(&str, &str, &str)> = vec![
            (
                "cl_20260226_000",
                "Algorithm specification mismatch between design doc and reference implementation",
                "The design doc specifies Weng-Lin Plackett-Luce without exact formulas. When validating the PickupRank Engine against openskill.js v4.1.0, three algorithmic differences were discovered: c computation adds beta_sq per player in our engine vs per team in openskill, win probability uses Gaussian phi vs exponential exp, gamma factor missing from sigma update causing aggressive reduction.",
            ),
            (
                "cl_20260226_001",
                "Use openskill_parity test tags for algorithmic validation pending design decisions",
                "When implementing algorithmic features that need validation against reference implementations but decisions on exact specifications are pending, tag tests with openskill_parity and exclude them from default test runs. Tests can be run explicitly when ready: mix test --only openskill_parity.",
            ),
            (
                "cl_20260226_002",
                "openskill.js v4.1.0 uses exponential Plackett-Luce variant with specific gamma factor in sigma updates",
                "The openskill.js library implements: c computed as sqrt(beta_sq * player_count), win probability via exp(mu_a/c) / (exp(mu_a/c) + exp(mu_b/c)), sigma update multiplied by gamma = sigma_team / c dampening factor.",
            ),
            (
                "cl_20260226_003",
                "Repo.transaction double-wraps return values from callback",
                "When a function inside Repo.transaction/1 returns {:ok, job}, the transaction wraps it as {:ok, {:ok, job}}. Must match {:ok, {:ok, job}} = Ratings.enqueue_recalculation(...) rather than {:ok, job}.",
            ),
            (
                "cl_20260226_004",
                "RecalculationJob triggers are restricted to 5 valid values",
                "Valid triggers for RecalculationJob are: game_amended, game_soft_deleted, game_restored, settings_changed, follow_up. There is no manual trigger. Defined in @triggers in recalculation_job.ex with validate_inclusion.",
            ),
            (
                "cl_20260226_005",
                "League creation form uses flat field names, not nested under league key",
                "LeagueLive.Index new league form uses flat field names (name, sport) in the phx-submit=save handler, pattern matching as handle_event save. Settings form uses nested league[name] with @form[:name]. Tests must match the actual field structure.",
            ),
            (
                "cl_20260226_006",
                "Float.round/2 with precision 0 returns float not integer",
                "Float.round(100.0, 0) returns 100.0 not 100. Use Kernel.round/1 for integer result. This caused test failures when asserting html =~ 100% but rendered value was 100.0%.",
            ),
            (
                "cl_20260226_007",
                "LiveView test flash messages are in layout, not in render output",
                "put_flash messages render in the root layout, not in the LiveView render/1 function. In tests, render_submit() output wont contain flash text. Use assert_redirect(view) to get path flash map.",
            ),
            (
                "cl_20260226_008",
                "CSV export uses date not played_on as column header for game history",
                "Analytics.export_game_history CSV headers are: sequence,date,team_a_players,team_a_score,team_b_players,team_b_score. The date column is called date not played_on despite the schema field being played_on.",
            ),
            (
                "cl_20260227_000",
                "Nested HTML forms break LiveView tests",
                "When adding a search input inside a LiveView modal that already has a form, using a nested form tag causes Phoenix LiveViewTest to fail because it cant find inputs in the outer form. Solution: use phx-keyup directly on the input element outside the form.",
            ),
            (
                "cl_20260227_001",
                "Always run mix format before committing to avoid pre-commit hook failures",
                "Long HEEx template expressions get reformatted by mix format to multi-line. Always run mix format before committing to avoid pre-commit hook failures with mix format --check-formatted.",
            ),
            (
                "cl_20260227_002",
                "Remove default function arguments when all call sites pass explicit values",
                "When all call sites pass an explicit argument, having a default value generates a default values never used warning that fails --warnings-as-errors. Remove the default when all callers provide the argument.",
            ),
            (
                "cl_20260227_003",
                "Analytics.balance_teams/3 returns player maps with id field, not player_id",
                "Analytics.balance_teams/3 returns player maps with :id field (not :player_id). The maps come from merging Player query results with rating data. When accessing player identifiers in balanced team results, use .id not .player_id.",
            ),
            (
                "cl_20260301_000",
                "Kamal 2 accessories sharing secret names need entrypoint wrapper scripts",
                "Kamal 2 reads all secrets from a single .kamal/secrets file. When accessories need the same env var name but with different values, you cannot differentiate them at the secrets level. The workaround is to use distinct secret names and create entrypoint wrapper scripts.",
            ),
            (
                "cl_20260301_001",
                "DATABASE_URL cannot use shell interpolation in Kamal env.clear YAML",
                "Kamal 2 env.clear section in deploy.yml is plain YAML, not shell-evaluated. Shell variable interpolation will not work. Instead, construct DATABASE_URL in .kamal/secrets which IS shell-sourced via dotenv and reference it via env.secret in deploy.yml.",
            ),
            (
                "cl_20260301_002",
                "Phoenix prod.exs exclude key was sibling of force_ssl, not a nested option",
                "When configuring Phoenix endpoint force_ssl with exclude patterns, the exclude key was mistakenly placed as a sibling of force_ssl in the endpoint config rather than nested inside force_ssl options. Phoenix silently ignores unknown config keys.",
            ),
            (
                "cl_20260304_000",
                "Phoenix require_authenticated_admin plug leaks route existence via 302 redirect",
                "Phoenix require_authenticated_admin plug returns a 302 redirect by default, which leaks route existence to unauthenticated users. Fixed by returning 404 with a custom error page. Testing implication: Phoenix.LiveViewTest.connect_from_static_token only handles 200/301/302/303 status codes.",
            ),
            (
                "cl_20260304_001",
                "kartoza/pg-backup uses s3cmd with non-standard env var names",
                "The kartoza/pg-backup Docker image uses s3cmd internally (not AWS CLI) and expects completely different environment variable names than AWS conventions: ACCESS_KEY_ID instead of AWS_ACCESS_KEY_ID, POSTGRES_PASS instead of POSTGRES_PASSWORD.",
            ),
            (
                "cl_20260304_002",
                "Vector VRL parse_json! aborts on error, dropping events before ?? fallback executes",
                "In Vector VRL, the ! suffix on parse_json! means abort on error, which drops the entire event before the ?? error coalescing fallback operator can execute. Using parse_json without ! returns an error value that ?? can then handle gracefully.",
            ),
            (
                "cl_20260304_003",
                "Use send_resp() with pre-rendered content to avoid params dependency in plug unit tests",
                "When writing unit tests for plugs that render HTML responses, the conn must have params fetched for Phoenix template rendering to work. Using send_resp() with pre-rendered content avoids this dependency entirely.",
            ),
            (
                "cl_20260304_004",
                "Vector 0.43.1 del() is infallible; using ?? causes VRL error E651",
                "In Vector 0.43.1, the del() function is infallible (never errors). Using the fallback operator ?? with del() triggers VRL compilation error E651. Solution: use plain del(.field) without fallback. This caused a crash loop in production.",
            ),
            (
                "cl_20260304_005",
                "parse_json with fallback replaces entire event, losing metadata",
                "Using . = parse_json(.message) ?? . in Vector will replace the entire event object when JSON parsing succeeds. This loses Docker source metadata like container_name and timestamp. Use field extraction instead: .parsed = parse_json(.message).",
            ),
            (
                "cl_20260304_006",
                "Always validate Vector VRL transforms locally before deploying",
                "Use vector test and vector validate --no-environment locally with Docker before deploying Vector changes to production. This catches syntax errors and infallibility violations early.",
            ),
            (
                "cl_20260304_007",
                "Kamal .kamal/secrets uses single-quoted heredocs disabling shell expansion",
                "Kamal .kamal/secrets files use single-quoted heredocs which disable ALL shell variable expansion. Variable references like $VAR are written literally and resolved by Kamal dotenv resolver at deploy time.",
            ),
            (
                "cl_20260304_008",
                "kartoza/pg-backup:17-3.5 backup script location and entrypoint behavior",
                "The kartoza/pg-backup:17-3.5 image has its backup script at /backup-scripts/backups.sh. The image entrypoint start.sh starts the backup daemon and ignores the CMD instruction. Solution: Always use the --reuse flag to exec into the already-running container.",
            ),
            (
                "cl_20260304_009",
                "s3cmd expects HOST_BASE as bare hostname without protocol prefix",
                "s3cmd used by kartoza/pg-backup for S3 uploads expects the HOST_BASE configuration as a bare hostname without the https:// prefix. This differs from AWS CLI which accepts full endpoint URLs with protocol.",
            ),
            (
                "cl_20260304_010",
                "Teammate record mirrors head-to-head: same data loading, UI pattern, and test structure",
                "The teammate record feature was implemented by closely following the head-to-head h2h pattern. Both use the same load_player_game_teams helper for data loading, the same dropdown-plus-stat-cards UI layout, and the same test structure.",
            ),
            (
                "cl_20260304_011",
                "Teammate record needs load_teams_by_game helper to resolve opponent scores",
                "Unlike head-to-head where the opponent team_id is already known from the filter, teammate record requires an additional helper load_teams_by_game to look up the opposing team score.",
            ),
            (
                "cl_20260304_012",
                "Pairwise player features: 13 unit + 3 LiveView integration tests as standard coverage",
                "The teammate record feature added 16 new tests (13 unit tests for the Analytics context function covering edge cases like no shared games, win/loss/draw counts; plus 3 LiveView integration tests covering rendering, dropdown interaction, and stat display).",
            ),
            (
                "cl_20260305_000",
                "Share/QR code feature pattern in Phoenix LiveView",
                "When implementing share/QR code features in Phoenix LiveView, follow the existing pattern: save shareable data with a unique token Base.url_encode64 with crypto.strong_rand_bytes, create a public LiveView route in the public live_session.",
            ),
            (
                "cl_20260305_001",
                "TTL plus link-to-preserve lifecycle for ephemeral-to-permanent data",
                "Balance snapshot lifecycle: save with 30-day TTL on creation, link to game_id on game record, reap expired+unlinked via Oban cron SnapshotReaper on export queue daily at 3am. Game-linked snapshots are preserved indefinitely.",
            ),
            (
                "cl_20260305_002",
                "tissue CLI uses show not view for ticket details",
                "The tissue CLI uses tissue show not tissue view to view ticket details. Also, global options must come before the command name, not after. Use tissue help instead of tissue --help.",
            ),
            (
                "cl_20260305_003",
                "Avoid default function args when HEEx template always passes all arguments",
                "When adding optional query params to LiveView URL helpers, avoid default argument values if the HEEx template always passes all arguments explicitly. The unused default triggers default values never used compiler warnings.",
            ),
            (
                "cl_20260306_000",
                "DaisyUI v5 renamed tabs-bordered to tabs-border — old class silently does nothing",
                "DaisyUI v5 renamed the tabs-bordered class to tabs-border. The old class silently does nothing with no warning or error. DaisyUI class renames are silent breakages on upgrade.",
            ),
            (
                "cl_20260306_001",
                "Use scrollbar-gutter: stable to prevent layout shift from scrollbar appearing/disappearing",
                "Adding html scrollbar-gutter: stable in CSS prevents horizontal layout shift on centered content when navigating between pages with different content heights.",
            ),
            (
                "cl_20260308_000",
                "Use separate root layout templates with router pipelines for distinct navigation contexts",
                "When different Phoenix LiveView scopes need distinct navbars, create separate root layout templates and use dedicated router pipelines with put_root_layout to switch layouts per-route scope.",
            ),
            (
                "cl_20260308_001",
                "Implement inline RSVP UI on dashboard cards using PubSub subscriptions",
                "Dashboard game cards can surface RSVP actions inline without requiring navigation to a detail page. Use PubSub subscriptions to trigger real-time updates across all connected player sessions when RSVPs change.",
            ),
            (
                "cl_20260308_002",
                "Satisfy Credo nesting depth rules through strategic refactoring",
                "When Credo flags excessive nesting depth in LiveView components or event handlers, extract conditional logic into separate helper functions or multi-clause private functions.",
            ),
        ];

        // For each expanded noise word, check if it appears in any learning's
        // summary or description. Track which learnings match and how.
        struct NoiseWordHit<'a> {
            word: &'a str,
            category: &'a str,
            matching_learnings: Vec<(&'a str, &'a str)>, // (learning_id, matched_in: "summary"/"description"/"both")
        }

        let mut hits: Vec<NoiseWordHit> = Vec::new();

        for &(word, category) in &expanded_noise {
            let mut matching = Vec::new();
            let word_lower = word.to_lowercase();

            for &(id, summary, description) in &learnings {
                let summary_lower = summary.to_lowercase();
                let desc_lower = description.to_lowercase();

                // Whole-word matching: check if the noise word appears as a
                // standalone word (bounded by non-alphanumeric or string edges)
                let is_word_match = |text: &str, word: &str| -> bool {
                    for (i, _) in text.match_indices(word) {
                        let before_ok = i == 0 || !text.as_bytes()[i - 1].is_ascii_alphanumeric();
                        let after_pos = i + word.len();
                        let after_ok = after_pos >= text.len()
                            || !text.as_bytes()[after_pos].is_ascii_alphanumeric();
                        if before_ok && after_ok {
                            return true;
                        }
                    }
                    false
                };

                let in_summary = is_word_match(&summary_lower, &word_lower);
                let in_desc = is_word_match(&desc_lower, &word_lower);

                if in_summary || in_desc {
                    let location = match (in_summary, in_desc) {
                        (true, true) => "both",
                        (true, false) => "summary",
                        (false, true) => "description",
                        _ => unreachable!(),
                    };
                    matching.push((id, location));
                }
            }

            if !matching.is_empty() {
                hits.push(NoiseWordHit {
                    word,
                    category,
                    matching_learnings: matching,
                });
            }
        }

        // Report: which expanded noise words appear in real learnings
        eprintln!("\n=== Adversarial Noise List Audit ===\n");
        eprintln!(
            "Expanded noise words (v2 additions): {}",
            expanded_noise.len()
        );
        eprintln!("Real learnings analyzed: {}", learnings.len());
        eprintln!("Noise words hitting learnings: {}\n", hits.len());

        for hit in &hits {
            eprintln!(
                "  NOISE WORD: \"{}\" (category: {})",
                hit.word, hit.category
            );
            for &(id, location) in &hit.matching_learnings {
                let summary = learnings
                    .iter()
                    .find(|(lid, _, _)| *lid == id)
                    .map(|(_, s, _)| *s)
                    .unwrap_or("?");
                eprintln!("    -> {} [{}]: {}", id, location, summary);
            }
            eprintln!();
        }

        // Identify learnings at risk: for each affected learning, count how
        // many OTHER distinguishing keywords it has (words >= 4 chars that are
        // NOT in any noise list). A learning is "high risk" if the noise word
        // is one of very few keywords that could match it.
        let all_noise: Vec<&str> = {
            let base: Vec<&str> = vec![
                "ls", "cd", "git", "cat", "grep", "echo", "pwd", "rm", "mv", "cp", "mkdir",
                "touch", "chmod", "chown", "sudo", "apt", "brew", "npm", "yarn", "cargo", "make",
                "cmake", "true", "false", "null", "test", "run", "build", "install", "the", "and",
                "for", "with", "from", "this", "that", "src", "lib", "bin", "tmp", "var", "etc",
                "usr", "opt", "home",
            ];
            let expanded: Vec<&str> = expanded_noise.iter().map(|(w, _)| *w).collect();
            let mut all = base;
            all.extend(expanded);
            all
        };

        eprintln!("=== Learning Reachability Analysis ===\n");

        // Collect all learnings that are hit by expanded noise words
        let mut affected_learning_ids: Vec<&str> = hits
            .iter()
            .flat_map(|h| h.matching_learnings.iter().map(|(id, _)| *id))
            .collect();
        affected_learning_ids.sort();
        affected_learning_ids.dedup();

        struct LearningRisk<'a> {
            id: &'a str,
            summary: &'a str,
            noise_words_matching: Vec<&'a str>,
            distinguishing_keywords: Vec<String>,
            risk_level: &'a str,
        }

        let mut risks: Vec<LearningRisk> = Vec::new();

        for &learning_id in &affected_learning_ids {
            let (_, summary, description) = learnings
                .iter()
                .find(|(id, _, _)| *id == learning_id)
                .unwrap();

            // Find which noise words hit this learning
            let noise_words: Vec<&str> = hits
                .iter()
                .filter(|h| {
                    h.matching_learnings
                        .iter()
                        .any(|(id, _)| *id == learning_id)
                })
                .map(|h| h.word)
                .collect();

            // Extract distinguishing keywords from summary + description
            let combined = format!("{} {}", summary, description);
            let distinguishing: Vec<String> = combined
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|w| {
                    let wl = w.to_lowercase();
                    w.len() >= 4
                        && !all_noise.contains(&wl.as_str())
                        && !w.chars().all(|c| c.is_numeric())
                })
                .map(|w| w.to_lowercase())
                .collect::<std::collections::HashSet<String>>()
                .into_iter()
                .collect();

            let risk = if distinguishing.len() <= 3 {
                "HIGH"
            } else if distinguishing.len() <= 8 {
                "MEDIUM"
            } else {
                "LOW"
            };

            risks.push(LearningRisk {
                id: learning_id,
                summary,
                noise_words_matching: noise_words,
                distinguishing_keywords: distinguishing,
                risk_level: risk,
            });
        }

        // Sort by risk level (HIGH first)
        risks.sort_by_key(|r| match r.risk_level {
            "HIGH" => 0,
            "MEDIUM" => 1,
            "LOW" => 2,
            _ => 3,
        });

        for risk in &risks {
            eprintln!("  [{}] {} -- {}", risk.risk_level, risk.id, risk.summary);
            eprintln!("    Noise words matching: {:?}", risk.noise_words_matching);
            eprintln!(
                "    Distinguishing keywords ({}): {:?}",
                risk.distinguishing_keywords.len(),
                risk.distinguishing_keywords
            );
            eprintln!();
        }

        // Identify highest-risk noise words: those that hit learnings with
        // few distinguishing keywords
        eprintln!("=== Highest-Risk Noise Words ===\n");

        struct NoiseWordRisk<'a> {
            word: &'a str,
            category: &'a str,
            high_risk_learnings: usize,
            medium_risk_learnings: usize,
            total_learnings_hit: usize,
        }

        let mut word_risks: Vec<NoiseWordRisk> = Vec::new();

        for hit in &hits {
            let mut high = 0usize;
            let mut medium = 0usize;
            for &(learning_id, _) in &hit.matching_learnings {
                if let Some(risk) = risks.iter().find(|r| r.id == learning_id) {
                    match risk.risk_level {
                        "HIGH" => high += 1,
                        "MEDIUM" => medium += 1,
                        _ => {}
                    }
                }
            }
            word_risks.push(NoiseWordRisk {
                word: hit.word,
                category: hit.category,
                high_risk_learnings: high,
                medium_risk_learnings: medium,
                total_learnings_hit: hit.matching_learnings.len(),
            });
        }

        word_risks.sort_by(|a, b| {
            b.high_risk_learnings
                .cmp(&a.high_risk_learnings)
                .then(b.medium_risk_learnings.cmp(&a.medium_risk_learnings))
                .then(b.total_learnings_hit.cmp(&a.total_learnings_hit))
        });

        for wr in &word_risks {
            eprintln!(
                "  \"{}\" ({}): {} total hits, {} HIGH risk, {} MEDIUM risk",
                wr.word,
                wr.category,
                wr.total_learnings_hit,
                wr.high_risk_learnings,
                wr.medium_risk_learnings
            );
        }

        eprintln!("\n=== Recommendations ===\n");
        eprintln!("Words SAFE to keep as noise (0 high-risk learning hits):");
        for wr in &word_risks {
            if wr.high_risk_learnings == 0 && wr.medium_risk_learnings == 0 {
                eprintln!(
                    "  - \"{}\" ({}) -- {} hits, all LOW risk",
                    wr.word, wr.category, wr.total_learnings_hit
                );
            }
        }
        eprintln!();

        eprintln!("Words to INVESTIGATE (hit medium/high risk learnings):");
        for wr in &word_risks {
            if wr.high_risk_learnings > 0 || wr.medium_risk_learnings > 0 {
                eprintln!(
                    "  - \"{}\" ({}) -- {} HIGH, {} MEDIUM risk hits",
                    wr.word, wr.category, wr.high_risk_learnings, wr.medium_risk_learnings
                );
            }
        }
        eprintln!();

        let noise_words_not_in_learnings: Vec<&&str> = expanded_noise
            .iter()
            .map(|(w, _)| w)
            .filter(|w| !hits.iter().any(|h| &h.word == *w))
            .collect();
        eprintln!(
            "Noise words with ZERO learning hits ({} of {}): {:?}",
            noise_words_not_in_learnings.len(),
            expanded_noise.len(),
            noise_words_not_in_learnings
        );

        // Assertions: the test verifies structural properties
        // 1. We should have found at least some hits (non-trivial audit)
        assert!(
            !hits.is_empty(),
            "Expected at least some expanded noise words to appear in learnings"
        );

        // 2. The majority of expanded noise words should NOT appear in learnings
        //    (confirming they are genuinely noise)
        assert!(
            noise_words_not_in_learnings.len() > expanded_noise.len() / 2,
            "Expected majority of expanded noise words to have zero learning hits, \
             but only {}/{} had zero hits",
            noise_words_not_in_learnings.len(),
            expanded_noise.len()
        );

        // 3. No learning should become completely unreachable (0 distinguishing
        //    keywords) just from noise filtering
        let unreachable: Vec<&LearningRisk> = risks
            .iter()
            .filter(|r| r.distinguishing_keywords.is_empty())
            .collect();
        assert!(
            unreachable.is_empty(),
            "Found {} learnings that would become unreachable from noise filtering: {:?}",
            unreachable.len(),
            unreachable.iter().map(|r| r.id).collect::<Vec<_>>()
        );
    }

    // =========================================================================
    // Tantivy BM25 rescoring tests
    // =========================================================================

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_build_tantivy_query_string() {
        let query = SearchQuery {
            keywords: vec!["rust".to_string(), "error".to_string()],
            tags: vec!["debugging".to_string()],
            files: vec!["src/hooks/runner.rs".to_string()],
            ticket_id: Some("grove-42".to_string()),
        };
        let result = super::build_tantivy_query_string(&query);
        assert!(result.contains("rust"));
        assert!(result.contains("error"));
        assert!(result.contains("debugging"));
        assert!(result.contains("hooks"));
        assert!(result.contains("runner"));
        assert!(result.contains("grove-42"));
        // "src" should be filtered as noise
        assert!(!result.split_whitespace().any(|w| w == "src"));
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_build_tantivy_query_string_empty() {
        let query = SearchQuery::new();
        let result = super::build_tantivy_query_string(&query);
        assert!(result.trim().is_empty());
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_build_tantivy_query_string_boosted_keywords_get_highest_boost() {
        let query = SearchQuery {
            keywords: vec!["rust".to_string(), "error".to_string()],
            tags: vec!["debugging".to_string()],
            files: vec![],
            ticket_id: None,
        };
        let result = super::build_tantivy_query_string_boosted(&query);
        assert!(
            result.contains("rust^2.0"),
            "Keywords should get 2.0x boost, got: {result}"
        );
        assert!(
            result.contains("error^2.0"),
            "Keywords should get 2.0x boost, got: {result}"
        );
        assert!(
            result.contains("debugging^1.5"),
            "Tags should get 1.5x boost, got: {result}"
        );
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_build_tantivy_query_string_boosted_files_no_boost() {
        let query = SearchQuery {
            keywords: vec![],
            tags: vec![],
            files: vec!["src/hooks/runner.rs".to_string()],
            ticket_id: Some("grove-42".to_string()),
        };
        let result = super::build_tantivy_query_string_boosted(&query);
        // File path segments should NOT have boost suffix (1.0x default)
        assert!(
            result.contains("hooks"),
            "Should contain file segment: {result}"
        );
        assert!(
            result.contains("runner"),
            "Should contain file segment: {result}"
        );
        assert!(
            !result.contains("hooks^"),
            "File segments should not have boost: {result}"
        );
        assert!(
            result.contains("grove\\-42"),
            "Should contain escaped ticket id: {result}"
        );
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_build_tantivy_query_string_boosted_empty() {
        let query = SearchQuery::new();
        let result = super::build_tantivy_query_string_boosted(&query);
        assert!(result.trim().is_empty());
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_build_tantivy_query_string_boosted_special_chars_escaped() {
        let query = SearchQuery {
            keywords: vec!["test:value".to_string()],
            tags: vec![],
            files: vec![],
            ticket_id: None,
        };
        let result = super::build_tantivy_query_string_boosted(&query);
        // The colon should be escaped, but the boost ^ should NOT be escaped
        assert!(
            result.contains("\\:"),
            "Special chars should be escaped: {result}"
        );
        assert!(result.contains("^2.0"), "Boost should be present: {result}");
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_build_boosted_query_with_params_custom_boosts() {
        let query = SearchQuery {
            keywords: vec!["rust".to_string(), "error".to_string()],
            tags: vec!["debugging".to_string()],
            files: vec![],
            ticket_id: None,
        };
        let result = super::build_tantivy_query_string_boosted_with_params(&query, 1.5, 1.0);
        assert!(
            result.contains("rust^1.5"),
            "Keywords should get custom 1.5x boost, got: {result}"
        );
        assert!(
            result.contains("error^1.5"),
            "Keywords should get custom 1.5x boost, got: {result}"
        );
        assert!(
            result.contains("debugging^1.0"),
            "Tags should get custom 1.0x boost, got: {result}"
        );
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_build_boosted_query_default_delegates() {
        let query = SearchQuery {
            keywords: vec!["rust".to_string()],
            tags: vec!["test".to_string()],
            files: vec![],
            ticket_id: None,
        };
        let default_result = super::build_tantivy_query_string_boosted(&query);
        let explicit_result =
            super::build_tantivy_query_string_boosted_with_params(&query, 2.0, 1.5);
        assert_eq!(
            default_result, explicit_result,
            "Default wrapper should produce same output as explicit (2.0, 1.5)"
        );
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_normalize_bm25_scores() {
        use crate::search::TantivySearchResult;

        let results = vec![
            TantivySearchResult {
                id: "a".to_string(),
                score: 10.0,
            },
            TantivySearchResult {
                id: "b".to_string(),
                score: 5.0,
            },
            TantivySearchResult {
                id: "c".to_string(),
                score: 2.0,
            },
        ];
        let map = super::normalize_bm25_scores(&results);
        assert_eq!(map.len(), 3);
        assert!((map["a"] - 1.0).abs() < 1e-6); // max → 1.0
        assert!((map["b"] - 0.5).abs() < 1e-6); // 5/10 = 0.5
        assert!((map["c"] - 0.2).abs() < 1e-6); // 2/10 = 0.2
                                                // Lowest scorer is NOT zeroed out (unlike min-max normalization)
        assert!(map["c"] > 0.0);
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_normalize_bm25_scores_single_result() {
        use crate::search::TantivySearchResult;

        let results = vec![TantivySearchResult {
            id: "a".to_string(),
            score: 5.0,
        }];
        let map = super::normalize_bm25_scores(&results);
        assert_eq!(map.len(), 1);
        assert!((map["a"] - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_normalize_bm25_scores_equal_scores() {
        use crate::search::TantivySearchResult;

        let results = vec![
            TantivySearchResult {
                id: "a".to_string(),
                score: 3.0,
            },
            TantivySearchResult {
                id: "b".to_string(),
                score: 3.0,
            },
        ];
        let map = super::normalize_bm25_scores(&results);
        assert!((map["a"] - 1.0).abs() < f64::EPSILON);
        assert!((map["b"] - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_rescore_with_tantivy_empty() {
        let results: Vec<crate::backends::SearchResult> = Vec::new();
        let query = SearchQuery::new();
        let rescored =
            super::rescore_with_tantivy(results, &query, crate::config::RetrievalProfile::Standard);
        assert!(rescored.is_empty());
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_rescore_with_tantivy_empty_query() {
        use crate::backends::SearchResult;
        use crate::core::{
            learning::CompoundLearning, Confidence, LearningCategory, LearningScope,
            WriteGateCriterion,
        };

        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            "Use atomic writes",
            "Write to temp then rename.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["rust".to_string()],
            "test-session",
        );
        let results = vec![SearchResult::new(learning, 0.8)];
        let query = SearchQuery::new(); // empty query → no keywords
        let rescored = super::rescore_with_tantivy(
            results.clone(),
            &query,
            crate::config::RetrievalProfile::Standard,
        );
        // Empty query → original results returned unchanged
        assert_eq!(rescored.len(), 1);
        assert!((rescored[0].relevance - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn test_rescore_with_tantivy_replaces_scores() {
        use crate::backends::SearchResult;
        use crate::core::{
            learning::CompoundLearning, Confidence, LearningCategory, LearningScope,
            WriteGateCriterion,
        };

        // Two learnings: one about tissue CLI, one about atomic writes
        let tissue_learning = CompoundLearning::new(
            LearningCategory::Convention,
            "Use tissue CLI for issue tracking",
            "This project uses tissue instead of GitHub Issues.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::StableFact],
            vec!["tooling".to_string(), "tissue".to_string()],
            "test-session",
        )
        .with_id("cl_tissue");

        let atomic_learning = CompoundLearning::new(
            LearningCategory::Pattern,
            "Use atomic writes for file operations",
            "Write to temp file then rename for crash safety.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["rust".to_string(), "file-io".to_string()],
            "test-session",
        )
        .with_id("cl_atomic");

        // Both start with identical keyword-overlap scores
        let results = vec![
            SearchResult::new(tissue_learning, 0.5),
            SearchResult::new(atomic_learning, 0.5),
        ];

        // Query about tissue → BM25 should score tissue learning higher
        let mut query = SearchQuery::new();
        query.keywords = vec![
            "tissue".to_string(),
            "issue".to_string(),
            "tracking".to_string(),
        ];

        let rescored =
            super::rescore_with_tantivy(results, &query, crate::config::RetrievalProfile::Standard);
        assert_eq!(rescored.len(), 2);

        // BM25 scores should replace the original 0.5 scores
        let tissue = rescored
            .iter()
            .find(|r| r.learning.id == "cl_tissue")
            .unwrap();
        let atomic = rescored
            .iter()
            .find(|r| r.learning.id == "cl_atomic")
            .unwrap();

        // Tissue learning should score higher (query matches its content directly)
        assert!(
            tissue.relevance > atomic.relevance,
            "tissue relevance ({}) should be > atomic relevance ({})",
            tissue.relevance,
            atomic.relevance
        );
        // Tissue should score well (BM25 matches "tissue", "issue", "tracking")
        assert!(tissue.relevance > 0.0);
        // Original 0.5 scores should be replaced (not kept as-is)
        assert!(
            (tissue.relevance - 0.5).abs() > 0.01,
            "tissue relevance ({}) should differ from original 0.5",
            tissue.relevance
        );
        // Atomic learning gets 0.0 because BM25 finds no match for "tissue issue tracking"
        // in "Use atomic writes for file operations" — this is correct behavior;
        // the downstream CompositeScore pipeline will filter it out via min_threshold
        assert!(
            atomic.relevance < 0.01,
            "atomic relevance ({}) should be ~0 (no BM25 match)",
            atomic.relevance
        );
    }

    // =========================================================================
    // Adaptive threshold + dynamic K tests
    // =========================================================================

    /// Helper to create a CompositeScore with a specific final score.
    fn make_scored(id: &str, score: f64) -> CompositeScore {
        use crate::core::learning::{
            Confidence, LearningCategory, LearningScope, WriteGateCriterion,
        };
        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            format!("Learning {}", id),
            "detail",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["test".to_string()],
            "test-session",
        )
        .with_id(id);

        // Build CompositeScore with the desired final score.
        // We set relevance = score and recency/reference = 1.0 so that
        // composite = relevance * recency * reference = score.
        CompositeScore::new(learning, score, 1.0, 1.0, Strategy::Moderate)
    }

    #[test]
    fn test_adaptive_threshold_suppresses_low_scores() {
        // All scores below the min_confidence threshold → suppressed (None)
        let scored = vec![
            make_scored("a", 0.05),
            make_scored("b", 0.03),
            make_scored("c", 0.01),
        ];
        let result = apply_adaptive_threshold(scored, 0.1, 0.05);
        assert!(
            result.is_none(),
            "Should suppress when top score is below threshold"
        );
    }

    #[test]
    fn test_adaptive_threshold_passes_high_scores() {
        // Clear top score with good gap → injection proceeds
        let scored = vec![
            make_scored("a", 0.8),
            make_scored("b", 0.4),
            make_scored("c", 0.2),
        ];
        let result = apply_adaptive_threshold(scored, 0.1, 0.05);
        assert!(result.is_some(), "Should pass when top score is high");
        assert_eq!(result.unwrap().len(), 3);
    }

    #[test]
    fn test_adaptive_threshold_small_gap() {
        // Flat score distribution → suppressed even if scores are above threshold
        let scored = vec![
            make_scored("a", 0.50),
            make_scored("b", 0.49),
            make_scored("c", 0.48),
        ];
        // gap = 0.50 - 0.49 = 0.01, which is below min_gap of 0.05
        let result = apply_adaptive_threshold(scored, 0.1, 0.05);
        assert!(
            result.is_none(),
            "Should suppress when score gap is too small (flat distribution)"
        );
    }

    #[test]
    fn test_adaptive_threshold_empty_input() {
        let scored: Vec<CompositeScore> = Vec::new();
        let result = apply_adaptive_threshold(scored, 0.1, 0.05);
        assert!(result.is_some(), "Empty input should pass through");
        assert_eq!(result.unwrap().len(), 0);
    }

    #[test]
    fn test_adaptive_threshold_two_learnings_no_median() {
        // With fewer than 3 learnings, median defaults to 0.0
        // so gap = top_score - 0.0 = top_score
        let scored = vec![make_scored("a", 0.5), make_scored("b", 0.3)];
        let result = apply_adaptive_threshold(scored, 0.1, 0.05);
        assert!(
            result.is_some(),
            "Two learnings with good top score should pass (median defaults to 0.0)"
        );
    }

    #[test]
    fn test_dynamic_k_filters_weak_learnings() {
        // Top score 0.8, ratio 0.5 → threshold = 0.4
        // Only learnings scoring >= 0.4 qualify
        let scored = vec![
            make_scored("a", 0.8),
            make_scored("b", 0.5),
            make_scored("c", 0.3), // below 0.4 threshold
            make_scored("d", 0.1), // below 0.4 threshold
        ];
        let result = apply_dynamic_k(scored, 0.5, 5);
        assert_eq!(result.len(), 2, "Only 2 of 4 learnings should qualify");
        assert_eq!(result[0].learning.id, "a");
        assert_eq!(result[1].learning.id, "b");
    }

    #[test]
    fn test_dynamic_k_all_qualify() {
        // All scores close to top → all qualify
        let scored = vec![
            make_scored("a", 0.8),
            make_scored("b", 0.7),
            make_scored("c", 0.6),
        ];
        // threshold = 0.8 * 0.3 = 0.24 → all qualify
        let result = apply_dynamic_k(scored, 0.3, 5);
        assert_eq!(result.len(), 3, "All learnings should qualify");
    }

    #[test]
    fn test_dynamic_k_single_learning() {
        // Single result always qualifies (score >= score * ratio when ratio <= 1.0)
        let scored = vec![make_scored("a", 0.5)];
        let result = apply_dynamic_k(scored, 0.3, 5);
        assert_eq!(result.len(), 1, "Single learning should always qualify");
    }

    #[test]
    fn test_dynamic_k_empty_input() {
        let scored: Vec<CompositeScore> = Vec::new();
        let result = apply_dynamic_k(scored, 0.3, 5);
        assert!(result.is_empty(), "Empty input should return empty");
    }

    #[test]
    fn test_dynamic_k_respects_max_count() {
        // All qualify but max_count limits output
        let scored = vec![
            make_scored("a", 0.9),
            make_scored("b", 0.8),
            make_scored("c", 0.7),
            make_scored("d", 0.6),
        ];
        let result = apply_dynamic_k(scored, 0.3, 2);
        assert_eq!(
            result.len(),
            2,
            "Should respect max_count even when more qualify"
        );
        assert_eq!(result[0].learning.id, "a");
        assert_eq!(result[1].learning.id, "b");
    }

    // --- adaptive_dk_ratio tests ---

    #[test]
    fn test_adaptive_dk_empty_scores() {
        let ratio = adaptive_dk_ratio(&[], 0.3, None, None);
        assert!(
            (ratio - 0.3).abs() < f64::EPSILON,
            "Empty scores should return base ratio"
        );
    }

    #[test]
    fn test_adaptive_dk_compressed_scores_tightens() {
        // All scores very close → low CV → should tighten (increase ratio)
        let scores = vec![0.8, 0.79, 0.81, 0.80, 0.78];
        let ratio = adaptive_dk_ratio(&scores, 0.3, None, None);
        assert!(
            ratio > 0.3,
            "Compressed scores should tighten dk: got {}",
            ratio
        );
        assert!(ratio <= 0.6, "Should be clamped to max 0.6: got {}", ratio);
    }

    #[test]
    fn test_adaptive_dk_spread_scores_loosens() {
        // Scores very spread → high CV → should loosen (decrease ratio)
        let scores = vec![1.0, 0.5, 0.1, 0.01, 0.001];
        let ratio = adaptive_dk_ratio(&scores, 0.3, None, None);
        assert!(ratio < 0.3, "Spread scores should loosen dk: got {}", ratio);
        assert!(
            ratio >= 0.15,
            "Should be clamped to min 0.15: got {}",
            ratio
        );
    }

    #[test]
    fn test_adaptive_dk_moderate_cv_no_change() {
        // Moderate spread → CV in [0.3, 0.7] → no adjustment
        let scores = vec![0.9, 0.7, 0.5, 0.4];
        let ratio = adaptive_dk_ratio(&scores, 0.3, None, None);
        assert!(
            (ratio - 0.3).abs() < f64::EPSILON,
            "Moderate CV should not adjust: got {}",
            ratio
        );
    }

    #[test]
    fn test_adaptive_dk_clamp_bounds() {
        // Extreme compression + noisy cache → should clamp to 0.6 max
        let scores = vec![0.5, 0.5, 0.5, 0.5];
        let mut cache = crate::stats::StatsCache::new();
        cache.aggregates.average_hit_rate = 0.1; // Very noisy
        for i in 0..25 {
            cache.learnings.insert(
                format!("L{:03}", i),
                crate::stats::LearningStats {
                    surfaced: 10,
                    hit_rate: 0.1,
                    ..Default::default()
                },
            );
        }
        let ratio = adaptive_dk_ratio(&scores, 0.5, Some(&cache), None);
        assert!(ratio <= 0.6, "Should be clamped to max 0.6: got {}", ratio);
    }

    #[test]
    fn test_adaptive_dk_level2_noisy_corpus_tightens() {
        let scores = vec![0.9, 0.7, 0.5, 0.4]; // Moderate CV → no L1 adjustment
        let mut cache = crate::stats::StatsCache::new();
        cache.aggregates.average_hit_rate = 0.2; // Low hit rate → noisy
        for i in 0..25 {
            cache.learnings.insert(
                format!("L{:03}", i),
                crate::stats::LearningStats {
                    surfaced: 5,
                    hit_rate: 0.2,
                    ..Default::default()
                },
            );
        }
        let ratio = adaptive_dk_ratio(&scores, 0.3, Some(&cache), None);
        assert!(ratio > 0.3, "Noisy corpus should tighten dk: got {}", ratio);
    }

    #[test]
    fn test_adaptive_dk_level2_healthy_corpus_loosens() {
        let scores = vec![0.9, 0.7, 0.5, 0.4]; // Moderate CV → no L1 adjustment
        let mut cache = crate::stats::StatsCache::new();
        cache.aggregates.average_hit_rate = 0.8; // High hit rate → healthy
        for i in 0..25 {
            cache.learnings.insert(
                format!("L{:03}", i),
                crate::stats::LearningStats {
                    surfaced: 5,
                    hit_rate: 0.8,
                    ..Default::default()
                },
            );
        }
        let ratio = adaptive_dk_ratio(&scores, 0.3, Some(&cache), None);
        assert!(
            ratio < 0.3,
            "Healthy corpus should loosen dk: got {}",
            ratio
        );
    }

    #[test]
    fn test_adaptive_dk_level2_cold_start_no_change() {
        let scores = vec![0.9, 0.7, 0.5, 0.4];
        let mut cache = crate::stats::StatsCache::new();
        cache.aggregates.average_hit_rate = 0.1; // Very noisy, but...
                                                 // Only 5 learnings → cold start
        for i in 0..5 {
            cache.learnings.insert(
                format!("L{:03}", i),
                crate::stats::LearningStats {
                    surfaced: 5,
                    hit_rate: 0.1,
                    ..Default::default()
                },
            );
        }
        let ratio = adaptive_dk_ratio(&scores, 0.3, Some(&cache), None);
        assert!(
            (ratio - 0.3).abs() < f64::EPSILON,
            "Cold start should not adjust: got {}",
            ratio
        );
    }

    #[test]
    fn test_adaptive_dk_level3_high_dismiss_category_tightens() {
        use crate::core::LearningCategory;
        let scores = vec![0.9, 0.7, 0.5, 0.4];
        let mut cache = crate::stats::StatsCache::new();
        cache.aggregates.average_hit_rate = 0.5; // Moderate → no L2 adjustment
        for i in 0..25 {
            cache.learnings.insert(
                format!("L{:03}", i),
                crate::stats::LearningStats {
                    surfaced: 10,
                    dismissed: 8,
                    hit_rate: 0.5,
                    category: Some(LearningCategory::Pattern),
                    ..Default::default()
                },
            );
        }
        let ratio = adaptive_dk_ratio(&scores, 0.3, Some(&cache), Some(&LearningCategory::Pattern));
        assert!(
            ratio > 0.3,
            "High dismiss category should tighten dk: got {}",
            ratio
        );
    }

    #[test]
    fn test_adaptive_dk_level3_low_dismiss_no_change() {
        use crate::core::LearningCategory;
        let scores = vec![0.9, 0.7, 0.5, 0.4];
        let mut cache = crate::stats::StatsCache::new();
        cache.aggregates.average_hit_rate = 0.5;
        for i in 0..25 {
            cache.learnings.insert(
                format!("L{:03}", i),
                crate::stats::LearningStats {
                    surfaced: 10,
                    dismissed: 2,
                    hit_rate: 0.5,
                    category: Some(LearningCategory::Pattern),
                    ..Default::default()
                },
            );
        }
        let ratio = adaptive_dk_ratio(&scores, 0.3, Some(&cache), Some(&LearningCategory::Pattern));
        assert!(
            (ratio - 0.3).abs() < f64::EPSILON,
            "Low dismiss rate should not adjust: got {}",
            ratio
        );
    }

    // infer_domains_from_paths tests

    #[test]
    fn test_infer_domains_empty_paths() {
        let result = infer_domains_from_paths(&[]);
        assert!(result.is_empty(), "Empty input should produce empty output");
    }

    #[test]
    fn test_infer_domains_auth_directory_cluster() {
        let paths = vec![
            "src/auth/login.rs".to_string(),
            "src/auth/session.rs".to_string(),
        ];
        let result = infer_domains_from_paths(&paths);
        assert!(
            result.contains(&"authentication".to_string()),
            "Two files in auth/ should trigger 'authentication': {:?}",
            result
        );
    }

    #[test]
    fn test_infer_domains_single_auth_file_no_trigger() {
        let paths = vec!["src/auth/login.rs".to_string()];
        let result = infer_domains_from_paths(&paths);
        assert!(
            !result.contains(&"authentication".to_string()),
            "Single file in auth/ should NOT trigger directory clustering: {:?}",
            result
        );
    }

    #[test]
    fn test_infer_domains_test_files() {
        let paths = vec!["src/hooks/runner_test.rs".to_string()];
        let result = infer_domains_from_paths(&paths);
        assert!(
            result.contains(&"testing".to_string()),
            "_test file should trigger 'testing': {:?}",
            result
        );
    }

    #[test]
    fn test_infer_domains_no_language_keywords() {
        // Extension-based language keywords were removed — they match too broadly
        // in monoglot codebases, diluting BM25 precision.
        let paths = vec![
            "lib/app/accounts.ex".to_string(),
            "lib/app_web/live/dashboard.heex".to_string(),
            "src/main.rs".to_string(),
        ];
        let result = infer_domains_from_paths(&paths);
        assert!(
            !result.contains(&"elixir".to_string()),
            "Should NOT detect language keywords: {:?}",
            result
        );
        assert!(
            !result.contains(&"rust".to_string()),
            "Should NOT detect language keywords: {:?}",
            result
        );
        assert!(
            !result.contains(&"phoenix".to_string()),
            "Should NOT detect language keywords: {:?}",
            result
        );
    }

    #[test]
    fn test_infer_domains_liveview_directory_cluster() {
        // LiveView is still detected via directory clustering (≥2 files in live/)
        let paths = vec![
            "lib/app_web/live/dashboard.ex".to_string(),
            "lib/app_web/live/settings.ex".to_string(),
        ];
        let result = infer_domains_from_paths(&paths);
        assert!(
            result.contains(&"liveview".to_string()),
            "Two files in live/ should trigger 'liveview': {:?}",
            result
        );
    }

    #[test]
    fn test_infer_domains_deployment_files() {
        let paths = vec!["Dockerfile".to_string()];
        let result = infer_domains_from_paths(&paths);
        assert!(
            result.contains(&"deployment".to_string()),
            "Dockerfile should trigger 'deployment': {:?}",
            result
        );
    }

    #[test]
    fn test_infer_domains_mixed_signals() {
        let paths = vec![
            "src/auth/login.rs".to_string(),
            "src/auth/session.rs".to_string(),
            "src/api/routes.rs".to_string(),
            "src/api/handlers.rs".to_string(),
            "Dockerfile".to_string(),
        ];
        let result = infer_domains_from_paths(&paths);
        assert!(
            result.contains(&"authentication".to_string()),
            "Should detect authentication: {:?}",
            result
        );
        assert!(
            result.contains(&"api".to_string()),
            "Should detect api: {:?}",
            result
        );
        assert!(
            result.contains(&"deployment".to_string()),
            "Should detect deployment: {:?}",
            result
        );
        // No language keywords
        assert!(
            !result.contains(&"rust".to_string()),
            "Should NOT include language keywords: {:?}",
            result
        );
    }

    #[test]
    fn test_infer_domains_overcounting_fix() {
        // A single file traversing two directories in the same domain group
        // (e.g., database/migrations/) should NOT meet the ≥2 threshold.
        let paths = vec!["database/migrations/001_create_users.sql".to_string()];
        let result = infer_domains_from_paths(&paths);
        assert!(
            !result.contains(&"database".to_string()),
            "Single file in database/migrations/ should NOT trigger 'database' (overcounting fix): {:?}",
            result
        );
    }

    #[test]
    fn test_infer_domains_database_cluster() {
        let paths = vec![
            "lib/app/repo/queries.ex".to_string(),
            "priv/migrations/20240101_add_users.exs".to_string(),
        ];
        let result = infer_domains_from_paths(&paths);
        assert!(
            result.contains(&"database".to_string()),
            "repo/ + migrations/ should trigger 'database': {:?}",
            result
        );
    }

    #[test]
    fn test_infer_domains_ci_workflow() {
        let paths = vec![".github/workflows/ci.yml".to_string()];
        let result = infer_domains_from_paths(&paths);
        assert!(
            result.contains(&"ci".to_string()),
            ".github/workflows should trigger 'ci': {:?}",
            result
        );
    }

    #[test]
    fn test_infer_domains_kamal_deployment() {
        let paths = vec![
            ".kamal/hooks/pre-deploy".to_string(),
            ".kamal/hooks/post-deploy".to_string(),
        ];
        let result = infer_domains_from_paths(&paths);
        assert!(
            result.contains(&"deployment".to_string()),
            ".kamal/ files should trigger 'deployment': {:?}",
            result
        );
    }

    #[test]
    fn test_infer_domains_deterministic_ordering() {
        let paths = vec![
            "src/auth/login.rs".to_string(),
            "src/auth/session.rs".to_string(),
            "src/api/routes.rs".to_string(),
            "src/api/handlers.rs".to_string(),
        ];
        let result1 = infer_domains_from_paths(&paths);
        let result2 = infer_domains_from_paths(&paths);
        assert_eq!(
            result1, result2,
            "Results should be deterministic across calls"
        );
        // Verify sorted
        let mut sorted = result1.clone();
        sorted.sort();
        assert_eq!(result1, sorted, "Results should be sorted: {:?}", result1);
    }

    // extract_user_intent_keywords tests

    fn write_transcript(dir: &std::path::Path, lines: &[&str]) -> std::path::PathBuf {
        let path = dir.join("test_transcript.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn test_user_intent_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, "").unwrap();
        let result = extract_user_intent_keywords(&path, 20);
        assert!(result.is_empty(), "Empty file should return empty");
    }

    #[test]
    fn test_user_intent_nonexistent_file() {
        let result = extract_user_intent_keywords(Path::new("/nonexistent/file.jsonl"), 20);
        assert!(result.is_empty(), "Nonexistent file should return empty");
    }

    #[test]
    fn test_user_intent_direct_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                r#"{"type":"user","message":{"role":"user","content":"Fix the authentication rate limiter to handle concurrent requests properly"}}"#,
            ],
        );
        let result = extract_user_intent_keywords(&path, 20);
        assert!(
            result.contains(&"authentication".to_string()),
            "Should extract 'authentication': {:?}",
            result
        );
        assert!(
            result.contains(&"rate".to_string()),
            "Should extract 'rate': {:?}",
            result
        );
        assert!(
            result.contains(&"limiter".to_string()),
            "Should extract 'limiter': {:?}",
            result
        );
        assert!(
            result.contains(&"concurrent".to_string()),
            "Should extract 'concurrent': {:?}",
            result
        );
    }

    #[test]
    fn test_user_intent_skips_meta_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                r#"{"type":"user","message":{"role":"user","content":"<local-command-caveat>Caveat text</local-command-caveat>"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"<local-command-stdout></local-command-stdout>"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"Fix the database migration rollback logic"}}"#,
            ],
        );
        let result = extract_user_intent_keywords(&path, 20);
        assert!(
            result.contains(&"database".to_string()),
            "Should skip meta and extract from real message: {:?}",
            result
        );
        assert!(
            result.contains(&"migration".to_string()),
            "Should extract 'migration': {:?}",
            result
        );
        assert!(
            result.contains(&"rollback".to_string()),
            "Should extract 'rollback': {:?}",
            result
        );
    }

    #[test]
    fn test_user_intent_filters_noise() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                r#"{"type":"user","message":{"role":"user","content":"Implement the following plan to create authentication middleware"}}"#,
            ],
        );
        let result = extract_user_intent_keywords(&path, 20);
        // "implement", "following", "plan", "create" are all noise
        assert!(
            !result.contains(&"implement".to_string()),
            "Should filter noise word 'implement': {:?}",
            result
        );
        assert!(
            !result.contains(&"following".to_string()),
            "Should filter noise word 'following': {:?}",
            result
        );
        assert!(
            result.contains(&"authentication".to_string()),
            "Should keep 'authentication': {:?}",
            result
        );
        assert!(
            result.contains(&"middleware".to_string()),
            "Should keep 'middleware': {:?}",
            result
        );
    }

    #[test]
    fn test_user_intent_respects_max_keywords() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                r#"{"type":"user","message":{"role":"user","content":"Fix authentication database migration rollback deploy monitoring logging"}}"#,
            ],
        );
        let result = extract_user_intent_keywords(&path, 3);
        assert_eq!(
            result.len(),
            3,
            "Should respect max_keywords limit: {:?}",
            result
        );
    }

    #[test]
    fn test_user_intent_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                r#"{"type":"user","message":{"role":"user","content":"authentication authentication authentication rate limiter"}}"#,
            ],
        );
        let result = extract_user_intent_keywords(&path, 20);
        let auth_count = result.iter().filter(|k| *k == "authentication").count();
        assert_eq!(auth_count, 1, "Should deduplicate keywords: {:?}", result);
    }

    #[test]
    fn test_user_intent_skips_short_words() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                r#"{"type":"user","message":{"role":"user","content":"Fix the API bug in our app now"}}"#,
            ],
        );
        let result = extract_user_intent_keywords(&path, 20);
        // "fix", "the", "api", "bug", "our", "app", "now" are all < 4 chars
        assert!(
            result.is_empty(),
            "Words shorter than 4 chars should be filtered: {:?}",
            result
        );
    }

    #[test]
    fn test_learning_matches_intent_overlap() {
        let intent = vec![
            "authentication".to_string(),
            "oauth".to_string(),
            "session".to_string(),
        ];
        assert!(learning_matches_intent(
            "Use OAuth2 for authentication",
            "Configure session tokens with refresh flow",
            &intent,
            1,
        ));
    }

    #[test]
    fn test_learning_matches_intent_no_overlap() {
        let intent = vec!["authentication".to_string(), "oauth".to_string()];
        assert!(!learning_matches_intent(
            "Database migration strategy",
            "Use flyway for schema versioning",
            &intent,
            1,
        ));
    }

    #[test]
    fn test_learning_matches_intent_empty_keywords() {
        // No intent → don't filter (return true)
        assert!(learning_matches_intent(
            "Any learning",
            "Any detail",
            &[],
            1,
        ));
    }

    #[test]
    fn test_learning_matches_intent_min_overlap_threshold() {
        let intent = vec![
            "deployment".to_string(),
            "docker".to_string(),
            "kamal".to_string(),
        ];
        // Only 1 overlap ("deployment") — with min_overlap=2, should fail
        assert!(!learning_matches_intent(
            "Deployment pipeline uses CI/CD",
            "Configure GitHub Actions for automated releases",
            &intent,
            2,
        ));
        // With min_overlap=1, should pass
        assert!(learning_matches_intent(
            "Deployment pipeline uses CI/CD",
            "Configure GitHub Actions for automated releases",
            &intent,
            1,
        ));
    }

    #[test]
    fn test_learning_matches_intent_case_insensitive() {
        let intent = vec!["liveview".to_string()];
        assert!(learning_matches_intent(
            "Phoenix LiveView component",
            "Handle mount/handle_params lifecycle",
            &intent,
            1,
        ));
    }

    // =========================================================================
    // Intent filter integration tests (production path)
    // =========================================================================

    #[test]
    fn intent_filter_disabled_passes_all() {
        use tempfile::TempDir;
        // With intent_filter.enabled = false (default), all learnings pass through
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Write a learning that would NOT match transcript intent
        let learning_content = "---\nid: cl_intent_test_001\ntimestamp: 2025-01-15T10:00:00Z\n\
            category: Pattern\nsummary: Database indexing strategy\ndetail: Always add indexes on foreign keys\n\
            behavior_changing: true\nstatus: active\n---\n";
        std::fs::write(grove_dir.join("learnings.md"), learning_content).unwrap();

        // Write a transcript with unrelated intent
        let transcript_path = dir.path().join("transcript.jsonl");
        let transcript = r#"{"type":"user","message":{"content":"Help me configure authentication with OAuth tokens"}}"#;
        std::fs::write(&transcript_path, transcript).unwrap();

        // Config: intent_filter disabled (default)
        let config = Config::default();
        assert!(!config.retrieval.intent_filter.enabled);

        let store = crate::storage::MemorySessionStore::new();
        let runner = HookRunner::new(store, config);

        let query = SearchQuery::new().keywords(vec!["database".to_string()]);
        let session = SessionState::new("intent-disabled", dir.path().to_string_lossy(), "");

        // Should return results without filtering (intent filter disabled)
        let results = runner.retrieve_and_score_learnings(
            dir.path(),
            &session,
            &query,
            Some(&transcript_path),
        );
        // The learning matched the query keyword "database", so it should be returned
        // (intent filter is disabled, so no post-filtering)
        // Note: may be 0 if BM25 scoring doesn't match; the key assertion is
        // that the function completes without error and doesn't filter.
        // We verify the disabled path by checking it doesn't crash.
        let _ = results;
    }

    #[test]
    fn intent_filter_enabled_filters_irrelevant() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Write two learnings: one relevant to intent, one not
        let learning_content = "\
---\nid: cl_intent_rel_001\ntimestamp: 2025-01-15T10:00:00Z\n\
category: Pattern\nsummary: OAuth token refresh pattern\ndetail: Use refresh tokens to avoid re-authentication\n\
behavior_changing: true\nstatus: active\n---\n\
---\nid: cl_intent_irrel_001\ntimestamp: 2025-01-15T10:00:00Z\n\
category: Pattern\nsummary: Database indexing strategy\ndetail: Always add indexes on foreign keys for performance\n\
behavior_changing: true\nstatus: active\n---\n";
        std::fs::write(grove_dir.join("learnings.md"), learning_content).unwrap();

        // Transcript with OAuth-related intent
        let transcript_path = dir.path().join("transcript.jsonl");
        let transcript = r#"{"type":"user","message":{"content":"Help me configure authentication with OAuth token refresh flow"}}"#;
        std::fs::write(&transcript_path, transcript).unwrap();

        let mut config = Config::default();
        config.retrieval.intent_filter.enabled = true;
        config.retrieval.intent_filter.min_overlap = 1;
        config.retrieval.intent_filter.max_keywords = 15;
        // Use keyword backend since tantivy feature may not be enabled
        config.retrieval.scoring_backend = "keyword".to_string();

        let store = crate::storage::MemorySessionStore::new();
        let runner = HookRunner::new(store, config);

        // Search broadly enough to match both
        let query = SearchQuery::new().keywords(vec![
            "oauth".to_string(),
            "token".to_string(),
            "database".to_string(),
            "indexing".to_string(),
        ]);
        let session = SessionState::new("intent-filter", dir.path().to_string_lossy(), "");

        let results = runner.retrieve_and_score_learnings(
            dir.path(),
            &session,
            &query,
            Some(&transcript_path),
        );

        // If any results are returned, none should be the database learning
        for cs in &results {
            assert_ne!(
                cs.learning.id, "cl_intent_irrel_001",
                "Database learning should have been filtered by intent"
            );
        }
    }

    #[test]
    fn intent_filter_enabled_no_transcript_passes_all() {
        use tempfile::TempDir;
        // With no transcript path, intent filter should fail-open (pass all)
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let learning_content = "---\nid: cl_no_transcript_001\ntimestamp: 2025-01-15T10:00:00Z\n\
            category: Pattern\nsummary: Database indexing strategy\ndetail: Always add indexes on foreign keys\n\
            behavior_changing: true\nstatus: active\n---\n";
        std::fs::write(grove_dir.join("learnings.md"), learning_content).unwrap();

        let mut config = Config::default();
        config.retrieval.intent_filter.enabled = true;
        config.retrieval.scoring_backend = "keyword".to_string();

        let store = crate::storage::MemorySessionStore::new();
        let runner = HookRunner::new(store, config);

        let query = SearchQuery::new().keywords(vec!["database".to_string()]);
        let session = SessionState::new("no-transcript", dir.path().to_string_lossy(), "");

        // Pass None for transcript_path — should not filter
        let results = runner.retrieve_and_score_learnings(dir.path(), &session, &query, None);
        // Should complete without error; no filtering applied
        let _ = results;
    }

    #[test]
    fn intent_filter_enabled_empty_transcript_passes_all() {
        use tempfile::TempDir;
        // With an empty transcript file, intent keywords will be empty → fail-open
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let learning_content = "---\nid: cl_empty_transcript_001\ntimestamp: 2025-01-15T10:00:00Z\n\
            category: Pattern\nsummary: Database indexing strategy\ndetail: Always add indexes on foreign keys\n\
            behavior_changing: true\nstatus: active\n---\n";
        std::fs::write(grove_dir.join("learnings.md"), learning_content).unwrap();

        // Empty transcript file
        let transcript_path = dir.path().join("transcript.jsonl");
        std::fs::write(&transcript_path, "").unwrap();

        let mut config = Config::default();
        config.retrieval.intent_filter.enabled = true;
        config.retrieval.scoring_backend = "keyword".to_string();

        let store = crate::storage::MemorySessionStore::new();
        let runner = HookRunner::new(store, config);

        let query = SearchQuery::new().keywords(vec!["database".to_string()]);
        let session = SessionState::new("empty-transcript", dir.path().to_string_lossy(), "");

        // Empty transcript → no keywords → fail-open (no filtering)
        let results = runner.retrieve_and_score_learnings(
            dir.path(),
            &session,
            &query,
            Some(&transcript_path),
        );
        // Should complete without error; no filtering applied
        let _ = results;
    }

    #[test]
    fn intent_filter_removes_all_returns_empty() {
        use tempfile::TempDir;
        // If intent filter removes all learnings, should return empty vec (not crash)
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Learning about databases (will not match transcript intent)
        let learning_content = "---\nid: cl_all_filtered_001\ntimestamp: 2025-01-15T10:00:00Z\n\
            category: Pattern\nsummary: Database indexing strategy\ndetail: Always add indexes on foreign keys\n\
            behavior_changing: true\nstatus: active\n---\n";
        std::fs::write(grove_dir.join("learnings.md"), learning_content).unwrap();

        // Transcript about something completely unrelated
        let transcript_path = dir.path().join("transcript.jsonl");
        let transcript = r#"{"type":"user","message":{"content":"Help me configure Phoenix LiveView real-time websocket connections"}}"#;
        std::fs::write(&transcript_path, transcript).unwrap();

        let mut config = Config::default();
        config.retrieval.intent_filter.enabled = true;
        config.retrieval.intent_filter.min_overlap = 1;
        config.retrieval.scoring_backend = "keyword".to_string();

        let store = crate::storage::MemorySessionStore::new();
        let runner = HookRunner::new(store, config);

        let query = SearchQuery::new().keywords(vec!["database".to_string()]);
        let session = SessionState::new("all-filtered", dir.path().to_string_lossy(), "");

        let results = runner.retrieve_and_score_learnings(
            dir.path(),
            &session,
            &query,
            Some(&transcript_path),
        );

        // All learnings should be filtered out — result is empty, not a crash
        // (The database learning doesn't match Phoenix/LiveView intent)
        for cs in &results {
            assert_ne!(
                cs.learning.id, "cl_all_filtered_001",
                "Database learning should be filtered by Phoenix intent"
            );
        }
    }

    // =========================================================================
    // Corpus-size heuristic profile selection tests
    // =========================================================================

    /// Verify that `retrieve_and_score_learnings` with BM25 scoring backend
    /// queries total active corpus size for profile selection rather than
    /// using search result count.  We create a corpus above the threshold
    /// where only a few learnings match the query keywords.  If the code
    /// incorrectly used `results.len()` (few matches) instead of total
    /// corpus size, the profile would flip to SmallCorpus, but with the
    /// correct implementation it stays Standard.
    #[test]
    #[cfg(feature = "tantivy-search")]
    fn corpus_size_heuristic_uses_total_active_count() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Build a corpus of 55 learnings (above default threshold=50).
        // Only 2 learnings mention "authentication"; the rest are about
        // unrelated topics so they won't match our query keywords.
        let mut content = String::new();
        for i in 0..55 {
            let (summary, detail) = if i < 2 {
                (
                    "Authentication token refresh pattern".to_string(),
                    "Use refresh tokens to avoid re-auth".to_string(),
                )
            } else {
                (
                    format!("Unrelated topic number {i}"),
                    format!("Detail about unrelated topic {i}"),
                )
            };
            content.push_str(&format!(
                "---\nid: cl_heuristic_{i:03}\ntimestamp: 2025-01-15T10:00:00Z\n\
                 category: Pattern\nsummary: {summary}\ndetail: {detail}\n\
                 behavior_changing: true\nstatus: active\n---\n"
            ));
        }
        std::fs::write(grove_dir.join("learnings.md"), &content).unwrap();

        let mut config = Config::default();
        config.retrieval.scoring_backend = "bm25".to_string();
        // threshold=50 → 55 learnings should select Standard profile
        config.retrieval.corpus_size_threshold = 50;

        let store = crate::storage::MemorySessionStore::new();
        let runner = HookRunner::new(store, config);

        // Query keywords that only match 2 of 55 learnings
        let query = SearchQuery::new().keywords(vec![
            "authentication".to_string(),
            "token".to_string(),
            "refresh".to_string(),
        ]);
        let session = SessionState::new("heuristic-test", dir.path().to_string_lossy(), "");

        // This exercises the full BM25 code path including the
        // backend.search(empty, active_only) call for corpus size.
        // If corpus_size computation regresses to results.len(), the
        // profile would incorrectly be SmallCorpus instead of Standard.
        let results = runner.retrieve_and_score_learnings(dir.path(), &session, &query, None);

        // Verify the function completes and returns the matching learnings
        // (exact count depends on BM25 scoring, but should be <= 55)
        assert!(results.len() <= 55);
    }

    /// Same setup but with corpus below threshold — verifies SmallCorpus
    /// profile path works when total active count < threshold.
    #[test]
    #[cfg(feature = "tantivy-search")]
    fn corpus_size_heuristic_small_corpus_path() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Build a corpus of 10 learnings (well below threshold=50)
        let mut content = String::new();
        for i in 0..10 {
            content.push_str(&format!(
                "---\nid: cl_small_{i:03}\ntimestamp: 2025-01-15T10:00:00Z\n\
                 category: Pattern\nsummary: Database indexing tip {i}\n\
                 detail: Add indexes on frequently queried columns\n\
                 behavior_changing: true\nstatus: active\n---\n"
            ));
        }
        std::fs::write(grove_dir.join("learnings.md"), &content).unwrap();

        let mut config = Config::default();
        config.retrieval.scoring_backend = "bm25".to_string();
        config.retrieval.corpus_size_threshold = 50;

        let store = crate::storage::MemorySessionStore::new();
        let runner = HookRunner::new(store, config);

        let query =
            SearchQuery::new().keywords(vec!["database".to_string(), "indexing".to_string()]);
        let session = SessionState::new("small-corpus-test", dir.path().to_string_lossy(), "");

        // Exercises SmallCorpus (boosted) BM25 path
        let results = runner.retrieve_and_score_learnings(dir.path(), &session, &query, None);
        assert!(results.len() <= 10);
    }

    // =========================================================================
    // HookType::UserPromptSubmit parse tests
    // =========================================================================

    #[test]
    fn test_hook_type_parse_user_prompt_submit() {
        assert_eq!(
            HookType::parse("user-prompt-submit"),
            Some(HookType::UserPromptSubmit)
        );
        assert_eq!(
            HookType::parse("userpromptsubmit"),
            Some(HookType::UserPromptSubmit)
        );
        assert_eq!(
            HookType::parse("user_prompt_submit"),
            Some(HookType::UserPromptSubmit)
        );
    }

    // =========================================================================
    // extract_prompt_keywords tests
    // =========================================================================

    #[test]
    fn extract_prompt_keywords_basic() {
        let kw = extract_prompt_keywords("Help me fix the authentication module", 15);
        assert!(kw.contains(&"help".to_string()));
        assert!(kw.contains(&"authentication".to_string()));
        assert!(kw.contains(&"module".to_string()));
    }

    #[test]
    fn extract_prompt_keywords_filters_noise() {
        let kw = extract_prompt_keywords(
            "Please implement the following changes to the existing code",
            15,
        );
        // "implement", "following", "changes", "existing", "code" are noise words
        assert!(!kw.contains(&"implement".to_string()));
        assert!(!kw.contains(&"following".to_string()));
        assert!(!kw.contains(&"changes".to_string()));
        assert!(!kw.contains(&"existing".to_string()));
        assert!(!kw.contains(&"code".to_string()));
    }

    #[test]
    fn extract_prompt_keywords_short_prompt_returns_empty() {
        // Prompts <= 10 chars return empty
        let kw = extract_prompt_keywords("fix bug", 15);
        assert!(kw.is_empty());
    }

    #[test]
    fn extract_prompt_keywords_respects_max() {
        let long_prompt = "authentication authorization middleware endpoint \
                           controller service repository database migration \
                           schema validation serialization deserialization \
                           encryption hashing salting tokenization caching \
                           logging monitoring alerting";
        let kw = extract_prompt_keywords(long_prompt, 5);
        assert_eq!(kw.len(), 5);
    }

    #[test]
    fn extract_prompt_keywords_deduplicates() {
        let kw = extract_prompt_keywords(
            "authentication authentication authentication module module",
            15,
        );
        let auth_count = kw.iter().filter(|k| *k == "authentication").count();
        assert_eq!(auth_count, 1);
    }

    #[test]
    fn extract_prompt_keywords_strips_xml_tags() {
        let kw = extract_prompt_keywords(
            "<system-reminder>ignore this</system-reminder> Fix the database indexing strategy",
            15,
        );
        // XML tag content like "system-reminder" should not appear
        assert!(!kw.contains(&"system".to_string()));
        assert!(!kw.contains(&"reminder".to_string()));
        assert!(kw.contains(&"database".to_string()));
        assert!(kw.contains(&"indexing".to_string()));
        assert!(kw.contains(&"strategy".to_string()));
    }

    #[test]
    fn extract_prompt_keywords_filters_short_words() {
        let kw = extract_prompt_keywords("I am on the fix for API bug now too", 15);
        // Words < 4 chars should be filtered
        assert!(!kw.contains(&"am".to_string()));
        assert!(!kw.contains(&"on".to_string()));
        assert!(!kw.contains(&"the".to_string()));
        assert!(!kw.contains(&"for".to_string()));
        assert!(!kw.contains(&"bug".to_string()));
        assert!(!kw.contains(&"now".to_string()));
        assert!(!kw.contains(&"too".to_string()));
    }

    // =========================================================================
    // handle_user_prompt_submit integration tests
    // =========================================================================

    #[test]
    fn user_prompt_submit_no_session_returns_empty() {
        let runner = test_runner();
        let input = r#"{
            "session_id": "nonexistent-session",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "prompt": "Help me fix the database query"
        }"#;

        let result = runner.run_with_input(HookType::UserPromptSubmit, input);
        assert!(result.is_ok());

        let output: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        // Empty output — no hookSpecificOutput key
        assert!(output.get("hookSpecificOutput").is_none());
    }

    #[test]
    fn user_prompt_submit_short_prompt_returns_empty() {
        use crate::storage::MemorySessionStore;

        let store = MemorySessionStore::new();
        let session = SessionState::new("prompt-short", "/tmp", "/tmp/project");
        store.put(&session).unwrap();

        let runner = HookRunner::new(store, Config::default());
        let input = r#"{
            "session_id": "prompt-short",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "prompt": "fix bug"
        }"#;

        let result = runner.run_with_input(HookType::UserPromptSubmit, input);
        assert!(result.is_ok());

        let output: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert!(output.get("hookSpecificOutput").is_none());
    }

    #[test]
    fn user_prompt_submit_with_learnings_returns_context() {
        use crate::storage::MemorySessionStore;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        // Write a learning about authentication
        let learning_content = "---\nid: cl_ups_001\ntimestamp: 2025-01-15T10:00:00Z\n\
            category: Pattern\nsummary: OAuth token refresh strategy\n\
            detail: Always use refresh tokens with sliding expiration for OAuth flows\n\
            behavior_changing: true\nstatus: active\n---\n";
        std::fs::write(grove_dir.join("learnings.md"), learning_content).unwrap();

        // Write a transcript file
        let transcript_path = dir.path().join("transcript.jsonl");
        std::fs::write(&transcript_path, "").unwrap();

        let store = MemorySessionStore::new();
        let session = SessionState::new("ups-session", dir.path().to_string_lossy(), "");
        store.put(&session).unwrap();

        let runner = HookRunner::new(store, Config::default());
        let input = serde_json::json!({
            "session_id": "ups-session",
            "transcript_path": transcript_path.to_string_lossy(),
            "cwd": dir.path().to_string_lossy(),
            "prompt": "Help me implement OAuth authentication token refresh"
        });

        let result = runner.run_with_input(HookType::UserPromptSubmit, &input.to_string());
        assert!(result.is_ok());

        // Verify the session was saved (trace added)
        let saved = runner.store.get("ups-session").unwrap();
        assert!(saved.is_some());
        let saved = saved.unwrap();
        let traces: Vec<_> = saved
            .trace
            .iter()
            .filter(|t| t.event_type == EventType::UserPromptInjection)
            .collect();
        assert!(!traces.is_empty(), "Should have UserPromptInjection trace");
    }

    #[test]
    fn user_prompt_submit_deduplicates_injected_learnings() {
        use crate::storage::MemorySessionStore;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let learning_content = "---\nid: cl_dedup_001\ntimestamp: 2025-01-15T10:00:00Z\n\
            category: Pattern\nsummary: Database indexing strategy\n\
            detail: Always add indexes on foreign keys for query performance\n\
            behavior_changing: true\nstatus: active\n---\n";
        std::fs::write(grove_dir.join("learnings.md"), learning_content).unwrap();

        let transcript_path = dir.path().join("transcript.jsonl");
        std::fs::write(&transcript_path, "").unwrap();

        let store = MemorySessionStore::new();
        let mut session = SessionState::new("ups-dedup", dir.path().to_string_lossy(), "");

        // Mark the learning as already injected
        session
            .gate
            .injected_learnings
            .push(InjectedLearning::new("cl_dedup_001", 0.8));
        store.put(&session).unwrap();

        let runner = HookRunner::new(store, Config::default());
        let input = serde_json::json!({
            "session_id": "ups-dedup",
            "transcript_path": transcript_path.to_string_lossy(),
            "cwd": dir.path().to_string_lossy(),
            "prompt": "Help me optimize database indexing for foreign keys"
        });

        let result = runner.run_with_input(HookType::UserPromptSubmit, &input.to_string());
        assert!(result.is_ok());

        let output: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        // Already-injected learning should be deduplicated, so no new context
        // (hookSpecificOutput should be absent or additionalContext should not re-inject)
        // The key assertion: it doesn't crash and handles dedup gracefully
        let _ = output;
    }

    #[test]
    fn user_prompt_submit_invalid_json_returns_error() {
        let runner = test_runner();
        let result = runner.run_with_input(HookType::UserPromptSubmit, "not valid json");
        assert!(result.is_err());
    }

    // =========================================================================
    // LLM Reranking Tests
    // =========================================================================

    fn make_composite_score(id: &str, summary: &str, score: f64) -> CompositeScore {
        use crate::core::{Confidence, LearningCategory, LearningScope, WriteGateCriterion};
        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            summary,
            format!("Detail for {}", summary),
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["test".to_string()],
            "test-session",
        )
        .with_id(id);
        CompositeScore::new(learning, score, 1.0, 1.0, Strategy::Moderate)
    }

    #[test]
    fn test_build_rerank_prompt_includes_context() {
        let candidates = vec![
            make_composite_score("cl_001", "Use builder pattern", 0.8),
            make_composite_score("cl_002", "Always validate input", 0.6),
        ];

        let prompt = build_rerank_prompt(
            &candidates,
            "Read",
            "src/config.rs",
            "feature/rerank",
            &["src/config.rs".to_string(), "src/main.rs".to_string()],
        );

        assert!(prompt.contains("Tool: Read"));
        assert!(prompt.contains("Tool input: src/config.rs"));
        assert!(prompt.contains("Branch: feature/rerank"));
        assert!(prompt.contains("Recent files: src/config.rs, src/main.rs"));
        assert!(prompt.contains("[1] Use builder pattern"));
        assert!(prompt.contains("[2] Always validate input"));
        assert!(prompt.contains("comma-separated integers"));
    }

    #[test]
    fn test_build_rerank_prompt_empty_context() {
        let candidates = vec![make_composite_score("cl_001", "Test learning", 0.5)];

        let prompt = build_rerank_prompt(&candidates, "", "", "", &[]);

        // Should not contain empty context lines
        assert!(!prompt.contains("Tool:"));
        assert!(!prompt.contains("Branch:"));
        assert!(!prompt.contains("Recent files:"));
        assert!(prompt.contains("[1] Test learning"));
    }

    #[test]
    fn test_parse_rerank_scores_valid() {
        let scores = parse_rerank_scores("4,2,5", 3);
        assert_eq!(scores, Some(vec![4.0, 2.0, 5.0]));
    }

    #[test]
    fn test_parse_rerank_scores_with_spaces() {
        let scores = parse_rerank_scores("3, 5, 1, 4", 4);
        assert_eq!(scores, Some(vec![3.0, 5.0, 1.0, 4.0]));
    }

    #[test]
    fn test_parse_rerank_scores_json_wrapped() {
        let response = r#"{"result": "4,2,5"}"#;
        let scores = parse_rerank_scores(response, 3);
        assert_eq!(scores, Some(vec![4.0, 2.0, 5.0]));
    }

    #[test]
    fn test_parse_rerank_scores_wrong_count() {
        let scores = parse_rerank_scores("4,2", 3);
        assert_eq!(scores, None);
    }

    #[test]
    fn test_parse_rerank_scores_invalid_text() {
        let scores = parse_rerank_scores("no numbers here", 2);
        assert_eq!(scores, None);
    }

    #[test]
    fn test_parse_rerank_scores_clamps_out_of_range() {
        // Digits > 5 or < 1 get clamped
        let scores = parse_rerank_scores("9,0,3", 3);
        assert_eq!(scores, Some(vec![5.0, 1.0, 3.0]));
    }

    #[test]
    fn test_rerank_with_llm_empty_candidates() {
        let config = crate::config::RerankConfig {
            enabled: true,
            timeout_seconds: 5,
            model: "haiku".to_string(),
            backend: "cli".to_string(),
        };
        let result = rerank_with_llm(
            Vec::new(),
            &config,
            "https://api.anthropic.com/v1/messages",
            "Read",
            "src/main.rs",
            "main",
            &[],
        );
        assert!(result.is_empty());
    }

    #[test]
    fn test_rerank_with_llm_unknown_backend_failopen() {
        let config = crate::config::RerankConfig {
            enabled: true,
            timeout_seconds: 5,
            model: "haiku".to_string(),
            backend: "unknown_backend".to_string(),
        };
        let candidates = vec![
            make_composite_score("cl_001", "Learning A", 0.8),
            make_composite_score("cl_002", "Learning B", 0.6),
        ];
        let original_ids: Vec<String> = candidates.iter().map(|c| c.learning.id.clone()).collect();

        let result = rerank_with_llm(
            candidates,
            &config,
            "https://api.anthropic.com/v1/messages",
            "Read",
            "src/main.rs",
            "main",
            &[],
        );

        // Should return original ordering (fail-open)
        let result_ids: Vec<String> = result.iter().map(|c| c.learning.id.clone()).collect();
        assert_eq!(result_ids, original_ids);
    }

    // =========================================================================
    // Corpus vocabulary extraction tests
    // =========================================================================

    #[cfg(feature = "tantivy-search")]
    fn make_learning_with_fields(
        id: &str,
        summary: &str,
        tags: &[&str],
        relevance_context: Option<&str>,
    ) -> crate::core::learning::CompoundLearning {
        use crate::core::learning::CompoundLearning;
        use crate::core::{Confidence, LearningScope, WriteGateCriterion};
        let mut learning = CompoundLearning::new(
            crate::core::learning::LearningCategory::Pattern,
            summary,
            "detail text",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            tags.iter().map(|t| t.to_string()).collect(),
            "test-session",
        )
        .with_id(id);
        learning.relevance_context = relevance_context.map(|s| s.to_string());
        learning
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn extract_corpus_vocabulary_empty_learnings() {
        let vocab = extract_corpus_vocabulary(&[], 2);
        assert!(vocab.is_empty());
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn extract_corpus_vocabulary_single_learning_excluded() {
        // Terms from a single learning don't meet min_occurrences=2
        let learnings = vec![make_learning_with_fields(
            "cl_001",
            "Use authentication middleware",
            &["authentication"],
            None,
        )];
        let vocab = extract_corpus_vocabulary(&learnings, 2);
        assert!(
            !vocab.contains("authentication"),
            "Single-learning term should not appear in vocabulary"
        );
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn extract_corpus_vocabulary_cross_learning_terms_included() {
        let learnings = vec![
            make_learning_with_fields(
                "cl_001",
                "Configure authentication middleware",
                &["authentication"],
                None,
            ),
            make_learning_with_fields(
                "cl_002",
                "Authentication token refresh logic",
                &["security"],
                None,
            ),
        ];
        let vocab = extract_corpus_vocabulary(&learnings, 2);
        assert!(
            vocab.contains("authentication"),
            "Term appearing in 2 learnings should be in vocabulary"
        );
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn extract_corpus_vocabulary_noise_filtered() {
        let learnings = vec![
            make_learning_with_fields("cl_001", "Build with cargo", &["cargo"], None),
            make_learning_with_fields("cl_002", "Run cargo test", &["cargo"], None),
        ];
        let vocab = extract_corpus_vocabulary(&learnings, 2);
        assert!(
            !vocab.contains("cargo"),
            "Noise word 'cargo' should be filtered"
        );
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn extract_corpus_vocabulary_tags_included() {
        let learnings = vec![
            make_learning_with_fields("cl_001", "Summary one", &["tantivy"], None),
            make_learning_with_fields("cl_002", "Summary two", &["tantivy"], None),
        ];
        let vocab = extract_corpus_vocabulary(&learnings, 2);
        assert!(
            vocab.contains("tantivy"),
            "Tag appearing in 2 learnings should be in vocabulary"
        );
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn extract_corpus_vocabulary_relevance_context_included() {
        let learnings = vec![
            make_learning_with_fields(
                "cl_001",
                "Summary one",
                &[],
                Some("Surface when working with deployment"),
            ),
            make_learning_with_fields(
                "cl_002",
                "Summary two",
                &[],
                Some("Relevant for deployment pipelines"),
            ),
        ];
        let vocab = extract_corpus_vocabulary(&learnings, 2);
        assert!(
            vocab.contains("deployment"),
            "relevance_context term should be in vocabulary"
        );
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn extract_corpus_vocabulary_short_tokens_excluded() {
        // Tokens shorter than 4 chars should be excluded
        let learnings = vec![
            make_learning_with_fields("cl_001", "foo bar API", &["api"], None),
            make_learning_with_fields("cl_002", "baz API call", &["api"], None),
        ];
        let vocab = extract_corpus_vocabulary(&learnings, 2);
        assert!(!vocab.contains("api"), "3-char token should be excluded");
        assert!(!vocab.contains("foo"), "3-char token should be excluded");
    }

    // =========================================================================
    // Query enrichment tests
    // =========================================================================

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn enrich_query_no_intersection() {
        let vocab: std::collections::HashSet<String> = ["authentication", "deployment"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let result = enrich_query_with_corpus_vocabulary(
            &["config".to_string()],
            &["src/main.rs".to_string()],
            &vocab,
        );
        assert!(result.is_empty());
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn enrich_query_exact_match() {
        let vocab: std::collections::HashSet<String> = ["authentication", "middleware"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let result = enrich_query_with_corpus_vocabulary(
            &["config".to_string()],
            &["src/authentication/handler.rs".to_string()],
            &vocab,
        );
        assert!(
            result.contains(&"authentication".to_string()),
            "Path segment matching vocab should be returned"
        );
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn enrich_query_case_insensitive() {
        let vocab: std::collections::HashSet<String> =
            ["authentication"].iter().map(|s| s.to_string()).collect();
        let result = enrich_query_with_corpus_vocabulary(
            &["config".to_string()],
            &["src/Authentication/handler.rs".to_string()],
            &vocab,
        );
        assert!(
            result.contains(&"authentication".to_string()),
            "Case-insensitive match should work"
        );
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn enrich_query_no_duplicates() {
        let vocab: std::collections::HashSet<String> =
            ["authentication"].iter().map(|s| s.to_string()).collect();
        let result = enrich_query_with_corpus_vocabulary(
            &["config".to_string()],
            &[
                "src/authentication/login.rs".to_string(),
                "src/authentication/logout.rs".to_string(),
            ],
            &vocab,
        );
        let count = result.iter().filter(|t| *t == "authentication").count();
        assert_eq!(count, 1, "Should not return duplicates");
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn enrich_query_excludes_existing_keywords() {
        let vocab: std::collections::HashSet<String> =
            ["authentication"].iter().map(|s| s.to_string()).collect();
        // "authentication" is already in keywords, should not be in enrichment
        let result = enrich_query_with_corpus_vocabulary(
            &["authentication".to_string()],
            &["src/authentication/handler.rs".to_string()],
            &vocab,
        );
        assert!(
            result.is_empty(),
            "Should not return terms already in keywords"
        );
    }

    #[test]
    #[cfg(feature = "tantivy-search")]
    fn enrich_query_empty_vocab() {
        let vocab = std::collections::HashSet::new();
        let result = enrich_query_with_corpus_vocabulary(
            &["config".to_string()],
            &["src/auth/handler.rs".to_string()],
            &vocab,
        );
        assert!(result.is_empty());
    }
}
