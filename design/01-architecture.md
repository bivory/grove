# grove - Architecture

This document describes the system architecture, domain model, and component
relationships at the conceptual level. It is intentionally
language-agnostic — the implementation document specifies concrete types and
file layouts.

## 1. System Overview

```mermaid
flowchart TB
    subgraph cli["grove CLI"]
        direction TB
        runner["Hook Runner"]
        core["Core Logic"]
        discovery["Discovery"]
        backends["Backend Adapters"]
        stats["Stats Engine"]
        storage["Session Storage"]
        runner --> core
        core --> discovery
        core --> backends
        core --> stats
        core --> storage
    end
```

| Layer | Responsibilities |
|-------|------------------|
| **Hook Runner** | Receives hook events from Claude Code, dispatches to core |
| **Core Logic** | Gate state machine, reflection orchestration, write gate filter |
| **Discovery** | Ticketing system and memory backend auto-detection |
| **Backend Adapters** | Uniform interface over built-in markdown, Total Recall, MCP |
| **Stats Engine** | Usage tracking, decay evaluation, retrieval scoring, insights |
| **Session Storage** | Per-session state persistence |

## 2. Logical Modules

| Module | Responsibility |
|--------|----------------|
| **Gate** | State machine governing when reflection is required |
| **Reflection** | Parsing structured reflection output, applying write gate filter |
| **Learning** | Learning entity: categories, scope, lifecycle |
| **Ticket Discovery** | Detecting which ticketing system is active |
| **Backend Discovery** | Detecting which memory backends are available |
| **Backend: Markdown** | Built-in append-only file storage |
| **Backend: Total Recall** | Adapter routing through Total Recall's commands |
| **Backend: MCP** | Adapter routing through MCP memory server |
| **Stats: Tracking** | Per-learning and per-reflection usage counters |
| **Stats: Decay** | Passive decay evaluation and archival |
| **Stats: Scoring** | Composite retrieval scoring for injection ranking |
| **Stats: Insights** | Pattern detection and tuning recommendations |
| **Session Storage** | File-based session state (gate status, trace events) |
| **Hook I/O** | Serialization/deserialization of Claude Code hook contracts |
| **Config** | Configuration loading, defaults, precedence |

## 3. Domain Model

### 3.1 Entities

```mermaid
classDiagram
    class Session {
        +id
        +cwd
        +transcript_path
        +created_at
        +updated_at
        +gate : GateState
        +ticket : TicketContext?
        +trace : TraceEvent[]
    }

    class GateState {
        +status : GateStatus
        +block_count
        +circuit_breaker_tripped
        +last_blocked_session_id : string?
        +reflection : ReflectionResult?
        +skip : SkipDecision?
        +subagent_observations : SubagentObservation[]
        +injected_learnings : InjectedLearning[]
    }

    class SubagentObservation {
        +note : string
        +timestamp : DateTime
    }

    class TicketContext {
        +ticket_id
        +source
        +title
        +description?
        +detected_at
    }

    class ReflectionResult {
        +learnings : Learning[]
        +rejected_candidates : RejectedCandidate[]
        +candidates_produced
        +candidates_accepted
        +completed_at
    }

    class Learning {
        +id
        +schema_version : u8
        +category : Category
        +summary
        +detail
        +scope : Scope
        +confidence : Confidence
        +criteria_met : WriteGateCriterion[]
        +tags : string[]
        +session_id
        +ticket_id?
        +timestamp
        +context_files : string[]?
        +status : LearningStatus
    }

    class SkipDecision {
        +reason
        +decider : SkipDecider
        +lines_changed
        +timestamp
    }

    class InjectedLearning {
        +learning_id
        +score
        +outcome : InjectionOutcome
    }

    Session *-- GateState
    Session *-- TicketContext
    GateState *-- ReflectionResult
    GateState *-- SkipDecision
    GateState *-- InjectedLearning
    GateState *-- SubagentObservation
    ReflectionResult *-- Learning
```

### 3.2 Learning Categories

