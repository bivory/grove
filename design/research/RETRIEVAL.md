# Retrieval Quality Research

> Research into finding and ranking the right learnings at query time.

## Problem

Grove retrieves learnings at session start using keywords extracted from git
context and tool calls. The fundamental challenge: bag-of-words methods cannot
bridge the semantic gap between what the developer is *doing* (file edits, tool
calls) and what they need to *know* (stored learnings about pitfalls,
conventions, patterns).

**Baseline performance** (single-corpus, 38 learnings, 35 sessions):

| Method | Avg Relevance | Noise (<=2) | Pairs |
|--------|--------------|-------------|-------|
| Keyword overlap | 2.32 | -- | 256 |
| BM25 | 2.76 | 54% | 145 |

## Experiment Descriptions

| # | Experiment | Goal |
|---|-----------|------|
| 1 | **Keyword overlap** | Baseline. Extract keywords from tool call inputs (file paths, arguments) and match against learning tags/text via set intersection. Establishes the floor for retrieval quality. |
| 2 | **BM25 rescoring** | Replace naive keyword overlap with Tantivy BM25 scoring. BM25's TF-IDF weighting should reward rare, discriminative terms and penalize common ones, improving ranking quality. |
| 3 | **BM25 + adaptive threshold** | Add a dynamic score cutoff (top score × ratio) to reject low-confidence matches instead of returning a fixed K. Should reduce noise by filtering out weak BM25 matches that clear a static threshold. |
| 4 | **Domain enrichment** | Expand queries with domain-specific synonyms inferred from file paths (e.g., `.ex` files → "elixir", "phoenix"). Hypothesis: bridging vocabulary gaps between tool inputs and learning text improves recall. |
| 5 | **User intent (query expand)** | Extract intent keywords from the full session transcript (what the developer is trying to accomplish) and add them to the BM25 query. Hypothesis: richer queries capture semantic context that tool inputs miss. |
| 6 | **Intent-as-filter** | Same intent signal as #5, but applied as a post-retrieval filter instead of query expansion. Start from BM25 + adaptive results and remove learnings whose content doesn't align with detected user intent. Hypothesis: subtractive filtering preserves precision while removing noise. |

## Experiment Chronology

All experiments evaluated via LLM judge (Haiku) scoring relevance 1-5.

| # | Experiment | Avg | Noise | Pairs | Delta vs Baseline | Outcome |
|---|-----------|-----|-------|-------|-------------------|---------|
| 1 | Keyword overlap | 2.32 | -- | 256 | baseline | -- |
| 2 | BM25 rescoring | 2.76 | 54% | 145 | +0.44 | Ship |
| 3 | BM25 + adaptive threshold | 2.88 | 50% | 122 | +0.56, -4pp noise | Ship |
| 4 | + domain enrichment | 2.66 | 57% | 147 | -0.22, +7pp noise | Regressed |
| 5 | + user intent (query expand) | 2.69 | 57% | 150 | -0.19, +7pp noise | Regressed |
| 6 | + intent-as-filter | 3.27 | 35% | 51 | +0.39, -15pp noise | Best single-corpus |

### Key Insight: Filter > Expand for Small Corpora

