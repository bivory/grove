//! Retroflect command for Grove.
//!
//! Mines past Claude Code session transcripts to generate learnings retroactively
//! via LLM synthesis. Writes through the existing validation pipeline with lenient
//! write gate settings and `#retroflect` provenance tagging.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::backends::{MarkdownBackend, MemoryBackend, SearchFilters, SearchQuery};
use crate::config::{project_grove_dir, project_learnings_path, project_stats_log_path, Config};
use crate::core::learning::CompoundLearning;
use crate::core::quality::QualityCheckMode;
use crate::core::reflect::{
    validate_with_duplicates_and_quality_semantic, CandidateLearning, WriteGateMode,
};
use crate::eval::corpus::{parse_session_transcript, SessionSummary};
use crate::llm;
use crate::llm::batch::{self, BatchRequest, BatchResultType};
use crate::stats::{StatsEventType, StatsLogger};

/// Callable that invokes an LLM. Args: (backend, model, system_prompt, user_prompt).
type LlmCaller = dyn Fn(&str, &str, &str, &str) -> Option<String>;

/// Default LLM caller that dispatches to the real CLI or API backend.
fn default_llm_caller(
    backend: &str,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> Option<String> {
    if backend == "cli" {
        llm::call_llm_cli(model, system_prompt, user_prompt)
    } else {
        llm::call_llm_api(
            model,
            "https://api.anthropic.com/v1/messages",
            system_prompt,
            user_prompt,
            4096,
        )
    }
}

/// CLI options for the retroflect command.
#[derive(Debug, Clone)]
pub struct RetroflectOptions {
    /// Project root to retroflect (default: current dir).
    pub project: Option<PathBuf>,
    /// Auto-discover all projects under ~/.claude/projects/.
    pub all: bool,
    /// LLM model for synthesis.
    pub model: String,
    /// LLM backend: "api" or "cli".
    pub backend: String,
    /// Max sessions to analyze.
    pub limit: usize,
    /// Skip sessions with fewer than N user turns.
    pub min_turns: usize,
    /// Show candidates without writing.
    pub dry_run: bool,
    /// Re-analyze previously retroflected sessions.
    pub force: bool,
    /// Skip cost confirmation prompt.
    pub yes: bool,
    /// Output results as JSON.
    pub json: bool,
    /// Use Batch API (50% cheaper, async processing).
    pub batch: bool,
}

impl Default for RetroflectOptions {
    fn default() -> Self {
        Self {
            project: None,
            all: false,
            model: "claude-sonnet-4-20250514".to_string(),
            backend: "api".to_string(),
            limit: 20,
            min_turns: 3,
            dry_run: false,
            force: false,
            yes: false,
            json: false,
            batch: false,
        }
    }
}

/// Output from the retroflect command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetroflectOutput {
    /// Whether the command succeeded.
    pub success: bool,
    /// Number of sessions analyzed.
    pub sessions_analyzed: usize,
    /// Total candidates produced by LLM.
    pub total_candidates: usize,
    /// Total learnings accepted and written.
    pub total_accepted: usize,
    /// Total candidates rejected.
    pub total_rejected: usize,
    /// Sessions skipped (parse error, LLM failure, etc.).
    pub sessions_skipped: usize,
    /// Per-session results.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub session_results: Vec<SessionResult>,
    /// Error message if command failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Per-session retroflect result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResult {
    /// Claude Code session ID (JSONL filename).
    pub session_id: String,
    /// Project path this session belongs to.
    pub project_path: String,
    /// Number of candidates produced.
    pub candidates: usize,
    /// Number of learnings accepted.
    pub accepted: usize,
    /// Reason if session was skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    /// Accepted learning summaries for progress display (not serialized).
    #[serde(skip)]
    pub accepted_summaries: Vec<AcceptedLearningSummary>,
}

/// Summary of an accepted learning for progress display.
#[derive(Debug, Clone, Default)]
pub struct AcceptedLearningSummary {
    pub category: String,
    pub summary: String,
}

/// A discovered project with its sessions.
struct ProjectSessions {
    /// Project root path (from cwd in JSONL).
    project_path: PathBuf,
    /// Session dir under ~/.claude/projects/.
    #[allow(dead_code)]
    session_dir: PathBuf,
    /// Eligible session summaries.
    sessions: Vec<SessionSummary>,
}

// =============================================================================
// LLM System Prompt
// =============================================================================

const RETROFLECT_SYSTEM_PROMPT: &str = r#"You are a compound learning extractor for software engineering sessions. Given a condensed transcript of a Claude Code session, extract 0-5 structured learnings that capture reusable knowledge.

## Learning Categories

1. **pattern** — A reusable approach or technique (e.g., "Use builder pattern for complex config objects")
2. **pitfall** — A mistake to avoid (e.g., "Don't use unwrap() in async handlers")
3. **convention** — A project/team convention (e.g., "All API endpoints return JSON envelopes")
4. **dependency** — External tool/library knowledge (e.g., "tokio::spawn requires 'static lifetime")
5. **process** — Workflow or process insight (e.g., "Run integration tests before submitting PR")
6. **domain** — Business/domain knowledge (e.g., "Premium users bypass rate limiting")
7. **debugging** — Debugging technique (e.g., "Check connection pool exhaustion when queries hang")

## Output Format

Respond with a JSON array of candidate learnings. Each candidate must have:

```json
[
  {
    "category": "pitfall",
    "summary": "Brief one-line description of the learning",
    "detail": "Detailed explanation of why this matters and what to do differently (at least 20 characters)",
    "scope": "project",
    "confidence": "high",
    "criteria_met": ["behavior-changing"],
    "tags": ["relevant", "tags", "retroflect"],
    "context_files": ["src/relevant/file.rs"],
    "relevance_context": "Surface when working on similar code or facing similar issues"
  }
]
```

## criteria_met Options

At least one must be claimed:
- **behavior-changing** — Would change how someone writes code or makes decisions
- **decision-rationale** — Captures why a choice was made (not just what)
- **stable-fact** — A durable fact unlikely to change soon
- **explicit-request** — The user explicitly asked to remember something

## Instructions

- Produce 0-5 candidates per session. Return `[]` if no significant learnings exist.
- **Always** include "retroflect" in the tags array for provenance tracking.
- Focus on: decisions and pivots, debugging breakthroughs, architectural choices, surprising behaviors.
- Skip: routine work, obvious operations, transient debugging steps, session-specific context.
- Each learning must be self-contained and useful without the original session context.
- Output ONLY the JSON array, no other text."#;

// =============================================================================
// Public API
// =============================================================================

/// Run the retroflect command.
pub fn run(options: &RetroflectOptions, cwd: &Path) -> RetroflectOutput {
    let result = if options.batch {
        run_inner_batch(options, cwd)
    } else {
        run_inner(options, cwd, &default_llm_caller)
    };
    match result {
        Ok(output) => output,
        Err(e) => RetroflectOutput {
            success: false,
            sessions_analyzed: 0,
            total_candidates: 0,
            total_accepted: 0,
            total_rejected: 0,
            sessions_skipped: 0,
            session_results: Vec::new(),
            error: Some(e),
        },
    }
}

/// Format a progress line for stderr during processing.
#[allow(clippy::too_many_arguments)]
fn format_progress_line(
    index: usize,
    total: usize,
    result: &SessionResult,
    user_turns: usize,
    file_count: usize,
    running_candidates: usize,
    running_accepted: usize,
    dry_run: bool,
) -> String {
    let sid = &result.session_id[..8.min(result.session_id.len())];
    if let Some(reason) = &result.skip_reason {
        format!(
            "  [{}/{}] {} — skipped: {} (total: {} found, {} accepted)",
            index, total, sid, reason, running_candidates, running_accepted
        )
    } else if dry_run {
        format!(
            "  [{}/{}] {} — eligible ({} turns, {} files)",
            index, total, sid, user_turns, file_count
        )
    } else if result.candidates > 0 {
        format!(
            "  [{}/{}] {} — {} candidates, {} accepted (total: {} found, {} accepted)",
            index,
            total,
            sid,
            result.candidates,
            result.accepted,
            running_candidates,
            running_accepted
        )
    } else {
        format!(
            "  [{}/{}] {} — no learnings found (total: {} found, {} accepted)",
            index, total, sid, running_candidates, running_accepted
        )
    }
}

/// Categorize a skip reason into a user-friendly label.
fn categorize_skip_reason(reason: &str) -> &'static str {
    if reason.contains("LLM call failed") || reason.contains("empty response") {
        "LLM failures"
    } else if reason.contains("parse") || reason.contains("Parse") {
        "parse errors"
    } else if reason.contains("initialize") || reason.contains("grove") {
        "init failures"
    } else {
        "other errors"
    }
}

/// Format retroflect output for display.
pub fn format_output(output: &RetroflectOutput, options: &RetroflectOptions) -> String {
    if options.json {
        return serde_json::to_string_pretty(output).unwrap_or_else(|_| "{}".to_string());
    }

    if let Some(err) = &output.error {
        return format!("Error: {}", err);
    }

    let mut lines = Vec::new();

    if options.dry_run {
        lines.push("Retroflect (dry run)".to_string());
    } else {
        lines.push("Retroflect complete".to_string());
    }

    lines.push(format!("  Sessions analyzed: {}", output.sessions_analyzed));

    if output.sessions_skipped > 0 {
        lines.push(format!("  Sessions skipped:  {}", output.sessions_skipped));
        // Categorize skip reasons for breakdown
        let mut reason_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for result in &output.session_results {
            if let Some(reason) = &result.skip_reason {
                *reason_counts
                    .entry(categorize_skip_reason(reason))
                    .or_insert(0) += 1;
            }
        }
        if !reason_counts.is_empty() {
            let mut sorted: Vec<_> = reason_counts.into_iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(&a.1));
            let breakdown: Vec<String> = sorted
                .iter()
                .map(|(cat, count)| format!("{} {}", count, cat))
                .collect();
            lines.push(format!("    ({})", breakdown.join(", ")));
        }
    }

    lines.push(format!("  Candidates:        {}", output.total_candidates));
    lines.push(format!("  Accepted:          {}", output.total_accepted));
    lines.push(format!("  Rejected:          {}", output.total_rejected));

    if !output.session_results.is_empty() {
        // Group results by project path, preserving insertion order
        let mut project_order: Vec<String> = Vec::new();
        let mut by_project: std::collections::HashMap<&str, Vec<&SessionResult>> =
            std::collections::HashMap::new();
        for result in &output.session_results {
            if !by_project.contains_key(result.project_path.as_str()) {
                project_order.push(result.project_path.clone());
            }
            by_project
                .entry(&result.project_path)
                .or_default()
                .push(result);
        }

        for project in &project_order {
            let results = &by_project[project.as_str()];
            lines.push(String::new());
            lines.push(format!("  {}", project));
            for result in results {
                let sid = &result.session_id[..8.min(result.session_id.len())];
                if let Some(reason) = &result.skip_reason {
                    lines.push(format!("    {} — skipped: {}", sid, reason));
                } else if result.candidates > 0 {
                    lines.push(format!(
                        "    {} — {} candidates, {} accepted",
                        sid, result.candidates, result.accepted
                    ));
                } else if options.dry_run {
                    lines.push(format!("    {} — eligible", sid));
                } else {
                    lines.push(format!("    {} — no learnings found", sid));
                }
            }
        }
    }

    lines.join("\n")
}

