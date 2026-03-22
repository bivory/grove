# grove - Implementation

This document describes the concrete implementation for all components
described in the [Architecture](./01-architecture.md). It specifies file
layouts, module structure, types, and where each piece lives in the source.

## 1. Project Structure

```text
grove/
├── Cargo.toml
├── src/
│   ├── lib.rs                 # Library root
│   ├── main.rs                # CLI entry point (clap-based)
│   │
│   ├── core/
│   │   ├── mod.rs
│   │   ├── state.rs           # SessionState, GateState, GateStatus
│   │   ├── gate.rs            # Gate state machine, skip evaluation
│   │   ├── reflect.rs         # Reflection parsing, write gate filter
│   │   └── learning.rs        # CompoundLearning, LearningCategory
│   │
│   ├── discovery/
│   │   ├── mod.rs
│   │   ├── tickets.rs         # Ticketing system detection
│   │   └── backends.rs        # Memory backend detection
│   │
│   ├── backends/
│   │   ├── mod.rs
│   │   ├── traits.rs          # MemoryBackend trait
│   │   ├── markdown.rs        # Built-in append-only markdown
│   │   └── total_recall.rs    # Total Recall adapter
│   │
│   ├── stats/
│   │   ├── mod.rs
│   │   ├── tracker.rs         # Per-learning usage tracking
│   │   ├── decay.rs           # Passive decay evaluation
│   │   ├── scoring.rs         # Retrieval composite scoring
│   │   ├── insights.rs        # Pattern detection, recommendations
│   │   └── skip.rs            # Skip decision tracking
│   │
│   ├── storage/
│   │   ├── mod.rs
│   │   ├── traits.rs          # SessionStore trait
│   │   ├── file.rs            # File-based session storage
│   │   └── memory.rs          # In-memory (testing)
│   │
│   ├── hooks/
│   │   ├── mod.rs
│   │   ├── input.rs           # HookInput deserialization
│   │   ├── output.rs          # HookOutput serialization
│   │   └── runner.rs          # Hook dispatch
│   │
│   ├── eval/
│   │   ├── mod.rs             # Offline evaluation harness
│   │   ├── corpus.rs          # Corpus loading (transcripts, learnings)
│   │   ├── judge.rs           # LLM judge (cache, CLI/API backends)
│   │   ├── metrics.rs         # Metrics aggregation and formatting
│   │   └── runner.rs          # Benchmark orchestration
│   │
│   ├── cli/
│   │   ├── mod.rs
│   │   ├── hook.rs            # hook command (runner)
│   │   ├── reflect.rs         # reflect command
│   │   ├── skip.rs            # skip command
│   │   ├── search.rs          # search command
│   │   ├── list.rs            # list command
│   │   ├── stats.rs           # stats command (dashboard)
│   │   ├── maintain.rs        # maintain command
│   │   ├── init.rs            # init command (scaffold)
│   │   ├── backends_cmd.rs    # backends command
│   │   ├── tickets_cmd.rs     # tickets command
│   │   ├── observe.rs         # observe command (subagent logging)
│   │   ├── sessions.rs        # sessions command (list sessions)
│   │   ├── debug.rs           # debug command
│   │   ├── trace.rs           # trace command
│   │   ├── clean.rs           # clean command
│   │   └── retroflect.rs      # retroflect command (retroactive reflection)
│   │
│   ├── llm.rs                 # Shared LLM call infrastructure (CLI + API)
│   ├── config.rs              # Configuration loading
│   └── error.rs               # Error types
│
├── tests/
│   ├── gate_flow.rs           # Gate state machine integration tests
│   ├── reflection_flow.rs     # Reflection + write gate tests
│   ├── discovery_flow.rs      # Ticketing/backend discovery tests
│   ├── stats_flow.rs          # Stats tracking integration tests
│   └── retroflect_flow.rs     # Retroflect session parsing, discovery, dedup
│
├── design/
│   ├── 00-overview.md
│   ├── 01-architecture.md
│   ├── 02-implementation.md
│   ├── 03-stats-and-quality.md
│   ├── 04-test-plan.md
│   └── 05-ci.md
│
├── .claude-plugin/
│   ├── plugin.json
│   └── install.sh
│
├── skills/
│   ├── compound-reflect/SKILL.md
│   ├── compound-search/SKILL.md
│   ├── compound-skip/SKILL.md
│   ├── compound-status/SKILL.md
│   └── compound-maintain/SKILL.md
│
├── hooks/
│   └── hooks.json
│
└── rules/
    └── compound-gate.md

# Runtime files created by grove init:
#
# ~/.grove/                        # User-level (not committed)
# ├── sessions/
# ├── stats-cache.json
# └── config.toml
#
# <project>/.grove/                # Project-level (committed)
# ├── config.toml
# ├── learnings.md
# └── stats.log
```

## 2. Core Types

### 2.1 Session State

| Type | Module | Description |
|------|--------|-------------|
| `SessionState` | `core/state` | Main session container (keyed by session_id from hook payload) |
| `GateState` | `core/state` | Gate tracking state |
| `GateStatus` | `core/state` | Idle/Active/Pending/Blocked/Reflected/Skipped |
| `TicketContext` | `core/state` | Detected ticket info |
| `TicketCloseIntent` | `core/state` | Pending ticket close (pre-confirmation) |
| `SubagentObservation` | `core/state` | Observation from subagent (note + timestamp) |
| `CircuitBreakerState` | `core/state` | Breaker state with `last_blocked_session_id` for reset logic |
| `TraceEvent` | `core/state` | Individual trace entry (event_type, timestamp, details) |
| `EventType` | `core/state` | SessionStart/TicketDetected/BackendDetected/LearningsInjected/TicketCloseDetected/TicketClosed/TicketCloseFailed/StopHookCalled/GateBlocked/ReflectionComplete/Skip/CircuitBreakerTripped/SessionEnd/ObservationRecorded/LearningReferenced/LearningDismissed/GateStatusChanged/UserPromptInjection |

