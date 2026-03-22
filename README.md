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

This is only required if you want to set configuration settings or use a backend
other than the builtin markdown learnings storage.

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

### Retroflect (Existing Projects)

Mine past Claude Code session transcripts to generate learnings retroactively:

```bash
grove retroflect                              # Current project, sequential
grove retroflect --all                        # All projects under ~/.claude/projects/
grove retroflect --batch --yes                # Batch API (50% cheaper, async)
grove retroflect --batch --model claude-haiku-4-5-20251001 --yes --all  # Cheap bulk run
```

Use `--force` to re-analyze previously retroflected sessions. Use `--dry-run`
to preview candidates without writing.

## Configuration

Grove uses layered TOML configuration:

1. `.grove/config.toml` (project, highest priority)
2. `~/.grove/config.toml` (user)
3. Built-in defaults

```toml
[ticketing]
discovery = ["tissue", "beads", "tasks", "session"]

[backends]
discovery = ["total-recall", "mcp", "markdown"]

[gate.auto_skip]
enabled = true
line_threshold = 5
decider = "agent"  # agent, always, or never

[gate.write_gate]
mode = "strict"  # strict, lenient, or disabled

[decay]
passive_duration_days = 90

[retrieval]
max_injections = 5
strategy = "moderate"             # conservative, moderate, or aggressive
scoring_backend = "bm25"          # "keyword" (overlap) or "bm25" (Tantivy BM25)
corpus_enrichment = true           # enrich queries with corpus vocabulary
corpus_size_threshold = 50         # < threshold uses boosted BM25; >= uses plain BM25
dynamic_k_ratio = 0.3             # only inject learnings scoring >= top_score * ratio
adaptive_dk = false               # per-query dynamic K adjustment (needs stats data)
min_confidence_threshold = 0.1    # suppress injection if top score below this
min_score_gap = 0.05              # suppress if gap between top and median is below this
recency_half_life_days = 90       # recency decay half-life in days

[retrieval.intent_filter]
enabled = false                   # post-retrieval filter using user intent keywords
min_overlap = 1                   # minimum keyword overlap to keep a learning
max_keywords = 15                 # max intent keywords extracted per session

[retrieval.rerank]
enabled = false                   # LLM reranking of retrieved learnings
model = "haiku"                   # model for reranking

[circuit_breaker]
max_blocks = 3
cooldown_seconds = 300
```

### Retrieval Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `max_injections` | `5` | Maximum learnings injected per session |
| `strategy` | `"moderate"` | Aggressiveness: `conservative`, `moderate`, `aggressive` |
| `scoring_backend` | `"bm25"` | `"keyword"` (overlap) or `"bm25"` (Tantivy BM25) |
| `corpus_enrichment` | `true` | Enrich BM25 queries with domain vocabulary extracted from learnings |
| `corpus_size_threshold` | `50` | Below this learning count, use boosted BM25; at or above, plain BM25 |
| `dynamic_k_ratio` | `0.3` | Only inject learnings scoring >= `top_score * ratio` |
| `adaptive_dk` | `false` | Per-query dynamic K adjustment using stats cache hit rates and dismiss rates. Enable after accumulating stats data. |
| `min_confidence_threshold` | `0.1` | Suppress injection entirely if top score is below this |
| `min_score_gap` | `0.05` | Suppress if top-to-median score gap is below this |
| `recency_half_life_days` | `90` | Days at which recency weight drops to ~0.3 |
| `intent_filter.enabled` | `false` | Post-retrieval filter: keep only learnings sharing vocabulary with user intent |
| `rerank.enabled` | `false` | LLM reranking of retrieved learnings before injection |

### Forcing a Specific Backend

By default, Grove auto-detects backends in discovery order. To force a specific
backend, set the discovery list to only that backend:

```toml
[backends]
discovery = ["total-recall"]  # Only use Total Recall
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
| **MCP** | Route through MCP memory servers (planned) |

### Total Recall Users

Grove auto-detects Total Recall when both exist:

- `memory/` directory
- `.claude/rules/total-recall.md`

The Total Recall skill (`/recall:recall-init`) creates the `memory/` structure but
not the rules file. After running the skill, prompt Claude to create the rules
file:

> "Create `.claude/rules/total-recall.md` with the Total Recall protocol"

Once detected, Grove routes learnings to Total Recall's daily logs and registers
instead of `.grove/learnings.md`.

## Fail-Open Philosophy

Infrastructure errors never block work. Missing state, backend issues, or parse
errors log warnings but always approve exit. The learning may be lost, but
you're never stuck.

## License

AGPL-3.0-or-later

See [DEVELOPMENT.md](DEVELOPMENT.md) for contribution guidelines.
