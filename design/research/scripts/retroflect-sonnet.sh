#!/usr/bin/env bash
# retroflect-sonnet.sh — Re-generate learnings with Sonnet and benchmark the delta
#
# Backs up Haiku-generated learnings, re-runs retroflect with Sonnet,
# benchmarks both, and produces a comparison.
#
# Usage:
#   ./design/research/scripts/retroflect-sonnet.sh                # full run (sync API)
#   ./design/research/scripts/retroflect-sonnet.sh --batch        # use Batch API (50% cheaper)
#   ./design/research/scripts/retroflect-sonnet.sh --dry-run      # preview without writing
#   ./design/research/scripts/retroflect-sonnet.sh --bench-only   # skip retroflect, just benchmark
#   ./design/research/scripts/retroflect-sonnet.sh --restore      # restore Haiku backups

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
MANIFEST="${REPO_ROOT}/.grove/corpora.toml"
GROVE_BIN="${REPO_ROOT}/target/release/grove"
DATE="$(date +%Y%m%d)"
OUT_DIR="${REPO_ROOT}/design/research/benchmarks/sonnet-retroflect-${DATE}"
MODEL="claude-sonnet-4-20250514"

# Corpus paths (must match corpora.toml)
GROVE_LEARNINGS="/Users/bryanivory/GitHub/bryan/grove/.grove/learnings.md"
PICKUP_LEARNINGS="/Users/bryanivory/GitHub/bryan/pickup-rank/.grove/learnings.md"
SYANTRA_LEARNINGS="/Users/bryanivory/GitHub/Mentorhead/Syantra/.grove/learnings.md"

GROVE_PROJECT="/Users/bryanivory/GitHub/bryan/grove"
PICKUP_PROJECT="/Users/bryanivory/GitHub/bryan/pickup-rank"
SYANTRA_PROJECT="/Users/bryanivory/GitHub/Mentorhead/Syantra"

ALL_LEARNINGS=("${GROVE_LEARNINGS}" "${PICKUP_LEARNINGS}" "${SYANTRA_LEARNINGS}")
ALL_PROJECTS=("${GROVE_PROJECT}" "${PICKUP_PROJECT}" "${SYANTRA_PROJECT}")
CORPUS_NAMES=("grove" "pickup-rank" "syantra")

# Defaults
BATCH_FLAG=""
DRY_RUN=false
BENCH_ONLY=false
RESTORE=false
BENCHMARK_CONFIGS="boosted-adaptive,heuristic-enriched,intent-boost"

for arg in "$@"; do
    case "${arg}" in
        --batch)     BATCH_FLAG="--batch" ;;
        --dry-run)   DRY_RUN=true ;;
        --bench-only) BENCH_ONLY=true ;;
        --restore)   RESTORE=true ;;
        --help|-h)
            echo "Usage: $0 [--batch] [--dry-run] [--bench-only] [--restore]"
            echo ""
            echo "  --batch       Use Batch API for retroflect (50% cheaper, async)"
            echo "  --dry-run     Preview retroflect candidates without writing"
            echo "  --bench-only  Skip retroflect, just run benchmarks on current learnings"
            echo "  --restore     Restore Haiku backup learnings and exit"
            exit 0
            ;;
        *)
            echo "Unknown arg: ${arg}" >&2
            exit 1
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Restore mode
# ---------------------------------------------------------------------------

if [[ "${RESTORE}" == "true" ]]; then
    echo "=== Restoring Haiku backups ==="
    for i in "${!ALL_LEARNINGS[@]}"; do
        backup="${ALL_LEARNINGS[$i]%.md}-haiku.md"
        if [[ -f "${backup}" ]]; then
            cp "${backup}" "${ALL_LEARNINGS[$i]}"
            echo "  Restored: ${CORPUS_NAMES[$i]} (from ${backup})"
        else
            echo "  SKIP: ${CORPUS_NAMES[$i]} (no backup at ${backup})"
        fi
    done
    echo "Done. Run benchmarks to verify."
    exit 0
