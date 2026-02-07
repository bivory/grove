# grove — Flaw Resolutions

Resolutions for the three high-severity flaws identified in the architecture
review, plus a bonus resolution for Flaw 2 (ticket close detection) since
the research uncovered a cleaner approach.

## Flaw 1: Session Identity Across Hook Invocations

### The Problem

Grove must correlate state across multiple hook invocations (SessionStart
sets up state, PreToolUse mutates it, Stop reads it). Each invocation is a
separate process. How does grove know which session it's operating on?

### Resolution: Use Claude Code's `session_id` Field

**Every hook event** includes `session_id` (UUID v4) as a common field in
the JSON payload on stdin. This is documented in the Claude Code hooks
reference and confirmed by multiple sources. The common input fields are:

```json
{
  "session_id": "eb5b0174-0555-4601-804e-672d68069c89",
  "transcript_path": "/Users/.../.claude/projects/.../eb5b0174.jsonl",
  "cwd": "/Users/you/my-project",
  "hook_event_name": "Stop",
  "permission_mode": "default"
}
```

Grove's session state file is keyed by this ID:
`~/.grove/sessions/<session_id>.json`

**Flow:**

1. SessionStart hook reads `session_id` from stdin → creates
   `~/.grove/sessions/<session_id>.json` with initial state
2. PostToolUse hook reads `session_id` → loads and mutates that file
3. Stop hook reads `session_id` → loads state, checks gate, responds

**Edge cases:**

| Case | Behavior |
|------|----------|
| `--resume` generates a new session_id | Acceptable. A resumed session is a new grove session. Prior gate state doesn't carry over — if the ticket was already reflected on, the new session starts fresh in Idle. |
| Subagents share parent session_id | Correct for grove's model. SubagentStop reads the same session state to find observations. The gate only fires on Stop (orchestrator), not SubagentStop. |
| Session file missing on Stop | Fail-open: approve exit, log warning. This handles the case where SessionStart hook didn't fire (e.g., grove was installed mid-session). |
| Concurrent writes from parallel hooks | Use atomic write (write to temp file, rename). Multiple hooks for the same event fire in parallel, but hooks for *different* events are sequential (SessionStart completes before PreToolUse fires). Within a single event, grove only reads — it doesn't need to coordinate. |

**Changes to architecture doc:**

- Add `session_id` to section 7 (Hook Behaviors) as the correlation key
- Add the common input schema to section 7 or a new Hook I/O section
- Document the "session file missing" fail-open case in section 12.2

### Status: **Resolved. Low effort.**

---

## Flaw 2: Ticket Close Detection (Bonus — Upgraded from Medium)

### The Problem (Flaw 2)

PreToolUse fires *before* the tool executes. If grove detects a ticket-close
command pattern here, the command might fail — producing a false positive
where grove thinks a ticket closed when it didn't. Pattern matching on
shell commands is also fragile (variables, pipes, compound commands).

### Resolution: Move Detection to PostToolUse

**PostToolUse** fires after the tool completes and includes `tool_response`
with success/failure information:

```json
{
  "hook_event_name": "PostToolUse",
  "tool_name": "Bash",
  "tool_input": {
    "command": "tissue status PROJ-123 closed"
  },
  "tool_response": {
    "stdout": "Ticket PROJ-123 closed",
    "exitCode": 0
  }
}
```

Grove should use PostToolUse (not PreToolUse) for ticket close detection:

1. Match the command pattern (same regex as before)
2. Check `tool_response` for success (exit code 0, or tool-specific success
   signal)
3. Only if both match: transition gate to Pending

This eliminates false positives from failed commands. False negatives from
unrecognized command patterns remain possible, but that's the same as
before and is handled by the session-fallback (Stop hook fires for every
session regardless).

**Pattern matching robustness:** The regex approach is inherently best-effort.
For tissue/beads specifically, grove could also verify by calling the
ticketing system directly (`tissue status <id>` to check actual state).
But this adds latency and complexity. The pragmatic approach: regex on
PostToolUse + success check covers >95% of cases. The session fallback
catches the rest.

**Changes to architecture doc:**

- Section 5.1: Change detection hook from PreToolUse to PostToolUse
- Section 7.2 (Pre-Tool-Use): Remove ticket close detection from this hook
- Add new section 7.2b (Post-Tool-Use): Ticket close detection with
  success verification