Both query expansion experiments (#4, #5) regressed by nearly identical margins
(-0.19 to -0.22, both +7pp noise). The mechanism: additive keywords push more
learnings above the adaptive threshold, undoing precision gains. With only 38
documents, BM25 IDF statistics are too noisy to compensate for broad terms.

The intent-as-filter approach (#6) inverts the mechanism: start from the same
BM25 + adaptive results and *remove* learnings that don't match user intent.
The 73 removed pairs averaged ~2.10 (noise); the 51 surviving pairs averaged
3.27. The 100% cache hit rate confirms the filter is purely subtractive.

## Production Configuration

After multi-corpus validation across 3 corpora (54, 44, and 18 learnings):

| Setting | Value | Rationale |
|---------|-------|-----------|
| `scoring_backend` | `"bm25"` | BM25 rescoring via Tantivy |
| `corpus_enrichment` | `true` | Corpus-derived vocabulary enrichment |
| `corpus_size_threshold` | `50` | Boosted BM25 for <50 learnings, plain for >=50 |
| `dynamic_k_ratio` | `0.3` | Only inject learnings scoring >= 30% of top score |
| `adaptive_dk` | `false` | Defaults off until stats accumulate |

The heuristic routing (`corpus_size_threshold`) selects boosted BM25 for small
corpora (better recall) and plain BM25 for large corpora (better precision).

## Multi-Corpus Benchmark Results

### 3-Corpus Sweep (Haiku judge, bootstrap 95% CIs)

| Config | Corpus A (54L) |  | Corpus B (44L) |  | Corpus C (18L) |  |
|--------|------|------|------|------|------|------|
| | F1 | Noise | F1 | Noise | F1 | Noise |
| bm25 | **0.654** | 34% | 0.534 | 47% | 0.637 | 38% |
| boosted-adaptive | 0.601 | 35% | **0.571** | 44% | **0.731** | 28% |
| heuristic(50) | **0.654** | 34% | **0.571** | 44% | **0.731** | 28% |
| corpus-enriched | 0.622 | 34% | 0.563 | 45% | **0.731** | 28% |

The heuristic correctly routes each corpus to its optimal config. Corpus-
enriched matches or improves on boosted-adaptive across all corpora.

### Cross-Corpus Negatives

Pairing learnings from one project with sessions from another measures false
positive rate (should be ~0%):

| Source -> Target | FPR@4 |
|-----------------|-------|
| Corpus A -> B | 3% |
| Corpus A -> C | 1% |
| Corpus B -> A | 8% |
| Corpus B -> C | 3% |
| Corpus C -> A | 1% |
| Corpus C -> B | 3% |

Corpus B has elevated inbound FPR due to generic domain vocabulary overlap
(common framework terms in learnings match broadly).

### Judge Reliability

| Comparison | Exact Agreement | Within +/-1 | Mean Abs Diff |
|-----------|----------------|-------------|---------------|
| Haiku vs fresh Haiku (intra-rater) | 62.3% | 93.7% | 0.45 |
| Haiku vs Sonnet (inter-rater) | 48.8% | 86.1% | 0.69 |

Haiku is sufficiently reliable for relative comparisons between configs.
Sonnet scores systematically lower on two of three corpora.

## Corpus Structural Analysis

### Why Corpus B Underperforms

Root causes identified through per-learning analysis:

1. **Over-surfacing of generic learnings**: 3 learnings account for ~37% of
   pairs at 58% noise. Generic domain vocabulary in both learnings and sessions
   causes broad BM25 matching.

2. **Dead learnings (28%)**: 11/39 learnings never score above 2. Three failure
   modes: wrong domain (5), too micro-specific (3), pattern without anchor (3).

3. **Decay system gap**: `last_surfaced` resets the decay clock. Dead learnings
   that keep being surfaced (BM25 matches vocabulary) and dismissed will never
   decay. Fixed by fast-track decay for 0% hit-rate learnings.

4. **Topic fragmentation**: Many loosely-related topics with 2-5 learnings each.
   Sessions match many learnings weakly rather than few learnings strongly.

## Adaptive Dynamic-K Tuning

Three-level system for per-query dk adjustment (implemented, default off):

| Level | Signal | Mechanism |
|-------|--------|-----------|
| L1 | Score distribution CV | Gentle nudge (+-0.03 max) based on score compression |
| L2 | Stats cache hit rates | Corpus-maturity signal (+-0.05, requires >=20 surfaced) |
| L3 | Per-category dismiss rates | Feedback loop (+0.08 max, requires >=3 learnings) |

Defaults off (`adaptive_dk = false`) until repos accumulate stats data for
L2/L3 to self-calibrate. L1 alone regressed on small corpora.

## Benchmark Infrastructure

### Running Benchmarks

```bash
# Single-corpus replay (from regular terminal, not Claude Code)
GROVE_LLM_JUDGE_BACKEND=cli cargo test --features tantivy-search \
  -- --ignored replay_tantivy_adaptive_llm_judge --nocapture

# Multi-corpus sweep
grove eval sweep --manifest .grove/corpora.toml \
  --configs bm25,boosted-adaptive,heuristic,corpus-enriched \
  --bootstrap 1000

# Cross-corpus negatives
grove eval sweep --manifest .grove/corpora.toml \
  --configs boosted-adaptive --cross-negatives
```

### Manifest Format

```toml
[[corpus]]
name = "my-project"
transcript_dir = "~/.claude/projects/-Users-dev-my-project"
learnings_path = "/home/dev/my-project/.grove/learnings.md"
```

### Available Benchmark Configs

| Config | Description |
|--------|-------------|
| `bm25` | Plain BM25 (no adaptive threshold) |
| `adaptive` | BM25 + adaptive threshold + dynamic K |
| `boosted-adaptive` | Per-term boosted BM25 + adaptive |
| `heuristic` / `heuristic(N)` | Corpus-size routing (threshold N, default 50) |
| `corpus-enriched` | BM25 + corpus vocabulary enrichment + adaptive |
| `adaptive-dk` | BM25 + adaptive + per-query adaptive dynamic K |
| `intent-filter` | BM25 + adaptive + user intent post-filter |
| `flat-recency` | BM25 + adaptive with flat 90-day half-life (ablation) |
| `boosted(kw=F,tag=F,dk=F)` | Custom boost parameters |

### Validation Policy

No retrieval changes ship to production unless they improve (or hold) F1 on
**all** available benchmark corpora.

### Automated Sweep Script

`design/research/scripts/bench-sweep.sh` automates building, sweeping, and
cross-negative analysis:

```bash
./design/research/scripts/bench-sweep.sh              # full sweep
./design/research/scripts/bench-sweep.sh --quick       # new configs only
./design/research/scripts/bench-sweep.sh --bootstrap   # with 95% CIs
```

## SOTA Techniques Not Yet Implemented

| Technique | Priority | Expected Impact |
|-----------|----------|----------------|
| Embedded vector search (sqlite-vec) | P1 | Bridges semantic gap; highest-leverage gap remaining |
| UserPromptSubmit hook | P2 | Mid-session re-retrieval as conversation evolves |
| Cross-encoder reranking | P2 | ~15-20% noise reduction (literature estimates) |
| Hybrid search (BM25 + embeddings via RRF) | P3 | Better recall at small corpus sizes |
| LLM query rewriting | P3 | Session context -> natural language query |
| Graph-based retrieval | P4 | Learning relationship graph for multi-hop reasoning |

## Files

| File | Purpose |
|------|---------|
| `src/hooks/runner.rs` | Retrieval pipeline, BM25 rescoring, adaptive threshold, intent filter |
| `src/eval/runner.rs` | Benchmark orchestration, config parsing |
| `src/eval/judge.rs` | LLM judge for (session, learning) pair scoring |
| `src/eval/metrics.rs` | F1, P@3, R@4, MRR, coverage, bootstrap CIs |
| `src/stats/scoring.rs` | Composite scoring (relevance x recency x reference boost) |
| `src/stats/recommendations.rs` | Stats-driven dk and strategy recommendations |
| `src/config.rs` | All retrieval config fields with defaults and env var overrides |