| Category | Description |
|----------|-------------|
| **Pattern** | A reusable code pattern or architectural approach |
| **Pitfall** | A mistake made or gotcha encountered (with fix) |
| **Convention** | A project convention learned or established |
| **Dependency** | Something learned about a library, API, or external system |
| **Process** | A workflow improvement or development process insight |
| **Domain** | Business logic or domain knowledge captured |
| **Debugging** | A debugging technique or diagnostic approach that worked |

### 3.3 Learning Scope

| Scope | Storage | Visibility |
|-------|---------|------------|
| **Project** | Primary backend (committed) | Whole team |
| **Team** | Primary backend (committed) | Whole team |
| **Personal** | `~/.grove/personal-learnings.md` (outside project) | Individual |
| **Ephemeral** | Daily log only (if available) | Transient |

**Scope routing** is a core logic responsibility, not a backend concern.
Core logic inspects the learning's scope and selects the appropriate
storage target. Project and Team scope route to the configured backend
(markdown, Total Recall, or MCP). Personal scope always routes to the
local personal learnings file. Ephemeral scope routes to the daily log
(if Total Recall is active) or is discarded. Backends receive learnings
and write them — they don't make routing decisions.

### 3.4 Learning Lifecycle

```mermaid
stateDiagram-v2
    [*] --> Active : write gate passes
    Active --> Archived : passive decay (no reference in N days)
    Active --> Superseded : corrected by newer learning
    Active --> Active : referenced (refreshes decay timer)
    Archived --> Active : restored via grove maintain
    Superseded --> [*] : permanent
```

## 4. Gate State Machine

```mermaid
stateDiagram-v2
    [*] --> Idle

    Idle --> Active : ticket detected
    Idle --> Pending : stop hook (session mode, non-trivial diff)

    Active --> Pending : ticket closed
    Active --> Idle : session ends without close

    Pending --> Blocked : stop hook fires
    Pending --> Skipped : auto-skip or manual skip
    Pending --> Idle : circuit breaker

    Blocked --> Reflected : reflection completes
    Blocked --> Skipped : manual skip
    Blocked --> Idle : circuit breaker

    Reflected --> [*] : exit
    Skipped --> [*] : exit

    Idle : no reflection needed
    Active : ticket in progress
    Pending : reflection required
    Blocked : exit blocked until reflection
    Reflected : learnings captured
    Skipped : skip logged to stats
```

### 4.1 Transitions

| From | Event | To | Action |
|------|-------|----|--------|
| Idle | Ticket detected via discovery | Active | Store ticket context |
| Idle | Stop hook (session mode, diff > threshold) | Pending | Enable gate |
| Active | Ticket close confirmed (PostToolUse) | Pending | Enable gate |
| Active | Ticket close failed (PostToolUse) | Active | Clear intent, log |
| Active | Session ends without close | Idle | No reflection needed |
| Pending | Stop hook fires | Blocked | Block with instructions |
| Pending | Auto-skip (under threshold) | Skipped | Log skip to stats |
| Pending | Manual skip with reason | Skipped | Log skip to stats |
| Pending | Circuit breaker trips | Idle | Force approve, log warning |
| Blocked | Reflection completes | Reflected | Write learnings, log stats |
| Blocked | Manual skip with reason | Skipped | Log skip to stats |
| Blocked | Circuit breaker trips | Idle | Force approve, log warning |
| Reflected | Stop hook fires again | [*] | Allow exit |
| Skipped | Stop hook fires again | [*] | Allow exit |

### 4.2 Circuit Breaker

Prevents infinite blocking loops.

| Parameter | Default | Description |
|-----------|---------|-------------|
| `max_blocks` | 3 | Maximum blocks before tripping |
| `cooldown_seconds` | 300 | Time before breaker resets |

Behavior: each block increments a counter. When the counter reaches
`max_blocks`, the breaker trips, forcing an approve and logging a warning.

**Reset conditions** (any one triggers reset):

1. Cooldown elapsed since last block
2. Different `session_id` from last blocked session
3. Successful reflection completes

The breaker tracks `last_blocked_session_id` to distinguish new sessions
from retries within the same session.

## 5. Discovery

### 5.1 Ticketing System Discovery

