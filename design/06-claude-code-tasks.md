# Grove - Claude Code Tasks Integration

This document describes how Grove integrates with Claude Code's task system
as a ticketing system.

## Background

Grove supports multiple ticketing systems for tracking work boundaries:

- **tissue**: Filesystem marker (`.tissue/` directory)
- **beads**: Filesystem marker (`.beads/` directory)
- **tasks**: Claude Code's built-in task system
- **session**: Fallback (per-session granularity)

This document covers the Claude Code tasks integration.

## Research Findings

### Claude Code Task System

Claude Code has a built-in task system with these characteristics:

1. **User Interface**: Tasks visible via `/tasks` command and `Ctrl+T` toggle
2. **Status Tracking**: Tasks have pending/in-progress/complete states
3. **Persistence**: Tasks can persist across sessions via `CLAUDE_CODE_TASK_LIST_ID`
4. **Storage**: Tasks stored in `~/.claude/tasks/` (format undocumented)

### Available Hooks

Claude Code provides a `TaskCompleted` hook that fires when tasks are marked
complete:

```json
{
  "session_id": "abc123",
  "transcript_path": "/Users/.../.claude/projects/.../transcript.jsonl",
  "cwd": "/Users/...",
  "hook_event_name": "TaskCompleted",
  "task_id": "task-001",
  "task_subject": "Implement user authentication",
  "task_description": "Add login and signup endpoints",
  "teammate_name": "implementer",  // Optional, for agent teams
  "team_name": "my-project"        // Optional, for agent teams
}
```

**Hook Behavior:**

- Fires when any agent marks a task as completed via TaskUpdate
- Exit code 2 blocks task completion (enforces quality gates)
- Stderr message is fed back to Claude as feedback

### Limitations

1. **No Filesystem Marker**: Unlike tissue/beads, tasks have no filesystem
   indicator at session start
2. **Storage Format Undocumented**: `~/.claude/tasks/` directory format is
   not publicly documented
3. **No Query API**: No documented way to query active tasks at session start

## Implementation Design

### Detection Strategy

Since there's no filesystem marker, tasks mode uses config-based opt-in:

```rust
pub fn probe_tasks(cwd: &Path, config: Option<&Config>) -> Option<TicketingInfo> {
    if let Some(config) = config {
        if config.ticketing.overrides.get("tasks") == Some(&true) {
            return Some(TicketingInfo::new(TicketingSystem::Tasks, None));
        }
    }
    None
}
```

Users enable tasks mode in their Grove config:

```toml
[ticketing.overrides]
tasks = true
```

### Task Completion Detection

Add a new hook handler for `TaskCompleted`:

**Input Structure:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCompletedInput {
    #[serde(flatten)]
    pub common: HookInput,
    pub task_id: String,
    pub task_subject: String,
    #[serde(default)]
    pub task_description: Option<String>,
    #[serde(default)]
    pub teammate_name: Option<String>,
    #[serde(default)]
    pub team_name: Option<String>,
}
```

**Hook Handler Behavior:**

1. Parse TaskCompletedInput from stdin
2. Create/update session with task context:
   - `ticket_id` = `task_id`
   - `ticket_title` = `task_subject`
   - `ticket_description` = `task_description`
3. Transition gate to Pending state
4. Return exit code 2 to block completion until reflection/skip

### Close Pattern

Unlike tissue/beads which detect close commands via Bash tool patterns,
task completion is detected via the dedicated `TaskCompleted` hook. No
pattern matching needed.

### Hook Configuration

Update `hooks/hooks.json`:

```json
{
  "hooks": {
    "TaskCompleted": [{
      "type": "command",
      "command": "grove hook task-completed"
    }]
  }
}
```

## Files to Modify

| File | Changes |
|------|---------|
| `src/discovery/tickets.rs` | Update `probe_tasks` for config-based detection |
| `src/hooks/input.rs` | Add `TaskCompletedInput` struct |
| `src/hooks/runner.rs` | Add `task-completed` handler |
| `src/core/state.rs` | Ensure TicketContext works with task data |
| `hooks/hooks.json` | Add TaskCompleted hook configuration |
| `design/implementation-tasks.md` | Mark as implemented |

## User Configuration

To enable Claude Code tasks as a ticketing system:

```toml
# .grove/config.toml
[ticketing]
discovery = ["tasks", "tissue", "beads", "session"]

[ticketing.overrides]
tasks = true
```

## Comparison with Other Ticketing Systems

| Aspect | tissue | beads | tasks |
|--------|--------|-------|-------|
| Detection | `.tissue/` dir | `.beads/` dir | Config opt-in |
| Close Signal | Bash command | Bash command | TaskCompleted hook |
| Ticket ID | Command arg | Command arg | `task_id` field |
| Title | From system | From system | `task_subject` field |

## Future Considerations

1. **Auto-detection**: If Claude Code documents the `~/.claude/tasks/`
   format, auto-detection could be added
2. **Task Query API**: A future Claude Code API could allow querying
   active tasks at session start
3. **Agent Teams**: The `teammate_name` and `team_name` fields could
   enable team-aware reflection workflows
