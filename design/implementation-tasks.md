# grove - Implementation Tasks

This document defines the staged implementation approach. Stage 1 delivers a
functional system with reduced complexity. Stage 2 adds sophistication based
on real usage patterns.

## Stage 1: Core System

Delivers: Gate enforcement, structured reflection, markdown backend, basic
stats tracking. Sufficient to prove the compound learning concept.

### 1.1 Core Module

| Task | Description | Files |
|------|-------------|-------|
| Session state types | `SessionState`, `GateState`, `GateStatus`, `TicketContext`, `SubagentObservation`, `CircuitBreakerState` | `core/state.rs` |
| Gate state machine | All transitions per architecture Section 4 | `core/gate.rs` |
| Learning types | `CompoundLearning` with `schema_version: u8` and `session_id` (always present), `LearningCategory`, `LearningScope`, `LearningStatus` | `core/learning.rs` |
| Reflection parsing | Parse structured output, schema validation (Layer 1) | `core/reflect.rs` |
| Write gate filter | Quality validation (Layer 2), criteria checking | `core/reflect.rs` |
| Near-duplicate detection | **Exact match only** (case-insensitive summary match) | `core/reflect.rs` |

**Deferred to Stage 2:**

- Substring/fuzzy duplicate detection

**Explicitly not implemented (per edge-cases review):**

- Multi-ticket stack (single-ticket tracking sufficient)
- `subagent_id` in observations (content matters, not source)
- `origin_author` in learnings (always an agent)
- Skip categories enum (freeform text, categorize later if needed)
- Learning invalidation detection (rely on decay + manual maintenance)
- In-memory learnings index (premature optimization)

### 1.2 Discovery Module

| Task | Description | Files |
|------|-------------|-------|
| Ticketing detection | Probe tissue, beads, session fallback | `discovery/tickets.rs` |
| Close pattern matching | Match `tissue status * closed`, `beads close *` | `discovery/tickets.rs` |
| Backend detection | Probe markdown backend only | `discovery/backends.rs` |

**Deferred to Stage 2:**

- Claude Code tasks as ticketing system
- Total Recall backend detection
- MCP backend detection

### 1.3 Backend Module

| Task | Description | Files |
|------|-------------|-------|
| MemoryBackend trait | `write`, `search`, `ping` | `backends/traits.rs` |
| Markdown backend | Append-only `.grove/learnings.md`, parse/search | `backends/markdown.rs` |
| Content sanitization | Sanitize summary, detail, tags before write | `backends/markdown.rs` |
| Archive support | Mark learning as archived (status change in-place) | `backends/markdown.rs` |

**Deferred to Stage 2:**

- `backends/total_recall.rs`
- `backends/mcp.rs`

**Explicitly not implemented:**

- File locking for concurrent writes (single-developer workflow)

### 1.4 Stats Module

| Task | Description | Files |
|------|-------------|-------|
| Event types | `StatsEvent` with `v: u8` version field, `StatsEventType` enum | `stats/tracker.rs` |
| Event log writer | Append JSONL to `.grove/stats.log` | `stats/tracker.rs` |
| Materialized cache | `StatsCache` struct, rebuild from log when stale (line count != processed) | `stats/tracker.rs` |
| Per-learning stats | surfaced/referenced/dismissed/hit_rate | `stats/tracker.rs` |
| Passive decay | Archive learnings past threshold (90 days) | `stats/decay.rs` |
| Decay immunity | Skip decay for high hit-rate learnings | `stats/decay.rs` |
| Basic retrieval | **Relevance matching only** (tags, files, keywords) | `stats/scoring.rs` |
| Basic insights | **DecayWarning** and **HighCrossPollination** only | `stats/insights.rs` |
| Skip tracking | Record skip events with reason/decider/lines | `stats/skip.rs` |

**Deferred to Stage 2:**