// =============================================================================
// Internal Implementation
// =============================================================================

fn run_inner(
    options: &RetroflectOptions,
    cwd: &Path,
    llm_caller: &LlmCaller,
) -> Result<RetroflectOutput, String> {
    // Discover projects and sessions
    let mut projects = discover_projects(options, cwd)?;

    if projects.is_empty() {
        return Err("No eligible sessions found".to_string());
    }

    // Collect all sessions across projects, respecting limit
    let mut all_sessions: Vec<(PathBuf, SessionSummary)> = Vec::new();
    for project in &mut projects {
        for session in project.sessions.drain(..) {
            all_sessions.push((project.project_path.clone(), session));
        }
    }

    // Sort by timestamp (most recent first) and apply limit
    all_sessions.sort_by(|a, b| b.1.timestamp.cmp(&a.1.timestamp));
    all_sessions.truncate(options.limit);
    let total_session_count = all_sessions.len();

    if all_sessions.is_empty() {
        return Err("No eligible sessions found after filtering".to_string());
    }

    // Cost estimation and confirmation
    let estimated_tokens: usize = all_sessions
        .iter()
        .map(|(_, s)| estimate_tokens(&s.condensed_transcript))
        .sum();

    if !options.yes && !options.dry_run {
        let est_cost = estimate_cost(estimated_tokens, all_sessions.len());
        eprintln!(
            "Found {} eligible sessions (~{} tokens, estimated cost: ${:.2})",
            all_sessions.len(),
            estimated_tokens,
            est_cost
        );
        if !confirm_prompt("Proceed?") {
            return Err("Aborted by user".to_string());
        }
    }

    let config = Config::load();

    let mut output = RetroflectOutput {
        success: true,
        sessions_analyzed: 0,
        total_candidates: 0,
        total_accepted: 0,
        total_rejected: 0,
        sessions_skipped: 0,
        session_results: Vec::new(),
        error: None,
    };

    // Group sessions by project for batch dedup
    let mut sessions_by_project: std::collections::HashMap<PathBuf, Vec<SessionSummary>> =
        std::collections::HashMap::new();
    for (project_path, session) in all_sessions {
        sessions_by_project
            .entry(project_path)
            .or_default()
            .push(session);
    }

    let mut session_index: usize = 0;

    for (project_path, sessions) in &sessions_by_project {
        // Ensure .grove/ exists for this project
        let grove_dir = project_grove_dir(project_path);
        if !grove_dir.exists() {
            if options.dry_run {
                eprintln!("Would initialize .grove/ at {}", project_path.display());
            } else if let Err(e) = init_grove_dir(&grove_dir) {
                eprintln!(
                    "Warning: failed to initialize .grove/ at {}: {}",
                    project_path.display(),
                    e
                );
                let reason = format!("Failed to initialize .grove/: {}", e);
                for session in sessions {
                    output.sessions_skipped += 1;
                    session_index += 1;
                    let result = SessionResult {
                        session_id: session.session_id.clone(),
                        project_path: project_path.display().to_string(),
                        candidates: 0,
                        accepted: 0,
                        skip_reason: Some(reason.clone()),
                        accepted_summaries: Vec::new(),
                    };
                    eprintln!(
                        "{}",
                        format_progress_line(
                            session_index,
                            total_session_count,
                            &result,
                            session.user_turns,
                            session.file_paths.len(),
                            output.total_candidates,
                            output.total_accepted,
                            options.dry_run,
                        )
                    );
                    output.session_results.push(result);
                }
                continue;
            }
        }

        // Load existing learnings for dedup
        let backend = MarkdownBackend::new(project_learnings_path(project_path));
        let existing: Vec<CompoundLearning> = backend
            .search(&SearchQuery::new(), &SearchFilters::active_only())
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.learning)
            .collect();

        // Batch-accepted learnings for cross-session dedup within this run
        let mut batch_accepted: Vec<CompoundLearning> = Vec::new();

        let stats_path = project_stats_log_path(project_path);
        let stats_logger = StatsLogger::new(&stats_path);

        for session in sessions {
            let result = process_session(
                session,
                project_path,
                &existing,
                &mut batch_accepted,
                &backend,
                &stats_logger,
                &config,
                options,
                llm_caller,
            );

            match &result.skip_reason {
                Some(_) => output.sessions_skipped += 1,
                None => {
                    output.sessions_analyzed += 1;
                    output.total_candidates += result.candidates;
                    output.total_accepted += result.accepted;
                    output.total_rejected += result.candidates.saturating_sub(result.accepted);
                }
            }

            session_index += 1;
            eprintln!(
                "{}",
                format_progress_line(
                    session_index,
                    total_session_count,
                    &result,
                    session.user_turns,
                    session.file_paths.len(),
                    output.total_candidates,
                    output.total_accepted,
                    options.dry_run,
                )
            );
            for ls in &result.accepted_summaries {
                eprintln!("      + [{}] {}", ls.category, ls.summary);
            }

            output.session_results.push(result);
        }
    }

    Ok(output)
}

/// Batch mode implementation using the Anthropic Message Batches API.
///
/// Three-phase approach:
/// 1. Collect: Build all API request params with custom_id = "retroflect:{session_id}"
/// 2. Submit and wait: Submit batch, poll with progress reporting
/// 3. Process results in order: Maintain sequential validation for cross-session dedup
fn run_inner_batch(options: &RetroflectOptions, cwd: &Path) -> Result<RetroflectOutput, String> {
    // Discover projects and sessions (shared with sequential mode)
    let mut projects = discover_projects(options, cwd)?;

    if projects.is_empty() {
        return Err("No eligible sessions found".to_string());
    }

    let mut all_sessions: Vec<(PathBuf, SessionSummary)> = Vec::new();
    for project in &mut projects {
        for session in project.sessions.drain(..) {
            all_sessions.push((project.project_path.clone(), session));
        }
    }

    all_sessions.sort_by(|a, b| b.1.timestamp.cmp(&a.1.timestamp));
    all_sessions.truncate(options.limit);

    if all_sessions.is_empty() {
        return Err("No eligible sessions found after filtering".to_string());
    }

    // Cost estimation (batch is 50% cheaper)
    let estimated_tokens: usize = all_sessions
        .iter()
        .map(|(_, s)| estimate_tokens(&s.condensed_transcript))
        .sum();

    if !options.yes && !options.dry_run {
        let est_cost = estimate_cost(estimated_tokens, all_sessions.len()) * 0.5;
        eprintln!(
            "Found {} eligible sessions (~{} tokens, estimated batch cost: ${:.2})",
            all_sessions.len(),
            estimated_tokens,
            est_cost
        );
        if !confirm_prompt("Proceed with batch?") {
            return Err("Aborted by user".to_string());
        }
    }

    if options.dry_run {
        // Dry run: just count sessions without submitting
        let mut output = RetroflectOutput {
            success: true,
            sessions_analyzed: 0,
            total_candidates: 0,
            total_accepted: 0,
            total_rejected: 0,
            sessions_skipped: 0,
            session_results: Vec::new(),
            error: None,
        };
        for (project_path, session) in &all_sessions {
            output.session_results.push(SessionResult {
                session_id: session.session_id.clone(),
                project_path: project_path.display().to_string(),
                candidates: 0,
                accepted: 0,
                skip_reason: None,
                accepted_summaries: Vec::new(),
            });
        }
        return Ok(output);
    }

    // Phase 1: Collect — build BatchRequest objects for all sessions
    eprintln!("Phase 1: Collecting {} requests...", all_sessions.len());
    let mut batch_requests: Vec<BatchRequest> = Vec::new();
    // Preserve original order mapping: custom_id -> index
    let mut order_map: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for (idx, (_, session)) in all_sessions.iter().enumerate() {
        let user_prompt = build_user_prompt(session);
        let custom_id =
            crate::llm::batch::encode_custom_id(&format!("retroflect:{}", session.session_id));
        order_map.insert(custom_id.clone(), idx);

        let params = serde_json::json!({
            "model": options.model,
            "max_tokens": 4096,
            "system": [{
                "type": "text",
                "text": RETROFLECT_SYSTEM_PROMPT,
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": [{
                "role": "user",
                "content": user_prompt
            }]
        });

        batch_requests.push(BatchRequest { custom_id, params });
    }

    // Phase 2: Submit and wait
    eprintln!(
        "Phase 2: Submitting batch ({} requests)...",
        batch_requests.len()
    );
    let api_url = "https://api.anthropic.com/v1/messages";
    let batch_state = match batch::create_batch(api_url, batch_requests) {
        Some(state) => state,
        None => {
            eprintln!("Warning: batch creation failed, falling back to sequential mode");
            return run_inner(options, cwd, &default_llm_caller);
        }
    };

    eprintln!(
        "Batch created: {} ({} requests)",
        batch_state.batch_id, batch_state.total_requests
    );

    let config = Config::load();
    let batch_timeout = config.judge.batch_timeout();

    let ended = match batch::poll_batch_until_ended(
        api_url,
        &batch_state.batch_id,
        batch_timeout,
        &|status, processing, succeeded, errored, expired| {
            eprintln!(
                "  [{}] processing={} succeeded={} errored={} expired={}",
                status, processing, succeeded, errored, expired
            );
        },
    ) {
        Some(ended) => ended,
        None => {
            eprintln!("Warning: batch polling failed");
            return Err("Batch polling failed".to_string());
        }
    };

    if !ended {
        eprintln!("Warning: batch timed out, retrieving partial results");
    }

    // Retrieve results
    let batch_results = match batch::retrieve_batch_results(api_url, &batch_state.batch_id) {
        Some(results) => results,
        None => {
            eprintln!("Warning: failed to retrieve batch results, falling back to sequential");
            return run_inner(options, cwd, &default_llm_caller);
        }
    };

    if batch_results.is_empty() {
        eprintln!("Warning: zero batch results, falling back to sequential");
        return run_inner(options, cwd, &default_llm_caller);
    }

    eprintln!("Phase 3: Processing {} results...", batch_results.len());

    // Phase 3: Process results in original session order
    // Sort results by original session order to preserve cross-session dedup invariant
    let mut indexed_results: Vec<(usize, &batch::BatchResult)> = batch_results
        .iter()
        .filter_map(|r| order_map.get(&r.custom_id).map(|&idx| (idx, r)))
        .collect();
    indexed_results.sort_by_key(|(idx, _)| *idx);

    let mut output = RetroflectOutput {
        success: true,
        sessions_analyzed: 0,
        total_candidates: 0,
        total_accepted: 0,
        total_rejected: 0,
        sessions_skipped: 0,
        session_results: Vec::new(),
        error: None,
    };

    // Group by project for dedup state
    let mut sessions_by_project: std::collections::HashMap<PathBuf, Vec<CompoundLearning>> =
        std::collections::HashMap::new();

    let total_count = indexed_results.len();

    for (result_idx, (orig_idx, batch_result)) in indexed_results.iter().enumerate() {
        let (project_path, session) = &all_sessions[*orig_idx];

        let batch_accepted = sessions_by_project.entry(project_path.clone()).or_default();

        let result = match &batch_result.result_type {
            BatchResultType::Succeeded(response_text) => process_batch_result(
                session,
                project_path,
                response_text,
                batch_accepted,
                &config,
            ),
            BatchResultType::Failed(reason) => SessionResult {
                session_id: session.session_id.clone(),
                project_path: project_path.display().to_string(),
                candidates: 0,
                accepted: 0,
                skip_reason: Some(format!("Batch request failed: {}", reason)),
                accepted_summaries: Vec::new(),
            },
        };

        match &result.skip_reason {
            Some(_) => output.sessions_skipped += 1,
            None => {
                output.sessions_analyzed += 1;
                output.total_candidates += result.candidates;
                output.total_accepted += result.accepted;
                output.total_rejected += result.candidates.saturating_sub(result.accepted);
            }
        }

        eprintln!(
            "{}",
            format_progress_line(
                result_idx + 1,
                total_count,
                &result,
                session.user_turns,
                session.file_paths.len(),
                output.total_candidates,
                output.total_accepted,
                false,
            )
        );
        for ls in &result.accepted_summaries {
            eprintln!("      + [{}] {}", ls.category, ls.summary);
        }

        output.session_results.push(result);
    }

    Ok(output)
}