```mermaid
flowchart TD
    A[Session Start] --> B{Config overrides?}
    B -->|Yes| C[Use configured order]
    B -->|No| D[Use default order]
    C --> E
    D --> E

    E{tissue detected?} -->|Yes| F[tissue active]
    E -->|No| G{beads detected?}
    G -->|Yes| H[beads active]
    G -->|No| I{Claude Code tasks?}
    I -->|Yes| J[tasks active]
    I -->|No| K[session fallback]
```

Default discovery order: `tissue → beads → tasks → session`

Each ticketing system has a detection probe (check for marker directory or
config) and a close pattern (command pattern that signals ticket completion).

| System | Detection | Close Pattern |
|--------|-----------|---------------|
| tissue | `.tissue/` exists | `tissue status * closed` |
| beads | `.beads/` exists | `beads close *`, `beads complete *` |
| tasks | Claude Code tasks active | Task completion signal |
| session | Always available | Stop hook fires (fallback) |

Discovery order is configurable. Individual systems can be disabled.

### 5.2 Memory Backend Discovery

```mermaid
flowchart TD
    A[Session Start] --> B{Config declares backend?}
    B -->|Yes| C[Use configured backend]
    B -->|No| D{Total Recall detected?}
    D -->|Yes| E[Total Recall active]
    D -->|No| F{MCP memory server?}
    F -->|Yes| G[MCP active]
    F -->|No| H[Built-in markdown]
```

Default discovery order: `config → total-recall → mcp → markdown`

| Backend | Detection |
|---------|-----------|
| Config explicit | `.grove/config.toml` declares a backend |
| Total Recall | `memory/` directory + `rules/total-recall.md` exists |
| MCP | MCP server with memory-compatible tools is registered |
| Markdown | Always available (built-in fallback) |

Multiple backends can be active simultaneously. Learnings route by scope.
Discovery order is configurable. Individual backends can be disabled.

## 6. Validation and Write Gate

Candidate learnings pass through a two-layer validation funnel before
being written to the backend. A third layer (stats-driven decay) operates
retrospectively.

### 6.1 Layer 1: Schema Validation

Structural validation. Deterministic, runs first.

| Field | Rule | On Failure |
|-------|------|------------|
| `category` | One of 7 enum values | Reject candidate |
| `summary` | 10-200 characters | Reject candidate |
| `detail` | 20-2000 characters | Reject candidate |
| `summary ≠ detail` | Must differ | Reject candidate |
| `tags` | 1-10 non-empty strings | Reject candidate |
| `scope` | One of 4 enum values | Default to `project` |
| `criteria_met` | At least one criterion claimed | Reject candidate |

On total failure (no valid JSON, or zero candidates after schema
validation): `grove reflect` returns an error. The Stop hook relays this
to Claude for retry. Circuit breaker applies if retries fail.

On partial failure (some candidates valid, some not): valid candidates
proceed to Layer 2. Rejections logged with reasons.

### 6.2 Layer 2: Write Gate Filter

Quality validation. Heuristic, runs on candidates that passed schema
validation.

Each candidate must claim at least one write gate criterion via the
`criteria_met` field:

| # | Criterion | Question |
|---|-----------|----------|
| 1 | `behavior_changing` | Would you do something differently next time? |
| 2 | `decision_rationale` | Why was X chosen over Y? |
| 3 | `stable_fact` | Will this matter in future sessions? |
| 4 | `explicit_request` | Did the user say "remember this"? |

Grove validates that the claimed criterion is plausible given the content.
The primary defense is tracking: if 100% of candidates claim
`behavior_changing` and hit rates are low, the insights engine flags this
as potential rubber-stamping.

### 6.3 Near-Duplicate Detection

Before writing, grove checks active learnings for substantially similar
summaries (case-insensitive substring match). Duplicates are rejected and
logged. This prevents the "I learned about the codebase" failure mode where
Claude produces the same generic learning every session.

### 6.4 Rejected Candidate Tracking

Rejected candidates are recorded (summary only) for write gate
effectiveness analysis. If a rejected topic appears in a later reflection
as an accepted learning, it signals the gate may be too aggressive.

### 6.5 Layer 3: Stats-Driven Decay (Retrospective)

