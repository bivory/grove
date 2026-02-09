#!/bin/bash
# tests/integration/harness.sh
#
# Common test harness functions for hook simulation tests.

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Test state
TEST_DIR=""
SESSION_ID=""
ORIGINAL_DIR=""

log() {
    echo -e "${GREEN}[test]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[test]${NC} $1"
}

fail() {
    echo -e "${RED}[FAIL]${NC} $1" >&2
    cleanup_test_env
    exit 1
}

# Set up isolated test environment
setup_test_env() {
    ORIGINAL_DIR="$(pwd)"
    TEST_DIR="$(mktemp -d)"
    SESSION_ID="test-$$-$(date +%s)"

    # Export for grove
    export GROVE_HOME="$TEST_DIR/.grove"

    # Create test project structure
    mkdir -p "$TEST_DIR/.grove"
    mkdir -p "$TEST_DIR/.grove/sessions"
    mkdir -p "$TEST_DIR/.tissue"
    touch "$TEST_DIR/.grove/learnings.md"

    # Create minimal config
    cat > "$TEST_DIR/.grove/config.toml" << 'EOF'
[gate]
max_blocks = 3
cooldown_seconds = 300

[decay]
passive_duration_days = 90

[write_gate]
enabled = true
EOF

    cd "$TEST_DIR"
    log "Test environment: $TEST_DIR"
    log "Session ID: $SESSION_ID"
}

# Clean up test environment
cleanup_test_env() {
    if [ -n "$ORIGINAL_DIR" ]; then
        cd "$ORIGINAL_DIR"
    fi
    if [ -n "$TEST_DIR" ] && [ -d "$TEST_DIR" ]; then
        rm -rf "$TEST_DIR"
    fi
}

# Trap cleanup on exit
trap cleanup_test_env EXIT

# Verify grove binary is available
check_grove() {
    if ! command -v grove &> /dev/null; then
        # Try cargo build location
        if [ -f "$ORIGINAL_DIR/target/debug/grove" ]; then
            export PATH="$ORIGINAL_DIR/target/debug:$PATH"
        elif [ -f "$ORIGINAL_DIR/target/release/grove" ]; then
            export PATH="$ORIGINAL_DIR/target/release:$PATH"
        else
            fail "grove binary not found. Run 'cargo build' first."
        fi
    fi
}

# Initialize check
check_grove
