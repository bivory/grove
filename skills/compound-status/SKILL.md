# compound-status

Show grove status including stats, backends, and gate state.

## When to Use

Use this skill when:

- Checking the current grove configuration
- Reviewing learning statistics
- Diagnosing grove issues
- Understanding what backends are available

## How to Invoke

This skill combines two commands:

```bash
# Show statistics
grove stats

# Show discovered backends
grove backends
```

### Stats Options

- `--json`: Output as JSON
- `--quiet`: Suppress output

### Backends Options

- `--json`: Output as JSON
- `--quiet`: Suppress output

## Examples

```bash
# Show human-readable stats
grove stats

# Show backend status
grove backends

# Get combined JSON status
grove stats --json && grove backends --json
```

## Stats Output

Shows:

- Total learnings captured
- Breakdown by category
- Hit rate (learnings referenced vs injected)
- Skip rate and reasons
- Recent insights and recommendations

## Backends Output

Shows:

- Discovered backends (markdown, Total Recall, MCP)
- Which backend is active (primary)
- Backend health status
- File paths or endpoints

## Notes

- Stats are derived from the append-only event log
- Backend discovery happens at session start
- Use `--json` for integration with other tools
