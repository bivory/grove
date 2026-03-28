#!/usr/bin/env bash
# bench-new-features.sh — Test and benchmark new retrieval features
#
# Exercises all features added in the reliability branch:
#   1. Stats version filter (--version)
#   2. Dedup audit (semantic-dedup feature)
#   3. Retrieval benchmarks: intent-boost, heuristic-enriched
#
# Usage:
#   ./design/research/scripts/bench-new-features.sh             # full run
#   ./design/research/scripts/bench-new-features.sh --skip-dedup  # skip dedup (no ONNX)
#   ./design/research/scripts/bench-new-features.sh --bootstrap   # include 95% CIs

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
MANIFEST="${REPO_ROOT}/.grove/corpora.toml"
DATE="$(date +%Y%m%d)"
OUT_DIR="${REPO_ROOT}/design/research/benchmarks/new-features-${DATE}"
BRANCH="$(git -C "${REPO_ROOT}" rev-parse --abbrev-ref HEAD)"
COMMIT="$(git -C "${REPO_ROOT}" rev-parse --short HEAD)"

SKIP_DEDUP=false
BOOTSTRAP=0

for arg in "$@"; do
    case "${arg}" in
        --skip-dedup)  SKIP_DEDUP=true ;;
        --bootstrap)   BOOTSTRAP=1000 ;;
        --help|-h)
            echo "Usage: $0 [--skip-dedup] [--bootstrap]"
            echo ""
            echo "  --skip-dedup  Skip semantic dedup audit (avoids ONNX dependency)"
            echo "  --bootstrap   Include 95% CIs via 1000 bootstrap resamples"
            exit 0
            ;;
        *)
            echo "Unknown arg: ${arg}" >&2
            exit 1
            ;;
    esac
done

echo "=============================================="
echo "  New Feature Benchmark Suite"
echo "  Branch: ${BRANCH} (${COMMIT})"
echo "  Date:   ${DATE}"
echo "=============================================="
echo ""

mkdir -p "${OUT_DIR}"

PASS=0
FAIL=0
SKIP=0

pass() { echo "  PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $1"; FAIL=$((FAIL + 1)); }
skip() { echo "  SKIP: $1"; SKIP=$((SKIP + 1)); }

# =========================================================================
# Phase 1: Build
# =========================================================================

echo "--- Phase 1: Build ---"
echo ""

echo "  Building release binary (tantivy-search)..."
if cargo build --release --features tantivy-search \
    --manifest-path "${REPO_ROOT}/Cargo.toml" 2>"${OUT_DIR}/build-tantivy.log"; then
    pass "release build (tantivy-search)"
else
    fail "release build (tantivy-search)"
    echo "  See: ${OUT_DIR}/build-tantivy.log"
    exit 1
fi

GROVE_BIN="${REPO_ROOT}/target/release/grove"

if [[ "${SKIP_DEDUP}" == "false" ]]; then
    echo "  Building release binary (semantic-dedup)..."
    if cargo build --release --features semantic-dedup \
        --manifest-path "${REPO_ROOT}/Cargo.toml" 2>"${OUT_DIR}/build-dedup.log"; then
        pass "release build (semantic-dedup)"
    else
        fail "release build (semantic-dedup)"
        echo "  See: ${OUT_DIR}/build-dedup.log"
        SKIP_DEDUP=true
    fi
fi

echo ""

# =========================================================================
# Phase 2: Stats version filter
# =========================================================================

echo "--- Phase 2: Stats Version Filter ---"
echo ""

if "${GROVE_BIN}" stats --version 0.9.0 --json > "${OUT_DIR}/stats-v0.9.0.json" 2>/dev/null; then
    pass "stats --version 0.9.0"
else
    fail "stats --version 0.9.0"
fi

if "${GROVE_BIN}" stats --version "pre:0.9.0" --json > "${OUT_DIR}/stats-pre-0.9.0.json" 2>/dev/null; then
    pass "stats --version pre:0.9.0"
else
    fail "stats --version pre:0.9.0"
fi

if "${GROVE_BIN}" stats --json > "${OUT_DIR}/stats-all.json" 2>/dev/null; then
    pass "stats (no filter, baseline)"
else
    fail "stats (no filter, baseline)"
fi

echo ""

# =========================================================================
# Phase 3: Dedup audit
# =========================================================================

echo "--- Phase 3: Dedup Audit ---"
echo ""

if [[ "${SKIP_DEDUP}" == "true" ]]; then
    skip "dedup audit (--skip-dedup or build failed)"