### 2.2 Learning Types

| Type | Module | Description |
|------|--------|-------------|
| `CompoundLearning` | `core/learning` | Full learning with metadata (includes `schema_version: u8`, `confidence: Confidence`) |
| `LearningCategory` | `core/learning` | Pattern/Pitfall/Convention/etc. |
| `LearningScope` | `core/learning` | Project/Personal/Team/Ephemeral |
| `LearningStatus` | `core/learning` | Active/Archived/Superseded |
| `Confidence` | `core/learning` | High/Medium/Low |

### 2.3 Reflection Types

| Type | Module | Description |
|------|--------|-------------|
| `ReflectionResult` | `core/reflect` | Full reflection output |
| `RejectedCandidate` | `core/reflect` | Candidate that failed schema or write gate |
| `WriteGateResult` | `core/reflect` | Pass/Fail with criterion matched |
| `CandidateLearning` | `core/reflect` | Pre-validation learning from reflection or retroflect (category, summary, detail, criteria_met, tags, scope) |
| `WriteGateCriterion` | `core/reflect` | BehaviorChanging/DecisionRationale/StableFact/ExplicitRequest |
| `SchemaValidationError` | `core/reflect` | Schema check failure detail |

### 2.4 Skip Types

| Type | Module | Description |
|------|--------|-------------|
| `SkipDecision` | `core/state` | Skip with reason and context |
| `SkipDecider` | `core/state` | Agent/User/AutoThreshold |

### 2.5 Stats Types

| Type | Module | Description |
|------|--------|-------------|
| `StatsEvent` | `stats/tracker` | Single JSONL event entry with `v: u8` version field |
| `StatsEventType` | `stats/tracker` | Enum of event types |
| `StatsCache` | `stats/tracker` | Materialized aggregate from event log |
| `LearningStats` | `stats/tracker` | Per-learning usage counters (derived from log) |
| `InjectedLearning` | `core/state` | Tracks what was injected + outcome |
| `InjectionOutcome` | `core/state` | Referenced/Dismissed/Corrected |
| `ReflectionStats` | `stats/tracker` | Per-reflection event metrics (derived from log) |
| `SkipStats` | `stats/skip` | Skip decision log entry |
| `AggregateStats` | `stats/tracker` | Rollup totals and category breakdown |
| `CrossPollination` | `stats/tracker` | Learning referenced outside origin ticket |
| `Insight` | `stats/insights` | Generated tuning recommendation |

`StatsEventType::Retroflect` fields:

| Field | Type | Description |
|-------|------|-------------|
| `session_id` | `String` | Grove session ID |
| `claude_session_id` | `String` | JSONL filename UUID (Claude Code session) |
| `candidates` | `usize` | LLM-produced candidates |
| `accepted` | `usize` | Candidates passing validation |
| `project_path` | `String` | Project root path |

### 2.6 Hook I/O

| Type | Module | Description |
|------|--------|-------------|
| `HookInput` | `hooks/input` | Input from Claude Code hooks (common fields: session_id, transcript_path, cwd) |
| `StopOutput` | `hooks/output` | Output for Stop hooks |
| `PreToolUseOutput` | `hooks/output` | Output for PreToolUse hooks |
| `PostToolUseInput` | `hooks/input` | Input for PostToolUse hooks (includes tool_response) |
| `SessionStartOutput` | `hooks/output` | Context injection for SessionStart |
| `SessionEndInput` | `hooks/input` | Input for SessionEnd hooks (includes reason) |
| `UserPromptSubmitInput` | `hooks/input` | Input for UserPromptSubmit hooks (includes prompt text) |
| `UserPromptSubmitOutput` | `hooks/output` | Output for UserPromptSubmit hooks (additionalContext injection) |

### 2.7 Retroflect Types

| Type | Module | Description |
|------|--------|-------------|
| `SessionSummary` | `eval/corpus` | Parsed session transcript with metadata |

`SessionSummary` fields:

| Field | Type | Description |
|-------|------|-------------|
| `session_id` | `String` | JSONL filename (UUID) |
| `project_cwd` | `PathBuf` | From `cwd` field in first JSONL entry |
| `timestamp` | `DateTime<Utc>` | First user message timestamp |
| `user_turns` | `usize` | Count of user messages |
| `tool_calls` | `usize` | Count of tool use blocks |
| `file_paths` | `Vec<String>` | File paths from tool inputs |
| `condensed_transcript` | `String` | Extracted conversation text |

### 2.8 LLM Module

Shared LLM call infrastructure extracted from the eval judge.

| Function | Module | Description |
|----------|--------|-------------|
| `call_llm_cli(model, prompt)` | `llm` | Invoke LLM via `claude` CLI |
| `call_llm_api(model, api_url, system_prompt, user_prompt)` | `llm` | Invoke LLM via Anthropic API |

Both functions return `Option<String>` — `None` on failure (fail-open).
The eval judge (`src/eval/judge.rs`) is refactored to use these shared
functions.

## 3. Discovery Layer

### 3.1 Ticketing Detection

| Function | Module | Description |
|----------|--------|-------------|
| `detect_ticketing_system` | `discovery/tickets` | Probe in config order |
| `probe_tissue` | `discovery/tickets` | Check for `.tissue/` directory |
| `probe_beads` | `discovery/tickets` | Check for `.beads/` directory |
| `probe_tasks` | `discovery/tickets` | Check for Claude Code tasks |
| `match_close_command` | `discovery/tickets` | Pattern match ticket close |

### 3.2 Backend Detection

