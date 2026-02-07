# compound-skip

Skip reflection when no meaningful learnings were captured during a session.

## When to Use

Use this skill when:

- The work was trivial (small config change, version bump)
- Changes were mechanical with no new insights
- The user explicitly requests to skip reflection
- Auto-skip threshold was triggered (very small diffs)

## How to Invoke

```bash
grove skip "reason for skipping" --session SESSION_ID
```

### Required Arguments

- **reason**: Brief explanation of why reflection is being skipped
- **--session**: Session ID (required)

### Options

- `--decider <who>`: Who decided to skip: `agent`, `user`, or `auto`
  (default: `agent`)
- `--lines-changed <n>`: Number of lines changed (for auto-skip decisions)
- `--json`: Output as JSON
- `--quiet`: Suppress output

## Examples

```bash
# Agent decides to skip for trivial change
grove skip "version bump only, no new patterns" --session abc123

# User explicitly requested skip
grove skip "user requested quick exit" --session abc123 --decider user

# Auto-skip due to small diff
grove skip "auto: 3 lines changed, under threshold" \
  --session abc123 --decider auto --lines-changed 3
```

## Output

Returns confirmation that skip was recorded:

- `success`: Whether skip was recorded
- `session_id`: The session that was skipped
- `reason`: The skip reason
- `decider`: Who made the decision

## Notes

- Skipping transitions the gate to `Skipped` state
- Skip decisions are logged for quality tracking
- Excessive skipping may affect quality metrics
- User-requested skips are always honored