- Section 7.3 (Stop): Add note that in session-fallback mode (no ticketing
  system detected), Stop itself is the trigger for Pending

**Hook configuration update:**

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "grove hook post-tool-use"
          }
        ]
      }
    ]
  }
}
```

### Status: **Resolved. Low effort. Strictly better than PreToolUse.**

---

## Flaw 4: Concurrent Write Contention on Shared Files

### The Problem (Flaw 4)

`stats.json` is a shared, committed JSON object. When two developers (or
two parallel Claude sessions) update stats for the same learning, git
cannot merge the changes. `learnings.md` (append-only) is less problematic
since git auto-merges non-overlapping appends, but stat counters are
the real problem.

### Resolution: JSONL Stats with Append-Only Events

Replace `stats.json` (mutable object) with `stats.jsonl` (append-only
event log).

**Current design (problematic):**

```json
{
  "learnings": {
    "L001": { "surfaced": 5, "referenced": 3, "dismissed": 2 }
  }
}
```

Two sessions surface L001 → both read `surfaced: 5` → both write
`surfaced: 6` → merge conflict.

**New design (append-only events):**

```jsonl
{"event":"surfaced","learning_id":"L001","session":"abc","ts":"2026-02-06T10:00:00Z"}
{"event":"surfaced","learning_id":"L001","session":"def","ts":"2026-02-06T10:01:00Z"}
{"event":"referenced","learning_id":"L001","session":"abc","ts":"2026-02-06T11:00:00Z"}
```

Each stat update appends a line. Git auto-merges non-overlapping appends.
Even simultaneous appends to the end of the file merge cleanly in most
cases (git sees both as additions at the end of the file, different content
→ auto-merges without conflict).

**Aggregation:** Stats are computed on read by scanning the event log.
`grove stats` aggregates events into the dashboard view. This is cheap —
even 10,000 events (a very active team) is a few hundred KB, parsed in
milliseconds.

**Periodic compaction (optional):** `grove maintain` can optionally compact
the event log into a summary + truncated log. This is a convenience for
large teams, not a correctness requirement.

**What this changes:**

| Aspect | Before | After |
|--------|--------|-------|
| Format | `stats.json` (mutable) | `stats.jsonl` (append-only) |
| Write pattern | Read-modify-write | Append |
| Merge conflicts | Likely on active teams | Effectively eliminated |
| Read pattern | Direct field access | Scan and aggregate |
| File growth | Bounded (one entry per learning) | Unbounded (one line per event) |
| Compaction | Not needed | Optional via `grove maintain` |

**File growth concern:** A learning surfaced 100 times produces 100 lines.
At ~100 bytes per line, even 100K events is 10MB. This is well within
reason for a committed file. For teams that find it too large, `grove
maintain --compact` reduces to summary form.

**Changes to architecture doc:**

- Section 10.1: Change `stats.json` → `stats.jsonl`
- Add note about append-only event model
- Section on Stats Engine: Describe aggregation-on-read pattern
- `grove maintain`: Add `--compact` flag for optional log compaction

### Status: Resolved

Medium effort (stats engine changes from direct access to aggregation).

---

## Flaw 6: Reflection Quality Validation

### The Problem (Flaw 6)

`grove reflect` receives Claude's reflection output and writes it. But it
doesn't validate structure (are required fields present? are categories
valid?) separately from quality (does it pass the write gate?). Claude
could produce malformed output and grove would either crash or write
garbage.

### Resolution: Two-Stage Validation (Structural + Quality)

Separate validation into two stages that run sequentially:

### Stage 1: Structural Validation (Schema Check)

Before applying the write gate, validate that the reflection output
conforms to the expected schema:

**Required structure per candidate learning:**

| Field | Type | Required | Validation |
|-------|------|----------|------------|
| `category` | enum | yes | Must be one of: pattern, pitfall, convention, dependency, process, domain, debugging |
| `summary` | string | yes | Non-empty, ≤ 200 characters |
| `detail` | string | yes | Non-empty, ≤ 2000 characters |
| `scope` | enum | yes | Must be one of: project, team, personal, ephemeral |
| `confidence` | enum | yes | Must be one of: high, medium, low |
| `tags` | string[] | no | Each tag ≤ 50 characters, max 10 tags |
| `context_files` | string[] | no | Each must be a valid relative path |

**Structural failures:**

- Missing required field → reject candidate with reason, continue
  processing others
- Invalid enum value → reject candidate with reason
- All candidates fail structural validation → log error, mark session
  as reflected (fail-open: don't re-block), emit warning

**Key principle:** Structural validation never blocks the session exit.
If reflection output is garbage, grove logs the error and lets the
developer leave. The gate has served its purpose by forcing the attempt.

### Stage 2: Quality Validation (Write Gate)

Structurally valid candidates proceed to the existing write gate filter
(behavior-changing, decision, stable fact, explicit request). This is
unchanged from the current design.

### What Does `grove reflect` Actually Do?

This resolution clarifies the boundary between Claude and grove:

```text
┌─────────────────────────────────────────────────────┐
│ Claude (agent)                                      │
│                                                     │
│  1. Reads reflection skill prompt                   │
│  2. Analyzes session: what happened, what learned   │
│  3. Produces structured JSON output                 │
│  4. Calls: grove reflect --input '<json>'           │
└──────────────────────┬──────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────┐
│ grove reflect (binary)                              │
│                                                     │
│  1. Parse JSON input                                │
│  2. Stage 1: Structural validation                  │
│     - Check required fields, enum values, lengths   │
│     - Reject malformed candidates (log reason)      │
│  3. Stage 2: Write gate filter                      │
│     - Apply four criteria                           │
│     - Reject low-quality candidates (log reason)    │
│  4. Write accepted learnings to backend             │
│  5. Update stats (surfaced→referenced for any       │
│     injected learnings that were cited)              │
│  6. Mark session gate as Reflected                  │
│  7. Output summary to stdout (JSON)                 │
│                                                     │
│  Exit 0 always (fail-open). Errors → stderr.        │
└─────────────────────────────────────────────────────┘
```

**The write gate is applied by grove, not Claude.** Claude produces
candidates. Grove filters them. This prevents Claude from gaming the
gate (intentionally or through prompt drift) and ensures consistent
quality filtering regardless of which model or prompt version is running.

### Handling Unparsable Output

If Claude's output isn't valid JSON at all:

1. Log the raw output to the session trace (for debugging)
2. Mark the session as Reflected (fail-open)
3. Record a stat event: `{"event":"parse_failure","session":"abc"}`
4. Emit warning to stderr: "Reflection output was not valid JSON.
   Session marked as reflected. No learnings captured."

This is strictly better than crashing or re-blocking. The developer
isn't punished for Claude's output quality, but the parse failure is
tracked so `grove stats` can surface it ("3 of your last 10 reflections
failed to parse — consider updating the reflection skill").

**Changes to architecture doc:**

- Add new section 6.5: Structural Validation (before the write gate
  section)
- Clarify section 6 (Write Gate) as Stage 2, running after structural
  validation
- Add parse failure to section 12.2 (Fail-Open Behaviors)
- Add parse_failure event type to stats model

### Status: **Resolved. Medium effort (schema validation + parse error handling).**

---

## Summary of Changes

| Flaw | Resolution | Architecture Sections Affected |
|------|-----------|-------------------------------|
| #1 Session identity | Use `session_id` from hook payload | 7 (Hook Behaviors), 12.2 (Fail-Open) |
| #2 Ticket close detection | Move to PostToolUse + success check | 5.1, 7.2, new 7.2b, 7.3 |
| #4 Write contention | JSONL append-only events | 10.1, Stats Engine, `grove maintain` |
| #6 Reflection validation | Two-stage: structural + quality | New 6.5, 6, 12.2, Stats model |

## Remaining Medium-Severity Flaws (Not Yet Resolved)

These should be addressed before implementation but are not blockers:

- **Flaw 5: Stop hook diff size** — Grove must shell out to `git diff
  --stat` in the Stop hook. Specify this explicitly, handle non-git repos
  (skip auto-skip, fall back to agent-decides-always).
- **Flaw 9: Retrieval underspecified** — Define search semantics for the
  markdown backend (tag match + keyword grep on summary/detail).
- **Flaw 10: Scope routing** — Core logic owns routing; backends just
  write. Personal scope → local-only backend. Ephemeral → session log.
  Project/team → configured backend.