/// Build the user prompt for a session (shared between sequential and batch).
fn build_user_prompt(session: &SessionSummary) -> String {
    format!(
        "## Session Transcript\n\n\
         Session ID: {}\n\
         Project: {}\n\
         User turns: {}\n\
         Tool calls: {}\n\
         Files touched: {}\n\n\
         ---\n\n\
         {}",
        session.session_id,
        session.project_cwd.display(),
        session.user_turns,
        session.tool_calls,
        session.file_paths.join(", "),
        session.condensed_transcript
    )
}

/// Process a single batch result: parse → validate → write.
fn process_batch_result(
    session: &SessionSummary,
    project_path: &Path,
    response_text: &str,
    batch_accepted: &mut Vec<CompoundLearning>,
    config: &Config,
) -> SessionResult {
    if response_text.trim().is_empty() {
        return SessionResult {
            session_id: session.session_id.clone(),
            project_path: project_path.display().to_string(),
            candidates: 0,
            accepted: 0,
            skip_reason: Some("Batch returned empty response".to_string()),
            accepted_summaries: Vec::new(),
        };
    }

    // Parse LLM response
    let candidates: Vec<CandidateLearning> = match parse_llm_response(response_text) {
        Ok(c) => c,
        Err(e) => {
            return SessionResult {
                session_id: session.session_id.clone(),
                project_path: project_path.display().to_string(),
                candidates: 0,
                accepted: 0,
                skip_reason: Some(format!("Failed to parse LLM response: {}", e)),
                accepted_summaries: Vec::new(),
            };
        }
    };

    if candidates.is_empty() {
        return SessionResult {
            session_id: session.session_id.clone(),
            project_path: project_path.display().to_string(),
            candidates: 0,
            accepted: 0,
            skip_reason: None,
            accepted_summaries: Vec::new(),
        };
    }

    // Inject #retroflect tag
    let candidates: Vec<CandidateLearning> = candidates
        .into_iter()
        .map(|mut c| {
            if !c.tags.iter().any(|t| t == "retroflect") {
                c.tags.push("retroflect".to_string());
            }
            c
        })
        .collect();

    let candidate_count = candidates.len();

    // Load existing learnings for dedup
    let backend = MarkdownBackend::new(project_learnings_path(project_path));
    let existing: Vec<CompoundLearning> = backend
        .search(&SearchQuery::new(), &SearchFilters::active_only())
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.learning)
        .collect();

    // Build dedup corpus: existing + batch accepted so far
    let mut dedup_corpus: Vec<CompoundLearning> = existing;
    dedup_corpus.extend(batch_accepted.iter().cloned());

    // Validate with lenient write gate
    let grove_dir = project_grove_dir(project_path);
    let session_id_for_validation = format!("retroflect-{}", session.session_id);

    let (mut valid_learnings, _rejected) = validate_with_duplicates_and_quality_semantic(
        candidates,
        &session_id_for_validation,
        &dedup_corpus,
        WriteGateMode::Lenient,
        QualityCheckMode::from_config(&config.gate.write_gate.quality_check),
        config.gate.write_gate.min_specificity_score,
        None,
        (0.0, 0.0, 0.0),
        Some((&grove_dir, &config.gate.semantic_dedup)),
    );

    // Assign IDs and write
    let ids = backend.next_ids(valid_learnings.len());
    for (learning, id) in valid_learnings.iter_mut().zip(ids) {
        learning.id = id;
    }

    // Ensure .grove/ exists
    if !grove_dir.exists() {
        let _ = init_grove_dir(&grove_dir);
    }

    let mut accepted_count = 0;
    let mut accepted_summaries = Vec::new();
    for learning in &valid_learnings {
        match backend.write(learning) {
            Ok(result) if result.success => {
                accepted_count += 1;
                accepted_summaries.push(AcceptedLearningSummary {
                    category: learning.category.display_name().to_string(),
                    summary: learning.summary.clone(),
                });
            }
            Ok(_) => {
                eprintln!(
                    "Warning: failed to write learning {} (backend reported failure)",
                    learning.id
                );
            }
            Err(e) => {
                eprintln!("Warning: failed to write learning {}: {}", learning.id, e);
            }
        }
    }

    // Add to batch accepted for cross-session dedup
    batch_accepted.extend(valid_learnings);

    // Log stats event
    let stats_path = project_stats_log_path(project_path);
    let stats_logger = StatsLogger::new(&stats_path);
    if let Err(e) = stats_logger.append_retroflect(
        &session_id_for_validation,
        &session.session_id,
        candidate_count,
        accepted_count,
        project_path.display().to_string(),
    ) {
        eprintln!("Warning: failed to log retroflect stats: {}", e);
    }

    SessionResult {
        session_id: session.session_id.clone(),
        project_path: project_path.display().to_string(),
        candidates: candidate_count,
        accepted: accepted_count,
        skip_reason: None,
        accepted_summaries,
    }
}

