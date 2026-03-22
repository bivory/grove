# Development Guide

## Prerequisites

- Rust 1.92+ (or use Dev Container)
- [mise](https://mise.jdx.dev/) (recommended task runner)

## Build Commands

Using mise:

```bash
mise build         # Build debug binary
mise test          # Run tests with nextest
mise clippy        # Run clippy lints
mise fmt           # Format code
mise pre-commit    # Run all pre-commit checks
mise ci            # Full CI pipeline
```

Or directly with cargo:

```bash
cargo build
cargo nextest run
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt
```

## Local Plugin Testing

1. Build release binary:

   ```bash
   cargo build --release
   ```

2. Add to PATH:

   ```bash
   # Option A: Symlink
   ln -sf "$(pwd)/target/release/grove" ~/.local/bin/grove

   # Option B: Add to PATH
   export PATH="$(pwd)/target/release:$PATH"
   ```

3. Install plugin locally in Claude Code:

   ```text
   /plugin marketplace add ./
   /plugin install grove@bivory
   ```

4. Verify installation:

   ```text
   /plugin list
   ```

5. Test by initializing in a project:

   ```bash
   grove init
   grove stats
   ```

## Debug Commands

| Command | Description |
|---------|-------------|
| `grove sessions` | List recent sessions |
| `grove debug <session_id>` | Dump session state |
| `grove trace <session_id>` | Show trace events |

## Troubleshooting

### Hooks not appearing

Claude Code hooks are active even if not shown in menus. Verify with:

```bash
grove hook session-start --help
```

### Sessions not creating

Check state directory exists:

```bash
ls -la .grove/
```

### Gate not triggering

Verify ticketing system detection:

```bash
grove tickets
```

## Dev Container

### VS Code

Open in VS Code, then "Reopen in Container" when prompted.

### Command Line

```bash
mise dc:build  # Build container
mise dc:shell  # Get a shell
mise dc:claude # Run Claude Code in container
```

## Releasing

Preview what will happen:

```bash
mise release:dry-run
```

Release (updates version, commits, tags, pushes):

```bash
mise release:patch  # 0.1.0 → 0.1.1
mise release:minor  # 0.1.0 → 0.2.0
mise release:major  # 0.1.0 → 1.0.0
```

## Benchmarking Retrieval Algorithms

Grove includes an offline eval system for measuring retrieval quality. It
requires the `tantivy-search` feature:

```bash
cargo build --features tantivy-search
```

### Corpus Setup

Create a corpus manifest listing projects with learnings and transcripts:

```toml
# .grove/corpora.toml
[[corpus]]
name = "my-project"
transcript_dir = "~/.claude/projects/-Users-you-my-project"
learnings_path = "/path/to/my-project/.grove/learnings.md"
```

### Generating Learnings with Retroflect

Bootstrap learnings from existing session transcripts:

```bash
# Single project, sequential (default model: Sonnet)
grove retroflect --project /path/to/project

# All projects, Batch API (50% cheaper, async processing)
grove retroflect --batch --yes --all

# Cheaper bulk run with Haiku
grove retroflect --batch --model claude-haiku-4-5-20251001 --yes --all

# Re-analyze with a better model (--force re-processes already-retroflected sessions)
grove retroflect --batch --model claude-sonnet-4-20250514 --yes --force --all
```

### Running Eval Benchmarks

Single corpus:

```bash
grove eval run --config boosted-adaptive \
  --transcript-dir ~/.claude/projects/-Users-you-my-project \
  --learnings-path /path/to/my-project/.grove/learnings.md
```

Multi-corpus sweep across algorithm configs:

```bash
grove eval sweep --manifest .grove/corpora.toml \
  --configs "bm25,boosted-adaptive"
```

Both `eval run` and `eval sweep` support `--batch` for 50% cheaper LLM judge
calls via the Anthropic Batch API:

```bash
grove eval sweep --manifest .grove/corpora.toml \
  --configs "bm25,boosted-adaptive" \
  --batch
```

### Available Benchmark Configs

| Config | Description |
|--------|-------------|
| `bm25` | BM25 search only (baseline) |
| `adaptive` | BM25 + adaptive threshold + dynamic K |
| `intent-filter` | BM25 + adaptive + intent-as-filter |
| `boosted-adaptive` | BM25 with per-term boost + adaptive |
| `adaptive-rerank` | BM25 + adaptive + LLM reranking |
| `boosted-adaptive-rerank` | BM25 boosted + adaptive + LLM reranking |
| `flat-recency` | BM25 + adaptive with flat 90-day half-life (ablation control) |
| `heuristic` | BM25 with corpus-size heuristic (plain for large, boosted for small) |
| `heuristic(N)` | Same, with custom threshold (default 50) |
| `corpus-enriched` | BM25 boosted + adaptive + corpus vocabulary enrichment |
| `adaptive-dk` | BM25 + adaptive + per-query adaptive dynamic K |
| `boosted(kw=F,tag=F,dk=F)` | Custom boost params (keyword, tag, dynamic_k_ratio) |

### Comparing Results

```bash
grove eval compare --configs bm25,boosted-adaptive
```

### Interpreting Benchmark Results

Eval sweep output includes these metrics per corpus per config:

| Metric | What It Measures |
|--------|-----------------|
| **Pairs** | Total (session, learning) pairs evaluated |
| **Avg/Med** | Mean/median relevance score (1-5 scale, LLM judge) |
| **Noise%** | Fraction of pairs scoring <= 2 (irrelevant) |
| **P@3g** | Global precision: % of surfaced pairs scoring >= 3 |
| **P@3** | Per-session precision: mean of per-session top-3 precision |
| **R@4** | Recall: % of ground-truth relevant pairs (>= 4) that were surfaced |
| **F1** | Harmonic mean of P@3g and R@4 — primary comparison metric |
| **Cov%** | Coverage: % of sessions receiving at least one result |
| **MRR** | Mean reciprocal rank of first relevant (>= 4) result per session |

**Key principles:**

- **F1 is the primary metric** for comparing configs across corpora
- **No single config wins all corpora** — corpus characteristics (learning count,
  domain diversity) determine which config is optimal
- **Coverage floor matters** — a config with high precision but < 80% coverage
  leaves too many sessions empty
- **Bootstrap CIs** (`--bootstrap 1000`) quantify confidence; differences within
  overlapping CIs are not statistically significant
- **Cross-corpus negatives** (`--cross-negatives`) measure false positive rate
  by pairing learnings from one project with sessions from another

**Validation policy:** No retrieval changes ship unless they improve (or hold) F1
on **all** available benchmark corpora. See `design/research/benchmarks/README.md`.

## Project Architecture

```text
grove/
├── src/
│   ├── core/          # Gate state machine, learning types
│   ├── backends/      # Memory backend implementations
│   ├── discovery/     # Ticketing and backend detection
│   ├── stats/         # Quality tracking and insights
│   ├── storage/       # Session state persistence
│   ├── hooks/         # Claude Code hook integration
│   └── cli/           # Command implementations
├── agents/            # Claude Code agent definitions
├── design/            # Design documentation
└── .claude-plugin/    # Plugin manifest and installer
```

## Design Documentation

Before making significant changes, review the design docs in `/design/`:

| Document | Content |
|----------|---------|
| `00-overview.md` | Vision and core concepts |
| `01-architecture.md` | Full system design (most comprehensive) |
| `02-implementation.md` | Concrete Rust module structure |
| `03-stats-and-quality.md` | Event log model, quality tracking |
| `04-test-plan.md` | Testing strategy and test cases |
| `05-ci.md` | CI workflows and release process |
| `implementation-tasks.md` | Implementation roadmap |

Decision rationale and risk analysis in `/documents/`.
