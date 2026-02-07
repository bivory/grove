# grove - Edge Cases and Risk Mitigations

This document addresses edge cases, risks, and gaps identified during
design review. Each section describes the issue, impact, and concrete
mitigation implemented in the design.

## Decision Summary

| # | Item | Decision |
|---|------|----------|
| 1 | Multi-Ticket Sessions | **Dropped** — single-ticket tracking sufficient |
| 2 | Schema Versioning | **Implement** — `v` field in JSONL, no version in markdown header |
| 3 | Concurrent Session Handling | **Dropped** — single developer workflow |
| 4 | Subagent Observation Aggregation | **Implement** — drop `subagent_id` field |
| 5 | Circuit Breaker Reset Logic | **Implement** — three reset conditions as proposed |
| 6 | Orphaned Pending State | **Simplified** — detect + warn + delete only |
| 7 | Learning Attribution | **Simplified** — keep `origin_session`, drop `origin_author` |
| 8 | Markdown Sanitization | **Implement** — all four sanitization rules |
| 9 | Skip Reason Structure | **Simplified** — freeform text only |
| 10 | Learning Invalidation Detection | **Deferred** — rely on decay + manual maintenance |
| 11 | Large Learning File Performance | **Simplified** — file size warning only |
| 12 | Hook Crash Behavior | **Implement** — panic handler + crash logging |
| 13 | Stats Cache Staleness | **Implement** — line count comparison with `!=` |

## 1. Multi-Ticket Sessions

> **Decision: DROPPED** — Single-ticket tracking is sufficient. Ticket
> interleaving is not expected in the target workflow. Claude agents work
> on one ticket at a time within a session.

### Issue

The per-ticket granularity design assumes developers work on one ticket at
a time. In practice, developers context-switch: work on ticket-A, pause,
work on ticket-B, close ticket-A. If the gate fires on ticket-A close, the
reflection covers work from both tickets but is attributed only to ticket-A.

### Impact

Learning attribution becomes incoherent. Stats credit ticket-A for
learnings that may have come from ticket-B work.

### Original Mitigation (Not Implemented)

The original proposal involved an active ticket stack. This complexity is
not needed for the single-ticket workflow.

## 2. Schema Versioning

> **Decision: IMPLEMENT** — Essential for evolution. Add `v` field to JSONL.
> Do NOT include version in markdown header (visual noise for humans).

### Issue

Neither learnings nor stats events have version fields. When formats
change, there's no migration story.

### Impact

Old entries become unparsable or misinterpreted. The append-only log
becomes a liability.

### Mitigation

**Version field in all persisted JSONL structures:**

```jsonl
{"v":1,"ts":"2026-02-06T10:00:00Z","event":"surfaced",...}
```

**Forward-compatible parsing.** Unknown fields are ignored. Missing fields
use defaults. The `v` field gates which fields to expect.

**Migration on read.** When grove reads an entry with `v < CURRENT_VERSION`,
it migrates in-memory to the current schema. No rewriting of the log.

**Markdown header unchanged.** Version is tracked internally in the
`CompoundLearning` struct but not displayed in the markdown output. The
markdown file is human-first; schema version is an implementation detail.

### Implementation Changes

| Component | Change |
|-----------|--------|
| `StatsEvent` | Add `v: u8` field, current = 1 |
| `CompoundLearning` | Add `schema_version: u8` field (internal, not in markdown) |
| Parsing | Check `v`, migrate if needed |

## 3. Concurrent Session Handling

> **Decision: DROPPED** — Single developer workflow. Concurrent writes
> are not a concern.

### Issue

Two developers working in the same repo simultaneously:

- Both appending to `learnings.md` at the same millisecond → malformed file
- Both appending to `stats.log` → safe (JSONL)
- Racing `grove hook stop` → race condition on session state

### Impact

File corruption. Lost learnings. Inconsistent state.

### Original Mitigation (Not Implemented)

File locking was proposed but is unnecessary for single-developer use.
Session state is already isolated by `session_id`. If multi-developer
support is needed later, file locking can be added.

## 4. Subagent Observation Aggregation

> **Decision: IMPLEMENT** — Subagents are used frequently. Drop the
> `subagent_id` field (YAGNI — content matters, not which agent said it).

### Issue

`grove observe` logs observations, and "orchestrator reflects"—but when
and how does the orchestrator see these observations?

### Impact

Subagent insights may be lost. Orchestrator reflection may miss valuable
context.

### Mitigation

**Observations stored in session state.** `grove observe` appends to
`GateState.subagent_observations`:

```rust
struct GateState {
    subagent_observations: Vec<SubagentObservation>,
    // ...
}

struct SubagentObservation {
    note: String,
    timestamp: DateTime<Utc>,
}
```

**Injected into reflection prompt.** The `/compound-reflect` skill
includes observations in the prompt context:

```text
Subagent observations from this session:
- [12:30] "auth middleware ordering matters"
- [12:35] "N+1 in user dashboard"

Consider these when extracting learnings.
```

**Synthesized, not duplicated.** The reflection extracts learnings from
observations—it doesn't copy them verbatim. Multiple related observations
can become a single learning.

### Implementation Changes

| Component | Change |
|-----------|--------|
| `GateState` | Add `subagent_observations: Vec<SubagentObservation>` |
| `grove observe` | Append to session state |
| `/compound-reflect` | Include observations in prompt |

## 5. Circuit Breaker Reset Logic

> **Decision: IMPLEMENT** — The breaker prevents infinite loops within a
> session. New session resets are acceptable (users won't abuse restarts).

### Issue

The circuit breaker "resets after cooldown or on new session." What
defines a new session? If a developer restarts Claude Code at intervals
shorter than cooldown, they might always hit the breaker immediately.

### Impact

Developers could be perpetually unable to reflect, or perpetually
auto-approved.

### Mitigation

**Explicit reset conditions:**

1. **Cooldown elapsed:** `now - last_block_time > cooldown_seconds`
2. **New session:** Different `session_id` from the last blocked session
3. **Successful reflection:** Any completed reflection resets the breaker

**Breaker state includes last blocked session:**

```rust
struct CircuitBreakerState {
    block_count: u32,
    last_block_time: Option<DateTime<Utc>>,
    last_blocked_session_id: Option<String>,
    tripped: bool,
}
```

**Reset check at Stop hook:**

```rust
fn should_reset_breaker(state: &CircuitBreakerState, session_id: &str) -> bool {
    // Different session always resets
    if state.last_blocked_session_id.as_deref() != Some(session_id) {
        return true;
    }
    // Cooldown elapsed resets
    if let Some(last) = state.last_block_time {
        if Utc::now() - last > config.cooldown {
            return true;
        }
    }
    false
}
```

### Implementation Changes

| Component | Change |
|-----------|--------|
| `CircuitBreakerState` | Add `last_blocked_session_id` |
| Stop hook | Check reset conditions before incrementing |
| `grove reflect` | Reset breaker on successful reflection |

## 6. Orphaned Pending State After Crash

> **Decision: SIMPLIFIED** — Detect orphans, warn, delete. Skip the stats
> reconciliation complexity (orphans are rare, stats impact is minimal).

### Issue

Machine crashes with gate status = Pending. Next session starts fresh
(new `session_id`), reflection never happened. Stats show surfaced
learnings with no reflection/skip event.

### Impact

Stats become inconsistent. Learnings appear permanently surfaced but
never resolved.

### Mitigation

**Session file cleanup with orphan detection.** `grove clean` identifies
orphaned sessions:

```rust
fn find_orphaned_sessions(sessions_dir: &Path) -> Vec<PathBuf> {
    list_session_files(sessions_dir)
        .filter(|f| {
            let state = load_session(f);
            // Orphaned if: pending/blocked AND older than 24 hours
            matches!(state.gate.status, Pending | Blocked | Active)
                && state.updated_at < Utc::now() - Duration::hours(24)
        })
        .collect()
}
```

**Simple resolution:** `grove clean --orphans` logs a warning and deletes
orphaned session files. No stats reconciliation — the missing events are
acceptable given the rarity of crashes.

### Implementation Changes

| Component | Change |
|-----------|--------|
| `grove clean` | Add `--orphans` flag |
| `find_orphaned_sessions` | Detect pending sessions older than threshold |
| Orphan cleanup | Log warning, delete session files |

## 7. Learning Attribution

> **Decision: SIMPLIFIED** — Keep `origin_session` for debugging. Drop
> `origin_author` (always an agent in this workflow).

### Issue

`origin_ticket` is tracked but not `origin_session` or `origin_author`.
Important for debugging and trust.

### Impact

Can't determine who created a learning or investigate session context
when a learning is wrong.

### Mitigation

**Session attribution only:**

```rust
struct CompoundLearning {
    origin_ticket: Option<String>,
    origin_session: String,         // Always present (was already session_id)
    // origin_author dropped — always "Claude" in this workflow
    // ...
}
```

**Markdown format includes session:**

```markdown
### [cl_20260206_001] N+1 query in UserDashboard#index
- **Category:** pitfall
- **Origin:** ticket T042 | session abc123
```

**Stats events include session for correlation:**

```jsonl
{"v":1,"event":"reflection","session_id":"abc123",...}
```

### Implementation Changes

| Component | Change |
|-----------|--------|
| `CompoundLearning` | Ensure `session_id` is always present |
| Markdown backend | Include session in attribution |
| Stats events | Include session_id in reflection events |

## 8. Markdown Sanitization

> **Decision: IMPLEMENT** — Cheap insurance against malformed output.
> All four sanitization rules as proposed.

### Issue

Learning summary/detail come from Claude and could contain markdown that
breaks file parsing or confuses retrieval.

### Impact

Malformed learnings file. Broken parsing. Potential for injection
attacks on tooling that consumes the file.

### Mitigation

**Sanitization rules for learning content:**

| Content | Rule |
|---------|------|
| Summary | Single line, no markdown headers, escape `\|` for tables |
| Detail | Allow markdown but escape unbalanced code fences |
| Tags | Alphanumeric + hyphens only, no markdown |
| Context files | Validated as relative paths, no `..` |

**Implementation:**

```rust
fn sanitize_summary(s: &str) -> String {
    s.lines().next().unwrap_or("")
        .replace('#', r"\#")
        .replace('|', r"\|")
        .trim()
        .to_string()
}

fn sanitize_detail(s: &str) -> String {
    // Balance code fences
    let fence_count = s.matches("```").count();
    if fence_count % 2 != 0 {
        format!("{}\n```", s)
    } else {
        s.to_string()
    }
}

