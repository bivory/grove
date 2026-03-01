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

    # =========================================================================
    # Test: Blocking gate context injection
    # When session-start is called on a session with Pending/Blocked gate,
    # it should inject context about the blocking state.
    # =========================================================================

    log "Testing blocking gate context injection..."

    # Create a new session for blocking test (use non-local vars like gate_block_flow.sh)
    BLOCKING_SESSION_ID="blocking-test-$$"
    BLOCKING_TRANSCRIPT="$TEST_DIR/transcript.jsonl"

    # 1. Session start (creates Idle session)
    log "Step 1: Creating blocking test session"
    echo "{\"session_id\":\"$BLOCKING_SESSION_ID\",\"transcript_path\":\"$BLOCKING_TRANSCRIPT\",\"cwd\":\"$TEST_DIR\"}" \
        | grove hook session-start > /dev/null

    # 2. Pre-tool-use: close a ticket
    log "Step 2: Pre-tool-use (ticket close)"
    cat << EOF | grove hook pre-tool-use > /dev/null
{
  "session_id": "$BLOCKING_SESSION_ID",
  "transcript_path": "$BLOCKING_TRANSCRIPT",
  "cwd": "$TEST_DIR",
  "tool_name": "Bash",
  "tool_input": {"command": "tissue status issue-999 closed"}
}
EOF

    # 3. Post-tool-use: confirm ticket close (transitions to Pending)
    log "Step 3: Post-tool-use (confirm ticket close)"
    cat << EOF | grove hook post-tool-use > /dev/null
{
  "session_id": "$BLOCKING_SESSION_ID",
  "transcript_path": "$BLOCKING_TRANSCRIPT",
  "cwd": "$TEST_DIR",
  "tool_name": "Bash",
  "tool_input": {"command": "tissue status issue-999 closed"},
  "tool_response": "issue-999"
}
EOF

    # Verify gate is Pending
    gate_status=$(grove debug "$BLOCKING_SESSION_ID" | jq -r '.session.gate.status')
    [ "$gate_status" = "pending" ] || fail "Gate should be Pending after ticket close, got: $gate_status"

    # 3. Call session-start again (simulates subagent starting)
    log "Step 3: Session-start again (subagent scenario)"
    subagent_result=$(echo "{\"session_id\":\"$BLOCKING_SESSION_ID\",\"transcript_path\":\"$BLOCKING_TRANSCRIPT\",\"cwd\":\"$TEST_DIR\"}" \
        | grove hook session-start)

    # 4. Verify additionalContext contains gate notice
    additional_context=$(echo "$subagent_result" | jq -r '.additionalContext // ""')

    # Check for gate notice header
    echo "$additional_context" | grep -q "Grove Gate Active" \
        || fail "Session-start should inject gate notice header"

    # Check for status
    echo "$additional_context" | grep -q "Pending" \
        || fail "Session-start should show Pending status"

    # Check for ticket info
    echo "$additional_context" | grep -q "issue-999" \
        || fail "Session-start should show ticket ID"

    # Check for resolution commands with session ID
    echo "$additional_context" | grep -q "grove reflect" \
        || fail "Session-start should include grove reflect command"
    echo "$additional_context" | grep -q "grove skip" \
        || fail "Session-start should include grove skip command"
    echo "$additional_context" | grep -q "$BLOCKING_SESSION_ID" \
        || fail "Session-start should include session ID in commands"

    log "Blocking gate context injection: PASSED"

    # =========================================================================
    # Test: No blocking context for terminal states
    # =========================================================================

    log "Testing no blocking context for terminal states..."

    # Skip the session (transitions to Skipped - terminal state)
    grove skip "test completed" --session-id "$BLOCKING_SESSION_ID" > /dev/null 2>&1

    # Verify gate is Skipped
    gate_status=$(grove debug "$BLOCKING_SESSION_ID" | jq -r '.session.gate.status')
    [ "$gate_status" = "skipped" ] || fail "Gate should be Skipped, got: $gate_status"

    # Call session-start again
    terminal_result=$(echo "{\"session_id\":\"$BLOCKING_SESSION_ID\",\"transcript_path\":\"$BLOCKING_TRANSCRIPT\",\"cwd\":\"$TEST_DIR\"}" \
        | grove hook session-start)

    # Verify NO gate notice is injected for terminal state
    terminal_context=$(echo "$terminal_result" | jq -r '.additionalContext // ""')

    if echo "$terminal_context" | grep -q "Grove Gate Active"; then
        fail "Session-start should NOT inject gate notice for terminal (Skipped) state"
    fi

    log "No blocking context for terminal states: PASSED"

    cleanup_test_env
    log "Session start flow: PASSED"
}

main "$@"
