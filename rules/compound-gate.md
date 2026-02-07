# Grove Compound Learning Gate Protocol

This rule defines the gate behavior for capturing learnings at ticket
boundaries.

## Gate Overview

Grove enforces structured reflection when completing tickets or significant
work. The gate blocks session exit until you either:

1. **Reflect**: Capture learnings using `/compound-reflect`
2. **Skip**: Explicitly skip with a reason using `/compound-skip`

## When the Gate Activates

The gate transitions to **Pending** (and then **Blocked** on stop) when:

- A ticket is closed (detected via Bash commands like
  `tissue status <id> closed`)
- Session ends with significant changes (in session-based mode)

## Responding to a Blocked Gate

When you see a gate block message:

1. **Review your work**: Consider what you learned during this session
2. **Identify learnings**: Look for patterns, pitfalls, conventions, or
   insights
3. **Capture or skip**: Use the appropriate skill

### Capturing Learnings

Use `/compound-reflect` with candidate learnings. Each learning must:

- Have a clear category (pattern, pitfall, convention, dependency, process,
  domain, debugging)
- Pass the write gate (claim at least one criterion)
- Not duplicate existing learnings

### Skipping Reflection

Use `/compound-skip` when:

- Work was trivial (version bump, config change)
- No new insights were gained
- User explicitly requests it

Always provide a meaningful reason for skipping.

## Write Gate Criteria

Each learning must claim at least one criterion to pass the write gate:

| Criterion | Description | Example |
|-----------|-------------|---------|
| `behavior-changing` | Affects how code behaves | "Use `--locked` with cargo" |
| `decision-rationale` | Explains a decision | "Chose SQLite for simplicity" |
| `stable-fact` | Unlikely to change | "API requires OAuth 2.0 PKCE" |
| `explicit-request` | User asked to remember | "Client prefers tabs" |

## Learning Categories

| Category | When to Use |
|----------|-------------|
| **Pattern** | Reusable code pattern or architectural approach |
| **Pitfall** | Mistake made or gotcha encountered (with fix) |
| **Convention** | Project convention learned or established |
| **Dependency** | Something about a library, API, or external system |
| **Process** | Workflow improvement or development process insight |
| **Domain** | Business logic or domain knowledge |
| **Debugging** | Debugging technique that worked |

## Circuit Breaker

If you're stuck in a block loop (gate keeps blocking despite attempts), the
circuit breaker will eventually allow exit after 3 consecutive blocks. This
prevents infinite blocking but logs a warning.

## Quality Guidelines

- **Be specific**: "Use `tokio::spawn` for CPU-bound work" > "Use async"
- **Include context**: Why is this important? When does it apply?
- **Add tags**: Help future search with relevant keywords
- **Choose scope wisely**: Most learnings should be `project` scope

## Session Start

At session start, grove injects relevant learnings from previous sessions.
These appear in the context and should inform your work. Reference them when
applicable to improve quality scores.
