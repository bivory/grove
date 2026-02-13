# reflect

Captures learnings from the current session after completing a ticket or
significant work.

## When to Use

Use this skill when:

- The grove gate has blocked exit and requires reflection
- You've completed a ticket and want to capture learnings
- You've discovered something worth remembering for future sessions

## How to Invoke

Run `grove reflect` with JSON input via stdin:

```bash
echo '{"session_id": "SESSION_ID", "candidates": [...]}' | grove reflect
```

## Input Format

The JSON input must include:

```json
{
  "session_id": "abc123",
  "candidates": [
    {
      "category": "pattern",
      "summary": "Brief description (one line)",
      "detail": "Detailed explanation with context",
      "scope": "project",
      "confidence": "high",
      "criteria_met": ["behavior-changing"],
      "tags": ["rust", "error-handling"],
      "context_files": ["src/lib.rs"]
    }
  ]
}
```

### Required Fields

- **category**: One of: `pattern`, `pitfall`, `convention`, `dependency`,
  `process`, `domain`, `debugging`
- **summary**: Brief one-line description
- **detail**: Detailed explanation with enough context for future use
- **criteria_met**: At least one of:
  - `behavior-changing`: Affects how code behaves
  - `decision-rationale`: Explains why a decision was made
  - `stable-fact`: A fact unlikely to change
  - `explicit-request`: User explicitly asked to remember this

### Optional Fields

- **scope**: `project` (default), `team`, `personal`, or `ephemeral`
- **confidence**: `high`, `medium` (default), or `low`
- **tags**: Categorization tags for search
- **context_files**: Files relevant to this learning

## Output

Returns JSON with:

- `success`: Whether reflection was accepted
- `candidates_submitted`: Number of candidates provided
- `learnings_accepted`: Number written to backend
- `learning_ids`: IDs of accepted learnings
- `rejected`: Array of rejected candidates with reasons

## Notes

- Each candidate is validated against schema and write gate
- Learnings similar to existing ones may be rejected as duplicates
- The gate state is updated after successful reflection
