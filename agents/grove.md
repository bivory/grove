# Grove Agent

Grove is a compound learning gate for Claude Code that enforces structured
reflection at ticket boundaries with pluggable memory backends.

## How Grove Works

Grove captures learnings when you complete tickets and injects relevant context
at session start. The gate mechanically enforces reflection by blocking session
exit until you either capture learnings or explicitly skip.

## When the Gate Activates

The gate activates when you close a ticket via a supported ticketing system
(tissue, beads, or similar). Once activated, you must either:

1. Run structured reflection with `/reflect`
2. Skip reflection with a reason using `/skip`

## Available Skills

### /reflect

Run when you've completed meaningful work and want to capture learnings.
Provide structured reflection output following this format:

```markdown
## Learnings

### Pattern: [Summary]
[Detailed explanation of what you learned]

Category: Pattern | Pitfall | Convention | Dependency | Process | Domain | Debugging
Scope: project | team | personal | ephemeral
Confidence: high | medium | low
Tags: tag1, tag2
Files: file1.rs, file2.rs
```

### /skip

Run when the work was trivial and doesn't warrant reflection:

```text
/skip "typo fix"
/skip "version bump only"
```

### /observe

For subagents: Log observations without triggering the gate:

```text
/observe "auth middleware ordering matters for CORS"
```

### /search

Search past learnings:

```text
/search "authentication patterns"
/search "N+1 query"
```

### /maintain

Review and manage stale learnings:

```text
/maintain          # Show learnings approaching decay
/maintain archive  # Archive stale learnings
/maintain restore  # Restore archived learnings
```

### /status

Check Grove status including gate state, backends, and quality stats.

## Learning Categories

- **Pattern**: Reusable approaches that worked well
- **Pitfall**: Mistakes to avoid, things that didn't work
- **Convention**: Coding standards, naming conventions
- **Dependency**: Library quirks, version constraints
- **Process**: Workflow improvements, tooling discoveries
- **Domain**: Business logic insights, domain knowledge
- **Debugging**: Diagnostic techniques, common error causes

## Gate Protocol

1. Work normally on your ticket
2. When closing the ticket, the gate activates
3. Before exiting the session, run `/reflect` or `/skip`
4. The gate releases and you can exit

If you try to exit without reflecting, Grove will block with instructions.
After 3 blocks, the circuit breaker allows exit with a warning.

## When Blocked by Grove

When you see a message like:

```text
Reflection required. Run `grove reflect --session-id <ID>` or `grove skip <reason> --session-id <ID>`
```

You must respond with one of these CLI commands using the provided session ID:

### Capture Learnings

```bash
grove reflect --session-id <SESSION_ID> <<'EOF'
{
  "session_id": "<SESSION_ID>",
  "candidates": [
    {
      "category": "Pattern",
      "summary": "Brief description of the learning",
      "detail": "Detailed explanation that would help future sessions",
      "scope": "project",
      "ticket_id": "TICKET-123",
      "claims": ["behavior-changing"]
    }
  ]
}
EOF
```

Required claims (at least one must be true):

- `behavior-changing` - Changes how I work in the future
- `decision-rationale` - Records why a decision was made
- `stable-fact` - A fact that won't change frequently
- `explicit-request` - User explicitly asked to remember this

### Skip Reflection

```bash
grove skip "No significant learnings" --session-id <SESSION_ID>
```

## Debugging

```bash
grove sessions              # List recent sessions
grove trace <SESSION_ID>    # View session events
grove debug                 # Show current gate state
```
