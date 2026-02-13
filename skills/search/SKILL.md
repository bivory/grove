# search

Search for relevant learnings from the grove memory backend.

## When to Use

Use this skill when:

- Starting work on a task and want relevant context
- Looking for past learnings about a specific topic
- Checking if something was already documented

## How to Invoke

```bash
grove search "your search query"
```

### Options

- `--category <cat>`: Filter by category (pattern, pitfall, convention, etc.)
- `--tag <tag>`: Filter by tag (can be repeated)
- `--limit <n>`: Maximum results (default: 10)
- `--json`: Output as JSON
- `--quiet`: Suppress output

## Examples

```bash
# Search for error handling patterns
grove search "error handling"

# Search for pitfalls related to async
grove search "async" --category pitfall

# Search with multiple tags
grove search "database" --tag postgres --tag migration

# Get JSON output for programmatic use
grove search "authentication" --json
```

## Output

Human-readable format shows:

- Learning ID
- Category and summary
- Relevance score
- Key details

JSON format includes full learning metadata for integration.

## Notes

- Search uses fuzzy matching on summary, detail, and tags
- Results are ranked by relevance score
- Categories and tags can narrow results significantly
