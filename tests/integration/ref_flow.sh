#!/bin/bash
# tests/integration/ref_flow.sh
#
# Tests the grove ref command: session-start injects learnings, grove ref
# records a reference, and the stats log contains the referenced event.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/harness.sh"

main() {
    setup_test_env

    log "Testing ref flow..."

    TRANSCRIPT="$TEST_DIR/transcript.jsonl"

    # Create a learning that can be injected
    cat > "$TEST_DIR/.grove/learnings.md" << 'LEARNING'
# Grove Learnings

---
## cl_ref_test_001

**Category:** Pattern
**Summary:** Always check return codes from tissue CLI
**Scope:** Project | **Confidence:** High | **Status:** Active
**Tags:** #testing #tissue
**Session:** old-session
**Criteria:** Behavior Changing
**Created:** 2026-01-01T00:00:00Z

The tissue CLI can return success even on partial failures.

---
LEARNING

    # 1. Session start (injects learnings)
    log "Step 1: Session start"
    local start_result
    start_result=$(echo "{\"session_id\":\"$SESSION_ID\",\"transcript_path\":\"$TRANSCRIPT\",\"cwd\":\"$TEST_DIR\"}" \
        | grove hook session-start)

    # Verify learning was injected
    local has_context
    has_context=$(echo "$start_result" | jq -r '.additionalContext // empty')
    if [ -z "$has_context" ]; then
        warn "No additional context (learning may not have been injected - this is OK if scoring filtered it out)"
    else
        log "Learning injection context present"
        # Verify grove ref command is in the citation guidance
        echo "$has_context" | grep -q "grove ref" \
            || fail "Citation guidance should contain 'grove ref'"
        echo "$has_context" | grep -q "$SESSION_ID" \
            || fail "Citation guidance should contain session ID"
    fi

    # 2. Run grove ref
    log "Step 2: grove ref"
    local ref_result
    ref_result=$(grove ref cl_ref_test_001 --session-id "$SESSION_ID" --json)

    # Verify ref succeeded
    local ref_success
    ref_success=$(echo "$ref_result" | jq -r '.success')
    [ "$ref_success" = "true" ] || fail "grove ref should succeed, got: $ref_result"

    local ref_count
    ref_count=$(echo "$ref_result" | jq -r '.referenced_count')
    [ "$ref_count" = "1" ] || fail "referenced_count should be 1, got: $ref_count"

    # 3. Verify stats log has referenced event
    log "Step 3: Verify stats log"
    local stats_log="$TEST_DIR/.grove/stats.log"
    if [ -f "$stats_log" ]; then
        grep -q '"referenced"' "$stats_log" \
            || fail "Stats log should contain a referenced event"
        grep -q 'cl_ref_test_001' "$stats_log" \
            || fail "Stats log should contain the learning ID"
    else
        warn "No stats log found (fail-open OK)"
    fi

    # 4. Verify trace event via debug
    log "Step 4: Verify trace event"
    local debug_output
    debug_output=$(grove debug "$SESSION_ID")
    echo "$debug_output" | jq -e '.session.trace[] | select(.event_type == "learning_referenced")' > /dev/null \
        || fail "Session trace should contain a learning_referenced event"

    # 5. Test multiple IDs
    log "Step 5: grove ref with multiple IDs"
    ref_result=$(grove ref cl_ref_test_001 cl_ref_test_002 --session-id "$SESSION_ID" --json)
    ref_count=$(echo "$ref_result" | jq -r '.referenced_count')
    [ "$ref_count" = "2" ] || fail "referenced_count should be 2 for multiple IDs, got: $ref_count"

    # 6. Test empty IDs rejected
    log "Step 6: grove ref with no IDs"
    local empty_exit=0
    grove ref --session-id "$SESSION_ID" --json 2>/dev/null || empty_exit=$?
    [ "$empty_exit" -ne 0 ] || fail "grove ref with no IDs should fail"

    # 7. Test --how flag
    log "Step 7: grove ref with --how"
    ref_result=$(grove ref cl_ref_test_001 --session-id "$SESSION_ID" --how "avoided the pitfall" --json)
    ref_success=$(echo "$ref_result" | jq -r '.success')
    [ "$ref_success" = "true" ] || fail "grove ref with --how should succeed"

    cleanup_test_env
    log "Ref flow: PASSED"
}

main "$@"
