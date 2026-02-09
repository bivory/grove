# Development Guide

## Prerequisites

- Rust 1.92+ (or use Dev Container)
- [mise](https://mise.jdx.dev/) (recommended task runner)

## Build Commands

Using mise:

```bash
mise build         # Build debug binary
mise test          # Run tests with nextest
mise clippy        # Run clippy lints
mise fmt           # Format code
mise pre-commit    # Run all pre-commit checks
mise ci            # Full CI pipeline
```

Or directly with cargo:

```bash
cargo build
cargo nextest run
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt
```

## Local Plugin Testing

1. Build release binary:

   ```bash
   cargo build --release
   ```

2. Add to PATH:

   ```bash
   # Option A: Symlink
   ln -sf "$(pwd)/target/release/grove" ~/.local/bin/grove

   # Option B: Add to PATH
   export PATH="$(pwd)/target/release:$PATH"
   ```

3. Install plugin locally in Claude Code:

   ```text
   /plugin marketplace add ./
   /plugin install grove@bivory
   ```

4. Verify installation:

   ```text
   /plugin list
   ```

5. Test by initializing in a project:

   ```bash
   grove init
   grove stats
   ```

## Debug Commands

| Command | Description |
|---------|-------------|
| `grove sessions` | List recent sessions |
| `grove debug <session_id>` | Dump session state |
| `grove trace <session_id>` | Show trace events |

## Troubleshooting

### Hooks not appearing

Claude Code hooks are active even if not shown in menus. Verify with:

```bash
grove hook session-start --help
```

### Sessions not creating

Check state directory exists:

```bash
ls -la .grove/
```

### Gate not triggering

Verify ticketing system detection:

```bash
grove tickets
```

## Dev Container

### VS Code

Open in VS Code, then "Reopen in Container" when prompted.

### Command Line

```bash
mise dc:build  # Build container
mise dc:shell  # Get a shell
mise dc:claude # Run Claude Code in container
```

## Releasing

Preview what will happen:

```bash
mise release:dry-run
```

Release (updates version, commits, tags, pushes):

```bash
mise release:patch  # 0.1.0 → 0.1.1
mise release:minor  # 0.1.0 → 0.2.0
mise release:major  # 0.1.0 → 1.0.0
```

## Project Architecture

```text
grove/
├── src/
│   ├── core/          # Gate state machine, learning types
│   ├── backends/      # Memory backend implementations
│   ├── discovery/     # Ticketing and backend detection
│   ├── stats/         # Quality tracking and insights
│   ├── storage/       # Session state persistence
│   ├── hooks/         # Claude Code hook integration
│   └── cli/           # Command implementations
├── agents/            # Claude Code agent definitions
├── design/            # Design documentation
└── .claude-plugin/    # Plugin manifest and installer
```

## Design Documentation

Before making significant changes, review the design docs in `/design/`:

| Document | Content |
|----------|---------|
| `00-overview.md` | Vision and core concepts |
| `01-architecture.md` | Full system design (most comprehensive) |
| `02-implementation.md` | Concrete Rust module structure |
| `03-stats-and-quality.md` | Event log model, quality tracking |
| `04-test-plan.md` | Testing strategy and test cases |
| `05-ci.md` | CI workflows and release process |
| `implementation-tasks.md` | Implementation roadmap |

Decision rationale and risk analysis in `/documents/`.
