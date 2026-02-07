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
│   │   ├── total_recall.rs    # Total Recall adapter
│   │   └── mcp.rs             # MCP memory server adapter
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
| `TraceEvent` | `core/state` | Individual trace entry |
| `EventType` | `core/state` | Event type enum |

### 2.2 Learning Types

| Type | Module | Description |
|------|--------|-------------|
| `CompoundLearning` | `core/learning` | Full learning with metadata |
| `LearningCategory` | `core/learning` | Pattern/Pitfall/Convention/etc. |
| `LearningScope` | `core/learning` | Project/Personal/Team/Ephemeral |
| `Confidence` | `core/learning` | High/Medium/Low |
| `LearningStatus` | `core/learning` | Active/Archived/Superseded |

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
| `StatsEvent` | `stats/tracker` | Single JSONL event entry (surfaced, referenced, dismissed, etc.) |
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
| `probe_mcp` | `discovery/backends` | Check for MCP memory server |
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
| `write` | Append learning entry with metadata header |
| `search` | Parse file, match against query and tags |
| `ping` | Check file exists and is writable |
| `archive` | Mark learning as archived (status change in-place) |
| `parse_learnings` | Parse markdown file into `Vec<CompoundLearning>` |

### 4.3 Total Recall Adapter

Translates `CompoundLearning` to Total Recall's format and shells out to
`recall-write`.

| Function | Description |
|----------|-------------|
| `write` | Format as recall note, invoke `recall-write` |
| `search` | Invoke `recall-search`, parse results |
| `ping` | Check `memory/` dir and `recall-write` availability |

### 4.4 MCP Adapter

Routes through MCP memory server tools.

| Function | Description |
|----------|-------------|
| `write` | Call MCP `memory_write` tool |
| `search` | Call MCP `memory_search` tool |
| `ping` | Call MCP health endpoint |

## 5. Hook Handlers

### 5.1 Session-Start Hook

`grove hook session-start`

1. Read `session_id`, `cwd`, `transcript_path`, `source` from stdin JSON
2. If `source` is `"resume"`: search for predecessor session via
   `find_recent(cwd, Active|Pending)` and inherit gate state
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
3. If match: record ticket close *intent* in session state (not yet confirmed)
4. Add trace event
5. Allow the tool to proceed

### 5.3 PostToolUse Hook

`grove hook post-tool-use`

1. Read `session_id`, `tool_name`, `tool_input`, `tool_response` from stdin
2. If a ticket close intent exists in session state:
   - Check `tool_response` for success
   - If successful: set gate status to `Pending`, add `TicketClosed` trace event
   - If failed: clear intent, add `TicketCloseFailed` trace event
3. If no intent: no-op

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

### 5.5 SubagentStop Hook

`grove hook subagent-stop`

Not used for gate enforcement (subagents don't trigger the gate). Used only
to capture subagent observations:

1. Check if subagent wrote observations via `grove observe`
2. Add `SubagentObservation` trace events
3. Always approve

### 5.6 SessionEnd Hook

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
| `grove maintain` | `cli/maintain` | Review stale learnings, prune, archive |
| `grove init` | `cli/init` | Scaffold config, learnings file, session dir |
| `grove backends` | `cli/backends_cmd` | Show discovered backends and status |
| `grove tickets` | `cli/tickets_cmd` | Show discovered ticketing system |

### 6.3 Debug Commands

| Command | Module | Description |
|---------|--------|-------------|
| `grove debug <session_id>` | `cli/debug` | Full session state dump |
| `grove trace <session_id>` | `cli/trace` | Trace event viewer |
| `grove clean --before <duration>` | `cli/clean` | Remove old session files |

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
| `backends.discovery` | `["config", "total-recall", "mcp", "markdown"]` |
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

## 9. Plugin Integration

### 9.1 hooks.json

Configures Claude Code hooks:

- `SessionStart` → `grove hook session-start`
- `PreToolUse` (Bash) → `grove hook pre-tool-use`
- `PostToolUse` (Bash) → `grove hook post-tool-use`
- `Stop` → `grove hook stop`
- `SubagentStop` → `grove hook subagent-stop`
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
