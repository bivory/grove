# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-02-08

Initial release of Grove, a compound learning gate for Claude Code.

### Added

#### Core Features

- Gate state machine (Idle, Active, Pending, Blocked, Reflected, Skipped)
- Structured reflection with seven learning categories
- Two-phase write gate validation (schema + quality criteria)
- Circuit breaker to prevent infinite blocking
- Near-duplicate detection with exact and substring matching

#### Backends

- Markdown backend with append-only `.grove/learnings.md` storage
- Total Recall backend adapter for `recall-log` and `recall-search`
- Multi-backend routing with automatic detection and fallback

#### Discovery

- Ticketing system detection (tissue, beads, tasks, session)
- Memory backend detection (config, total-recall, mcp, markdown)
- Close pattern matching for ticket completion detection

#### Stats and Insights

- Append-only JSONL event log (`.grove/stats.log`)
- Materialized cache with automatic rebuild
- Composite scoring with relevance, recency, and reference boost
- Strategy modes (conservative, moderate, aggressive)
- Passive decay with 90-day threshold and hit-rate immunity
- Insights: DecayWarning, HighCrossPollination, StaleTopLearning
- Insights: LowHitCategory, HighValueRare, RubberStamping
- Insights: WriteGateTooStrict, WriteGateTooLoose, SkipMiss

#### CLI Commands

- `grove hook` - Claude Code hook integration
- `grove reflect` - Structured reflection capture
- `grove skip` - Skip reflection with reason
- `grove observe` - Record observations
- `grove search` - Search learnings
- `grove list` - List all learnings
- `grove stats` - Dashboard with insights
- `grove maintain` - Manage stale learnings
- `grove init` - Initialize Grove in a project
- `grove backends` - Show detected backends
- `grove tickets` - Show ticketing system
- `grove debug` - Debug session state
- `grove trace` - View session trace
- `grove clean` - Clean old sessions

#### Plugin Integration

- Claude Code hooks for session lifecycle
- Plugin manifest and installer
- Skills for reflection, skip, observe, search, and maintain
- Gate protocol rules

#### Infrastructure

- CI workflow for PRs
- Release workflow with GitHub releases
- Nightly workflow for comprehensive checks
- Dependabot configuration for weekly updates

### Documentation

- Comprehensive README with installation and usage guide
- Architecture design documents
- Implementation task roadmap

[Unreleased]: https://github.com/anthropics/grove/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/anthropics/grove/releases/tag/v0.1.0
