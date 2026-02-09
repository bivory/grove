#!/bin/bash
# tests/integration/run_all.sh
#
# Run all integration tests

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

PASSED=0
FAILED=0
SKIPPED=0

log() {
    echo -e "${GREEN}[runner]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[runner]${NC} $1"
}

error() {
    echo -e "${RED}[runner]${NC} $1" >&2
}

run_test() {
    local test_script="$1"
    local test_name
    test_name=$(basename "$test_script" .sh)

    echo ""
    log "Running: $test_name"
    echo "─────────────────────────────────────────"

    if bash "$test_script"; then
        PASSED=$((PASSED + 1))
    else
        error "FAILED: $test_name"
        FAILED=$((FAILED + 1))
    fi
}

main() {
    log "Grove Integration Test Suite"
    echo ""

    # Ensure grove is built
    if ! command -v grove &> /dev/null; then
        log "Building grove..."
        cd "$PROJECT_DIR"
        cargo build --quiet
        export PATH="$PROJECT_DIR/target/debug:$PATH"
    fi

    # Find and run all test scripts
    for test_script in "$SCRIPT_DIR"/*_flow.sh; do
        if [ -f "$test_script" ]; then
            run_test "$test_script"
        fi
    done

    # Summary
    echo ""
    echo "═══════════════════════════════════════════"
    log "Results: $PASSED passed, $FAILED failed, $SKIPPED skipped"
    echo "═══════════════════════════════════════════"

    if [ "$FAILED" -gt 0 ]; then
        exit 1
    fi
}

main "$@"