| Function | Module | Description |
|----------|--------|-------------|
| `detect_backends` | `discovery/backends` | Probe in config order |
| `probe_total_recall` | `discovery/backends` | Check for `memory/` + rules |
| `probe_markdown` | `discovery/backends` | Check for `.grove/learnings.md` |
| `create_default_backend` | `discovery/backends` | Scaffold markdown backend |

## 4. Backend Adapters

### 4.1 MemoryBackend Trait

| Method | Description |
|--------|-------------|
| `write(learning) -> WriteResult` | Write learning to storage |
| `search(query, filters) -> Vec<CompoundLearning>` | Search learnings |
| `ping() -> bool` | Health check |

### 4.2 Markdown Backend

Appends to `.grove/learnings.md` in structured markdown format.

| Function | Description |
|----------|-------------|
| `write` | Append learning entry with metadata header (after sanitization) |
| `search` | Parse file, match against query and tags |
| `ping` | Check file exists and is writable |
| `archive` | Mark learning as archived (status change in-place) |
| `parse_learnings` | Parse markdown file into `Vec<CompoundLearning>` |
| `sanitize_summary` | Single line, escape `#` and `\|` |
| `sanitize_detail` | Balance unbalanced code fences |
| `sanitize_tag` | Alphanumeric + hyphens only, lowercase |

### 4.3 Total Recall Adapter

Translates `CompoundLearning` to Total Recall's format and invokes Total
Recall commands. This adapter enables Grove to leverage Total Recall's
tiered memory system while maintaining Grove's structured reflection model.

#### 4.3.1 Detection

Total Recall is detected when both conditions are met:

- `memory/` directory exists in the project root
- `rules/total-recall.md` OR `.claude/rules/total-recall.md` exists

Detection function in `discovery/backends.rs`:

```rust
fn probe_total_recall(cwd: &Path) -> Option<BackendInfo> {
    let memory_dir = cwd.join("memory");
    let rules_v1 = cwd.join("rules/total-recall.md");
    let rules_v2 = cwd.join(".claude/rules/total-recall.md");

    if memory_dir.is_dir() && (rules_v1.is_file() || rules_v2.is_file()) {
        Some(BackendInfo::new(BackendType::TotalRecall, Some(memory_dir), false))
    } else {
        None
    }
}
```

#### 4.3.2 Architecture Alignment

Total Recall and Grove share similar write gate philosophies but with
different criteria. Grove's write gate maps to Total Recall's as follows:

| Grove Criterion | Total Recall Criterion |
|-----------------|------------------------|
| `behavior_changing` | Behavioral impact (preferences, patterns) |
| `decision_rationale` | Decisions (choices with rationale) |
| `stable_fact` | Stable facts (non-transient info) |
| `explicit_request` | Explicit requests ("remember this") |
| — | Commitments (deadlines, deliverables) |

Grove learnings always pass at least one criterion before reaching the
adapter. The adapter does NOT invoke Total Recall's write gate again —
it writes directly to the daily log file to avoid double-gating.

#### 4.3.3 Scope to Tier Routing

Grove's learning scopes map to Total Recall's memory tiers:

| Grove Scope | Total Recall Destination | Rationale |
|-------------|--------------------------|-----------|
| `Project` | `memory/daily/YYYY-MM-DD.md` | Team-visible via daily log |
| `Team` | `memory/daily/YYYY-MM-DD.md` | Same as Project |
| `Personal` | `~/.grove/personal-learnings.md` | Bypasses TR entirely |
| `Ephemeral` | `memory/daily/YYYY-MM-DD.md` | Captures but no promotion |

**Important:** Grove writes to daily logs only. Promotion to registers
is a user-driven action via Total Recall's `/recall-promote` command.
Grove does not auto-promote learnings.

#### 4.3.4 Format Translation

Grove `CompoundLearning` translates to Total Recall daily log format:

**Grove Learning:**

```json
{
  "id": "learn-abc123",
  "category": "Pitfall",
  "summary": "Using unwrap() in async context causes panics",
  "detail": "When an async task panics due to unwrap(), the entire...",
  "scope": "Project",
  "confidence": "High",
  "criteria_met": ["behavior_changing"],
  "tags": ["rust", "async", "error-handling"],
  "context_files": ["src/api/handler.rs"],
  "ticket_id": "grove-abc123"
}
```

**Total Recall Daily Log Entry:**

```markdown
## Learnings

[14:32] **Pitfall** (grove:learn-abc123): Using unwrap() in async context causes panics
> When an async task panics due to unwrap(), the entire...

Tags: #rust #async #error-handling | Confidence: High | Ticket: grove-abc123 | Files: src/api/handler.rs
```

#### 4.3.5 Functions

| Function | Description |
|----------|-------------|
| `write` | Format learning, append directly to daily log file |
| `search` | Read daily logs and registers, filter for `grove:` entries |
| `ping` | Check `memory/` dir exists and is writable |
| `format_learning` | Convert `CompoundLearning` to Total Recall daily log format |
| `parse_search_result` | Parse daily log entries to `CompoundLearning` |

#### 4.3.6 Direct File Operations

**Write Operation:**

Grove writes directly to Total Recall's daily log files rather than
invoking CLI commands. Total Recall's skills (`/recall-write`,
`/recall-log`) are interactive Claude Code skills that work within
conversations, not CLI commands that can be invoked as subprocesses.

```rust
fn write(&self, learning: &CompoundLearning) -> Result<WriteResult> {
    let entry = self.format_learning(learning);
    let daily_log = self.project_dir.join(format!(
        "memory/daily/{}.md",
        Utc::now().format("%Y-%m-%d")
    ));

    // Ensure daily log directory exists
    if let Some(parent) = daily_log.parent() {
        fs::create_dir_all(parent)?;
    }

    // Append to daily log file
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&daily_log)?;
    writeln!(file, "\n{}", entry)?;

    Ok(WriteResult::success(&learning.id, daily_log.display().to_string()))
}
```

