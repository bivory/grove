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
│   │   └── clean.rs           # clean command
│   │
│   ├── config.rs              # Configuration loading
│   └── error.rs               # Error types
│
├── tests/
│   ├── gate_flow.rs           # Gate state machine integration tests
│   ├── reflection_flow.rs     # Reflection + write gate tests
│   ├── discovery_flow.rs      # Ticketing/backend discovery tests
│   └── stats_flow.rs          # Stats tracking integration tests
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
| `EventType` | `core/state` | SessionStart/TicketDetected/BackendDetected/LearningsInjected/TicketCloseDetected/TicketClosed/TicketCloseFailed/StopHookCalled/GateBlocked/ReflectionComplete/Skip/CircuitBreakerTripped/SessionEnd/ObservationRecorded/LearningReferenced/LearningDismissed/GateStatusChanged |

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

### 2.6 Hook I/O

| Type | Module | Description |
|------|--------|-------------|
| `HookInput` | `hooks/input` | Input from Claude Code hooks (common fields: session_id, transcript_path, cwd) |
| `StopOutput` | `hooks/output` | Output for Stop hooks |
| `PreToolUseOutput` | `hooks/output` | Output for PreToolUse hooks |
| `PostToolUseInput` | `hooks/input` | Input for PostToolUse hooks (includes tool_response) |
| `SessionStartOutput` | `hooks/output` | Context injection for SessionStart |
| `SessionEndInput` | `hooks/input` | Input for SessionEnd hooks (includes reason) |

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
it uses `recall-log` for direct capture to avoid double-gating.

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
| `write` | Format learning, append to daily log via `recall-log` |
| `search` | Invoke `recall-search`, parse results into `Vec<CompoundLearning>` |
| `ping` | Check `memory/` dir exists and is writable |
| `format_learning` | Convert `CompoundLearning` to Total Recall daily log format |
| `parse_search_result` | Parse Total Recall search output to `CompoundLearning` |

#### 4.3.6 Command Invocation

**Write Operation:**

Grove invokes `recall-log` (not `recall-write`) to bypass Total Recall's
write gate since Grove already applies its own validation:

```rust
fn write(&self, learning: &CompoundLearning) -> Result<WriteResult> {
    let note = self.format_learning(learning);
    let output = Command::new("claude")
        .args(["skill", "recall-log", &note])
        .current_dir(&self.project_dir)
        .output()?;

    if output.status.success() {
        let daily_log = format!("memory/daily/{}.md", Utc::now().format("%Y-%m-%d"));
        Ok(WriteResult::success(&learning.id, daily_log))
    } else {
        // Fail-open: log warning but don't block
        let msg = String::from_utf8_lossy(&output.stderr);
        warn!("Total Recall write failed: {}", msg);
        Ok(WriteResult::failure(&learning.id, "Backend unavailable"))
    }
}
```

**Search Operation:**

```rust
fn search(&self, query: &SearchQuery, filters: &SearchFilters) -> Result<Vec<CompoundLearning>> {
    let search_term = self.build_search_term(query);
    let output = Command::new("claude")
        .args(["skill", "recall-search", &search_term])
        .current_dir(&self.project_dir)
        .output()?;

    if output.status.success() {
        self.parse_search_results(&output.stdout)
    } else {
        // Fail-open: return empty results
        warn!("Total Recall search failed: {}", String::from_utf8_lossy(&output.stderr));
        Ok(vec![])
    }
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
| `grove observe <note>` | `cli/observe` | Append subagent observation to session |
| `grove search <query>` | `cli/search` | Search across all active backends |
| `grove list` | `cli/list` | List recent learnings from active backend |
| `grove stats` | `cli/stats` | Quality dashboard with insights |
| `grove maintain` | `cli/maintain` | Review stale learnings, list candidates |
| `grove maintain archive <ids>` | `cli/maintain` | Archive specific learnings by ID |
| `grove maintain restore <ids>` | `cli/maintain` | Restore archived learnings by ID |
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
| `GateConfig` | Auto-skip settings |
| `AutoSkipConfig` | Threshold, decider |
| `DecayConfig` | Passive duration |
| `RetrievalConfig` | Max injections, strategy |
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
- `SessionEnd` → `grove hook session-end`

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

## Related Documents

- [Overview](./00-overview.md) - Vision, core concepts, design principles
- [Architecture](./01-architecture.md) - System diagrams, domain model,
  sequences
- [Stats and Quality](./03-stats-and-quality.md) - Quality tracking model,
  retrieval scoring, insights engine
- [Test Plan](./04-test-plan.md) - Testing strategy
- [CI](./05-ci.md) - Version management and release workflow
