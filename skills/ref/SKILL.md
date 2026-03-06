# ref

Record that surfaced learnings were referenced (used) during a session.

## When to Use

Use this skill when:

- A learning surfaced at session start helped your work
- You applied advice, followed a pattern, or avoided a pitfall from a learning
- You want to strengthen the scoring signal for a useful learning

## How to Invoke

```bash
grove ref <ID> [<ID> ...] --session-id SESSION_ID
```

### Required Arguments

- **learning_ids**: One or more learning IDs (positional, space-separated)
- **--session-id**: Session ID (required)

### Options

- `--how <description>`: Optional context on how the learning was used
- `--json`: Output as JSON
- `--quiet`: Suppress output

## Examples

```bash
# Single learning referenced
grove ref cl_001 --session-id abc123

# Multiple learnings referenced
grove ref cl_001 cl_002 cl_003 --session-id abc123

# With context on usage
grove ref cl_001 --session-id abc123 --how "followed the auth ordering pattern"
```

## Output

Returns confirmation that reference was recorded:

- `success`: Whether the reference was recorded
- `referenced_count`: Number of learnings referenced
- `learning_ids`: The IDs that were referenced

## Notes

- Referencing updates the scoring feedback loop (reference_boost)
- Learnings with higher reference rates get surfaced more often
- References are logged as stats events for quality tracking
- The session-start injection shows the session ID and this command
