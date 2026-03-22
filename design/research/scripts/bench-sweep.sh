#!/usr/bin/env bash
# bench-sweep.sh — Benchmark retrieval configs across all corpora
#
# Runs retrieval configs against all corpora in the manifest alongside
# existing baselines for comparison.  Produces both JSON (machine-readable)
# and text (human-readable) output.
#
# Usage:
#   ./design/research/scripts/bench-sweep.sh              # full sweep (all configs)
#   ./design/research/scripts/bench-sweep.sh --quick      # new configs only (faster)
#   ./design/research/scripts/bench-sweep.sh --bootstrap  # include 95% CIs (slower)

set -euo pipefail

# Clean up background processes on exit
BG_PIDS=()
cleanup() { for pid in "${BG_PIDS[@]}"; do kill "${pid}" 2>/dev/null || true; done; }
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
MANIFEST="${REPO_ROOT}/.grove/corpora.toml"
GROVE_BIN="${REPO_ROOT}/target/release/grove"
DATE="$(date +%Y%m%d)"
OUT_DIR="${REPO_ROOT}/design/research/benchmarks/sweep-${DATE}"
BRANCH="$(git -C "${REPO_ROOT}" rev-parse --abbrev-ref HEAD)"
COMMIT="$(git -C "${REPO_ROOT}" rev-parse --short HEAD)"

# Configs
BASELINE_CONFIGS="bm25,boosted-adaptive"
NEW_CONFIGS="heuristic,corpus-enriched"
ALL_CONFIGS="${BASELINE_CONFIGS},${NEW_CONFIGS}"

# Defaults
CONFIGS="${ALL_CONFIGS}"
BOOTSTRAP=0
QUICK=false

# ---------------------------------------------------------------------------
# Parse args
# ---------------------------------------------------------------------------

for arg in "$@"; do
    case "${arg}" in
        --quick)
            QUICK=true
            CONFIGS="${NEW_CONFIGS}"
            ;;
        --bootstrap)
            BOOTSTRAP=1000
            ;;
        --help|-h)
            echo "Usage: $0 [--quick] [--bootstrap]"
            echo ""
            echo "  --quick      Only run new configs (heuristic, corpus-enriched)"
            echo "  --bootstrap  Include 95% CIs via 1000 bootstrap resamples"
            exit 0
            ;;
        *)
            echo "Unknown arg: ${arg}" >&2
            exit 1
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Pre-flight checks
# ---------------------------------------------------------------------------

echo "=== Benchmark Sweep ==="
echo "  Branch:    ${BRANCH} (${COMMIT})"
echo "  Manifest:  ${MANIFEST}"
echo "  Configs:   ${CONFIGS}"
echo "  Bootstrap: ${BOOTSTRAP}"
echo "  Output:    ${OUT_DIR}/"
echo ""

if [[ ! -f "${MANIFEST}" ]]; then
    echo "ERROR: Manifest not found: ${MANIFEST}" >&2
    exit 1
fi

# Build release binary
echo "--- Building release binary ---"
cargo build --release --features tantivy-search --manifest-path "${REPO_ROOT}/Cargo.toml"

if [[ ! -x "${GROVE_BIN}" ]]; then
    echo "ERROR: Binary not found: ${GROVE_BIN}" >&2
    exit 1
fi

# Create output directory
mkdir -p "${OUT_DIR}"

# ---------------------------------------------------------------------------
# Run sweeps: text first (warms cache), then JSON (all cache hits, fast)
# ---------------------------------------------------------------------------

echo ""
echo "--- Running eval sweep (${CONFIGS}) ---"
echo ""

# Text sweep first — shows progress, warms judge cache
"${GROVE_BIN}" eval sweep \
    --manifest "${MANIFEST}" \
    --configs "${CONFIGS}" \
    --bootstrap "${BOOTSTRAP}" \
    2>&1 | tee "${OUT_DIR}/results.txt"

# JSON sweep second — 100% cache hits, near-instant
"${GROVE_BIN}" eval sweep \
    --manifest "${MANIFEST}" \
    --configs "${CONFIGS}" \
    --bootstrap "${BOOTSTRAP}" \
    --json \
    > "${OUT_DIR}/results.json" 2>"${OUT_DIR}/sweep-json.log" &
BG_PIDS+=($!)

# ---------------------------------------------------------------------------
# Run cross-corpus negatives (if not --quick)
# ---------------------------------------------------------------------------

if [[ "${QUICK}" == "false" ]]; then
    echo ""
    echo "--- Running cross-corpus negatives ---"
    echo ""

    # Text first (warms cache), then JSON in background
    "${GROVE_BIN}" eval sweep \
        --manifest "${MANIFEST}" \
        --configs "${CONFIGS}" \
        --cross-negatives \
        --negative-config boosted-adaptive \
        2>&1 | tee "${OUT_DIR}/negatives.txt"

    "${GROVE_BIN}" eval sweep \
        --manifest "${MANIFEST}" \
        --configs "${CONFIGS}" \
        --cross-negatives \
        --negative-config boosted-adaptive \
        --json \
        > "${OUT_DIR}/negatives.json" 2>"${OUT_DIR}/negatives.log" &
    BG_PIDS+=($!)
fi

# Wait for any background JSON exports to finish
for pid in "${BG_PIDS[@]}"; do
    if wait "${pid}"; then
        true
    else
        echo "WARNING: Background process ${pid} failed (exit $?)" >&2
    fi
done

# ---------------------------------------------------------------------------
# Write metadata
# ---------------------------------------------------------------------------

cat > "${OUT_DIR}/metadata.txt" <<METADATA
# Benchmark Sweep
# Date: ${DATE}
# Branch: ${BRANCH} (${COMMIT})
# Configs: ${CONFIGS}
# Bootstrap: ${BOOTSTRAP}
# Manifest: ${MANIFEST}
#
# Available configs:
#   bm25              — Plain BM25 (no boosting, no adaptive threshold)
#   boosted-adaptive  — Per-term boosted BM25 + adaptive threshold + dynamic K
#   heuristic         — Corpus-size heuristic (threshold=50).
#                       Selects boosted BM25 for small corpora (<50 learnings),
#                       plain BM25 for large corpora (>=50 learnings).
#   corpus-enriched   — Corpus-agnostic vocabulary enrichment.
#                       Extracts domain terms from learning text and uses them
#                       to bridge BM25 vocabulary gap at query time.
METADATA

echo ""
echo "=== Benchmark complete ==="
echo "  Results:   ${OUT_DIR}/results.txt"
echo "  JSON:      ${OUT_DIR}/results.json"
echo "  Metadata:  ${OUT_DIR}/metadata.txt"
[[ "${QUICK}" == "false" ]] && echo "  Negatives: ${OUT_DIR}/negatives.txt"
echo ""