fi

echo "=============================================="
echo "  Retroflect with Sonnet"
echo "  Model:  ${MODEL}"
echo "  Date:   ${DATE}"
echo "  Output: ${OUT_DIR}/"
echo "=============================================="
echo ""

mkdir -p "${OUT_DIR}"

# ---------------------------------------------------------------------------
# Phase 1: Build
# ---------------------------------------------------------------------------

echo "--- Phase 1: Build ---"
cargo build --release --features tantivy-search \
    --manifest-path "${REPO_ROOT}/Cargo.toml" 2>"${OUT_DIR}/build.log"
echo "  Built release binary"
echo ""

# ---------------------------------------------------------------------------
# Phase 2: Backup Haiku learnings
# ---------------------------------------------------------------------------

if [[ "${BENCH_ONLY}" == "false" ]]; then
    echo "--- Phase 2: Backup Haiku learnings ---"
    for i in "${!ALL_LEARNINGS[@]}"; do
        src="${ALL_LEARNINGS[$i]}"
        backup="${src%.md}-haiku.md"
        if [[ -f "${src}" ]]; then
            if [[ -f "${backup}" ]]; then
                echo "  EXISTS: ${CORPUS_NAMES[$i]} backup already at ${backup}"
            else
                cp "${src}" "${backup}"
                echo "  Backed up: ${CORPUS_NAMES[$i]} ($(wc -l < "${src}") lines)"
            fi
        else
            echo "  SKIP: ${CORPUS_NAMES[$i]} (no learnings file)"
        fi
    done
    echo ""

    # Count learnings before
    echo "  Learning counts (before):"
    for i in "${!ALL_LEARNINGS[@]}"; do
        count=$(grep -c "^id:" "${ALL_LEARNINGS[$i]}" 2>/dev/null || echo "0")
        echo "    ${CORPUS_NAMES[$i]}: ${count}"
    done
    echo ""

    # -----------------------------------------------------------------------
    # Phase 3: Retroflect with Sonnet
    # -----------------------------------------------------------------------

    echo "--- Phase 3: Retroflect with Sonnet ---"
    echo ""

    if [[ "${DRY_RUN}" == "true" ]]; then
        echo "  (DRY RUN — showing candidates only)"
        echo ""
    fi

    for i in "${!ALL_PROJECTS[@]}"; do
        echo "  --- ${CORPUS_NAMES[$i]} ---"
        DRY_RUN_FLAG=""
        if [[ "${DRY_RUN}" == "true" ]]; then
            DRY_RUN_FLAG="--dry-run"
        fi

        if "${GROVE_BIN}" retroflect \
            --project "${ALL_PROJECTS[$i]}" \
            --model "${MODEL}" \
            --force \
            --yes \
            ${BATCH_FLAG} \
            ${DRY_RUN_FLAG} \
            2>&1 | tee "${OUT_DIR}/retroflect-${CORPUS_NAMES[$i]}.log"; then
            echo "  DONE: ${CORPUS_NAMES[$i]}"
        else
            echo "  FAILED: ${CORPUS_NAMES[$i]} (see ${OUT_DIR}/retroflect-${CORPUS_NAMES[$i]}.log)"
        fi
        echo ""
    done

    if [[ "${DRY_RUN}" == "true" ]]; then
        echo "Dry run complete. Re-run without --dry-run to generate learnings."
        exit 0
    fi

    # If batch mode, remind user to check back
    if [[ -n "${BATCH_FLAG}" ]]; then
        echo "=============================================="
        echo "  Batch API requests submitted."
        echo "  Check progress and re-run with --bench-only"
        echo "  once batches complete."
        echo "=============================================="
        exit 0
    fi

    # Count learnings after
    echo "  Learning counts (after Sonnet retroflect):"
    for i in "${!ALL_LEARNINGS[@]}"; do
        count=$(grep -c "^id:" "${ALL_LEARNINGS[$i]}" 2>/dev/null || echo "0")
        echo "    ${CORPUS_NAMES[$i]}: ${count}"
    done
    echo ""