**Search Operation:**

Grove reads daily log files and registers directly, filtering for
`grove:` prefixed entries:

```rust
fn search(&self, query: &SearchQuery, filters: &SearchFilters) -> Result<Vec<CompoundLearning>> {
    let search_term = self.build_search_term(query);
    let mut learnings = Vec::new();

    // Read last 14 days of daily logs
    for day_offset in 0..14 {
        let date = Utc::now() - Duration::days(day_offset);
        let daily_log = self.project_dir.join(format!(
            "memory/daily/{}.md",
            date.format("%Y-%m-%d")
        ));
        if daily_log.exists() {
            let content = fs::read_to_string(&daily_log)?;
            learnings.extend(self.parse_grove_entries(&content, &search_term));
        }
    }

    // Read register files
    let registers_dir = self.project_dir.join("memory/registers");
    if registers_dir.is_dir() {
        for entry in fs::read_dir(&registers_dir)? {
            let path = entry?.path();
            if path.extension().map_or(false, |e| e == "md") {
                let content = fs::read_to_string(&path)?;
                learnings.extend(self.parse_grove_entries(&content, &search_term));
            }
        }
    }

    Ok(learnings)
}
```

#### 4.3.7 Search Term Construction

Grove constructs search terms from available context:

```rust
fn build_search_term(&self, query: &SearchQuery) -> String {
    let mut terms = Vec::new();

    // Add ticket context
    if let Some(title) = &query.ticket_title {
        terms.push(title.clone());
    }

    // Add file path stems
    for path in &query.file_paths {
        if let Some(stem) = Path::new(path).file_stem() {
            terms.push(stem.to_string_lossy().to_string());
        }
    }

    // Add tags
    terms.extend(query.tags.iter().cloned());

    terms.join(" ")
}
```

#### 4.3.8 Search Result Parsing

Total Recall returns markdown-formatted results. Grove parses these back
into `CompoundLearning` objects:

```rust
fn parse_search_results(&self, output: &[u8]) -> Result<Vec<CompoundLearning>> {
    let text = String::from_utf8_lossy(output);
    let mut learnings = Vec::new();

    // Pattern: [HH:MM] **Category** (grove:id): Summary
    let entry_re = Regex::new(
        r"\[(\d{2}:\d{2})\] \*\*(\w+)\*\* \(grove:([^)]+)\): (.+)"
    )?;

    for cap in entry_re.captures_iter(&text) {
        let category = cap[2].parse().unwrap_or(LearningCategory::Pattern);
        let id = cap[3].to_string();
        let summary = cap[4].to_string();

        learnings.push(CompoundLearning {
            id,
            category,
            summary,
            // Detail, tags, etc. may not be fully recoverable from search
            // results - marked as partial
            ..Default::default()
        });
    }

    Ok(learnings)
}
```

**Note:** Search results from Total Recall may not contain all fields
present in the original learning. The adapter marks these as partial
results. Full learning details require parsing the daily log file directly.

#### 4.3.9 Supersession Handling

When a Grove learning supersedes an existing one:

1. Grove marks the old learning as `Superseded` in its stats
2. The adapter prepends a supersession note to the new entry:

```markdown
[14:45] **Pitfall** (grove:learn-def456): Correct approach for async error handling
> [supersedes grove:learn-abc123 — previous advice was incomplete]
> Use `?` operator with proper error context...
```

This aligns with Total Recall's contradiction protocol.

#### 4.3.10 Error Handling

| Error Condition | Behavior |
|-----------------|----------|
| `memory/` not writable | Log warning, return `BackendUnavailable` |
| `claude` CLI not found | Log warning, return `BackendUnavailable` |
| Skill invocation fails | Log warning, return `BackendUnavailable` |
| Parse error on search | Log warning, return empty results |

All errors follow fail-open philosophy: Grove continues operation
with degraded functionality rather than blocking the user.

## 5. Hook Handlers

### 5.1 Session-Start Hook

`grove hook session-start`

1. Read `session_id`, `cwd`, `transcript_path`, `source` from stdin JSON
2. If `source` is `"resume"`: create a fresh session (no inheritance)
3. If `source` is `"compact"`: load existing session file for this `session_id`
4. Otherwise: create new session state keyed by `session_id`
5. Discover ticketing system (probe in order)
6. Discover memory backends (probe in order)
7. Load learnings index from active backend
8. Search for relevant learnings (using ticket context if available)
9. Score and rank learnings (composite score)
10. Append "surfaced" events to `.grove/stats.log` for each injected learning
11. Return `additionalContext` with top N learnings
12. Add `SessionStart`, `TicketDetected`, `BackendDetected`,
   `LearningsInjected` trace events

### 5.2 PreToolUse Hook

`grove hook pre-tool-use`

1. Read `session_id`, `tool_name`, `tool_input` from stdin
2. Match against configured ticket close patterns
3. If match: transition gate immediately (Idle/Active → Pending), add
   `TicketClosed` trace event
4. Allow the tool to proceed

**Design note:** Gate transitions happen in PreToolUse rather than PostToolUse
because PostToolUse hooks may not fire reliably in all Claude Code configurations.
This follows the same pattern used by the Roz quality gate plugin. The circuit
breaker provides a safety valve if the command fails.

### 5.3 PostToolUse Hook

`grove hook post-tool-use`

Fallback mechanism — may not fire reliably in all configurations.

1. Read `session_id`, `tool_name`, `tool_input`, `tool_response` from stdin
2. If gate is Pending and `tool_response.success` is false:
   - Revert gate status Pending → Active, add `TicketCloseFailed` trace event