else
    # Rebuild with semantic-dedup for the dedup binary
    GROVE_DEDUP="${REPO_ROOT}/target/release/grove"
    cargo build --release --features semantic-dedup \
        --manifest-path "${REPO_ROOT}/Cargo.toml" 2>/dev/null

    # Run against each corpus in the manifest
    for corpus_learnings in $(grep learnings_path "${MANIFEST}" | sed 's/.*= "//;s/"//'); do
        corpus_name="$(basename "$(dirname "$(dirname "${corpus_learnings}")")")"
        if [[ -f "${corpus_learnings}" ]]; then
            if "${GROVE_DEDUP}" eval dedup-audit \
                --learnings-path "${corpus_learnings}" \
                --threshold 0.85 \
                --json > "${OUT_DIR}/dedup-${corpus_name}.json" 2>/dev/null; then
                pass "dedup audit: ${corpus_name}"
            else
                fail "dedup audit: ${corpus_name}"
            fi
        else
            skip "dedup audit: ${corpus_name} (learnings not found)"
        fi
    done
fi

echo ""

# =========================================================================
# Phase 4: Retrieval benchmarks (new configs)
# =========================================================================

echo "--- Phase 4: Retrieval Benchmarks ---"
echo ""

# Rebuild with tantivy for retrieval benchmarks
cargo build --release --features tantivy-search \
    --manifest-path "${REPO_ROOT}/Cargo.toml" 2>/dev/null

if [[ ! -f "${MANIFEST}" ]]; then
    fail "manifest not found: ${MANIFEST}"
    echo ""
else
    NEW_CONFIGS="intent-boost,heuristic-enriched"
    COMPARE_CONFIGS="boosted-adaptive,${NEW_CONFIGS}"

    echo "  Sweep: ${NEW_CONFIGS} (new) + boosted-adaptive (baseline)"
    echo ""

    # Text output (warms judge cache)
    if "${GROVE_BIN}" eval sweep \
        --manifest "${MANIFEST}" \
        --configs "${COMPARE_CONFIGS}" \
        --bootstrap "${BOOTSTRAP}" \
        2>&1 | tee "${OUT_DIR}/retrieval-results.txt"; then
        pass "retrieval sweep (text)"
    else
        fail "retrieval sweep (text)"
    fi

    echo ""

    # JSON output (cache hits, fast)
    if "${GROVE_BIN}" eval sweep \
        --manifest "${MANIFEST}" \
        --configs "${COMPARE_CONFIGS}" \
        --bootstrap "${BOOTSTRAP}" \
        --json > "${OUT_DIR}/retrieval-results.json" 2>/dev/null; then
        pass "retrieval sweep (json)"
    else
        fail "retrieval sweep (json)"
    fi

    # Cross-corpus negatives
    echo ""
    echo "  Cross-corpus negatives..."
    if "${GROVE_BIN}" eval sweep \
        --manifest "${MANIFEST}" \
        --configs "${COMPARE_CONFIGS}" \
        --cross-negatives \
        --negative-config boosted-adaptive \
        2>&1 | tee "${OUT_DIR}/negatives-results.txt"; then
        pass "cross-corpus negatives (text)"
    else
        fail "cross-corpus negatives (text)"
    fi

    "${GROVE_BIN}" eval sweep \
        --manifest "${MANIFEST}" \
        --configs "${COMPARE_CONFIGS}" \
        --cross-negatives \
        --negative-config boosted-adaptive \
        --json > "${OUT_DIR}/negatives-results.json" 2>/dev/null \
        && pass "cross-corpus negatives (json)" \
        || fail "cross-corpus negatives (json)"
fi

echo ""

# =========================================================================
# Summary
# =========================================================================

cat > "${OUT_DIR}/metadata.txt" <<METADATA
# New Feature Benchmark
# Date: ${DATE}
# Branch: ${BRANCH} (${COMMIT})
# Bootstrap: ${BOOTSTRAP}
# Manifest: ${MANIFEST}
#
# Features tested:
#   1. Stats --version filter (exact and pre: prefix)
#   2. Dedup audit (semantic-dedup, cosine threshold 0.85)
#   3. intent-boost    — Intent overlap ratio as +0.1*r additive tiebreaker
#   4. heuristic-enriched — Corpus-size routing + vocabulary enrichment (production default)
METADATA

echo "=============================================="
echo "  Results: ${PASS} passed, ${FAIL} failed, ${SKIP} skipped"
echo "=============================================="
echo ""
echo "  Output:     ${OUT_DIR}/"
echo "  Retrieval:  ${OUT_DIR}/retrieval-results.txt"
echo "  Negatives:  ${OUT_DIR}/negatives-results.txt"
echo "  Dedup:      ${OUT_DIR}/dedup-*.json"
echo "  Stats:      ${OUT_DIR}/stats-*.json"
echo "  Metadata:   ${OUT_DIR}/metadata.txt"
echo ""

if [[ "${FAIL}" -gt 0 ]]; then
    exit 1
fi
