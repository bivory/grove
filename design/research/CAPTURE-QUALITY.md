# Capture Quality Research

> Research into ensuring captured developer learnings are high-quality,
> non-generic, and project-specific.

## Problem

Grove captures learnings at ticket boundaries via `grove reflect`. The write
gate validates structure (schema) and keyword plausibility (criteria claims),
but nothing evaluates whether a learning is genuinely useful. A learning like
"Always write tests for your code" passes all layers despite providing zero
project-specific value.

The stats engine tracks downstream quality signals (hit rate, decay) but these
are lagging indicators. By the time stats reveal a low-quality learning, it
has already polluted the corpus.

## Specificity Heuristics (Implemented)

Three heuristic checks score learnings at capture time (`src/core/quality.rs`):

### Named Entity Density (NED)

Code-specific entities per 100 words: file paths, snake_case/camelCase/
PascalCase identifiers, version numbers, qualified names. High-quality
learnings reference concrete artifacts; generic advice scores NED ~0.

### Project-Specific Term Frequency (PSTF)

Ratio of project-specific tags to total tags. Tags like `#tantivy`,
`#bm25-scoring` indicate domain knowledge. Tags like `#testing`, `#debugging`
could apply to any project. 12 generic tag patterns are maintained.

### Generic Phrase Detection (GPD)

Scans text for generic advice phrases: "always remember to", "best practice",
"be careful with", "consider using", etc. Each match costs 1.5 penalty points
(capped at 5.0).

### Composite Formula

```
composite = clamp(ned * 0.4 + pstf * 5.0 * 0.4 + (5.0 - generic_penalty) * 0.2, 0.0, 5.0)
```

Threshold: 1.5 (configurable via `write_gate.min_specificity_score`).
Mode: `enforce` (reject), `warn` (log only), or `disabled`.

### Calibration Examples

| Learning Type | NED | PSTF | GPD | Score | Decision |
|---------------|-----|------|-----|-------|----------|
| Code-heavy with specific paths | ~6.0 | 1.0 | 0 | 5.0 | Accept |
| Domain-specific with project tags | ~2.0 | 0.8 | 0 | 3.4 | Accept |
| Generic advice with filler phrases | ~0.0 | 0.0 | 2 | 0.4 | Reject |
| Vague caution | ~0.0 | 0.0 | 1 | 0.7 | Reject |

## Validation: 38-Learning Corpus Audit

Ran scoring against 38 real learnings. Results:

- **Zero false rejections** at default threshold (1.5)
- Score distribution: 42% scored 5.0 (max), only 8% in the 1.5-1.99 zone
- 3 borderline learnings (1.50-2.33) were genuinely useful but expressed in
  natural language rather than code identifiers
- Aggregate: mean composite 4.20, NED range 0-30.0, PSTF mean 0.72

The borderline zone (1.50-2.50) is the exact use case for an LLM judge:
heuristics miss semantic content that a language model would catch.

## Keyword Extraction Audit

Evaluated `extract_tool_input_keywords` against 34 transcripts (~1,200 tool
calls, 38 learnings):

| Metric | Value |
|--------|-------|
| True positive rate | 15% |
| False positive rate | 60% |
| Learnings never matched | 23/38 (61%) |

### Root Causes

1. **Path prefix pollution**: File path components (`users`, `github`, `src`)
   match broadly but carry no semantic signal. Fixed in R1 (path stripping)
   and R2 (expanded noise list).
2. **Noise list too small**: Original 44-entry list missed common build/test
   terms, package management, and generic CLI vocabulary. Expanded to 100+.
3. **Tool input is structurally limited**: Tool calls describe *what files*
   the agent touches, not *what the developer needs to know*. This semantic
   gap is fundamental to keyword-based retrieval from tool input.

### Key Findings

- BM25 rescoring dramatically improved over keyword overlap (2.76 vs 2.32 avg)
- User intent keywords from transcripts provide high signal but high variance
- Post-retrieval filtering (intent-as-filter) outperforms query expansion for
  small corpora (<100 learnings)

## SOTA Techniques Surveyed

### Implemented

| Technique | Status | Impact |
|-----------|--------|--------|
| Specificity heuristics (NED/PSTF/GPD) | Production | Zero false rejections on test corpus |
| Expanded noise word list | Production | Path pollution eliminated |
| Path stripping | Production | Removed home dir / hosting prefix from keywords |

### Not Yet Implemented

| Technique | Priority | Notes |
|-----------|----------|-------|
| LLM-as-judge for borderline zone | P2 | Multi-axis rubric (specificity, novelty, actionability). 3-5 calibration examples. Haiku latency ~0.8s/eval. Constraint: 5s hook timeout requires parallel evaluation. |
| Embedding-based near-duplicate detection | P3 | MinHash/SimHash for fuzzy dedup. Current exact-match + substring approach misses paraphrased duplicates. |
| Knowledge distillation | P4 | Compress multiple related learnings into fewer, richer ones. Manual via `grove review`, automated via LLM in future. |
| Novelty detection | P4 | TF-IDF surprise scoring against existing corpus to flag redundant captures. |
| Spaced repetition | P4 | Leitner-box scheduling for learning lifecycle management. |

## Architecture

```
grove reflect
  -> validate_schema()         (Layer 1: structure)
  -> validate_write_gate()     (Layer 2: criteria claims)
  -> assess_specificity()      (Layer 3: content quality)
  -> check_near_duplicates()   (Layer 4: deduplication)
```

Quality check is fail-open via `std::panic::catch_unwind`. Infrastructure
errors never block work.

## Files

| File | Purpose |
|------|---------|
| `src/core/quality.rs` | NED, PSTF, GPD heuristics, composite scoring (32 unit tests) |
| `src/core/reflect.rs` | Quality-aware validators, `WriteGateConfidence::Rejected` (8 integration tests) |
| `src/config.rs` | `quality_check`, `min_specificity_score` in `WriteGateConfig` |