fi

# ---------------------------------------------------------------------------
# Phase 4: Benchmark Sonnet learnings
# ---------------------------------------------------------------------------

echo "--- Phase 4: Benchmark Sonnet learnings ---"
echo ""

"${GROVE_BIN}" eval sweep \
    --manifest "${MANIFEST}" \
    --configs "${BENCHMARK_CONFIGS}" \
    2>&1 | tee "${OUT_DIR}/sonnet-results.txt"

"${GROVE_BIN}" eval sweep \
    --manifest "${MANIFEST}" \
    --configs "${BENCHMARK_CONFIGS}" \
    --json > "${OUT_DIR}/sonnet-results.json" 2>/dev/null

echo ""

# ---------------------------------------------------------------------------
# Phase 5: Benchmark Haiku learnings (swap, bench, swap back)
# ---------------------------------------------------------------------------

echo "--- Phase 5: Benchmark Haiku learnings (baseline) ---"
echo ""

# Check that backups exist
HAVE_BACKUPS=true
for i in "${!ALL_LEARNINGS[@]}"; do
    backup="${ALL_LEARNINGS[$i]%.md}-haiku.md"
    if [[ ! -f "${backup}" ]]; then
        echo "  SKIP: No Haiku backup for ${CORPUS_NAMES[$i]}"
        HAVE_BACKUPS=false
    fi
done

if [[ "${HAVE_BACKUPS}" == "true" ]]; then
    # Swap in Haiku learnings
    for i in "${!ALL_LEARNINGS[@]}"; do
        backup="${ALL_LEARNINGS[$i]%.md}-haiku.md"
        sonnet_backup="${ALL_LEARNINGS[$i]%.md}-sonnet.md"
        cp "${ALL_LEARNINGS[$i]}" "${sonnet_backup}"
        cp "${backup}" "${ALL_LEARNINGS[$i]}"
    done

    "${GROVE_BIN}" eval sweep \
        --manifest "${MANIFEST}" \
        --configs "${BENCHMARK_CONFIGS}" \
        2>&1 | tee "${OUT_DIR}/haiku-results.txt"

    "${GROVE_BIN}" eval sweep \
        --manifest "${MANIFEST}" \
        --configs "${BENCHMARK_CONFIGS}" \
        --json > "${OUT_DIR}/haiku-results.json" 2>/dev/null

    # Restore Sonnet learnings (the new default)
    for i in "${!ALL_LEARNINGS[@]}"; do
        sonnet_backup="${ALL_LEARNINGS[$i]%.md}-sonnet.md"
        cp "${sonnet_backup}" "${ALL_LEARNINGS[$i]}"
        rm "${sonnet_backup}"
    done

    echo ""
else
    echo "  Skipping Haiku baseline (no backups). Run full pipeline first."
    echo ""
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

cat > "${OUT_DIR}/metadata.txt" <<METADATA
# Sonnet Retroflect Benchmark
# Date: ${DATE}
# Model: ${MODEL}
# Configs: ${BENCHMARK_CONFIGS}
# Corpora: grove, pickup-rank, syantra
#
# Files:
#   sonnet-results.txt/json  — Benchmarks with Sonnet-generated learnings
#   haiku-results.txt/json   — Benchmarks with original Haiku-generated learnings
#   retroflect-*.log         — Retroflect output per corpus
#
# To restore Haiku learnings:
#   $0 --restore
METADATA

echo "=============================================="
echo "  Complete"
echo "=============================================="
echo ""
echo "  Sonnet results:  ${OUT_DIR}/sonnet-results.txt"
if [[ "${HAVE_BACKUPS}" == "true" ]]; then
    echo "  Haiku baseline:  ${OUT_DIR}/haiku-results.txt"
fi
echo "  Metadata:        ${OUT_DIR}/metadata.txt"
echo "  Restore Haiku:   $0 --restore"
echo ""