Not a write-time check. Learnings that pass Layers 1 and 2 and are written
to the backend are subject to passive decay based on usage stats. Learnings
that are surfaced but never referenced accumulate low hit rates and are
eventually archived. See Section 8 (Passive Decay) in the Stats document.

## 7. Hook Behaviors

### 7.1 Session Start

Fires when a Claude Code session begins.

**Session identity:** All hook payloads include `session_id` (UUID v4) and
`transcript_path` via stdin JSON. Grove uses `session_id` as the primary
key for session state, stored at `~/.grove/sessions/<session_id>.json`.

**Resume handling:** When `source` is `"resume"`, Claude Code issues a new
`session_id`. Grove creates a fresh session. Ticket context and learnings
are discovered anew.

**Compact handling:** When `source` is `"compact"`, the same `session_id`
is reused. Grove loads the existing session file.

Steps:

1. Read `session_id`, `cwd`, `transcript_path` from stdin JSON
2. Create or load session state
3. Discover ticketing system (probe in configured order)
4. Discover memory backends (probe in configured order)
5. Load stats and learnings index
6. Search for learnings relevant to current context
7. Score and rank candidates (composite: relevance × recency × hit rate)
8. Record "surfaced" event in stats log for each injected learning
9. Return context with top N learnings via `additionalContext`
10. Run passive decay check (throttled to once per day)

### 7.2 Pre-Tool-Use

Fires before Claude Code executes a tool.

1. Match tool name and input against ticket close patterns
2. If match: record ticket close *intent* in session state, add trace event
3. Allow the tool to proceed (gate fires on Stop, not here)

Design choice: unlike roz which denies tools, grove allows ticket-close tools.
The ticket close should succeed; reflection happens before the session ends.

**Note:** PreToolUse fires *before* execution — the command might fail. Grove
records the intent here and confirms via PostToolUse.

### 7.3 Post-Tool-Use

Fires after Claude Code executes a tool.

1. If a ticket close intent was recorded in PreToolUse:
   - Check `tool_response.success` from the PostToolUse payload
   - If successful: transition gate status Active → Pending
   - If failed: clear the intent, log trace event ("ticket close failed")
2. If no intent recorded: no-op

This two-phase approach (PreToolUse records intent, PostToolUse confirms)
solves the problem of pattern-matching against commands that might fail.
The gate only transitions when the ticket close actually succeeded.

### 7.4 Stop

Fires when Claude Code attempts to end a response.

1. If gate status is Reflected or Skipped → approve
2. If gate status is Idle (session mode):
   - Compute diff size via `git diff --stat` (cached in session state)
   - If not a git repo, treat diff size as "unknown" (skip threshold check)
   - If under threshold and auto-skip enabled → agent decides → log → approve
3. If gate status is Active or Pending:
   - Check circuit breaker → if tripped, force approve with warning
   - Increment block counter
   - Block with instructions to run reflection
4. Log trace event

**Diff size computation:** Grove shells out to `git diff --stat HEAD` in
the Stop hook. This adds ~50ms latency but gives accurate line counts. The
result is cached in session state so subsequent Stop invocations (after
block → retry) don't recompute. In non-git repositories, diff size is
unavailable and the auto-skip threshold check is bypassed (agent always
decides).

### 7.5 Session End

Fires when a Claude Code session terminates.

1. Load session state
2. For each injected learning not marked as referenced: append a
   `dismissed` event to the stats log
3. Clean up any temporary state
4. Always allows session termination (SessionEnd hooks cannot block)

This hook is the authoritative point for dismissed detection. Previously
dismissed inference happened at the Stop hook, but Stop fires before
session end and may fire multiple times. SessionEnd fires exactly once,
after all work is complete.

**Note:** SessionEnd reason codes include `clear`, `logout`,
`prompt_input_exit`, and `other`. Grove processes all of them identically.

## 8. Multi-Agent Behavior

When parallel subagents are running:

- **Subagents** log lightweight observations via `grove observe`. No gate,
  no structured reflection, just append-only notes to the session.
- **The orchestrator** performs the full structured reflection when the ticket
  closes. It draws on subagent observations when synthesizing learnings.
- **The gate only fires for the orchestrator.** Subagent stop events do not
  trigger the compound learning gate.

