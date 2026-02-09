# Grove

**Compound learning gate for Claude Code** — enforces structured reflection at
ticket boundaries with pluggable memory backends.

## Overview

Grove captures learnings when you complete tickets and injects relevant context
at session start. The gate mechanically enforces reflection — blocking session
exit until you either capture learnings or explicitly skip.

## Installation

### Binary Install

```bash
curl -fsSL https://raw.githubusercontent.com/bivory/grove/main/.claude-plugin/install.sh | bash
```

This downloads the correct binary for your platform to `~/.local/bin`.

### Plugin Install

After installing the binary, add the Grove plugin to Claude Code:

```text
/plugin marketplace add bivory/claude-plugin-marketplace
/plugin install grove@bivory
```

### Manual Install

Download from [Releases](https://github.com/bivory/grove/releases):

| Platform | Binary |
|----------|--------|
| Linux x86_64 | `grove-x86_64-unknown-linux-gnu` |
| Linux ARM64 | `grove-aarch64-unknown-linux-gnu` |
| macOS ARM64 | `grove-aarch64-apple-darwin` |

## Quick Start

### 1. Initialize in your project

```bash
grove init
```

Creates `.grove/` with learnings database, stats log, and config.

### 2. Work normally

Grove watches for ticket close events. When you close a ticket (via tissue,
beads, or similar), the gate activates.

### 3. Reflect at ticket boundaries

When the gate blocks:

```bash
grove reflect       # Capture structured learnings
grove skip "reason" # Skip with reason
```

## Gate Behavior

```text
Idle → Active → Pending → Blocked → Reflected/Skipped
```

The gate approves exit once you reflect or skip.

## Commands

### Reflection

| Command | Description |
|---------|-------------|
| `grove reflect` | Capture structured learnings |
| `grove skip "reason"` | Skip reflection with reason |
| `grove observe "note"` | Log observation (no gate trigger) |

### Information

| Command | Description |
|---------|-------------|
| `grove stats` | Quality dashboard with insights |
| `grove search "query"` | Search past learnings |
| `grove list` | List recent learnings |
| `grove backends` | Show discovered backends |
| `grove tickets` | Show detected ticketing system |

### Maintenance

| Command | Description |
|---------|-------------|
| `grove maintain` | Review and archive stale learnings |
| `grove clean --before 30d` | Remove old session files |

## Configuration

Grove uses layered TOML configuration:

1. `.grove/config.toml` (project, highest priority)
2. `~/.config/grove/config.toml` (user)
3. Built-in defaults

```toml
[gate]
enabled = true
auto_approve_threshold = 5

[backends]
preference = ["markdown", "total_recall"]

[stats]
passive_decay_days = 90
```

## Learning Categories

| Category | Example |
|----------|---------|
| **Pattern** | "Use builder pattern for complex constructors" |
| **Pitfall** | "Remember `--locked` in CI cargo builds" |
| **Convention** | "All API routes use kebab-case" |
| **Dependency** | "Redis client requires explicit ping" |
| **Process** | "Run clippy before opening PR" |
| **Domain** | "Orders auto-cancel after 30 days" |
| **Debugging** | "Use `RUST_BACKTRACE=1` for panic traces" |

## Backends

| Backend | Description |
|---------|-------------|
| **Markdown** | Default. Append-only `.grove/learnings.md` |
| **Total Recall** | Integration with Total Recall memory |
| **MCP** | Route through MCP memory servers |

## Fail-Open Philosophy

Infrastructure errors never block work. Missing state, backend issues, or parse
errors log warnings but always approve exit. The learning may be lost, but
you're never stuck.

## License

AGPL-3.0-or-later

See [DEVELOPMENT.md](DEVELOPMENT.md) for contribution guidelines.
