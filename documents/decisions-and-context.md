# grove — Design Decisions and Context

This document captures the questions asked, decisions made, and reasoning
behind them during the grove design process. It exists so that a new
contributor (human or AI) can understand not just *what* the design is, but
*why* it is this way.

The design evolved across two sessions. Session 1 was exploratory Q&A that
resolved all open architectural questions. Session 2 restructured the
monolithic design doc into numbered documents following roz's pattern.

## Origins

Grove grew out of a desire to bring compound engineering practices (as
popularized by the [EveryInc compound-engineering-plugin](https://github.com/EveryInc/compound-engineering-plugin))
into a more structured, enforced, and pluggable form. The key gap identified
was that no existing tool combined all three of: an enforced reflection gate
at session boundaries, structured learning extraction, and pluggable memory
backends.

Three existing tools informed the design:

- **Compound Engineering** (Every): Excellent reflection workflows, but
  stores learnings in flat files and doesn't enforce the reflection step.
- **Total Recall**: Curated tiered memory with write gates, but no
  structured reflection workflow.
- **CLAUDE.md / rules**: Session context, but relies on manual maintenance.

Grove sits in the intersection: enforced gate + structured reflection +
pluggable persistence.

## Relationship to roz

[roz](https://github.com/bivory/roz) is a quality gate for Claude Code that
blocks agent exit until a reviewer agent approves work. Grove borrows
heavily from roz's patterns:

- **Rust CLI binary** that hooks shell out to (same as roz's architecture)
- **Stop hook gate enforcement** via exit code 2 blocking
- **Circuit breaker** to prevent infinite blocking (roz's config pattern)
- **Session state as local JSON files** in a user-level directory
- **Design doc structure**: numbered files (00-overview, 01-architecture, etc.)
  following roz's `design/` directory pattern
- **Plugin distribution**: `.claude-plugin/` with `install.sh`

Key difference: roz gates on *review quality* (did a reviewer approve?),
grove gates on *reflection completion* (did learning extraction happen?).

## Naming

**Decision: `grove`**

The name was chosen from a set of candidates grouped by theme:

- Growth/nature: grove, sap, graft, rhizo
- Learning/memory: koan, dojo, reps, kata
- Compounding: accrue, steep, anneal, temper
- Short/punchy: mull, hone, kiln

"Grove" won because: trees growing together is a natural metaphor for
knowledge accumulating; it's 5 characters (good CLI ergonomics); and all
subcommands read well (`grove reflect`, `grove stats`, `grove search`).

## Decision Log

### 1. Gate Granularity: Per-Ticket

**Question:** Should the gate fire per-commit, per-ticket, or per-session?

**Decision:** Per-ticket, with fallback to per-session.

**Reasoning:** Per-commit is too frequent — a typo fix shouldn't trigger
reflection. Per-session is too arbitrary — you might span three sessions on
one ticket or handle two tickets in one session. Per-ticket aligns with the
natural unit of work that produces learnings.

**Implication:** Requires ticketing system discovery to detect when a ticket
closes. When no ticketing system is found, falls back to per-session
(the Stop hook fires on every session end).

### 2. Ticketing System Discovery

**Question:** How does grove know which ticketing system is active?

**Decision:** Auto-discovery with configurable priority order. Default:
`tissue → beads → tasks → session`.

**Reasoning:** The user may have tissue, beads, Claude Code tasks, or
nothing installed. Rather than requiring explicit configuration, grove
probes for marker directories (`.tissue/`, `.beads/`) and falls back
gracefully. The discovery order is configurable for teams that use multiple
systems and want to prioritize one.

**Ticket close detection:** Uses a PreToolUse hook on Bash commands to
match ticket-closing patterns (e.g., `tissue status * closed`). The tool
is allowed to proceed — grove doesn't block the ticket close itself, only
the subsequent session exit.

### 3. Multi-Agent Behavior

**Question:** When subagents are running, should each one reflect?

**Decision:** Only the orchestrator reflects. Subagents log lightweight
observations via `grove observe`.

**Reasoning:** Subagents doing parallel work (security review, perf review,
etc.) would produce noisy, overlapping reflections. Instead, subagents
append one-line observations to the session. The orchestrator's reflection
at the gate synthesizes these observations into proper learnings.

**Implementation:** The gate only fires on `Stop`, not `SubagentStop`.
Subagent observations are stored in the session state and available to the
orchestrator during reflection.

### 4. Learning Decay

**Question:** Should old learnings expire?

**Decision:** Passive decay with a 90-day default, configurable.

**Reasoning:** Stale learnings pollute retrieval and waste context window.
But aggressive expiry loses valuable knowledge. 90 days balances these —
if nobody references a learning in 3 months, it's probably not useful.

**Mechanism:** `last_verified = max(last_referenced, last_surfaced,
created_at)`. When `now - last_verified > 90 days`, the learning is
archived. Archived learnings are still searchable via `grove search` and
restorable via `grove maintain`.

**Immunity:** Learnings with a hit rate above 0.8 are immune to decay.
If something is referenced 80%+ of the time it's surfaced, it's clearly
valuable regardless of age.

### 5. Built-in Backend: Single Append-Only File

**Question:** For teams without Total Recall or MCP, how should the
built-in backend store learnings? Options: single file, structured
directory, or separate branch.

**Decision:** Single append-only markdown file (`.grove/learnings.md`).

**Reasoning:** This question only applies when Total Recall is NOT the
active backend. For the "no frills" fallback, simplicity wins. A single
file means: one small diff per PR, easy to grep, no directory management,
low cognitive overhead. Teams that want sophisticated storage should use a
real memory backend.

The structured directory approach (individual files per learning) was
considered but rejected as premature optimization — it solves a "too many
learnings in one file" problem that most teams won't hit, and it creates
noisier PRs.

### 6. Retrieval Strategy: Moderate

**Question:** How aggressively should past learnings be injected at
session start?

**Decision:** Moderate strategy by default, with conservative and
aggressive as config options.

**Options considered:**

- **Conservative:** Only match learnings against files already in the diff.
  Very precise but misses broader domain knowledge.
- **Moderate:** Match on ticket description and file paths. Broader but
  requires ticket context.
- **Aggressive:** Inject all recent learnings plus area matches. Wide net
  but risks context bloat.

**Reasoning:** Moderate balances relevance and coverage. When a ticket
exists, use it as the search context. When no ticket exists, fall back to
recent learnings. Cap at 5 injections (configurable) to avoid overwhelming
the context window.

### 7. Stats: Team-Shared, Committed

**Question:** Should usage stats (hit rates, skip rates) be personal or
team-shared?

**Decision:** Committed to the repo, shared across the team.

**Reasoning:** If learnings are shared, the signal about whether they're
useful should be shared too. A learning that nobody on the team references
is a stronger demotion signal than one person ignoring it. Team-shared
stats enable collective quality tuning.

**Location:** `.grove/stats.json`

### 8. Stats Tracking: Surfaced/Referenced/Dismissed/Corrected

**Question:** Can we track how useful injections are and learn over time?

**Decision:** Yes. Four counters per learning: surfaced, referenced,
dismissed, corrected. Hit rate derived as `referenced / surfaced`.

**Reasoning:** This closes the feedback loop. Without it, retrieval is
blind — you don't know if surfaced learnings are actually helping. With it,
high-hit-rate learnings get boosted, low-hit-rate ones get demoted, and
the system auto-tunes over time.

**Detection methods:**

- **Surfaced:** SessionStart hook injected the learning
- **Referenced:** Reflection notes "applied learning [ID]"
- **Dismissed:** Surfaced but not referenced (inferred at session end)
- **Corrected:** Explicitly marked wrong/stale

### 9. Skip Mechanism: Agent-Decides with Threshold

**Question:** How should the escape hatch work for trivial sessions?

**Decision:** Configurable auto-skip. Default: agent decides when diff is
under 5 lines. All skip decisions logged with reasoning.

**Options considered:**

- Always prompt (no auto-skip)
- Always skip under threshold
- Agent decides under threshold

**Reasoning:** "Agent decides" gives the best balance. A 2-line version
bump shouldn't need reflection, but a 3-line security fix might. The agent
can see context the line count can't. Logging every skip decision enables
retrospective analysis — if skipped sessions frequently produce learnings
in the same area later, the threshold or agent behavior needs tuning.

### 10. Write Gate Filter (from Total Recall)

**Question:** How do we prevent low-value learnings from being persisted?

**Decision:** Adapted Total Recall's write gate. A learning must pass at
least one of four criteria before being written.

**Criteria:**

1. Changes future behavior? (would you do something differently?)
2. Decision with rationale? (why X over Y?)
3. Stable fact that will matter again?
4. User explicitly said "remember this"?

**Reasoning:** Without a write gate, reflection produces noise — generic
observations that don't help future sessions. The filter ensures only
actionable, decision-relevant, or stable knowledge gets promoted.

Rejected candidates are tracked (summary only) for write gate effectiveness
analysis. If rejected topics appear in later reflections, the gate may be
too strict.

### 11. Cross-Pollination Tracking

**Question:** How do we measure the compound effect — knowledge flowing
across tickets?

**Decision:** Track when a learning from ticket A is referenced during
ticket B. This is the "cross-pollination" metric.

**Reasoning:** This is the strongest signal that compound learning is
working. A pitfall discovered during feature work that prevents a bug
during a different feature is exactly the value proposition. The
`grove stats` dashboard highlights the most cross-pollinated learnings.

### 12. File Location: All Under `.grove/`

**Question:** Where should grove's project-level files live?

**Decision:** All under `.grove/` in the project root.

**Reasoning (from review feedback):** Originally files were split between
`.claude/` (learnings, stats) and `~/.grove/` (sessions). This was
inconsistent — grove should own its own namespace. All committed project
files go in `.grove/` (config, learnings, stats). Session state stays in
`~/.grove/sessions/` (user-level, not committed).

### 13. CLI Command Audience Annotations

**Question:** Who is each CLI command intended for?

**Decision (from review feedback):** Commands are grouped by audience:

- **User Commands** — run directly by the developer (stats, search, list,
  maintain, init, backends, tickets, debug, trace, clean)
- **Agent Commands** — invoked by Claude Code via skills during a session
  (reflect, skip, observe)
- **Hook Commands** — invoked automatically by Claude Code hooks, not for
  direct use (hook session-start, hook pre-tool-use, hook stop, hook
  subagent-stop)

**Reasoning:** Makes it clear which commands a developer would type vs
which are infrastructure. Prevents confusion about "should I run
`grove hook stop` manually?" (no).

### 14. Architecture Document: Language-Agnostic

**Question:** Should the architecture doc specify Rust types and module
structure?

**Decision (from review feedback):** No. The architecture doc (01) is
conceptual and language-agnostic. The implementation doc (02) is where
concrete types, file layouts, and language decisions live.

**Reasoning:** Architecture describes the *what* and *why* at the domain
level — entities, state machines, sequences, interfaces. Implementation
describes the *how* — Rust structs, module paths, build system. Separating
them means the architecture can be understood without knowing Rust, and
could theoretically guide a reimplementation in a different language.

## Design Principles (Why These?)

### "The gate is non-negotiable"

If reflection is optional, it won't happen. The Stop hook with exit code 2
makes it impossible to end a session without either reflecting or explicitly
skipping (with a logged reason). This is the core value proposition.

### "Fail-open philosophy"

Infrastructure errors should never block developer work. If grove can't
read state, write stats, or reach a backend, it approves the exit and logs
a warning. A lost learning is better than a stuck developer.

### "Auto-discovery over configuration"

Most teams should get value from `grove init` without writing config.
Discovery probes for what's installed and adapts. Config exists for teams
that need to override defaults, not as a prerequisite.

### "Measure what matters"

Stats aren't just for dashboards — they drive system behavior. Hit rates
tune retrieval ranking. Skip patterns reveal gate sensitivity issues.
Cross-pollination proves the compound effect is real.

## Open Questions (Not Yet Resolved)

These were identified but intentionally deferred:

1. **Token cost tracking:** Can/should grove measure the token cost of
   reflection and report it in stats? Useful for teams optimizing costs.

2. **Learning deduplication:** When a similar learning is captured across
   multiple tickets, should grove detect and merge them? Or leave it to
   `grove maintain`?

3. **Confidence evolution:** Should a learning's confidence increase when
   it's referenced frequently? Currently confidence is set at creation
   and only changes via manual correction.

4. **Team vs individual retrieval ranking:** Should retrieval scoring
   weight personal hit rates differently from team-wide hit rates?

## Related Documents

- [Overview](./00-overview.md) — Vision, concepts, CLI surface
- [Architecture](./01-architecture.md) — Domain model, state machine, sequences
- [Implementation](./02-implementation.md) — Rust types, module structure
- [Stats and Quality](./03-stats-and-quality.md) — Quality tracking, scoring, insights