- Composite scoring (relevance × recency × hit_rate)
- Recency weight with exponential decay
- Reference boost calculation
- Strategy modes (conservative/moderate/aggressive)
- 7 additional insight types (LowHitCategory, HighValueRare, SkipMiss,
  WriteGateTooStrict, WriteGateTooLoose, RubberStamping, StaleTopLearning)
- Retrospective miss detection
- Retrospective skip miss detection

### 1.5 Storage Module

| Task | Description | Files |
|------|-------------|-------|
| SessionStore trait | `get`, `put`, `list`, `delete` | `storage/traits.rs` |
| File-based storage | JSON files in `~/.grove/sessions/` | `storage/file.rs` |
| Atomic writes | Temp file + rename pattern | `storage/file.rs` |
| In-memory storage | For testing | `storage/memory.rs` |

**Removed:**

- `find_recent(cwd, status)` — not needed without resume inheritance

### 1.6 Hook Module

| Task | Description | Files |
|------|-------------|-------|
| Input deserialization | Parse Claude Code hook JSON | `hooks/input.rs` |
| Output serialization | Format hook responses | `hooks/output.rs` |
| Hook dispatch | Route to appropriate handler | `hooks/runner.rs` |

### 1.7 Hook Handlers

| Hook | Stage 1 Behavior | Files |
|------|------------------|-------|
| session-start | Create new session, discover ticketing/backend, inject learnings (basic relevance), run decay check | `cli/hook.rs` |
| pre-tool-use | Detect ticket close intent, record in session | `cli/hook.rs` |
| post-tool-use | Confirm/reject ticket close, transition gate | `cli/hook.rs` |
| stop | Check gate status, block/approve, circuit breaker with 3 reset conditions | `cli/hook.rs` |
| session-end | Log dismissed events, cleanup | `cli/hook.rs` |

**Removed:**

- subagent-stop hook

### 1.8 CLI Commands

| Command | Stage 1 Behavior | Files |
|---------|------------------|-------|
| `grove hook <event>` | Dispatch to handlers | `cli/hook.rs` |
| `grove reflect` | Schema validate, write gate, write to markdown, log stats | `cli/reflect.rs` |
| `grove skip <reason>` | Record skip, set gate to Skipped | `cli/skip.rs` |
| `grove observe <note>` | Append observation to session | `cli/observe.rs` |
| `grove search <query>` | Search markdown backend | `cli/search.rs` |
| `grove list` | List learnings from markdown | `cli/list.rs` |
| `grove stats` | Dashboard with basic insights, warn if learnings.md > 500KB | `cli/stats.rs` |
| `grove maintain` | Review stale, archive, restore | `cli/maintain.rs` |
| `grove init` | Scaffold config, learnings, sessions | `cli/init.rs` |
| `grove backends` | Show markdown backend status | `cli/backends_cmd.rs` |
| `grove tickets` | Show ticketing system | `cli/tickets_cmd.rs` |
| `grove debug <id>` | Dump session state | `cli/debug.rs` |
| `grove trace <id>` | Show trace events | `cli/trace.rs` |
| `grove clean` | Remove old sessions, detect orphans with `--orphans` | `cli/clean.rs` |
| Panic handler | Global panic hook with exit(3), crash logging | `main.rs` |

**Removed from `grove maintain`:**

- `--compact-log` flag
- `--rebuild-stats` (cache rebuilds automatically when stale)

### 1.9 Configuration

| Task | Description | Files |
|------|-------------|-------|
| Config loading | TOML parsing, precedence | `config.rs` |
| Default values | All defaults per architecture Section 11.2 | `config.rs` |

### 1.10 Plugin Integration

| Task | Description | Files |
|------|-------------|-------|
| hooks.json | Configure 5 hooks (no subagent-stop) | `hooks/hooks.json` |
| plugin.json | Plugin manifest | `.claude-plugin/plugin.json` |
| install.sh | Binary installer | `.claude-plugin/install.sh` |
| Skills | 5 skill definitions | `skills/*/SKILL.md` |
| Rules | Gate protocol | `rules/compound-gate.md` |

---

## Stage 2: Sophistication

