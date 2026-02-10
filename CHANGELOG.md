# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/bivory/grove/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/bivory/grove/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/bivory/grove/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/bivory/grove/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/bivory/grove/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/bivory/grove/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/bivory/grove/releases/tag/v0.1.0
