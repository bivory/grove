#!/bin/bash
# tests/integration/gate_block_flow.sh
#
# Tests the full gate lifecycle: detect -> close -> block -> reflect -> approve

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/harness.sh"

main() {
    setup_test_env

    log "Testing gate block flow..."

    # Common input fields for this session
    TRANSCRIPT="$TEST_DIR/transcript.jsonl"

    # 1. Session start
    log "Step 1: Session start"
    echo "{\"session_id\":\"$SESSION_ID\",\"transcript_path\":\"$TRANSCRIPT\",\"cwd\":\"$TEST_DIR\"}" \
        | grove hook session-start > /dev/null

    # Verify gate is Idle
    local gate_status
    gate_status=$(grove debug "$SESSION_ID" | jq -r '.session.gate.status')
    [ "$gate_status" = "idle" ] || fail "Gate should be Idle after session-start, got: $gate_status"

    # 2. Pre-tool-use: ticket close detection
    log "Step 2: Pre-tool-use (ticket close detection)"
    cat << EOF | grove hook pre-tool-use > /dev/null
{
  "session_id": "$SESSION_ID",
  "transcript_path": "$TRANSCRIPT",
  "cwd": "$TEST_DIR",
  "tool_name": "Bash",
  "tool_input": {"command": "tissue status issue-123 closed"}
}
EOF

    # 3. Post-tool-use: ticket close confirmed
    log "Step 3: Post-tool-use (ticket close confirmed)"
    cat << EOF | grove hook post-tool-use > /dev/null
{
  "session_id": "$SESSION_ID",
  "transcript_path": "$TRANSCRIPT",
  "cwd": "$TEST_DIR",
  "tool_name": "Bash",
  "tool_input": {"command": "tissue status issue-123 closed"},
  "tool_response": "issue-123"
}
EOF

    # Verify gate transitioned to Pending
    gate_status=$(grove debug "$SESSION_ID" | jq -r '.session.gate.status')
    [ "$gate_status" = "pending" ] || fail "Gate should be Pending after ticket close, got: $gate_status"

    # 4. Stop hook should block (exit code 2 = block signal to Claude Code)
    log "Step 4: Stop hook (should block)"
    local stop_result
    local stop_exit_code=0
    stop_result=$(echo "{\"session_id\":\"$SESSION_ID\",\"transcript_path\":\"$TRANSCRIPT\",\"cwd\":\"$TEST_DIR\"}" | grove hook stop) || stop_exit_code=$?
    [ "$stop_exit_code" -eq 2 ] || fail "Stop hook should return exit code 2 when blocking, got: $stop_exit_code"
    echo "$stop_result" | jq -e '.decision == "block"' > /dev/null \
        || fail "Stop should return block decision"

    # Verify gate transitioned to Blocked
    gate_status=$(grove debug "$SESSION_ID" | jq -r '.session.gate.status')
    [ "$gate_status" = "blocked" ] || fail "Gate should be Blocked after stop, got: $gate_status"

    # 5. Run reflection
    log "Step 5: Reflect"
    cat << EOF | grove reflect
{
  "session_id": "$SESSION_ID",
  "candidates": [
    {
      "category": "pitfall",
      "summary": "Always verify ticket exists before closing",
      "detail": "The tissue CLI returns success even if the issue ID doesn't exist.",
      "criteria_met": ["behavior_changing"],
      "tags": ["tissue", "cli"],
      "scope": "project",
      "confidence": "high"
    }
  ]
}
EOF

    # Verify gate transitioned to Reflected
    gate_status=$(grove debug "$SESSION_ID" | jq -r '.session.gate.status')
    [ "$gate_status" = "reflected" ] || fail "Gate should be Reflected after reflection, got: $gate_status"

    # 6. Stop hook should now approve (exit code 0)
    log "Step 6: Stop hook (should approve)"
    stop_exit_code=0
    stop_result=$(echo "{\"session_id\":\"$SESSION_ID\",\"transcript_path\":\"$TRANSCRIPT\",\"cwd\":\"$TEST_DIR\"}" | grove hook stop) || stop_exit_code=$?
    [ "$stop_exit_code" -eq 0 ] || fail "Stop hook should return exit code 0 after reflection, got: $stop_exit_code"
    echo "$stop_result" | jq -e '.decision == "approve"' > /dev/null \
        || fail "Stop should return approve decision after reflection"

    # 7. Verify learning was written
    log "Step 7: Verify learning persisted"
    local learning_count
    learning_count=$(grove list --json 2>/dev/null | jq 'length')
    [ "$learning_count" -ge 1 ] || fail "At least one learning should be persisted"

    cleanup_test_env
    log "Gate block flow: PASSED"
}

main "$@"