Adds: Multiple backends, advanced scoring, full insights engine, additional
ticketing systems. Build after Stage 1 is validated with real usage.

### 2.1 Additional Backends

| Task | Description | Files |
|------|-------------|-------|
| Total Recall adapter | Route to `recall-write`, `recall-search` | `backends/total_recall.rs` |
| MCP adapter | Route through MCP memory server | `backends/mcp.rs` |
| Multi-backend routing | Route by scope to appropriate backend | `discovery/backends.rs` |

### 2.2 Advanced Retrieval Scoring

| Task | Description | Files |
|------|-------------|-------|
| Composite scoring | `relevance × recency_weight × reference_boost` | `stats/scoring.rs` |
| Recency weight | Exponential decay from creation date | `stats/scoring.rs` |
| Reference boost | `0.5 + (hit_rate × 0.5)` | `stats/scoring.rs` |
| Strategy modes | conservative/moderate/aggressive behaviors | `stats/scoring.rs` |

### 2.3 Full Insights Engine

| Task | Description | Files |
|------|-------------|-------|
| LowHitCategory | Category hit rate < 0.3 | `stats/insights.rs` |
| HighValueRare | Category hit rate > 0.7, count < 5 | `stats/insights.rs` |
| SkipMiss | Skipped session later produced learnings | `stats/insights.rs` |
| WriteGateTooStrict | Pass rate < 0.5 | `stats/insights.rs` |
| WriteGateTooLoose | Pass rate > 0.95, hit rate < 0.3 | `stats/insights.rs` |
| RubberStamping | >90% claim same criterion | `stats/insights.rs` |
| StaleTopLearning | Top learning not referenced in 60+ days | `stats/insights.rs` |
| Retrospective misses | Rejected topic later accepted | `stats/insights.rs` |

### 2.4 Additional Ticketing

| Task | Description | Files |
|------|-------------|-------|
| Claude Code tasks | Detect and track task completion | `discovery/tickets.rs` |

### 2.5 Enhanced Duplicate Detection

| Task | Description | Files |
|------|-------------|-------|
| Substring matching | Case-insensitive substring match for near-duplicates | `core/reflect.rs` |

### 2.6 Correction Propagation

| Task | Description | Files |
|------|-------------|-------|
| Proactive notices | Inject correction notice for recently-surfaced corrected learnings | `hooks/runner.rs` |

---

## Implementation Order (Stage 1)

Recommended sequence for Stage 1 development:

1. **Foundation**
   - Error types (`error.rs`)
   - Config loading (`config.rs`)
   - Core types (`core/state.rs`, `core/learning.rs`)

2. **Storage**
   - Session storage trait and file implementation
   - In-memory storage for tests

3. **Gate**
   - Gate state machine (`core/gate.rs`)
   - All transitions with unit tests

4. **Backend**
   - MemoryBackend trait
   - Markdown backend (write, search, archive)

5. **Stats**
   - Event log writer
   - Materialized cache
   - Basic retrieval scoring

6. **Reflection**
   - Schema validation
   - Write gate filter
   - Exact-match deduplication

7. **Discovery**
   - Ticketing detection (tissue, beads, session)
   - Backend detection (markdown only)

8. **Hooks**
   - Input/output serialization
   - All 5 hook handlers

9. **CLI**
   - All commands
   - Integration tests

10. **Plugin**
    - hooks.json, plugin.json, skills, rules

---

## Success Criteria

### Stage 1 Complete When

- [ ] Gate blocks exit until reflection or skip
- [ ] Reflection writes to markdown backend
- [ ] Stats log tracks surfaced/referenced/dismissed
- [ ] `grove stats` shows hit rates and cross-pollination
- [ ] Circuit breaker prevents infinite blocking
- [ ] Passive decay archives stale learnings

### Stage 2 Complete When

- [ ] Total Recall and MCP backends functional
- [ ] Composite scoring improves retrieval relevance
- [ ] Full insights engine provides actionable recommendations
- [ ] Claude Code tasks supported as ticketing system
