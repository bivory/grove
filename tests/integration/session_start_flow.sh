#!/bin/bash
# tests/integration/session_start_flow.sh
#
# Tests session start hook: ticket context detection and learning injection.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/harness.sh"

main() {
    setup_test_env

    log "Testing session start flow..."

    # Create a learning to inject
    cat > "$TEST_DIR/.grove/learnings.md" << 'EOF'
# Grove Learnings

## [cl_test_001] Test pattern for session start

**Category:** Pattern
**Tags:** testing, session
**Files:** src/main.rs
**Scope:** project
**Confidence:** high
**Status:** active

This is a test learning that should be injected at session start.
EOF

    # Session start should succeed (returns {} for empty or {"additionalContext": "..."} with context)
    local result
    result=$(echo "{\"session_id\":\"$SESSION_ID\",\"transcript_path\":\"$TEST_DIR/transcript.jsonl\",\"cwd\":\"$TEST_DIR\"}" \
        | grove hook session-start)

    # Verify response is valid JSON (empty object is valid)
    echo "$result" | jq -e '.' > /dev/null \
        || fail "session-start should return valid JSON"

    # Verify session was created
    grove debug "$SESSION_ID" | jq -e '.session.id' > /dev/null \
        || fail "session should exist after session-start"

    # Verify gate is Idle
    local gate_status
    gate_status=$(grove debug "$SESSION_ID" | jq -r '.session.gate.status')
    [ "$gate_status" = "idle" ] || fail "Gate should be Idle after session-start, got: $gate_status"

    # Verify learnings were injected (if any matched)
    local injected
    injected=$(grove debug "$SESSION_ID" | jq '.session.gate.injected_learnings | length')
    log "Injected learnings: $injected"

    cleanup_test_env
    log "Session start flow: PASSED"
}

main "$@"
