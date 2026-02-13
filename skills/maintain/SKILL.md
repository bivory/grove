# maintain

Manage and maintain the grove learning database.

## When to Use

Use this skill when:

- Cleaning up old or irrelevant learnings
- Archiving stale learnings
- Restoring previously archived learnings
- Reviewing the learning database

## How to Invoke

```bash
grove maintain <subcommand>
```

### Subcommands

- `list`: Show learnings that need attention
- `archive <id>`: Archive a specific learning
- `restore <id>`: Restore an archived learning
- `dedupe`: Find and merge duplicate learnings

### Options

- `--json`: Output as JSON
- `--quiet`: Suppress output
- `--dry-run`: Show what would happen without making changes

## Examples

```bash
# List learnings that may need archival
grove maintain list

# Archive a stale learning
grove maintain archive learning-123

# Restore an archived learning
grove maintain restore learning-456

# Check for duplicates
grove maintain dedupe --dry-run
```

## Output

### list

Shows learnings sorted by:

- Days since last reference
- Decay status (approaching archival threshold)
- Category and summary

### archive/restore

Confirms the action with:

- Learning ID
- Previous and new status
- Reason for change

### dedupe

Shows potential duplicates with:

- Similarity score
- Both learning summaries
- Recommendation (merge, keep both, etc.)

## Notes

- Archiving doesn't delete learnings; they can be restored
- Passive decay archival happens automatically based on reference patterns
- Deduplication uses semantic similarity, not exact matching
- Use `--dry-run` before making bulk changes