fn sanitize_tag(s: &str) -> Option<String> {
    let cleaned: String = s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect();
    if cleaned.is_empty() { None } else { Some(cleaned.to_lowercase()) }
}
```

**Validation at write time.** The markdown backend validates before
appending. Invalid learnings are rejected with clear error messages.

### Implementation Changes

| Component | Change |
|-----------|--------|
| `core/reflect` | Add sanitization pass after parsing |
| Markdown backend | Validate before write |
| Error messages | Clear feedback on what was sanitized/rejected |

## 9. Skip Reason Structure

> **Decision: SIMPLIFIED** — Freeform text only for v1. Categories can be
> derived retroactively via LLM analysis if needed.

### Issue

`grove skip <reason>` takes free text. No categories for analyzing
skip patterns.

### Impact

Can't aggregate skip patterns. "later", "L8R", "tbd" are all different.
Skip abuse is undetectable.

### Mitigation

**Freeform text with context:**

```rust
struct SkipDecision {
    reason: String,           // Freeform text
    decider: SkipDecider,
    lines_changed: u32,
    timestamp: DateTime<Utc>,
}
```

**CLI unchanged:**

```bash
grove skip "version bump"
grove skip "hotfix, will reflect tomorrow"
```

**Deferred categorization:** If skip pattern analysis becomes valuable,
categories can be derived from existing freeform text via pattern matching
or LLM classification. No upfront structure required.

### Implementation Changes

| Component | Change |
|-----------|--------|
| `SkipDecision` | Keep simple: reason, decider, lines_changed, timestamp |
| Stats | Store raw skip reasons, analyze later if needed |

## 10. Learning Invalidation Detection

> **Decision: DEFERRED** — Rely on passive decay (90-day expiry) and
> manual maintenance via `grove maintain`. Invalidation detection adds
> complexity for uncertain payoff.

### Issue

A learning says "use pattern X." The codebase refactors away from pattern
X. The learning is now actively harmful but nothing detects this.

### Impact

Stale learnings mislead developers. Compound learning becomes compound
confusion.

### Deferred Mitigation

The original proposal involved tracking context file changes and flagging
modified/deleted files. This is speculative complexity:

- "Heavily modified" is fuzzy and hard to threshold
- File changes don't necessarily invalidate learnings
- Pattern matching requires semantic parsing

**v1 approach:** Rely on existing mechanisms:

- Passive decay archives learnings not referenced in 90 days
- `grove maintain` allows manual review and archival
- If stale learnings become a real problem, revisit with simpler heuristics
  (e.g., file deleted = flag for review)

## 11. Large Learning File Performance

> **Decision: SIMPLIFIED** — File size warning only. Defer in-memory
> indexing until scale problems are observed. Math: 260 learnings/year
> × 500 bytes = 130KB. Not a concern for years.

### Issue

All learnings in a single file. Searching for duplicates requires reading
the entire file. After a year, the file could be megabytes.

### Impact

Slow duplicate detection. Slow injection. Poor UX.

### Mitigation

**File growth monitoring only.** `grove stats` warns if learnings file
exceeds threshold (default: 500KB):

```text
⚠️  learnings.md is 1.2MB — consider archiving old learnings
```

**Deferred: in-memory indexing.** The `LearningsIndex` structure and
caching can be added if/when file size becomes a real problem. Reading
500KB is ~1ms on modern disks.

**Deferred: archive command.** `grove maintain --archive` can be added
later to move old learnings to a separate file.

### Implementation Changes

| Component | Change |
|-----------|--------|
| `grove stats` | Warn on large file (>500KB) |

## 12. Hook Crash Behavior

> **Decision: IMPLEMENT** — Essential for production reliability. Panic
> handler with distinct exit code and crash logging.

### Issue

What if the grove binary crashes during hook execution? What exit code
does a panic produce? Does Claude Code distinguish "hook said block" from
"hook crashed"?

### Impact

Unclear behavior. Potential for stuck sessions or silent failures.

### Mitigation

**Panic handler with clean exit.** Set a panic hook that logs the error
and exits with a distinct code:

```rust
fn main() {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("grove panic: {}", info);
        // Exit 3 = crash (distinct from 0=approve, 2=block)
        std::process::exit(3);
    }));
    // ... normal execution
}
```

**Exit code semantics:**

| Code | Meaning |
|------|---------|
| 0 | Approve (hook completed, allow action) |
| 2 | Block (hook completed, deny action) |
| 3 | Crash (hook failed, fail-open: allow action) |
| Other | Treated as crash |

**Claude Code behavior on crash.** Document expected behavior: Claude Code
should treat non-0, non-2 exit codes as "hook crashed, proceed with
action" (fail-open). If Claude Code doesn't implement this, the plugin
docs should note the limitation.

**Note:** Verify Claude Code's actual behavior on unknown exit codes
before finalizing the crash exit code.

**Crash logging.** Panics write to `~/.grove/crash.log` before exit:

```text
2026-02-06T10:00:00Z session=abc123 hook=stop panic="index out of bounds"
```

### Implementation Changes

| Component | Change |
|-----------|--------|
| `main.rs` | Set panic hook with exit(3) |
| Panic hook | Log to `~/.grove/crash.log` |
| Plugin docs | Document exit code semantics |

## 13. Stats Cache Staleness Across Git Operations

> **Decision: IMPLEMENT** — Line count comparison with `!=` (catches both
> growth and shrinkage from truncation/corruption).

### Issue

Developer runs `git pull`, pulling new `stats.log` entries. The local
cache was built from the old log. Staleness detection uses file mtime
which may not update reliably.

### Impact

Dashboard shows stale stats. Insights miss recent team activity.

### Mitigation

**Line count comparison, not mtime.** Staleness check compares
`log_entries_processed` in cache against actual line count:

```rust
fn is_cache_stale(cache: &StatsCache, log_path: &Path) -> bool {
    let line_count = count_lines(log_path);
    // Use != to catch both growth (new entries) and shrinkage (truncation)
    line_count != cache.log_entries_processed
}
```

**Automatic rebuild on staleness.** `grove stats` checks staleness before
displaying. If stale, rebuild transparently.

### Implementation Changes

| Component | Change |
|-----------|--------|
| Staleness check | Compare line count with `!=`, not mtime |
| `grove stats` | Auto-rebuild on stale |
| Cache struct | Already has `log_entries_processed` |

## Summary

This document addresses 13 edge cases and risks identified during design
review. After discussion, decisions were made to implement, simplify,
defer, or drop each item.

**Implemented (6):** Schema versioning, subagent observations, circuit
breaker reset, markdown sanitization, crash behavior, cache staleness.

**Simplified (4):** Orphaned state cleanup, learning attribution, skip
reasons, large file handling.

**Dropped (2):** Multi-ticket sessions, concurrent session handling.

**Deferred (1):** Learning invalidation detection.

Key themes in the final design:

1. **Single-ticket workflow** — No multi-ticket complexity needed
2. **Versioning for evolution** — Schema versions in JSONL data
3. **Defensive parsing** — Sanitization, validation, fail-open on errors
4. **Defer until needed** — Indexing, categorization, invalidation detection
5. **Observability** — Crash logging, session attribution

## Related Documents

- [Overview](./00-overview.md) - Vision, core concepts, design principles
- [Architecture](./01-architecture.md) - System diagrams, domain model
- [Implementation](./02-implementation.md) - Rust types, module structure
- [Stats and Quality](./03-stats-and-quality.md) - Quality tracking model
