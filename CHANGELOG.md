# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.0] - 2026-03-07

### Added

- `grove ref` command for agents to record when surfaced learnings are useful,
  closing the scoring feedback loop (reference_boost) that previously had no signal
- `--how` flag on `grove ref` for optional usage context (stored in trace events)
- Git context injection at session-start: SearchQuery populated with recent
  file paths and keywords from `git diff` for contextual relevance scoring
- Keyword matching against learning detail text (not just summary/tags)
- Configurable `recency_half_life_days` for tuning recency decay lambda
- Configurable `min_pool_size` to relax strategy filtering when few learnings exist
- Integration test for `grove ref` lifecycle
- Skill documentation for `grove ref`

### Changed

- Session-start injection footer now shows session ID and `grove ref` command
  instead of asking agents to mention learnings in reflections
- Scoring model switched to additive with diminishing returns
  (keyword summary weight raised from 0.3 to 0.5)
- Reference boost range widened from [0.5, 1.0] to [0.3, 1.0]
- Strategy system wired up for learning injection caps

### Fixed

- Whole-word matching for keyword relevance scoring (prevents partial matches)
- Shared path component requirement for file matching (reduces false positives)
- Per-session surfacing deduplication (prevents duplicate stats events)
- Dismissed events now emitted for all sessions with injected learnings
- Correction notices appended to learning context instead of overwriting
- Stale comments referencing old constant values in scoring module

## [0.7.0] - 2026-02-18

### Added

- Sorting options for `grove list` command
  (`--sort` flag with name, category, date, hits options)
- Reverse sort option (`--reverse` flag) for `grove list`

### Changed

- Project root detection now uses `git rev-parse --show-toplevel`
  instead of raw cwd (more reliable in nested directories)

## [0.6.0] - 2026-02-17

### Added

- Optional Tantivy full-text search behind `tantivy-search` feature flag
  - Stemming tokenizer for better term matching (track/tracking, write/writes)
  - Fuzzy search with dynamic edit distance based on term length
  - BM25 relevance scoring with field boosts
  - Generous search mode (stemming + fuzzy fallback) as default
  - Query injection prevention via character escaping
  - Upsert behavior for document updates
- `TantivySearchIndex` and `TantivySearchResult` exports from `grove::search`

### Changed

- Total Recall backend search now matches all query terms individually
  (improved recall for multi-word queries)

## [0.5.0] - 2026-02-17

### Added

- Usage statistics in `grove list` output (hit count, last accessed date)
- Category-aware decay tracking for stale learning detection
- Git SHA and dirty status in `grove --version` output

### Fixed

- Correctly count total learnings across all categories in stats
- Track categories properly in reflection stats events

## [0.4.0] - 2026-02-10

### Fixed

- CLI commands now respect configured backend discovery instead of hardcoding
  MarkdownBackend (`grove reflect`, `grove search`, `grove list`, `grove maintain`)
- Excluded `.grove/` from markdown linting (generated content)

### Added

- `create_primary_backend` exported from library for backend discovery
- `archive()`, `restore()`, `list_all()` methods to MemoryBackend trait
- Blanket implementation of MemoryBackend for `Box<dyn MemoryBackend>`
- Integration tests for backend routing and gate transitions

### Changed

- `ListCommand` and `MaintainCommand` are now generic over MemoryBackend

## [0.3.0] - 2026-02-10

### Added

- Agent instructions file (`agents/grove.md`) with complete Grove protocol
- `hooks.json` for automatic Claude Code hook configuration
- Session ID included in block messages for reflect/skip commands

### Changed

- Removed "config" from default backend discovery list (use discovery array directly)
- Simplified hooks configuration to only match Bash commands

### Fixed

- Fixed user config path in documentation (`~/.grove/config.toml` not `~/.config/grove`)
- Removed invalid `gate.enabled` field from config template (use `gate.auto_skip.enabled`)
- Fixed `/skip` examples in agent instructions to use CLI with `--session-id` flag
- Fixed reflect input format to use JSON with `criteria_met` field
- Added missing `[ticketing]`, `[retrieval]`, `[circuit_breaker]` config sections
  to README
- Added inline documentation for valid `decider` and `strategy` option values

## [0.2.2] - 2026-02-09

### Fixed

- Fixed release artifact naming to match install script expectations
- Added SHA256 checksums for release binaries

## [0.2.1] - 2026-02-09

### Fixed

- Removed x86_64-apple-darwin from release workflow (macos-13 unsupported)

## [0.2.0] - 2026-02-09

### Changed

- Simplified CI workflows by removing MSRV checks and consolidating jobs
- Removed x86_64-apple-darwin from platform matrix (GitHub Actions limitation)
- Consolidated dependabot update schedule to weekly

### Fixed

- Resolved CI workflow Rust version conflicts
- Bumped minimum supported Rust version from 1.75.0 to 1.82.0

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

[Unreleased]: https://github.com/bivory/grove/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/bivory/grove/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/bivory/grove/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/bivory/grove/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/bivory/grove/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/bivory/grove/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/bivory/grove/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/bivory/grove/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/bivory/grove/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/bivory/grove/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/bivory/grove/releases/tag/v0.1.0