/// Process a single session: LLM synthesis → validation → write.
#[allow(clippy::too_many_arguments)]
fn process_session(
    session: &SessionSummary,
    project_path: &Path,
    existing: &[CompoundLearning],
    batch_accepted: &mut Vec<CompoundLearning>,
    backend: &MarkdownBackend,
    stats_logger: &StatsLogger,
    config: &Config,
    options: &RetroflectOptions,
    llm_caller: &LlmCaller,
) -> SessionResult {
    // Build user prompt with session transcript
    let user_prompt = format!(
        "## Session Transcript\n\n\
         Session ID: {}\n\
         Project: {}\n\
         User turns: {}\n\
         Tool calls: {}\n\
         Files touched: {}\n\n\
         ---\n\n\
         {}",
        session.session_id,
        session.project_cwd.display(),
        session.user_turns,
        session.tool_calls,
        session.file_paths.join(", "),
        session.condensed_transcript
    );

    // In dry-run mode, skip LLM call entirely
    if options.dry_run {
        return SessionResult {
            session_id: session.session_id.clone(),
            project_path: project_path.display().to_string(),
            candidates: 0,
            accepted: 0,
            skip_reason: None,
            accepted_summaries: Vec::new(),
        };
    }

    // Call LLM
    let response = llm_caller(
        &options.backend,
        &options.model,
        RETROFLECT_SYSTEM_PROMPT,
        &user_prompt,
    );

    let response_text = match response {
        Some(text) if !text.trim().is_empty() => text,
        _ => {
            return SessionResult {
                session_id: session.session_id.clone(),
                project_path: project_path.display().to_string(),
                candidates: 0,
                accepted: 0,
                skip_reason: Some("LLM call failed or returned empty response".to_string()),
                accepted_summaries: Vec::new(),
            };
        }
    };

    // Parse LLM response as JSON array of CandidateLearning
    let candidates: Vec<CandidateLearning> = match parse_llm_response(&response_text) {
        Ok(c) => c,
        Err(e) => {
            return SessionResult {
                session_id: session.session_id.clone(),
                project_path: project_path.display().to_string(),
                candidates: 0,
                accepted: 0,
                skip_reason: Some(format!("Failed to parse LLM response: {}", e)),
                accepted_summaries: Vec::new(),
            };
        }
    };

    if candidates.is_empty() {
        return SessionResult {
            session_id: session.session_id.clone(),
            project_path: project_path.display().to_string(),
            candidates: 0,
            accepted: 0,
            skip_reason: None,
            accepted_summaries: Vec::new(),
        };
    }

    // Inject #retroflect tag if not already present
    let candidates: Vec<CandidateLearning> = candidates
        .into_iter()
        .map(|mut c| {
            if !c.tags.iter().any(|t| t == "retroflect") {
                c.tags.push("retroflect".to_string());
            }
            c
        })
        .collect();

    let candidate_count = candidates.len();

    // Combine existing + batch for dedup corpus
    let mut dedup_corpus: Vec<CompoundLearning> = existing.to_vec();
    dedup_corpus.extend(batch_accepted.iter().cloned());

    // Validate with lenient write gate
    let grove_dir = project_grove_dir(project_path);
    let session_id_for_validation = format!("retroflect-{}", session.session_id);

    let (mut valid_learnings, _rejected) = validate_with_duplicates_and_quality_semantic(
        candidates,
        &session_id_for_validation,
        &dedup_corpus,
        WriteGateMode::Lenient,
        QualityCheckMode::from_config(&config.gate.write_gate.quality_check),
        config.gate.write_gate.min_specificity_score,
        None, // No judge for retroflect (LLM already produced these)
        (0.0, 0.0, 0.0),
        Some((&grove_dir, &config.gate.semantic_dedup)),
    );

    // Assign IDs and write
    let ids = backend.next_ids(valid_learnings.len());
    for (learning, id) in valid_learnings.iter_mut().zip(ids) {
        learning.id = id;
    }

    let mut accepted_count = 0;
    let mut accepted_summaries = Vec::new();
    for learning in &valid_learnings {
        match backend.write(learning) {
            Ok(result) if result.success => {
                accepted_count += 1;
                accepted_summaries.push(AcceptedLearningSummary {
                    category: learning.category.display_name().to_string(),
                    summary: learning.summary.clone(),
                });
            }
            Ok(_) => {
                eprintln!(
                    "Warning: failed to write learning {} (backend reported failure)",
                    learning.id
                );
            }
            Err(e) => {
                eprintln!("Warning: failed to write learning {}: {}", learning.id, e);
            }
        }
    }

    // Add accepted learnings to batch for cross-session dedup
    batch_accepted.extend(valid_learnings);

    // Log retroflect stats event
    if let Err(e) = stats_logger.append_retroflect(
        &session_id_for_validation,
        &session.session_id,
        candidate_count,
        accepted_count,
        project_path.display().to_string(),
    ) {
        eprintln!("Warning: failed to log retroflect stats: {}", e);
    }

    SessionResult {
        session_id: session.session_id.clone(),
        project_path: project_path.display().to_string(),
        candidates: candidate_count,
        accepted: accepted_count,
        skip_reason: None,
        accepted_summaries,
    }
}

// =============================================================================
// Project Discovery
// =============================================================================

/// Discover projects and their sessions based on CLI options.
fn discover_projects(
    options: &RetroflectOptions,
    cwd: &Path,
) -> Result<Vec<ProjectSessions>, String> {
    if options.all {
        discover_all_projects(options)
    } else {
        let project_path = options.project.as_deref().unwrap_or(cwd);

        discover_single_project(project_path, options)
    }
}

/// Discover sessions for a single project.
fn discover_single_project(
    project_path: &Path,
    options: &RetroflectOptions,
) -> Result<Vec<ProjectSessions>, String> {
    let session_dir = find_session_dir(project_path)?;

    let sessions = collect_eligible_sessions(&session_dir, options)?;

    if sessions.is_empty() {
        return Ok(Vec::new());
    }

    Ok(vec![ProjectSessions {
        project_path: project_path.to_path_buf(),
        session_dir,
        sessions,
    }])
}

/// Auto-discover all projects under ~/.claude/projects/.
fn discover_all_projects(options: &RetroflectOptions) -> Result<Vec<ProjectSessions>, String> {
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let projects_dir = home.join(".claude").join("projects");

    if !projects_dir.exists() {
        return Err(format!(
            "Claude projects directory not found: {}",
            projects_dir.display()
        ));
    }

    let mut projects = Vec::new();

    let entries = std::fs::read_dir(&projects_dir)
        .map_err(|e| format!("Cannot read projects directory: {}", e))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        // Read cwd from one JSONL file to determine original project path
        let project_path = match read_project_cwd_from_dir(&path) {
            Some(p) => p,
            None => continue,
        };

        // Auto-init .grove/ if missing (retroflect implies wanting learnings)
        let grove_dir = project_grove_dir(&project_path);
        if !grove_dir.exists() && !options.dry_run {
            if let Err(e) = init_grove_dir(&grove_dir) {
                eprintln!(
                    "Warning: failed to initialize .grove/ at {}: {}",
                    project_path.display(),
                    e
                );
                continue;
            }
            eprintln!("Initialized .grove/ at {}", project_path.display());
        }

        let sessions = match collect_eligible_sessions(&path, options) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Warning: error scanning {}: {}", project_path.display(), e);
                continue;
            }
        };

        if sessions.is_empty() {
            continue;
        }

        projects.push(ProjectSessions {
            project_path,
            session_dir: path,
            sessions,
        });
    }

    Ok(projects)
}

/// Find the Claude Code session directory for a given project path.
///
/// Claude Code encodes project paths as directory names under ~/.claude/projects/
/// by replacing path separators with hyphens. Since this encoding is lossy
/// (/, ., _ all map to -), we cannot reverse it. Instead, we forward-encode
/// the given path and look for a matching directory.
fn find_session_dir(project_path: &Path) -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let projects_dir = home.join(".claude").join("projects");

    if !projects_dir.exists() {
        return Err(format!(
            "Claude projects directory not found: {}",
            projects_dir.display()
        ));
    }

    // Forward-encode: replace all non-alphanumeric with hyphens
    let canonical = project_path
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();

    let session_dir = projects_dir.join(&canonical);
    if session_dir.is_dir() {
        return Ok(session_dir);
    }

    // Fallback: try with absolute path
    let abs_path =
        std::fs::canonicalize(project_path).unwrap_or_else(|_| project_path.to_path_buf());
    let canonical_abs = abs_path
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();

    let session_dir = projects_dir.join(&canonical_abs);
    if session_dir.is_dir() {
        return Ok(session_dir);
    }

    Err(format!(
        "No Claude session directory found for project: {}",
        project_path.display()
    ))
}

/// Collect eligible sessions from a session directory.
fn collect_eligible_sessions(
    session_dir: &Path,
    options: &RetroflectOptions,
) -> Result<Vec<SessionSummary>, String> {
    let entries = std::fs::read_dir(session_dir)
        .map_err(|e| format!("Cannot read session directory: {}", e))?;

    // Load existing retroflect stats events to skip already-analyzed sessions
    let already_retroflected = if !options.force {
        load_retroflected_sessions(session_dir)
    } else {
        std::collections::HashSet::new()
    };

    let mut sessions = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();

        // Only top-level .jsonl files (skip subdirectories = subagent sessions)
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }

        let session_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        // Skip already-retroflected sessions
        if already_retroflected.contains(&session_id) {
            continue;
        }

        // Parse transcript
        let summary = match parse_session_transcript(&path) {
            Some(s) => s,
            None => continue,
        };

        // Filter by min-turns
        if summary.user_turns < options.min_turns {
            continue;
        }

        sessions.push(summary);
    }

    Ok(sessions)
}

/// Read the project cwd from the first parseable JSONL file in a directory.
fn read_project_cwd_from_dir(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }

        let summary = parse_session_transcript(&path)?;
        if summary.project_cwd.as_os_str().is_empty() {
            continue;
        }
        return Some(summary.project_cwd);
    }

    None
}

/// Load the set of Claude session IDs that have already been retroflected.
///
/// Reads from .grove/stats.log of the project associated with this session dir.
fn load_retroflected_sessions(session_dir: &Path) -> std::collections::HashSet<String> {
    let mut retroflected = std::collections::HashSet::new();

    // Determine the project path from one session file
    let project_path = match read_project_cwd_from_dir(session_dir) {
        Some(p) => p,
        None => return retroflected,
    };

    let stats_path = project_stats_log_path(&project_path);
    let stats_logger = StatsLogger::new(&stats_path);
    let events = match stats_logger.read_all() {
        Ok(e) => e,
        Err(_) => return retroflected,
    };

    for event in events {
        if let StatsEventType::Retroflect {
            claude_session_id, ..
        } = event.data
        {
            retroflected.insert(claude_session_id);
        }
    }

    retroflected
}

// =============================================================================
// LLM Response Parsing
// =============================================================================

/// Parse the LLM response into a vector of CandidateLearning.
///
/// Handles both raw JSON arrays and JSON wrapped in markdown code fences.
fn parse_llm_response(response: &str) -> Result<Vec<CandidateLearning>, String> {
    let trimmed = response.trim();

    // Try parsing directly
    if let Ok(candidates) = serde_json::from_str::<Vec<CandidateLearning>>(trimmed) {
        return Ok(candidates);
    }

    // Try extracting from markdown code fence
    let json_str = extract_json_from_markdown(trimmed);
    if let Ok(candidates) = serde_json::from_str::<Vec<CandidateLearning>>(&json_str) {
        return Ok(candidates);
    }

    // Try parsing as a single object (wrap in array)
    if let Ok(candidate) = serde_json::from_str::<CandidateLearning>(trimmed) {
        return Ok(vec![candidate]);
    }

    Err(format!(
        "Cannot parse LLM response as JSON array: {}",
        &trimmed[..100.min(trimmed.len())]
    ))
}

/// Extract JSON content from markdown code fences.
fn extract_json_from_markdown(text: &str) -> String {
    // Look for ```json ... ``` or ``` ... ```
    if let Some(start) = text.find("```json") {
        let after_fence = &text[start + 7..];
        if let Some(end) = after_fence.find("```") {
            return after_fence[..end].trim().to_string();
        }
    }

    if let Some(start) = text.find("```") {
        let after_fence = &text[start + 3..];
        if let Some(end) = after_fence.find("```") {
            return after_fence[..end].trim().to_string();
        }
    }

    text.to_string()
}