This prevents noisy, overlapping reflections while preserving
subagent-specific insights.

## 9. Sequence Diagrams

### 9.1 Happy Path: Ticket Close with Reflection

```mermaid
sequenceDiagram
    participant User
    participant CC as Claude Code
    participant SS as session-start
    participant PT as pre-tool-use
    participant Stop as stop
    participant Grove as grove

    User->>CC: Work on ticket
    CC->>SS: session start
    Note over SS: discover ticketing<br/>discover backends<br/>inject learnings
    SS-->>CC: context + learnings

    Note over CC: agent works on ticket...

    CC->>PT: tissue status X closed
    Note over PT: ticket close detected<br/>gate → Pending
    PT-->>CC: allow (tool proceeds)

    CC->>Stop: tries to exit
    Note over Stop: gate Pending<br/>reflection required
    Stop-->>CC: block + instructions

    CC->>Grove: grove reflect
    Note over Grove: structured reflection<br/>write gate filter<br/>route to backend<br/>update stats
    Grove-->>CC: reflection complete

    CC->>Stop: tries to exit
    Note over Stop: gate Reflected
    Stop-->>CC: approve

    CC-->>User: Done
```

### 9.2 Auto-Skip: Trivial Change

```mermaid
sequenceDiagram
    participant CC as Claude Code
    participant Stop as stop
    participant Grove as grove

    CC->>Stop: tries to exit
    Note over Stop: check diff size<br/>2 lines changed<br/>under threshold

    Stop->>Grove: grove skip "auto: 2 lines, version bump"
    Note over Grove: log skip to stats

    Stop-->>CC: approve
```

### 9.3 Multi-Agent: Subagent Observations

```mermaid
sequenceDiagram
    participant Orch as Orchestrator
    participant Sub1 as Security Reviewer
    participant Sub2 as Perf Reviewer
    participant Stop as stop
    participant Grove as grove

    Orch->>Sub1: review security
    Note over Sub1: finds pattern
    Sub1->>Grove: grove observe "auth middleware ordering matters"

    Orch->>Sub2: review performance
    Note over Sub2: finds N+1
    Sub2->>Grove: grove observe "N+1 in user dashboard"

    Note over Orch: ticket closes, gate → Pending

    Orch->>Stop: tries to exit
    Stop-->>Orch: block

    Orch->>Grove: grove reflect
    Note over Grove: reads subagent<br/>observations<br/>synthesizes learnings
    Grove-->>Orch: reflection complete

    Orch->>Stop: approve
```

### 9.4 Circuit Breaker

```mermaid
sequenceDiagram
    participant CC as Claude Code
    participant Stop as stop

    Note over Stop: block_count = 2<br/>max_blocks = 3

    CC->>Stop: tries to exit
    Note over Stop: block_count = 3
    Stop-->>CC: block + instructions

    Note over CC: still doesn't reflect...

    CC->>Stop: tries to exit
    Note over Stop: block_count >= max_blocks<br/>CIRCUIT BREAKER
    Stop-->>CC: approve (forced, with warning)
```

### 9.5 Retrieval: Learning Injection at Session Start

```mermaid
sequenceDiagram
    participant CC as Claude Code
    participant SS as session-start
    participant Stats as Stats
    participant Backend as Memory Backend

    CC->>SS: session start
    SS->>SS: detect ticket context

    SS->>Backend: search(ticket context, file paths)
    Backend-->>SS: matching learnings

    SS->>Stats: score learnings
    Note over Stats: relevance × recency × hit_rate<br/>cap at max_injections

    SS->>Stats: record "surfaced" for each

    SS-->>CC: context with top N learnings
```

## 10. Storage Architecture

### 10.1 File Layout

```text
~/.grove/                          # User-level (not committed)
├── sessions/
│   ├── <session-id>.json          # Per-session state
│   └── ...
├── stats-cache.json               # Materialized stats aggregate
└── config.toml                    # Optional user config

<project>/
├── .grove/
│   ├── config.toml                # Optional project config
│   ├── learnings.md               # Append-only learnings (built-in backend)
│   └── stats.log                  # Append-only event log (JSONL)
```

### 10.2 Session Storage Interface

