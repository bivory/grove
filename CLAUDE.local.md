# Working Memory

> This is your working memory. Auto-loaded every session via CLAUDE.local.md.
> ~1500 word limit. Only behavior-changing facts earn a place here.
> Last updated: 2026-02-12

## Active Context

**Current Focus**: Grove - compound learning gate for Claude Code
**Key Deadline**: [none]
**Blockers**: [none]

## Project State

- **Grove**: Implementation complete (1860 tests passing)
  - All 13 modules implemented per /design/ specs
  - Gate state machine: Idle -> Active -> Pending -> Blocked -> Reflected/Skipped
  - Fail-open philosophy: infrastructure errors never block work

## Critical Preferences

- Use mise for all build/test commands (see mise.toml)
- Author commits as Bryan Ivory <bivory@gmail.com>
- 70% line coverage threshold enforced

## Key Decisions in Effect

- Append-only JSONL stats log (eliminates git merge conflicts)
- Two-phase validation for learnings (schema + write gate filter)
- Circuit breaker with 300s cooldown to prevent infinite blocking

## People Context

- [None captured yet]

## Open Loops

- [None yet]

## Session Continuity

Total Recall memory system initialized 2026-02-09.

---
*For detailed history, see memory/registers/*
*For daily logs, see memory/daily/*
