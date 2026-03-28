# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with
code in this repository.

## Project Overview

Grove is a "compound learning gate" for Claude Code that enforces structured
reflection at ticket boundaries with pluggable memory backends. It captures
learnings when developers complete tickets and injects relevant context at
session start.

**Current Status:** Implementation complete. All 13 modules implemented with 1860 tests passing.

## Build & Development Commands

All commands use mise (task runner configured in `mise.toml`):

```bash
mise build              # Build the project
mise check              # Type check without building
mise test               # Run tests with nextest
mise clippy             # Run clippy lints
mise fmt                # Format Rust code
mise fmt:check          # Check Rust formatting

# Quality checks
mise coverage           # Run tests with 70% line coverage threshold
mise audit              # Check for security vulnerabilities
mise deny               # Check dependency licenses and bans
mise typos              # Check for typos
mise dupes              # Check for code duplication
mise docs:lint          # Lint markdown documentation

# Composite tasks
mise pre-commit         # typos, docs:lint, dupes
mise ci                 # Full CI pipeline
```

## Architecture

### Core Concept: Gate State Machine

The gate enforces reflection before session exit:

```text
Idle → Active → Pending → Blocked → Reflected/Skipped
```

When a ticket is closed, the gate blocks exit (via stop hook returning exit
code 2) until the developer runs `grove reflect` or `grove skip`.

### Module Structure (13 modules)

| Module | Purpose |
|--------|---------|
| **core** | Gate state machine, learning types, reflection parsing, quality checks, embeddings |
| **discovery** | Auto-detect ticketing systems and backends |
| **backends** | Trait-based memory persistence (markdown, Total Recall, fallback) |
| **stats** | Quality tracking via append-only JSONL event log |
| **storage** | Session state persistence with atomic writes |
| **hooks** | Claude Code integration (7 hooks) |
| **cli** | 19 commands for reflection, search, stats, maintenance, eval |
| **config** | TOML settings with precedence chain |
| **error** | Unified error types with fail-open philosophy |
| **search** | Tantivy-based full-text search (feature-gated) |
| **llm** | Shared LLM call infrastructure (CLI and API backends) |
| **eval** | Offline retrieval quality evaluation (corpus, judge, metrics, runner) |
| **util** | Shared utilities (file size limits, string truncation) |

### Key Design Patterns

- **Fail-Open Philosophy:** Infrastructure errors never block work. Missing
  state → approve exit. Backend unreachable → skip write but mark reflected.
- **Append-Only Stats:** `.grove/stats.log` is JSONL (eliminates git merge
  conflicts). `~/.grove/stats-cache.json` is local materialized cache.
- **Two-Phase Validation:** Schema validation → Write gate filter (must claim
  ≥1 of: behavior-changing, decision-rationale, stable-fact, explicit-request)
- **Circuit Breaker:** Prevents infinite blocking with 3 reset conditions:
  cooldown elapsed (300s), different session_id, successful reflection
- **Corpus-Enriched Retrieval:** BM25 queries are augmented with domain
  vocabulary extracted from the learning corpus (terms appearing in >= 2
  learnings). Heuristic routing selects plain BM25 for large corpora (>= 50
  learnings) and boosted BM25 for small corpora. Configurable via
  `retrieval.corpus_enrichment` (default: true).

### Learning Categories

Seven structured categories for captured learnings:

- Pattern, Pitfall, Convention, Dependency, Process, Domain, Debugging

### Hook Integration

Grove plugs into Claude Code via 7 hooks defined in Claude Code's
configuration. The stop hook is critical—it returns exit code 2 to block
session exit when gate is pending/blocked.

## Design Documentation

Comprehensive design docs in `/design/`:

- `00-overview.md` - Vision and core concepts
- `01-architecture.md` - Full system design (most comprehensive)
- `02-implementation.md` - Concrete Rust module structure
- `03-stats-and-quality.md` - Event log model, quality tracking, insights
- `04-test-plan.md` - Testing strategy and test cases
- `05-ci.md` - CI workflows and release process

Decision rationale and risk mitigations in `/documents/`.

## Implementation Status

All modules fully implemented:

- **Core:** Gate state machine, learning types, two-phase validation, near-duplicate detection, specificity scoring, embeddings
- **Discovery:** Auto-detection for ticketing systems (tissue, beads, tasks, session) and backends
- **Backends:** Markdown, Total Recall, fallback backends
- **Stats:** JSONL event log (12 event types), composite scoring, insights engine, recommendations
- **Storage:** Atomic session state persistence
- **Hooks:** All 7 hook types integrated
- **CLI:** All 19 commands implemented
- **Config:** TOML settings with full precedence chain
- **Error:** Unified error types with fail-open philosophy
- **Search:** Tantivy BM25 backend (feature-gated)
- **LLM:** CLI and API backends for judge and reranking
- **Eval:** Offline benchmark harness with corpus, judge, metrics, runner

## Commits

Use Bryan Ivory <bivory@gmail.com> as the Author.
