#!/bin/bash
# tests/integration/circuit_breaker_flow.sh
#
# Tests the circuit breaker: multiple blocks -> forced approve

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/harness.sh"

main() {
    setup_test_env

    log "Testing circuit breaker flow..."

    # Common input fields for this session
    TRANSCRIPT="$TEST_DIR/transcript.jsonl"

    # 1. Session start
    log "Step 1: Session start"
    echo "{\"session_id\":\"$SESSION_ID\",\"transcript_path\":\"$TRANSCRIPT\",\"cwd\":\"$TEST_DIR\"}" \
        | grove hook session-start > /dev/null

    # 2. Force gate to Pending state (simulates ticket close)
    log "Step 2: Force gate to Pending"
    grove debug "$SESSION_ID" --set-gate pending > /dev/null

    # Verify gate is Pending
    local gate_status
    gate_status=$(grove debug "$SESSION_ID" | jq -r '.session.gate.status')
    [ "$gate_status" = "pending" ] || fail "Gate should be Pending, got: $gate_status"

    # 3. Block (max_blocks - 1) times before circuit breaker triggers
    # Stop hook returns exit code 2 when blocking
    # max_blocks=3 means: block 1, block 2, then 3rd attempt triggers circuit breaker
    log "Step 3: Block 2 times"
    for i in {1..2}; do
        log "  Block attempt $i"
        local stop_result
        local stop_exit_code=0
        stop_result=$(echo "{\"session_id\":\"$SESSION_ID\",\"transcript_path\":\"$TRANSCRIPT\",\"cwd\":\"$TEST_DIR\"}" | grove hook stop) || stop_exit_code=$?
        [ "$stop_exit_code" -eq 2 ] || fail "Stop $i should return exit code 2, got: $stop_exit_code"

        echo "$stop_result" | jq -e '.decision == "block"' > /dev/null \
            || fail "Stop $i should return block decision"

        # Verify block count
        local block_count
        block_count=$(grove debug "$SESSION_ID" | jq '.session.gate.block_count')
        [ "$block_count" -eq "$i" ] || fail "Block count should be $i, got: $block_count"
    done

    # 4. Third stop should trigger circuit breaker (block_count would be 3 = max_blocks)
    log "Step 4: Circuit breaker trigger (3rd attempt)"
    local stop_result
    local stop_exit_code=0
    stop_result=$(echo "{\"session_id\":\"$SESSION_ID\",\"transcript_path\":\"$TRANSCRIPT\",\"cwd\":\"$TEST_DIR\"}" | grove hook stop) || stop_exit_code=$?
    [ "$stop_exit_code" -eq 0 ] || fail "Circuit breaker should return exit code 0, got: $stop_exit_code"

    echo "$stop_result" | jq -e '.decision == "approve"' > /dev/null \
        || fail "Circuit breaker should approve"

    # Check for forced flag or circuit_breaker_tripped
    local tripped
    tripped=$(grove debug "$SESSION_ID" | jq '.session.gate.circuit_breaker_tripped')
    [ "$tripped" = "true" ] || fail "Circuit breaker should be tripped"

    cleanup_test_env
    log "Circuit breaker flow: PASSED"
}

main "$@"