| Operation | Description |
|-----------|-------------|
| `get(id)` | Retrieve session by ID |
| `put(session)` | Save session (atomic write) |
| `list(limit)` | List recent sessions |
| `delete(id)` | Remove session |

### 10.3 Memory Backend Interface

| Operation | Description |
|-----------|-------------|
| `write(learning)` | Write a learning to the backend |
| `search(query, filters)` | Search for relevant learnings |
| `ping()` | Health check / availability |

**Search semantics vary by backend:**

| Backend | Search Implementation |
|---------|---------------------|
| **Markdown** | Parse `.grove/learnings.md`, match tags against query tags (exact match = 1.0, partial = 0.5), match file paths against `context_files` (overlap = 0.8), keyword substring match against summary and detail (0.3). Return all matches with relevance scores. |
| **Total Recall** | Delegate to Total Recall's search command. Relevance scoring handled by Total Recall. |
| **MCP** | Invoke the MCP server's search tool. Relevance scoring handled by the server. |

The `query` parameter is a structured object containing available context:
ticket title, ticket description, file paths from the current diff, and
tags extracted from the ticket. It is not a free-text search string.

The `filters` parameter supports: `status` (active only by default),
`scope` (project/team/personal), `created_after` (for recency), and
`max_results`.

### 10.4 Backend Implementations

| Backend | Storage | Use Case |
|---------|---------|----------|
| **Markdown** | `.grove/learnings.md` | Default, no dependencies |
| **Total Recall** | `memory/` (Total Recall managed) | Tiered memory with write gate |
| **MCP** | External server | Custom or shared memory systems |
| **In-Memory** | None (ephemeral) | Testing only |

## 11. Configuration

All configuration is optional. The system runs with sensible defaults when
no config exists.

### 11.1 Config Precedence

1. Environment variables (`GROVE_HOME`, etc.)
2. Project config (`.grove/config.toml`)
3. User config (`~/.grove/config.toml`)
4. Defaults

### 11.2 Config Shape

| Section | Key | Default | Description |
|---------|-----|---------|-------------|
| ticketing | discovery | `[tissue, beads, tasks, session]` | Ordered probe list |
| ticketing | overrides | `{}` | Per-system enable/disable |
| backends | discovery | `[config, total-recall, mcp, markdown]` | Ordered probe list |
| backends | overrides | `{}` | Per-backend enable/disable |
| gate | auto_skip.enabled | `true` | Allow auto-skip for trivial changes |
| gate | auto_skip.line_threshold | `5` | Diff size below which auto-skip applies |
| gate | auto_skip.decider | `agent` | Who decides: `agent`, `always`, `never` |
| decay | passive_duration_days | `90` | Days without reference before archival |
| decay | immunity_hit_rate | `0.8` | Hit rate above which decay is skipped |
| retrieval | max_injections | `5` | Max learnings injected per session |
| retrieval | strategy | `moderate` | `conservative`, `moderate`, `aggressive` |
| circuit_breaker | max_blocks | `3` | Blocks before forced approve |
| circuit_breaker | cooldown_seconds | `300` | Cooldown before breaker resets |

## 12. Error Philosophy

### 12.1 Fail-Open

Infrastructure errors never block work. If grove can't read state, write
stats, or reach a backend, the gate approves and logs a warning. A lost
learning is better than a stuck developer.

### 12.2 Specific Fail-Open Behaviors

| Scenario | Behavior |
|----------|----------|
| Session state unreadable | Approve exit |
| Backend unreachable | Log warning, skip learning write, still mark reflected |
| Stats file corrupt | Log warning, rebuild on next `grove maintain` |
| Discovery fails | Proceed with no ticketing / fallback backend |
| Reflection parsing fails | Approve exit, log error |

## Related Documents

- [Overview](./00-overview.md) - Vision, core concepts, design principles
- [Implementation](./02-implementation.md) - Concrete types, file layouts,
  module structure
- [Stats and Quality](./03-stats-and-quality.md) - Quality tracking model,
  retrieval scoring, insights engine
- [Test Plan](./04-test-plan.md) - Testing strategy
- [CI](./05-ci.md) - Version management and release workflow