3. Otherwise: no-op

### 5.4 Stop Hook

`grove hook stop`

1. Read `session_id`, `stop_hook_active` from stdin
2. Load session state
3. If gate status is `Reflected` or `Skipped` → approve
4. If gate status is `Idle` (no ticket close, session mode):
   - Compute diff size via `git diff --stat HEAD` (cache in session state)
   - If not a git repo: skip threshold check, let agent decide
   - If under threshold and auto-skip enabled → agent decides → log → approve
5. If gate status is `Active` or `Pending`:
   - Check circuit breaker (`block_count >= max_blocks` → force approve)
   - Increment `block_count`
   - Block with instructions to run `/compound-reflect`
6. Add `StopHookCalled` trace event

### 5.5 SessionEnd Hook

`grove hook session-end`

1. Read `session_id`, `reason` from stdin
2. Load session state
3. For each learning in `injected_learnings` not marked as referenced:
   append a `dismissed` event to `.grove/stats.log`
4. Add `SessionEnd` trace event
5. Always allows termination (SessionEnd hooks cannot block)

## 6. CLI Commands

### 6.1 Main Entry Point

`src/main.rs` — clap-based CLI with subcommands.

### 6.2 Core Commands

| Command | Module | Description |
|---------|--------|-------------|
| `grove hook <event>` | `cli/hook` | Hook runner, reads stdin JSON |
| `grove reflect` | `cli/reflect` | Schema-validate reflection output, write gate filter, near-duplicate check, route to backend, append stats events |
| `grove skip <reason>` | `cli/skip` | Record skip decision, set gate to Skipped |
| `grove ref <ids>` | `cli/ref_cmd` | Record referenced learnings, append stats events |
| `grove observe <note>` | `cli/observe` | Append subagent observation to session |
| `grove search <query>` | `cli/search` | Search across all active backends |
| `grove list` | `cli/list` | List recent learnings from active backend |
| `grove stats` | `cli/stats` | Quality dashboard with insights |
| `grove maintain` | `cli/maintain` | Review stale learnings, list candidates |
| `grove maintain archive <ids>` | `cli/maintain` | Archive specific learnings by ID |
| `grove maintain restore <ids>` | `cli/maintain` | Restore archived learnings by ID |
| `grove review` | `cli/review` | Sample learnings for quality rating (feedback loop) |
| `grove retroflect` | `cli/retroflect` | Retroactive reflection from session history |
| `grove init` | `cli/init` | Scaffold config, learnings file, session dir |
| `grove backends` | `cli/backends_cmd` | Show discovered backends and status |
| `grove tickets` | `cli/tickets_cmd` | Show discovered ticketing system |

### 6.3 Debug Commands

| Command | Module | Description |
|---------|--------|-------------|
| `grove sessions` | `cli/sessions` | List recent sessions with status |
| `grove debug <session_id>` | `cli/debug` | Full session state dump |
| `grove trace <session_id>` | `cli/trace` | Trace event viewer |
| `grove clean --before <duration>` | `cli/clean` | Remove old session files |
| `grove eval run` | `cli/eval` | Run benchmark, output scorecard |
| `grove eval compare` | `cli/eval` | Run multiple configs, show comparison |

**Note:** Debug commands are intended for development and troubleshooting only.
They may expose internal state manipulation (e.g., `--set-gate`) that bypasses
normal gate enforcement. These escape hatches exist for testing scenarios where
the gate needs to be manually controlled.

## 7. Configuration

### 7.1 Config Structure

| Type | Description |
|------|-------------|
| `Config` | Main config struct |
| `TicketingConfig` | Discovery order and overrides |
| `BackendsConfig` | Discovery order, backend settings |
| `GateConfig` | Auto-skip settings, write gate, semantic dedup |
| `SemanticDedupConfig` | Embedding similarity threshold, enable flag |
| `AutoSkipConfig` | Threshold, decider |
| `DecayConfig` | Passive duration |
| `RetrievalConfig` | Max injections, strategy, BM25 config, corpus enrichment, adaptive dk, category decay |
| `CategoryHalfLifeConfig` | Per-category recency decay half-lives |
| `IntentFilterConfig` | Post-retrieval intent-based filtering |
| `RerankConfig` | LLM reranking during deferred injection |
| `CircuitBreakerConfig` | Max blocks, cooldown |

### 7.2 Config Precedence

1. Environment variables (`GROVE_HOME`, etc.)
2. Project config (`.grove/config.toml`)
3. User config (`~/.grove/config.toml`)
4. Defaults

### 7.3 Default Values

| Setting | Default |
|---------|---------|
| `ticketing.discovery` | `["tissue", "beads", "tasks", "session"]` |
| `backends.discovery` | `["config", "total-recall", "markdown"]` |
| `gate.auto_skip.enabled` | `true` |
| `gate.auto_skip.line_threshold` | `5` |
| `gate.auto_skip.decider` | `"agent"` |
| `decay.passive_duration_days` | `90` |
| `retrieval.max_injections` | `5` |
| `retrieval.strategy` | `"moderate"` |
| `retrieval.scoring_backend` | `"bm25"` |
| `retrieval.corpus_enrichment` | `true` |
| `retrieval.corpus_size_threshold` | `50` |
| `retrieval.dynamic_k_ratio` | `0.3` |
| `retrieval.adaptive_dk` | `false` |
| `retrieval.min_confidence_threshold` | `0.1` |
| `retrieval.min_score_gap` | `0.05` |
| `retrieval.recency_half_life_days` | `90` |
| `retrieval.category_half_lives.*` | `90` (all categories) |
| `retrieval.intent_filter.enabled` | `false` |
| `retrieval.intent_filter.min_overlap` | `1` |
| `retrieval.intent_filter.max_keywords` | `15` |
| `retrieval.rerank.enabled` | `false` |
| `retrieval.rerank.timeout_seconds` | `15` |
| `retrieval.rerank.model` | `"haiku"` |
| `retrieval.rerank.backend` | `"cli"` |
| `circuit_breaker.max_blocks` | `3` |
| `circuit_breaker.cooldown_seconds` | `300` |

