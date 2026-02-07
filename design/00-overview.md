# grove - Compound Learning Gate Design Document: Overview

*"Each unit of engineering work should make subsequent units easier."*

## 1. Vision

A compound learning gate for Claude Code that enforces structured reflection
at ticket boundaries, with pluggable memory backends and quality tracking.

Inspired by compound engineering (Every) and Total Recall's write gate. Takes
the core concept (structured knowledge capture at work boundaries) and
implements it as a Rust CLI with mechanical enforcement via hooks.

## 2. Core Concepts

| Concept | Description |
|---------|-------------|
| **Learning gate** | Mechanical enforcement via Stop hook, not prompts |
| **Structured reflection** | Categorized extraction of learnings, not freeform |
| **Pluggable memory** | Backend interface for storage (built-in, Total Recall, MCP) |
| **Ticket-driven** | Gate fires per-ticket, with auto-discovery of ticketing systems |
| **Quality tracking** | Stats on every learning and decision, shared with team |

## 3. Design Principles

### 3.1 File-Based Storage (Built-in Backend)

Learnings are stored as a single append-only markdown file at
`.grove/learnings.md`. Stats are tracked as an append-only JSONL event log
in `.grove/stats.log`. Both are committed to the repo and shared with the
team. A local materialized cache (`~/.grove/stats-cache.json`) aggregates
the log for fast dashboard reads.

Session state (gate markers, active ticket context) is stored as JSON files
in `~/.grove/sessions/`, keyed by the `session_id` received from Claude
Code's hook payloads. Writes are atomic via temp file + rename pattern.

### 3.2 Per-Ticket Granularity

The gate fires when a ticket is closed, not on every session end. Ticketing
systems are auto-discovered in configurable priority order (tissue, beads,
Claude Code tasks). Falls back to per-session when no ticketing system is
detected.

### 3.3 Storage Tradeoffs

Session state uses JSON files. Learnings use either the built-in markdown
backend or an external system.

| Aspect | JSON Files | Database |
|--------|------------|----------|
| Dependencies | None | SQLite library |
| Simplicity | High | Medium |
| Atomic writes | Needs care | Built-in |
| Cross-session queries | List files | SQL queries |
| Team sharing | Git-native | Needs export |

For grove's use case (per-ticket state with team-shared learnings),
git-native files are preferred over a database.

**Concurrency**: No concurrent access within a session. Claude Code hooks run
synchronously. Multiple sessions get different IDs, writing to separate files.

**Risks and mitigations**:

- **Crash mid-write**: Atomic write pattern (temp file + rename)
- **Stats corruption**: Stats log is append-only JSONL; materialized cache
  rebuilt from log if corrupt. Log itself can be rebuilt from learnings
  file (with loss of surfacing/reference history).
- **Orphaned sessions**: Cleanup command removes old sessions
  (`grove clean --before 30d`)
- **Merge conflicts**: Stats log is append-only JSONL — git auto-merges
  concurrent appends. The materialized cache is local (not committed).

### 3.4 Minimal External Dependencies

Core functionality requires only:

- Rust standard library
- serde (serialization)
- chrono (timestamps)
- glob (pattern matching for ticketing/backend discovery)

Single binary includes all backends — no feature flags needed.

### 3.5 Fail-Open Philosophy

Infrastructure errors never block work. If grove can't read state, can't
write stats, or can't reach a memory backend, the gate approves and logs a
warning. The learning is lost but the developer isn't stuck.

## 4. CLI Surface

Commands are grouped by intended audience.

### 4.1 User Commands

Run directly by the developer.

```text
grove stats                    # Quality dashboard (hit rates, trends, insights)
grove stats --json             # Machine-readable stats output
grove search "n+1"             # Search past learnings across all backends
grove list                     # List recent learnings
grove list --stale             # List learnings approaching decay threshold
grove maintain                 # Review and prune stale learnings
grove init                     # Scaffold config, learnings file, session dir
grove backends                 # Show discovered memory backends
grove tickets                  # Show discovered ticketing system
grove debug <session_id>       # Full session state dump
grove trace <session_id>       # Show trace events for session
grove clean --before 30d       # Remove old session files
```

### 4.2 Agent Commands

Invoked by Claude Code via skills during a session.

```text
grove reflect                  # Run compound reflection (structured extraction)
grove skip "typo fix"          # Skip reflection with reason (logged to stats)
grove observe "auth ordering"  # Log subagent observation (no gate, append-only)
```

### 4.3 Hook Commands

Invoked automatically by Claude Code hooks. Not intended for direct use.

```text
grove hook session-start       # Discovery, context injection (reads stdin JSON)
grove hook pre-tool-use        # Ticket close detection (reads stdin JSON)
grove hook post-tool-use       # Ticket close confirmation (reads stdin JSON)
grove hook stop                # Gate enforcement (reads stdin JSON)
grove hook session-end         # Dismissed detection, cleanup (reads stdin JSON)
```

All commands support `--json` for machine output and `--quiet` for ID-only
output, following the tissue/roz convention.

## Related Documents

- [Architecture](./01-architecture.md) - System diagrams, domain model,
  state machine, sequences
- [Implementation](./02-implementation.md) - Rust types, storage, hooks, CLI
  commands
- [Stats and Quality](./03-stats-and-quality.md) - Quality tracking model,
  retrieval scoring, insights engine
- [Test Plan](./04-test-plan.md) - Testing strategy
- [CI](./05-ci.md) - Version management and release workflow