// =============================================================================
// Helpers
// =============================================================================

/// Rough token estimate (~4 chars per token).
fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Estimate cost in USD for a given input token count.
///
/// Uses Sonnet-class pricing as a rough estimate:
/// - Input: $3 per 1M tokens
/// - Output: ~$15 per 1M tokens, estimated at ~200 output tokens per session
fn estimate_cost(input_tokens: usize, session_count: usize) -> f64 {
    let input_cost = (input_tokens as f64 / 1_000_000.0) * 3.0;
    let output_tokens = session_count * 200;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * 15.0;
    input_cost + output_cost
}

/// Prompt the user for confirmation on stderr/stdin. Returns true if confirmed.
fn confirm_prompt(message: &str) -> bool {
    eprint!("{} [y/N] ", message);
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Initialize a minimal .grove/ directory.
fn init_grove_dir(grove_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(grove_dir)
        .map_err(|e| format!("Failed to create {}: {}", grove_dir.display(), e))?;

    let learnings_path = grove_dir.join("learnings.md");
    if !learnings_path.exists() {
        std::fs::write(
            &learnings_path,
            "# Compound Learnings\n\n<!-- Grove learnings file -->\n",
        )
        .map_err(|e| format!("Failed to create learnings.md: {}", e))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_llm_response_valid_array() {
        let json = r#"[
            {
                "category": "pitfall",
                "summary": "Test summary",
                "detail": "Test detail with enough characters to pass validation",
                "scope": "project",
                "confidence": "high",
                "criteria_met": ["behavior-changing"],
                "tags": ["test", "retroflect"]
            }
        ]"#;
        let result = parse_llm_response(json);
        assert!(result.is_ok());
        let candidates = result.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].category, "pitfall");
    }

    #[test]
    fn parse_llm_response_empty_array() {
        let result = parse_llm_response("[]");
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn parse_llm_response_markdown_fence() {
        let json = r#"Here are the learnings:

```json
[
    {
        "category": "pattern",
        "summary": "Use builder pattern",
        "detail": "Builder pattern works well for complex config",
        "criteria_met": ["behavior-changing"],
        "tags": ["retroflect"]
    }
]
```"#;
        let result = parse_llm_response(json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn parse_llm_response_single_object() {
        let json = r#"{
            "category": "debugging",
            "summary": "Check connection pools",
            "detail": "Connection pool exhaustion causes query hangs",
            "criteria_met": ["behavior-changing"],
            "tags": ["retroflect"]
        }"#;
        let result = parse_llm_response(json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn parse_llm_response_invalid_json() {
        let result = parse_llm_response("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn extract_json_from_markdown_with_json_fence() {
        let input = "Some text\n```json\n[1, 2, 3]\n```\nMore text";
        assert_eq!(extract_json_from_markdown(input), "[1, 2, 3]");
    }

    #[test]
    fn extract_json_from_markdown_with_plain_fence() {
        let input = "```\n{\"key\": \"value\"}\n```";
        assert_eq!(extract_json_from_markdown(input), "{\"key\": \"value\"}");
    }

    #[test]
    fn extract_json_from_markdown_no_fence() {
        let input = "[1, 2, 3]";
        assert_eq!(extract_json_from_markdown(input), "[1, 2, 3]");
    }

    #[test]
    fn estimate_tokens_rough() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("a".repeat(100).as_str()), 25);
    }

    #[test]
    fn retroflect_tag_injection() {
        let candidate = CandidateLearning {
            category: "pattern".to_string(),
            summary: "Test".to_string(),
            detail: "Test detail".to_string(),
            scope: "project".to_string(),
            confidence: "high".to_string(),
            criteria_met: vec!["behavior-changing".to_string()],
            tags: vec!["rust".to_string()],
            context_files: None,
            relevance_context: None,
        };

        // Simulate the tag injection logic
        let mut tags = candidate.tags.clone();
        if !tags.iter().any(|t| t == "retroflect") {
            tags.push("retroflect".to_string());
        }
        assert!(tags.contains(&"retroflect".to_string()));
    }

    #[test]
    fn retroflect_tag_no_duplicate() {
        let candidate = CandidateLearning {
            category: "pattern".to_string(),
            summary: "Test".to_string(),
            detail: "Test detail".to_string(),
            scope: "project".to_string(),
            confidence: "high".to_string(),
            criteria_met: vec!["behavior-changing".to_string()],
            tags: vec!["retroflect".to_string()],
            context_files: None,
            relevance_context: None,
        };

        let mut tags = candidate.tags.clone();
        if !tags.iter().any(|t| t == "retroflect") {
            tags.push("retroflect".to_string());
        }
        assert_eq!(tags.iter().filter(|t| *t == "retroflect").count(), 1);
    }

    #[test]
    fn format_output_json() {
        let output = RetroflectOutput {
            success: true,
            sessions_analyzed: 3,
            total_candidates: 8,
            total_accepted: 5,
            total_rejected: 3,
            sessions_skipped: 1,
            session_results: Vec::new(),
            error: None,
        };
        let options = RetroflectOptions {
            json: true,
            ..Default::default()
        };
        let formatted = format_output(&output, &options);
        assert!(formatted.contains("\"sessions_analyzed\": 3"));
    }

    #[test]
    fn format_output_text() {
        let output = RetroflectOutput {
            success: true,
            sessions_analyzed: 3,
            total_candidates: 8,
            total_accepted: 5,
            total_rejected: 3,
            sessions_skipped: 0,
            session_results: Vec::new(),
            error: None,
        };
        let options = RetroflectOptions::default();
        let formatted = format_output(&output, &options);
        assert!(formatted.contains("Retroflect complete"));
        assert!(formatted.contains("Sessions analyzed: 3"));
    }

    #[test]
    fn format_output_error() {
        let output = RetroflectOutput {
            success: false,
            sessions_analyzed: 0,
            total_candidates: 0,
            total_accepted: 0,
            total_rejected: 0,
            sessions_skipped: 0,
            session_results: Vec::new(),
            error: Some("something went wrong".to_string()),
        };
        let options = RetroflectOptions::default();
        let formatted = format_output(&output, &options);
        assert!(formatted.contains("Error: something went wrong"));
    }

    // =========================================================================
    // Filtering Tests (7.5)
    // =========================================================================

    fn write_session_jsonl(dir: &Path, session_id: &str, user_turns: usize, cwd: &str) {
        let path = dir.join(format!("{}.jsonl", session_id));
        let mut lines = Vec::new();
        for i in 0..user_turns {
            lines.push(format!(
                r#"{{"type":"user","cwd":"{}","message":{{"content":[{{"type":"text","text":"turn {}"}}]}}}}"#,
                cwd, i
            ));
            lines.push(format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"response {}"}}]}}}}"#,
                i
            ));
        }
        std::fs::write(&path, lines.join("\n")).unwrap();
    }

    #[test]
    fn skips_sessions_below_min_turns() {
        let dir = tempfile::TempDir::new().unwrap();
        write_session_jsonl(dir.path(), "short-session", 2, "/dev/project");

        let options = RetroflectOptions {
            min_turns: 3,
            ..Default::default()
        };
        let sessions = collect_eligible_sessions(dir.path(), &options).unwrap();
        assert!(sessions.is_empty(), "should skip session with only 2 turns");
    }

    #[test]
    fn includes_sessions_at_min_turns() {
        let dir = tempfile::TempDir::new().unwrap();
        write_session_jsonl(dir.path(), "ok-session", 3, "/dev/project");

        let options = RetroflectOptions {
            min_turns: 3,
            ..Default::default()
        };
        let sessions = collect_eligible_sessions(dir.path(), &options).unwrap();
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn skips_subagent_session_files() {
        let dir = tempfile::TempDir::new().unwrap();

        // Top-level file should be included
        write_session_jsonl(dir.path(), "top-level", 5, "/dev/project");

        // Subdir file should be excluded
        let subdir = dir.path().join("subagent");
        std::fs::create_dir_all(&subdir).unwrap();
        write_session_jsonl(&subdir, "sub-session", 5, "/dev/project");

        let options = RetroflectOptions {
            min_turns: 1,
            ..Default::default()
        };
        let sessions = collect_eligible_sessions(dir.path(), &options).unwrap();
        assert_eq!(sessions.len(), 1, "only top-level session should be found");
        assert_eq!(sessions[0].session_id, "top-level");
    }

    #[test]
    fn skips_non_jsonl_files() {
        let dir = tempfile::TempDir::new().unwrap();
        write_session_jsonl(dir.path(), "valid", 5, "/dev/project");

        // Write a non-jsonl file
        std::fs::write(dir.path().join("notes.txt"), "some notes").unwrap();

        let options = RetroflectOptions {
            min_turns: 1,
            ..Default::default()
        };
        let sessions = collect_eligible_sessions(dir.path(), &options).unwrap();
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn skips_already_retroflected_sessions() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = dir.path().join("project");
        let session_dir = dir.path().join("sessions");
        std::fs::create_dir_all(project_dir.join(".grove")).unwrap();
        std::fs::create_dir_all(&session_dir).unwrap();

        // Write a session that points to our project
        write_session_jsonl(
            &session_dir,
            "already-done",
            5,
            &project_dir.to_string_lossy(),
        );

        // Write a retroflect stats event for this session
        let stats_path = project_dir.join(".grove").join("stats.log");
        let stats_logger = StatsLogger::new(&stats_path);
        stats_logger
            .append_retroflect(
                "retroflect-already-done",
                "already-done",
                3,
                2,
                project_dir.display().to_string(),
            )
            .unwrap();

        let options = RetroflectOptions {
            min_turns: 1,
            force: false,
            ..Default::default()
        };
        let sessions = collect_eligible_sessions(&session_dir, &options).unwrap();
        assert!(
            sessions.is_empty(),
            "should skip already-retroflected session"
        );
    }

    #[test]
    fn force_overrides_already_retroflected() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = dir.path().join("project");
        let session_dir = dir.path().join("sessions");
        std::fs::create_dir_all(project_dir.join(".grove")).unwrap();
        std::fs::create_dir_all(&session_dir).unwrap();

        write_session_jsonl(
            &session_dir,
            "already-done",
            5,
            &project_dir.to_string_lossy(),
        );

        let stats_path = project_dir.join(".grove").join("stats.log");
        let stats_logger = StatsLogger::new(&stats_path);
        stats_logger
            .append_retroflect(
                "retroflect-already-done",
                "already-done",
                3,
                2,
                project_dir.display().to_string(),
            )
            .unwrap();

        let options = RetroflectOptions {
            min_turns: 1,
            force: true,
            ..Default::default()
        };
        let sessions = collect_eligible_sessions(&session_dir, &options).unwrap();
        assert_eq!(sessions.len(), 1, "force should override skip");
    }

    // =========================================================================
    // Init Tests (7.9)
    // =========================================================================

    #[test]
    fn init_grove_dir_creates_structure() {
        let dir = tempfile::TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");

        assert!(!grove_dir.exists());
        init_grove_dir(&grove_dir).unwrap();
        assert!(grove_dir.exists());
        assert!(grove_dir.join("learnings.md").exists());

        let content = std::fs::read_to_string(grove_dir.join("learnings.md")).unwrap();
        assert!(content.contains("Compound Learnings"));
    }

    #[test]
    fn init_grove_dir_idempotent() {
        let dir = tempfile::TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");

        init_grove_dir(&grove_dir).unwrap();
        // Write something to learnings.md
        std::fs::write(
            grove_dir.join("learnings.md"),
            "# Custom header\n\n### [cl_001] Test\n",
        )
        .unwrap();

        // Re-init should NOT overwrite existing learnings.md
        init_grove_dir(&grove_dir).unwrap();
        let content = std::fs::read_to_string(grove_dir.join("learnings.md")).unwrap();
        assert!(content.contains("Custom header"), "should not overwrite");
    }

    // =========================================================================
    // Stats Event Tests (7.10)
    // =========================================================================

    #[test]
    fn logs_retroflect_event_with_correct_fields() {
        let dir = tempfile::TempDir::new().unwrap();
        let stats_path = dir.path().join("stats.log");
        let logger = StatsLogger::new(&stats_path);

        logger
            .append_retroflect(
                "retroflect-abc",
                "abc-session-uuid",
                4,
                2,
                "/home/dev/project",
            )
            .unwrap();

        let events = logger.read_all().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data.event_name(), "retroflect");

        if let StatsEventType::Retroflect {
            session_id,
            claude_session_id,
            candidates,
            accepted,
            project_path,
        } = &events[0].data
        {
            assert_eq!(session_id, "retroflect-abc");
            assert_eq!(claude_session_id, "abc-session-uuid");
            assert_eq!(*candidates, 4);
            assert_eq!(*accepted, 2);
            assert_eq!(project_path, "/home/dev/project");
        } else {
            panic!("Expected Retroflect event");
        }
    }

    #[test]
    fn parses_retroflect_event_from_jsonl() {
        let jsonl = r#"{"v":1,"ts":"2026-02-06T12:00:00Z","event":"retroflect","session_id":"ret-1","claude_session_id":"550e8400","candidates":4,"accepted":2,"project_path":"/dev/project"}"#;
        let event: crate::stats::StatsEvent = serde_json::from_str(jsonl).unwrap();
        assert_eq!(event.data.event_name(), "retroflect");

        if let StatsEventType::Retroflect {
            claude_session_id,
            candidates,
            accepted,
            ..
        } = &event.data
        {
            assert_eq!(claude_session_id, "550e8400");
            assert_eq!(*candidates, 4);
            assert_eq!(*accepted, 2);
        } else {
            panic!("Expected Retroflect event");
        }
    }

    // =========================================================================
    // Discovery Tests (7.4)
    // =========================================================================

    #[test]
    fn read_project_cwd_from_dir_finds_cwd() {
        let dir = tempfile::TempDir::new().unwrap();
        write_session_jsonl(dir.path(), "session-1", 3, "/home/dev/my-project");

        let cwd = read_project_cwd_from_dir(dir.path());
        assert!(cwd.is_some());
        assert_eq!(cwd.unwrap(), PathBuf::from("/home/dev/my-project"));
    }

    #[test]
    fn read_project_cwd_from_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let cwd = read_project_cwd_from_dir(dir.path());
        assert!(cwd.is_none());
    }

    #[test]
    fn load_retroflected_sessions_empty_stats() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = load_retroflected_sessions(dir.path());
        assert!(result.is_empty());
    }

    // =========================================================================
    // Session Results in Output (7.11)
    // =========================================================================

    #[test]
    fn format_output_with_session_results() {
        let output = RetroflectOutput {
            success: true,
            sessions_analyzed: 2,
            total_candidates: 5,
            total_accepted: 3,
            total_rejected: 2,
            sessions_skipped: 1,
            session_results: vec![
                SessionResult {
                    session_id: "abc-12345-long-uuid".to_string(),
                    project_path: "/dev/project-a".to_string(),
                    candidates: 3,
                    accepted: 2,
                    skip_reason: None,
                    accepted_summaries: Vec::new(),
                },
                SessionResult {
                    session_id: "def-67890-long-uuid".to_string(),
                    project_path: "/dev/project-a".to_string(),
                    candidates: 2,
                    accepted: 1,
                    skip_reason: None,
                    accepted_summaries: Vec::new(),
                },
                SessionResult {
                    session_id: "ghi-skipped-uuid".to_string(),
                    project_path: "/dev/project-b".to_string(),
                    candidates: 0,
                    accepted: 0,
                    skip_reason: Some("LLM call failed".to_string()),
                    accepted_summaries: Vec::new(),
                },
            ],
            error: None,
        };
        let options = RetroflectOptions::default();
        let formatted = format_output(&output, &options);
        assert!(formatted.contains("abc-1234"));
        assert!(formatted.contains("3 candidates, 2 accepted"));
        assert!(formatted.contains("skipped: LLM call failed"));
        // Error breakdown should appear
        assert!(formatted.contains("Sessions skipped:  1"));
        assert!(formatted.contains("1 LLM failures"));
    }

    #[test]
    fn format_output_dry_run() {
        let output = RetroflectOutput {
            success: true,
            sessions_analyzed: 3,
            total_candidates: 8,
            total_accepted: 5,
            total_rejected: 3,
            sessions_skipped: 0,
            session_results: Vec::new(),
            error: None,
        };
        let options = RetroflectOptions {
            dry_run: true,
            ..Default::default()
        };
        let formatted = format_output(&output, &options);
        assert!(formatted.contains("dry run"));
    }

    // =========================================================================
    // Default Options Test
    // =========================================================================

    #[test]
    fn default_options_match_design_spec() {
        let options = RetroflectOptions::default();
        assert_eq!(options.limit, 20);
        assert_eq!(options.min_turns, 3);
        assert_eq!(options.backend, "api");
        assert!(!options.all);
        assert!(!options.dry_run);
        assert!(!options.force);
        assert!(!options.yes);
        assert!(!options.json);
    }

    // =========================================================================
    // Process Session Tests (orchestration layer)
    // =========================================================================

    fn make_test_session(id: &str, project_cwd: &Path) -> SessionSummary {
        SessionSummary {
            session_id: id.to_string(),
            project_cwd: project_cwd.to_path_buf(),
            timestamp: None,
            user_turns: 10,
            tool_calls: 5,
            file_paths: vec!["src/main.rs".to_string()],
            condensed_transcript: "User: fix the bug\nAssistant: I fixed the bug in main.rs"
                .to_string(),
        }
    }

    fn mock_llm_json_response() -> String {
        r#"[
            {
                "category": "pitfall",
                "summary": "Avoid unwrap in error paths",
                "detail": "Using unwrap in error handling code causes panics instead of graceful failure. Use proper error propagation with the ? operator or match expressions.",
                "scope": "project",
                "confidence": "high",
                "criteria_met": ["behavior-changing"],
                "tags": ["testing"]
            },
            {
                "category": "pattern",
                "summary": "Use builder pattern for complex configs",
                "detail": "Builder pattern with method chaining provides a clean API for constructing complex configuration objects with many optional fields.",
                "scope": "project",
                "confidence": "medium",
                "criteria_met": ["decision-rationale"],
                "tags": ["architecture"]
            }
        ]"#
        .to_string()
    }

    fn setup_project_with_grove(dir: &Path) -> PathBuf {
        let project_dir = dir.join("project");
        let grove_dir = project_dir.join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();
        std::fs::write(
            grove_dir.join("learnings.md"),
            "# Compound Learnings\n\n<!-- Grove learnings file -->\n",
        )
        .unwrap();
        project_dir
    }

    fn make_backend(project_dir: &Path) -> MarkdownBackend {
        MarkdownBackend::new(project_learnings_path(project_dir))
    }

    #[test]
    fn process_session_dry_run_skips_llm() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-dry-run", &project_dir);
        let backend = make_backend(&project_dir);
        let stats_logger = StatsLogger::new(project_dir.join(".grove/stats.log"));
        let config = Config::default();
        let options = RetroflectOptions {
            dry_run: true,
            ..Default::default()
        };
        let mut batch_accepted = Vec::new();

        // LLM caller that panics if invoked — dry run must not call it
        let panic_caller = |_: &str, _: &str, _: &str, _: &str| -> Option<String> {
            panic!("LLM should not be called in dry-run mode");
        };

        let result = process_session(
            &session,
            &project_dir,
            &[],
            &mut batch_accepted,
            &backend,
            &stats_logger,
            &config,
            &options,
            &panic_caller,
        );

        assert!(result.skip_reason.is_none());
        assert_eq!(result.candidates, 0);
    }

    #[test]
    fn process_session_llm_failure_returns_skip() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-llm-fail", &project_dir);
        let backend = make_backend(&project_dir);
        let stats_logger = StatsLogger::new(project_dir.join(".grove/stats.log"));
        let config = Config::default();
        let options = RetroflectOptions::default();
        let mut batch_accepted = Vec::new();

        let failing_caller = |_: &str, _: &str, _: &str, _: &str| -> Option<String> { None };

        let result = process_session(
            &session,
            &project_dir,
            &[],
            &mut batch_accepted,
            &backend,
            &stats_logger,
            &config,
            &options,
            &failing_caller,
        );

        assert!(result.skip_reason.is_some());
        assert!(result.skip_reason.unwrap().contains("LLM call failed"));
    }

    #[test]
    fn process_session_empty_response_returns_skip() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-empty", &project_dir);
        let backend = make_backend(&project_dir);
        let stats_logger = StatsLogger::new(project_dir.join(".grove/stats.log"));
        let config = Config::default();
        let options = RetroflectOptions::default();
        let mut batch_accepted = Vec::new();

        let empty_caller =
            |_: &str, _: &str, _: &str, _: &str| -> Option<String> { Some("  ".to_string()) };

        let result = process_session(
            &session,
            &project_dir,
            &[],
            &mut batch_accepted,
            &backend,
            &stats_logger,
            &config,
            &options,
            &empty_caller,
        );

        assert!(result.skip_reason.is_some());
        assert!(result.skip_reason.unwrap().contains("LLM call failed"));
    }

    #[test]
    fn process_session_malformed_json_returns_skip() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-bad-json", &project_dir);
        let backend = make_backend(&project_dir);
        let stats_logger = StatsLogger::new(project_dir.join(".grove/stats.log"));
        let config = Config::default();
        let options = RetroflectOptions::default();
        let mut batch_accepted = Vec::new();

        let bad_json_caller = |_: &str, _: &str, _: &str, _: &str| -> Option<String> {
            Some("This is not valid JSON at all".to_string())
        };

        let result = process_session(
            &session,
            &project_dir,
            &[],
            &mut batch_accepted,
            &backend,
            &stats_logger,
            &config,
            &options,
            &bad_json_caller,
        );

        assert!(result.skip_reason.is_some());
        assert!(result
            .skip_reason
            .unwrap()
            .contains("Failed to parse LLM response"));
    }

    #[test]
    fn process_session_valid_candidates_accepted() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-valid", &project_dir);
        let backend = make_backend(&project_dir);
        let stats_logger = StatsLogger::new(project_dir.join(".grove/stats.log"));
        let config = Config::default();
        let options = RetroflectOptions::default();
        let mut batch_accepted = Vec::new();

        let mock_response = mock_llm_json_response();
        let valid_caller = move |_: &str, _: &str, _: &str, _: &str| -> Option<String> {
            Some(mock_response.clone())
        };

        let result = process_session(
            &session,
            &project_dir,
            &[],
            &mut batch_accepted,
            &backend,
            &stats_logger,
            &config,
            &options,
            &valid_caller,
        );

        assert!(result.skip_reason.is_none());
        assert_eq!(result.candidates, 2);
        assert!(
            result.accepted > 0,
            "at least one candidate should be accepted"
        );
    }

    #[test]
    fn process_session_injects_retroflect_tag() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-tag-inject", &project_dir);
        let backend = make_backend(&project_dir);
        let stats_logger = StatsLogger::new(project_dir.join(".grove/stats.log"));
        let config = Config::default();
        let options = RetroflectOptions::default();
        let mut batch_accepted = Vec::new();

        // Response WITHOUT #retroflect tag — should be injected
        let response = r#"[{
            "category": "pitfall",
            "summary": "Always check error returns",
            "detail": "Functions that return Result should always have their errors checked rather than silently ignored, to prevent subtle runtime failures.",
            "scope": "project",
            "confidence": "high",
            "criteria_met": ["behavior-changing"],
            "tags": ["error-handling"]
        }]"#
        .to_string();

        let tag_caller =
            move |_: &str, _: &str, _: &str, _: &str| -> Option<String> { Some(response.clone()) };

        let result = process_session(
            &session,
            &project_dir,
            &[],
            &mut batch_accepted,
            &backend,
            &stats_logger,
            &config,
            &options,
            &tag_caller,
        );

        assert!(result.skip_reason.is_none());
        // Verify the written learning has the retroflect tag
        if result.accepted > 0 {
            assert!(
                batch_accepted
                    .iter()
                    .any(|l| l.tags.iter().any(|t| t == "retroflect")),
                "accepted learnings should have #retroflect tag"
            );
        }
    }

    #[test]
    fn process_session_logs_stats_event() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-stats-log", &project_dir);
        let backend = make_backend(&project_dir);
        let stats_path = project_dir.join(".grove/stats.log");
        let stats_logger = StatsLogger::new(&stats_path);
        let config = Config::default();
        let options = RetroflectOptions::default();
        let mut batch_accepted = Vec::new();

        let mock_response = mock_llm_json_response();
        let caller = move |_: &str, _: &str, _: &str, _: &str| -> Option<String> {
            Some(mock_response.clone())
        };

        let _result = process_session(
            &session,
            &project_dir,
            &[],
            &mut batch_accepted,
            &backend,
            &stats_logger,
            &config,
            &options,
            &caller,
        );

        // Verify stats event was logged
        let events = stats_logger.read_all().unwrap();
        let retroflect_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.data, StatsEventType::Retroflect { .. }))
            .collect();
        assert_eq!(
            retroflect_events.len(),
            1,
            "should log exactly one retroflect event"
        );

        if let StatsEventType::Retroflect {
            claude_session_id,
            candidates,
            ..
        } = &retroflect_events[0].data
        {
            assert_eq!(claude_session_id, "test-stats-log");
            assert_eq!(*candidates, 2);
        }
    }

    #[test]
    fn process_session_empty_candidates_no_stats() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-empty-candidates", &project_dir);
        let backend = make_backend(&project_dir);
        let stats_path = project_dir.join(".grove/stats.log");
        let stats_logger = StatsLogger::new(&stats_path);
        let config = Config::default();
        let options = RetroflectOptions::default();
        let mut batch_accepted = Vec::new();

        // LLM returns empty array — no candidates
        let caller =
            |_: &str, _: &str, _: &str, _: &str| -> Option<String> { Some("[]".to_string()) };

        let result = process_session(
            &session,
            &project_dir,
            &[],
            &mut batch_accepted,
            &backend,
            &stats_logger,
            &config,
            &options,
            &caller,
        );

        assert!(result.skip_reason.is_none());
        assert_eq!(result.candidates, 0);
        assert_eq!(result.accepted, 0);
        // No stats event should be logged for empty candidates
        let events = stats_logger.read_all().unwrap_or_default();
        assert!(events.is_empty(), "no stats event for empty candidates");
    }

    // =========================================================================
    // Run Inner Tests (end-to-end orchestration)
    // =========================================================================

    #[test]
    fn run_inner_dry_run_counts_sessions() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());

        // Create a fake session dir matching the project path encoding
        let home = dirs::home_dir().unwrap();
        let projects_dir = home.join(".claude/projects");
        let encoded = project_dir
            .to_string_lossy()
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let session_dir = projects_dir.join(&encoded);

        // Skip if we can't create the session dir (e.g., permissions)
        if std::fs::create_dir_all(&session_dir).is_err() {
            return;
        }

        // Write 3 eligible sessions
        write_session_jsonl(&session_dir, "sess-001", 5, &project_dir.to_string_lossy());
        write_session_jsonl(&session_dir, "sess-002", 5, &project_dir.to_string_lossy());
        write_session_jsonl(&session_dir, "sess-003", 5, &project_dir.to_string_lossy());

        let options = RetroflectOptions {
            project: Some(project_dir.clone()),
            dry_run: true,
            min_turns: 1,
            limit: 100,
            yes: true,
            ..Default::default()
        };

        let panic_caller = |_: &str, _: &str, _: &str, _: &str| -> Option<String> {
            panic!("LLM should not be called in dry-run mode");
        };

        let result = run_inner(&options, &project_dir, &panic_caller);

        // Clean up session dir
        let _ = std::fs::remove_dir_all(&session_dir);

        let output = result.unwrap();
        assert_eq!(output.sessions_analyzed, 3);
        assert_eq!(output.sessions_skipped, 0);
    }

    #[test]
    fn run_inner_respects_limit() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());

        let home = dirs::home_dir().unwrap();
        let projects_dir = home.join(".claude/projects");
        let encoded = project_dir
            .to_string_lossy()
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let session_dir = projects_dir.join(&encoded);

        if std::fs::create_dir_all(&session_dir).is_err() {
            return;
        }

        // Write 5 sessions
        for i in 0..5 {
            write_session_jsonl(
                &session_dir,
                &format!("limit-sess-{:03}", i),
                5,
                &project_dir.to_string_lossy(),
            );
        }

        let options = RetroflectOptions {
            project: Some(project_dir.clone()),
            dry_run: true,
            min_turns: 1,
            limit: 2,
            yes: true,
            ..Default::default()
        };

        let noop_caller = |_: &str, _: &str, _: &str, _: &str| -> Option<String> { None };

        let result = run_inner(&options, &project_dir, &noop_caller);

        // Clean up
        let _ = std::fs::remove_dir_all(&session_dir);

        let output = result.unwrap();
        // With dry_run, limit=2: should only process 2 sessions
        assert_eq!(
            output.sessions_analyzed + output.sessions_skipped,
            2,
            "should respect limit of 2"
        );
    }

    // =========================================================================
    // Progress Output Tests
    // =========================================================================

    #[test]
    fn format_progress_line_normal_session() {
        let result = SessionResult {
            session_id: "abc12345-long-uuid".to_string(),
            project_path: "/dev/test".to_string(),
            candidates: 3,
            accepted: 2,
            skip_reason: None,
            accepted_summaries: Vec::new(),
        };
        let line = format_progress_line(3, 71, &result, 0, 0, 8, 5, false);
        assert!(line.contains("[3/71]"));
        assert!(line.contains("abc12345"));
        assert!(line.contains("3 candidates, 2 accepted"));
        assert!(line.contains("total: 8 found, 5 accepted"));
    }

    #[test]
    fn format_progress_line_skipped() {
        let result = SessionResult {
            session_id: "skip1234-uuid".to_string(),
            project_path: "/dev/test".to_string(),
            candidates: 0,
            accepted: 0,
            skip_reason: Some("LLM call failed".to_string()),
            accepted_summaries: Vec::new(),
        };
        let line = format_progress_line(1, 10, &result, 0, 0, 5, 3, false);
        assert!(line.contains("[1/10]"));
        assert!(line.contains("skipped: LLM call failed"));
        assert!(line.contains("total: 5 found, 3 accepted"));
    }

    #[test]
    fn format_progress_line_dry_run() {
        let result = SessionResult {
            session_id: "dry12345-uuid".to_string(),
            project_path: "/dev/test".to_string(),
            candidates: 0,
            accepted: 0,
            skip_reason: None,
            accepted_summaries: Vec::new(),
        };
        let line = format_progress_line(2, 5, &result, 10, 3, 0, 0, true);
        assert!(line.contains("[2/5]"));
        assert!(line.contains("eligible"));
        assert!(line.contains("10 turns"));
        assert!(line.contains("3 files"));
        // Dry-run should NOT show running totals
        assert!(!line.contains("total:"));
    }

    #[test]
    fn format_progress_line_no_learnings() {
        let result = SessionResult {
            session_id: "nope1234-uuid".to_string(),
            project_path: "/dev/test".to_string(),
            candidates: 0,
            accepted: 0,
            skip_reason: None,
            accepted_summaries: Vec::new(),
        };
        let line = format_progress_line(5, 20, &result, 0, 0, 10, 7, false);
        assert!(line.contains("[5/20]"));
        assert!(line.contains("no learnings found"));
        assert!(line.contains("total: 10 found, 7 accepted"));
    }

    #[test]
    fn categorize_skip_reason_llm_failure() {
        assert_eq!(
            categorize_skip_reason("LLM call failed or returned empty response"),
            "LLM failures"
        );
    }

    #[test]
    fn categorize_skip_reason_parse_error() {
        assert_eq!(
            categorize_skip_reason("Failed to parse LLM response: invalid JSON"),
            "parse errors"
        );
    }

    #[test]
    fn categorize_skip_reason_other() {
        assert_eq!(
            categorize_skip_reason("something unexpected happened"),
            "other errors"
        );
    }

    #[test]
    fn format_output_error_breakdown() {
        let output = RetroflectOutput {
            success: true,
            sessions_analyzed: 2,
            total_candidates: 5,
            total_accepted: 3,
            total_rejected: 2,
            sessions_skipped: 3,
            session_results: vec![
                SessionResult {
                    session_id: "s1".to_string(),
                    project_path: "/p".to_string(),
                    candidates: 0,
                    accepted: 0,
                    skip_reason: Some("LLM call failed or returned empty response".to_string()),
                    accepted_summaries: Vec::new(),
                },
                SessionResult {
                    session_id: "s2".to_string(),
                    project_path: "/p".to_string(),
                    candidates: 0,
                    accepted: 0,
                    skip_reason: Some("LLM call failed or returned empty response".to_string()),
                    accepted_summaries: Vec::new(),
                },
                SessionResult {
                    session_id: "s3".to_string(),
                    project_path: "/p".to_string(),
                    candidates: 0,
                    accepted: 0,
                    skip_reason: Some("Failed to parse LLM response: bad json".to_string()),
                    accepted_summaries: Vec::new(),
                },
            ],
            error: None,
        };
        let options = RetroflectOptions::default();
        let formatted = format_output(&output, &options);
        assert!(formatted.contains("Sessions skipped:  3"));
        assert!(formatted.contains("2 LLM failures"));
        assert!(formatted.contains("1 parse errors"));
    }

    #[test]
    fn format_output_no_breakdown_when_zero_skipped() {
        let output = RetroflectOutput {
            success: true,
            sessions_analyzed: 5,
            total_candidates: 10,
            total_accepted: 8,
            total_rejected: 2,
            sessions_skipped: 0,
            session_results: Vec::new(),
            error: None,
        };
        let options = RetroflectOptions::default();
        let formatted = format_output(&output, &options);
        assert!(!formatted.contains("Sessions skipped"));
        assert!(!formatted.contains("LLM failures"));
    }

    #[test]
    fn accepted_summaries_not_serialized() {
        let result = SessionResult {
            session_id: "test".to_string(),
            project_path: "/dev/test".to_string(),
            candidates: 2,
            accepted: 1,
            skip_reason: None,
            accepted_summaries: vec![AcceptedLearningSummary {
                category: "Pitfall".to_string(),
                summary: "Avoid unwrap in production".to_string(),
            }],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("accepted_summaries"));
        assert!(!json.contains("Avoid unwrap"));
    }

    #[test]
    fn process_session_populates_accepted_summaries() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-summaries", &project_dir);
        let backend = make_backend(&project_dir);
        let stats_logger = StatsLogger::new(project_dir.join(".grove/stats.log"));
        let config = Config::default();
        let options = RetroflectOptions::default();
        let mut batch_accepted = Vec::new();

        let mock_response = mock_llm_json_response();
        let caller = move |_: &str, _: &str, _: &str, _: &str| -> Option<String> {
            Some(mock_response.clone())
        };

        let result = process_session(
            &session,
            &project_dir,
            &[],
            &mut batch_accepted,
            &backend,
            &stats_logger,
            &config,
            &options,
            &caller,
        );

        assert!(result.skip_reason.is_none());
        assert!(result.accepted > 0);
        assert_eq!(result.accepted_summaries.len(), result.accepted);
        for ls in &result.accepted_summaries {
            assert!(!ls.category.is_empty());
            assert!(!ls.summary.is_empty());
        }
    }

    #[test]
    fn process_session_dry_run_empty_summaries() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-dry-summaries", &project_dir);
        let backend = make_backend(&project_dir);
        let stats_logger = StatsLogger::new(project_dir.join(".grove/stats.log"));
        let config = Config::default();
        let options = RetroflectOptions {
            dry_run: true,
            ..Default::default()
        };
        let mut batch_accepted = Vec::new();

        let panic_caller = |_: &str, _: &str, _: &str, _: &str| -> Option<String> {
            panic!("LLM should not be called in dry-run mode");
        };

        let result = process_session(
            &session,
            &project_dir,
            &[],
            &mut batch_accepted,
            &backend,
            &stats_logger,
            &config,
            &options,
            &panic_caller,
        );

        assert!(result.accepted_summaries.is_empty());
    }

    // =========================================================================
    // Batch Mode Tests
    // =========================================================================

    #[test]
    fn default_options_batch_is_false() {
        let options = RetroflectOptions::default();
        assert!(!options.batch);
    }

    #[test]
    fn build_user_prompt_contains_session_info() {
        let session = make_test_session("test-prompt", &PathBuf::from("/dev/project"));
        let prompt = build_user_prompt(&session);
        assert!(prompt.contains("test-prompt"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("Session Transcript"));
    }

    #[test]
    fn process_batch_result_empty_response() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-batch-empty", &project_dir);
        let config = Config::default();
        let mut batch_accepted = Vec::new();

        let result =
            process_batch_result(&session, &project_dir, "   ", &mut batch_accepted, &config);
        assert!(result.skip_reason.is_some());
        assert!(result.skip_reason.unwrap().contains("empty response"));
    }

    #[test]
    fn process_batch_result_invalid_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-batch-bad-json", &project_dir);
        let config = Config::default();
        let mut batch_accepted = Vec::new();

        let result = process_batch_result(
            &session,
            &project_dir,
            "not valid json",
            &mut batch_accepted,
            &config,
        );
        assert!(result.skip_reason.is_some());
        assert!(result.skip_reason.unwrap().contains("parse"));
    }

    #[test]
    fn process_batch_result_valid_candidates() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-batch-valid", &project_dir);
        let config = Config::default();
        let mut batch_accepted = Vec::new();

        let response = mock_llm_json_response();
        let result = process_batch_result(
            &session,
            &project_dir,
            &response,
            &mut batch_accepted,
            &config,
        );
        assert!(result.skip_reason.is_none());
        assert_eq!(result.candidates, 2);
        assert!(result.accepted > 0);
    }

    #[test]
    fn process_batch_result_cross_session_dedup() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let config = Config::default();
        let mut batch_accepted = Vec::new();

        // Process first session
        let session1 = make_test_session("batch-dedup-1", &project_dir);
        let response = mock_llm_json_response();
        let result1 = process_batch_result(
            &session1,
            &project_dir,
            &response,
            &mut batch_accepted,
            &config,
        );
        let first_accepted = result1.accepted;
        assert!(first_accepted > 0, "first session should accept learnings");

        // Process second session with identical response
        let session2 = make_test_session("batch-dedup-2", &project_dir);
        let result2 = process_batch_result(
            &session2,
            &project_dir,
            &response,
            &mut batch_accepted,
            &config,
        );
        // Second session should accept fewer (or zero) because of dedup
        assert!(
            result2.accepted <= first_accepted,
            "cross-session dedup should reduce duplicates: first={}, second={}",
            first_accepted,
            result2.accepted
        );
    }

    #[test]
    fn process_batch_result_injects_retroflect_tag() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let config = Config::default();
        let mut batch_accepted = Vec::new();

        let response = r#"[{
            "category": "pitfall",
            "summary": "Always check error returns in batch",
            "detail": "Functions returning Result should always have errors checked to prevent silent failures in batch processing.",
            "scope": "project",
            "confidence": "high",
            "criteria_met": ["behavior-changing"],
            "tags": ["error-handling"]
        }]"#;

        let session = make_test_session("batch-tag-test", &project_dir);
        let result = process_batch_result(
            &session,
            &project_dir,
            response,
            &mut batch_accepted,
            &config,
        );
        assert!(result.skip_reason.is_none());
        if result.accepted > 0 {
            assert!(
                batch_accepted
                    .iter()
                    .any(|l| l.tags.iter().any(|t| t == "retroflect")),
                "batch-accepted learnings should have retroflect tag"
            );
        }
    }

    #[test]
    fn process_batch_result_empty_candidates() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = setup_project_with_grove(dir.path());
        let session = make_test_session("test-batch-empty-arr", &project_dir);
        let config = Config::default();
        let mut batch_accepted = Vec::new();

        let result =
            process_batch_result(&session, &project_dir, "[]", &mut batch_accepted, &config);
        assert!(result.skip_reason.is_none());
        assert_eq!(result.candidates, 0);
        assert_eq!(result.accepted, 0);
    }
}