## 8. Error Handling

### 8.1 Error Types

| Error | Description |
|-------|-------------|
| `Storage` | I/O errors (session files) |
| `Backend` | Memory backend errors |
| `Serde` | JSON/markdown parsing errors |
| `InvalidState` | State machine violations |
| `SessionNotFound` | Missing session |
| `Config` | Config loading errors |
| `Discovery` | Ticketing/backend detection errors |
| `Reflection` | Reflection parsing errors |

### 8.2 Fail-Open Philosophy

All hook handlers follow fail-open: infrastructure errors approve rather than
block. Specific patterns:

- Session-start: if discovery fails → proceed with no injections
- Pre-tool-use: if state read fails → allow tool
- Stop hook: if state read fails → approve exit
- Reflect: if backend write fails → log warning, still mark reflected
- Stats: if stats write fails → log warning, don't block

### 8.3 Panic Handling

A global panic handler ensures crashes produce predictable behavior:

```rust
fn main() {
    std::panic::set_hook(Box::new(|info| {
        // Log to ~/.grove/crash.log
        eprintln!("grove panic: {}", info);
        std::process::exit(3);  // Distinct from 0=approve, 2=block
    }));
    // ...
}
```

Exit codes: 0 = approve, 2 = block, 3 = crash (fail-open).

## 9. Plugin Integration

### 9.1 hooks.json

Configures Claude Code hooks:

- `SessionStart` → `grove hook session-start`
- `PreToolUse` (Bash) → `grove hook pre-tool-use`
- `PostToolUse` (Bash) → `grove hook post-tool-use`
- `Stop` → `grove hook stop`
- `TaskCompleted` → `grove hook task-completed`
- `SessionEnd` → `grove hook session-end`
- `UserPromptSubmit` → `grove hook user-prompt-submit`

### 9.2 Plugin Structure

| File | Description |
|------|-------------|
| `.claude-plugin/plugin.json` | Plugin manifest |
| `.claude-plugin/install.sh` | Binary installer (postinstall) |
| `skills/*/SKILL.md` | Slash command definitions |
| `hooks/hooks.json` | Hook configuration |
| `rules/compound-gate.md` | Auto-loaded protocol |

### 9.3 Skill Definitions

Each skill provides instructions for Claude to invoke the appropriate
`grove` CLI command:

| Skill | Invokes |
|-------|---------|
| `compound-reflect` | `grove reflect` (parses Claude's reflection output) |
| `compound-search` | `grove search <query>` |
| `compound-skip` | `grove skip <reason>` |
| `compound-status` | `grove stats` + `grove backends` |
| `compound-maintain` | `grove maintain` |

## 10. Testing

### 10.1 Unit Tests

Each module contains `#[cfg(test)]` sections:

| Module | Coverage |
|--------|----------|
| `core/state` | State serialization, defaults, transitions |
| `core/gate` | Gate state machine, all transitions |
| `core/reflect` | Write gate filter, reflection parsing |
| `core/learning` | Learning serialization, category parsing |
| `discovery/tickets` | Probe functions, pattern matching |
| `discovery/backends` | Probe functions, priority ordering |
| `backends/markdown` | Read/write/search/archive |
| `stats/tracker` | Counter updates, hit rate calculation |
| `stats/decay` | Threshold evaluation, archival |
| `stats/scoring` | Composite score calculation |
| `stats/insights` | Insight generation logic |
| `storage/file` | CRUD, atomic writes |
| `storage/memory` | CRUD |
| `hooks/input` | JSON parsing |
| `hooks/output` | Serialization |
| `config` | Loading, defaults, precedence |

### 10.2 Integration Tests

| Test File | Coverage |
|-----------|----------|
| `gate_flow` | Full gate lifecycle: detect → close → block → reflect → approve |
| `reflection_flow` | Reflection → write gate → backend write → stats update |
| `discovery_flow` | Ticketing + backend discovery with various project layouts |
| `stats_flow` | Stats accumulation, decay, scoring, insight generation |
| `retroflect_flow` | Session parsing, discovery, filtering, LLM output, cross-session dedup |

## 11. Retroflect Command

### 11.1 CLI Interface

```text
grove retroflect [OPTIONS]

Options:
  --project <PATH>     Project root to retroflect (default: current dir)
  --all                Auto-discover all projects under ~/.claude/projects/
  --init               Auto-initialize .grove/ in discovered projects (for --all)
  --model <MODEL>      LLM model for synthesis (default: sonnet)
  --backend <BACKEND>  LLM backend: api or cli (default: api)
  --limit <N>          Max sessions to analyze (default: 20)
  --min-turns <N>      Skip sessions with fewer than N user turns (default: 3)
  --dry-run            Show candidates without writing
  --force              Re-analyze previously retroflected sessions
  --yes                Skip cost confirmation prompt
  --json               Output results as JSON
```

### 11.2 Orchestration

`src/cli/retroflect.rs` is a single-file orchestrator that:

1. Discovers eligible sessions (three modes: `--project`, current dir, `--all`)
2. Filters sessions (min-turns, already-analyzed unless `--force`, top-level
   only — no subagent sessions)
3. Estimates cost and prompts for confirmation (unless `--yes`)
4. For each session:
   - Parses JSONL transcript via `parse_session_transcript()` in `eval/corpus`
   - Sends condensed transcript to LLM via shared `llm` module
   - Validates candidates through existing pipeline
     (`validate_with_duplicates_and_quality_semantic()`) with lenient
     write gate threshold (retroactive learnings lack real-time context)
   - Deduplicates against existing learnings + batch-accepted learnings
   - Injects `#retroflect` tag if not already present
   - Writes accepted learnings to backend
   - Logs `StatsEventType::Retroflect` event
   - Flushes stats (crash recovery checkpoint)
5. Reports summary

### 11.3 Session Parsing

Extended in `src/eval/corpus.rs`:

```rust
pub fn parse_session_transcript(path: &Path) -> Option<SessionSummary>
```

Handles JSONL entry types per the architecture doc (Section 13.3). Extracts
user text (excluding `tool_result` blocks), assistant text (excluding
`thinking` and `tool_use` blocks), and file paths from tool inputs.

### 11.4 Project Discovery for `--all` Mode

For `--all`, retroflect globs `~/.claude/projects/*/` and reads the `cwd`
field from one JSONL file per directory to determine the original project
path. Sessions are grouped by project. If a project's `.grove/` directory
is missing, retroflect prompts for per-project initialization confirmation
unless `--init` is passed.

### 11.5 LLM System Prompt

The system prompt includes:

- Seven category definitions with examples
- `CandidateLearning` JSON schema
- `criteria_met` options with what constitutes evidence
- Instruction: produce 0-5 candidates per session, tag each with `#retroflect`
- Instruction: focus on decisions, pivots, debugging breakthroughs,
  not routine work

Malformed JSON responses are handled gracefully (session skipped with
warning).

## 12. Batch Processing

### 12.1 Module Structure

Convert `src/llm.rs` into a directory module:

```text
src/llm/
├── mod.rs       # Re-exports everything currently in llm.rs
└── batch.rs     # Batch API logic (reusable across eval and retroflect)
```

`mod.rs` re-exports `call_llm_cli` and `call_llm_api` to preserve
backward compatibility. All batch-specific logic lives in `batch.rs`.

### 12.2 Batch Types

| Type | Module | Description |
|------|--------|-------------|
| `BatchRequest` | `llm/batch` | A single request to include in a batch |
| `BatchResult` | `llm/batch` | The result of a single request within a completed batch |
| `BatchResultType` | `llm/batch` | Succeeded(String) / Failed(String) |
| `BatchState` | `llm/batch` | Tracks an in-flight batch (batch_id, created_at, total_requests) |

```rust
/// A single request to include in a batch.
pub struct BatchRequest {
    /// Unique identifier to match results back to callers.
    pub custom_id: String,
    /// Standard Messages API params as a serde_json::Value.
    pub params: serde_json::Value,
}

/// The result of a single request within a completed batch.
pub struct BatchResult {
    pub custom_id: String,
    pub result_type: BatchResultType,
}

pub enum BatchResultType {
    /// Request succeeded; contains the text response.
    Succeeded(String),
    /// Request failed (errored, canceled, or expired).
    Failed(String),
}

/// Tracks the state of an in-flight batch.
pub struct BatchState {
    pub batch_id: String,
    pub created_at: String,
    pub total_requests: usize,
}
```

### 12.3 Batch Functions

| Function | Module | Description |
|----------|--------|-------------|
| `create_batch` | `llm/batch` | Submit a batch of requests to the Anthropic Message Batches API |
| `poll_batch_until_ended` | `llm/batch` | Poll a batch until "ended" status or timeout |
| `retrieve_batch_results` | `llm/batch` | Retrieve results for a completed batch as `Vec<BatchResult>` |
| `cancel_batch` | `llm/batch` | Cancel a batch (best-effort, used for Ctrl+C handling) |

```rust
/// Submit a batch of requests to the Anthropic Message Batches API.
/// Returns a BatchState for polling, or None on failure (fail-open).
pub fn create_batch(
    api_url: &str,  // "https://api.anthropic.com/v1/messages/batches"
    requests: Vec<BatchRequest>,
) -> Option<BatchState>

/// Poll a batch until it reaches "ended" status or timeout.
/// Uses exponential backoff: 10s, 20s, 40s, 60s, 60s, ...
/// Returns true if ended, false if timed out.
pub fn poll_batch_until_ended(
    api_url: &str,
    batch_id: &str,
    timeout_seconds: u64,
    progress_callback: &dyn Fn(&str, usize, usize, usize, usize),
) -> Option<bool>

/// Retrieve results for a completed batch as a Vec<BatchResult>.
/// Streams the JSONL response and parses each line.
pub fn retrieve_batch_results(
    api_url: &str,
    batch_id: &str,
) -> Option<Vec<BatchResult>>

/// Cancel a batch (best-effort, used for Ctrl+C handling).
pub fn cancel_batch(api_url: &str, batch_id: &str)
```

All functions use `curl` subprocess (consistent with existing
`call_llm_api`). `ANTHROPIC_API_KEY` is read from the environment
inside each function. All failures return `None` (fail-open).

**Implementation details:**

- **`create_batch`**: Constructs JSON body `{"requests": [...]}`, calls
  `curl -s -X POST` with standard Anthropic headers. Parses response to
  extract `id`, `processing_status`, and `request_counts`.
- **`poll_batch_until_ended`**: Calls `curl -s GET` in a loop with
  `std::thread::sleep`. Backoff: 10s initial, doubling up to 60s max.
  Calls `progress_callback` with status counts on each poll.
- **`retrieve_batch_results`**: Calls `curl -s GET .../results`. Response
  is JSONL (one JSON object per line). Splits by newline, parses each
  line to extract `custom_id`, `result.type`, and for succeeded results,
  extracts `result.message.content[0].text`.
- **`cancel_batch`**: Calls `curl -s -X POST .../cancel`. Best-effort,
  errors are logged and ignored.

### 12.4 Eval Judge Batch Helpers

Added to `src/eval/judge.rs`:

```rust
/// Build a BatchRequest for a (session, learning) pair.
/// Returns None if the pair is already cached.
pub fn build_judge_batch_request(
    session_file: &str,
    learning: &CompoundLearning,
    ctx: &SessionContext,
    cache: &BTreeMap<String, f64>,
    judge: &JudgeContext,
) -> Option<BatchRequest>

/// Apply a batch result to the judge cache.
/// Parses the score from the response text and inserts into cache.
/// Returns the JudgeResult if successful.
pub fn apply_judge_batch_result(
    result: &BatchResult,
    cache: &mut BTreeMap<String, f64>,
) -> Option<JudgeResult>
```

`build_judge_batch_request` reuses the existing `judge_cache_key()`
function for `custom_id`, which already produces
`{session_file}:{learning_id}`. Cache hits return `None` (skip).

`apply_judge_batch_result` parses the score via `parse_judge_score`,
inserts into the cache on success, logs a warning on failure. Failed
results don't corrupt the cache.

### 12.5 Runner Changes

Added to `src/eval/runner.rs`:

```rust
/// Phase 1: Collect all (session, learning) pairs that need judging.
/// Phase 2: Submit batch, poll, retrieve.
/// Phase 3: Apply results and compute metrics.
pub fn run_benchmark_batch(
    config: &BenchmarkConfig,
    corpus: &Corpus,
    judge_ctx: &JudgeContext,
    cache: &mut BTreeMap<String, f64>,
    cache_path: &Path,
    transcript_dir: &Path,
) -> crate::Result<EvalOutput>
```

The search/filter/composite-score loop body (currently inline in
`run_benchmark`) is extracted into a shared helper that returns
`Vec<(session_file, learning_id, CompositeScore)>`. Both sequential
and batch paths use this helper.

### 12.6 Retroflect Changes

Added to `src/cli/retroflect.rs`:

- `batch: bool` field on `RetroflectOptions`
- `run_inner_batch` function implementing the three-phase approach

**Phase 1 — Collect:** Iterate eligible sessions, build user prompts,
construct `BatchRequest` objects with
`custom_id = "retroflect:{session_id}"`. Store mapping from `custom_id`
to `(project_path, SessionSummary)`.

**Phase 2 — Submit and wait:** Call `batch::create_batch`,
`batch::poll_batch_until_ended` with progress reporting to stderr.

**Phase 3 — Process results in order:** Sort results by original session
order. For each: parse via `parse_llm_response`, run validation pipeline
with accumulating `batch_accepted`, write accepted learnings to backend,
log stats events. This preserves cross-session dedup ordering.

Retroflect batch request params structure:

```rust
serde_json::json!({
    "model": model,
    "max_tokens": 1024,
    "system": [{
        "type": "text",
        "text": RETROFLECT_SYSTEM_PROMPT,
        "cache_control": { "type": "ephemeral" }
    }],
    "messages": [{
        "role": "user",
        "content": user_prompt
    }]
})
```

### 12.7 CLI Flags

**`grove retroflect`** — add `--batch` flag:

```rust
/// Use Batch API (50% cheaper, async processing)
#[arg(long)]
batch: bool,
```

**`grove eval run`** and **`grove eval compare`** — add `--batch` flag:

```rust
/// Use Batch API for judge calls (50% cheaper, async processing)
#[arg(long)]
batch: bool,
```

When `batch == true`, the eval runner calls `run_benchmark_batch`
instead of `run_benchmark`. The retroflect command calls
`run_inner_batch` instead of the sequential `run_inner`.

### 12.8 Config Changes

Add `batch_timeout` to `JudgeConfig` in `src/config.rs`:

| Setting | Default | Description |
|---------|---------|-------------|
| `judge.batch_timeout` | `3600` | Max seconds to wait for batch completion |

Default of 3600 seconds (1 hour) matches typical batch completion time
from the Anthropic API documentation.

### 12.9 custom_id Encoding

| System | Format | Example |
|--------|--------|---------|
| Eval judge | `{session_file}:{learning_id}` | `abc123:cl_007` |
| Retroflect | `retroflect:{session_id}` | `retroflect:550e8400-e29b-41d4-a716-446655440000` |

Eval judge reuses the existing `judge_cache_key()` function. The `:`
separator is safe in `custom_id` strings. Retroflect uses session UUIDs,
guaranteed unique within a batch.

### 12.10 Testing

**Unit tests in `src/llm/batch.rs`:**

- JSON construction for batch request body
- JSONL parsing of batch results (succeeded/errored/expired/canceled)
- `custom_id` round-trip encoding
- Timeout/backoff logic with mock clock

**Unit tests in `src/eval/judge.rs`:**

- `build_judge_batch_request` produces correct params and skips cached
  entries
- `apply_judge_batch_result` correctly populates cache
- Failed results don't corrupt cache

**Integration tests:**

- Batch mode for eval produces same metrics as sequential (deterministic
  with seeded mock)
- Retroflect batch mode preserves cross-session dedup ordering
- Partial failure handling (some requests errored, rest succeed)
- Fallback when batch creation fails (graceful degradation to sequential)

**Mock testing:** Use the existing `LlmCaller` type alias pattern for
batch mode — accept a `batch_caller` trait/closure that can be mocked in
tests.

## Related Documents

- [Overview](./00-overview.md) - Vision, core concepts, design principles
- [Architecture](./01-architecture.md) - System diagrams, domain model,
  sequences
- [Stats and Quality](./03-stats-and-quality.md) - Quality tracking model,
  retrieval scoring, insights engine
- [Test Plan](./04-test-plan.md) - Testing strategy
- [CI](./05-ci.md) - Version management and release workflow
