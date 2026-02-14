//! Grove - Compound Learning Gate for Claude Code
//!
//! CLI entry point with global panic handler.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use grove::config::{grove_home, Config};
use grove::error::exit_codes;
use grove::hooks::{HookRunner, HookType};
use grove::storage::FileSessionStore;

// =============================================================================
// Version
// =============================================================================

/// Get the version string.
///
/// - Release builds (on a git tag): "0.5.0"
/// - Development builds: "0.5.0-dev (abc1234)"
/// - Dirty working directory: "0.5.0-dev (abc1234-dirty)"
fn version() -> &'static str {
    const VERSION: &str = env!("CARGO_PKG_VERSION");
    const GIT_HASH: &str = env!("GROVE_GIT_HASH");
    const IS_RELEASE: &str = env!("GROVE_IS_RELEASE");

    // Use a static to avoid repeated allocations
    static VERSION_STRING: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    VERSION_STRING.get_or_init(|| {
        if IS_RELEASE == "true" {
            VERSION.to_string()
        } else {
            format!("{VERSION}-dev ({GIT_HASH})")
        }
    })
}

// =============================================================================
// CLI Definition
// =============================================================================

/// Grove - Compound Learning Gate for Claude Code
#[derive(Parser)]
#[command(name = "grove")]
#[command(author, version = version(), about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// [Internal] Run a hook (JSON stdin/stdout). Called by Claude Code hooks
    Hook {
        /// The hook event type
        #[arg(value_enum)]
        event: HookEvent,
    },

    /// [Agent] Record structured reflection and capture learnings
    Reflect {
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
        /// Session ID to use
        #[arg(long)]
        session_id: Option<String>,
    },

    /// [Agent] Skip reflection with a reason
    Skip {
        /// Reason for skipping
        reason: String,
        /// Session ID to use
        #[arg(long)]
        session_id: String,
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
        /// Who decided to skip (agent, user, auto)
        #[arg(long)]
        decider: Option<String>,
        /// Lines changed in the session
        #[arg(long)]
        lines_changed: Option<u32>,
    },

    /// [Agent] Record a subagent observation
    Observe {
        /// The observation note
        note: String,
        /// Session ID to use
        #[arg(long)]
        session_id: String,
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
    },

    /// [User/Agent] Search for learnings
    Search {
        /// Search query
        query: String,
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
        /// Maximum number of results
        #[arg(long, short)]
        limit: Option<usize>,
        /// Include archived learnings
        #[arg(long)]
        include_archived: bool,
    },

    /// [User] List recent learnings
    List {
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
        /// Maximum number of results
        #[arg(long, short)]
        limit: Option<usize>,
        /// Show only stale learnings
        #[arg(long)]
        stale: bool,
        /// Include archived learnings
        #[arg(long)]
        include_archived: bool,
        /// Days until decay to consider stale
        #[arg(long)]
        stale_days: Option<u32>,
        /// Hide usage statistics
        #[arg(long)]
        no_stats: bool,
    },

    /// [User] Display quality statistics and insights
    Stats {
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
        /// Show detailed stats
        #[arg(long, short)]
        detailed: bool,
        /// Force rebuild the cache
        #[arg(long)]
        rebuild: bool,
    },

    /// [User] Maintain learnings (archive stale, restore)
    Maintain {
        /// Action to perform
        #[command(subcommand)]
        action: MaintainAction,
        /// Output as JSON
        #[arg(long, short, global = true)]
        json: bool,
        /// Suppress output
        #[arg(long, short, global = true)]
        quiet: bool,
        /// Days until decay to consider stale
        #[arg(long, global = true)]
        stale_days: Option<u32>,
        /// Perform archive without confirmation
        #[arg(long, global = true)]
        auto_archive: bool,
        /// Show dry run only
        #[arg(long, global = true)]
        dry_run: bool,
    },

    /// [User] Initialize Grove configuration
    Init {
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
        /// Force overwrite existing files
        #[arg(long, short)]
        force: bool,
    },

    /// [User] Show discovered memory backends
    Backends {
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
    },

    /// [User] Show detected ticketing system
    Tickets {
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
    },

    /// [Developer] List recent sessions
    Sessions {
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
        /// Maximum number of sessions to show
        #[arg(long, short, default_value = "20")]
        limit: usize,
    },

    /// [Developer] Debug session state
    Debug {
        /// Session ID to inspect
        session_id: String,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
        /// Set gate status (testing escape hatch)
        #[arg(long)]
        set_gate: Option<String>,
    },

    /// [Developer] View trace events for a session
    Trace {
        /// Session ID to inspect
        session_id: String,
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
        /// Maximum number of events
        #[arg(long, short)]
        limit: Option<usize>,
        /// Filter by event type
        #[arg(long)]
        event_type: Option<String>,
    },

    /// [User] Clean old session files
    Clean {
        /// Output as JSON
        #[arg(long, short)]
        json: bool,
        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
        /// Remove sessions older than duration (e.g., "7d", "24h")
        #[arg(long)]
        before: Option<String>,
        /// Remove orphaned sessions
        #[arg(long)]
        orphans: bool,
        /// Show what would be cleaned without removing
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, ValueEnum)]
enum HookEvent {
    SessionStart,
    PreToolUse,
    PostToolUse,
    Stop,
    SessionEnd,
}

impl From<HookEvent> for HookType {
    fn from(event: HookEvent) -> Self {
        match event {
            HookEvent::SessionStart => HookType::SessionStart,
            HookEvent::PreToolUse => HookType::PreToolUse,
            HookEvent::PostToolUse => HookType::PostToolUse,
            HookEvent::Stop => HookType::Stop,
            HookEvent::SessionEnd => HookType::SessionEnd,
        }
    }
}

#[derive(Subcommand)]
enum MaintainAction {
    /// List stale learnings
    List,
    /// Archive specified learnings
    Archive {
        /// Learning IDs to archive
        learning_ids: Vec<String>,
    },
    /// Restore archived learnings
    Restore {
        /// Learning IDs to restore
        learning_ids: Vec<String>,
    },
}

// =============================================================================
// Main Entry Point
// =============================================================================

fn main() -> ExitCode {
    // Set up panic handler
    setup_panic_handler();

    // Run the CLI
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("grove error: {}", e);
            ExitCode::from(exit_codes::APPROVE as u8) // Fail-open
        }
    }
}

/// Set up the global panic handler.
///
/// On panic, logs to ~/.grove/crash.log and exits with code 3.
/// This ensures crashes don't block the user (fail-open philosophy).
fn setup_panic_handler() {
    std::panic::set_hook(Box::new(|info| {
        // Log to stderr
        eprintln!("grove panic: {}", info);

        // Try to log to crash file
        if let Some(home) = grove_home() {
            let crash_log = home.join("crash.log");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&crash_log)
            {
                let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
                let _ = writeln!(file, "[{}] {}", timestamp, info);
            }
        }

        // Exit with crash code (fail-open)
        std::process::exit(exit_codes::CRASH);
    }));
}

/// Run the CLI and return the exit code.
fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;

    match cli.command {
        Commands::Hook { event } => run_hook(event.into()),
        Commands::Reflect {
            json,
            quiet,
            session_id,
        } => run_reflect(json, quiet, session_id, &cwd),
        Commands::Skip {
            reason,
            session_id,
            json,
            quiet,
            decider,
            lines_changed,
        } => run_skip(&session_id, &reason, json, quiet, decider, lines_changed),
        Commands::Observe {
            note,
            session_id,
            json,
            quiet,
        } => run_observe(&session_id, &note, json, quiet),
        Commands::Search {
            query,
            json,
            quiet,
            limit,
            include_archived,
        } => run_search(&query, json, quiet, limit, include_archived, &cwd),
        Commands::List {
            json,
            quiet,
            limit,
            stale,
            include_archived,
            stale_days,
            no_stats,
        } => run_list(
            json,
            quiet,
            limit,
            stale,
            include_archived,
            stale_days,
            no_stats,
            &cwd,
        ),
        Commands::Stats {
            json,
            quiet,
            detailed,
            rebuild,
        } => run_stats(json, quiet, detailed, rebuild, &cwd),
        Commands::Maintain {
            action,
            json,
            quiet,
            stale_days,
            auto_archive,
            dry_run,
        } => run_maintain(action, json, quiet, stale_days, auto_archive, dry_run, &cwd),
        Commands::Init { json, quiet, force } => run_init(json, quiet, force, &cwd),
        Commands::Backends { json, quiet } => run_backends(json, quiet, &cwd),
        Commands::Tickets { json, quiet } => run_tickets(json, quiet, &cwd),
        Commands::Sessions { json, quiet, limit } => run_sessions(json, quiet, limit),
        Commands::Debug {
            session_id,
            quiet,
            set_gate,
        } => run_debug(&session_id, quiet, set_gate),
        Commands::Trace {
            session_id,
            json,
            quiet,
            limit,
            event_type,
        } => run_trace(&session_id, json, quiet, limit, event_type),
        Commands::Clean {
            json,
            quiet,
            before,
            orphans,
            dry_run,
        } => run_clean(json, quiet, before, orphans, dry_run),
    }
}

// =============================================================================
// Command Implementations
// =============================================================================

fn run_hook(hook_type: HookType) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let config = Config::load();
    let store = FileSessionStore::new()?;
    let runner = HookRunner::new(store, config);

    // Run the hook (reads from stdin)
    let output = runner.run(hook_type)?;

    // Print output
    println!("{}", output);

    // Determine exit code based on hook output
    if hook_type == HookType::Stop {
        // Parse output to check decision
        match serde_json::from_str::<serde_json::Value>(&output) {
            Ok(stop_output) => {
                match stop_output.get("decision").and_then(|d| d.as_str()) {
                    Some("block") => {
                        return Ok(ExitCode::from(exit_codes::BLOCK as u8));
                    }
                    Some("approve") => {
                        // Expected value, fall through to APPROVE
                    }
                    Some(unexpected) => {
                        tracing::warn!(
                            decision = unexpected,
                            "stop hook returned unexpected decision, defaulting to approve"
                        );
                    }
                    None => {
                        tracing::warn!(
                            "stop hook output missing 'decision' field, defaulting to approve"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to parse stop hook output as JSON, defaulting to approve"
                );
            }
        }
    }

    Ok(ExitCode::from(exit_codes::APPROVE as u8))
}

/// Convert a success boolean to an exit code.
fn success_to_exit_code(success: bool) -> ExitCode {
    if success {
        ExitCode::from(exit_codes::APPROVE as u8)
    } else {
        ExitCode::from(exit_codes::ERROR as u8)
    }
}

fn run_reflect(
    json: bool,
    quiet: bool,
    session_id: Option<String>,
    cwd: &Path,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::reflect::{ReflectCommand, ReflectOptions};
    use grove::create_primary_backend;

    let config = Config::load();
    let store = FileSessionStore::new()?;

    // Set up backend using discovery
    let backend = create_primary_backend(cwd, Some(&config));

    let cmd = ReflectCommand::new(store, backend, config);
    let options = ReflectOptions {
        json,
        quiet,
        session_id,
    };

    let output = cmd.run(&options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

fn run_skip(
    session_id: &str,
    reason: &str,
    json: bool,
    quiet: bool,
    decider: Option<String>,
    lines_changed: Option<u32>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::skip::{SkipCommand, SkipOptions};
    use grove::core::SkipDecider;

    let config = Config::load();
    let store = FileSessionStore::new()?;

    let decider = decider.map(|d| match d.to_lowercase().as_str() {
        "agent" => SkipDecider::Agent,
        "auto" | "autothreshold" => SkipDecider::AutoThreshold,
        _ => SkipDecider::User,
    });

    let cmd = SkipCommand::new(store, config);
    let options = SkipOptions {
        json,
        quiet,
        decider,
        lines_changed,
    };

    let output = cmd.run(session_id, reason, &options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

fn run_observe(
    session_id: &str,
    note: &str,
    json: bool,
    quiet: bool,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::observe::{ObserveCommand, ObserveOptions};

    let config = Config::load();
    let store = FileSessionStore::new()?;

    let cmd = ObserveCommand::new(store, config);
    let options = ObserveOptions { json, quiet };

    let output = cmd.run(session_id, note, &options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

fn run_search(
    query: &str,
    json: bool,
    quiet: bool,
    limit: Option<usize>,
    include_archived: bool,
    cwd: &Path,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::search::{SearchCommand, SearchOptions};
    use grove::create_primary_backend;

    let config = Config::load();
    let backend = create_primary_backend(cwd, Some(&config));

    let cmd = SearchCommand::new(backend, config);
    let options = SearchOptions {
        json,
        quiet,
        limit,
        include_archived,
    };

    let output = cmd.run(query, &options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

#[allow(clippy::too_many_arguments)]
fn run_list(
    json: bool,
    quiet: bool,
    limit: Option<usize>,
    stale: bool,
    include_archived: bool,
    stale_days: Option<u32>,
    no_stats: bool,
    cwd: &Path,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::list::{ListCommand, ListOptions};
    use grove::config::{project_stats_log_path, stats_cache_path};
    use grove::create_primary_backend;
    use grove::stats::StatsCacheManager;

    let config = Config::load();
    let backend = create_primary_backend(cwd, Some(&config));

    // Load stats cache if not disabled
    let stats_cache = if no_stats {
        None
    } else {
        stats_cache_path().and_then(|cache_path| {
            let log_path = project_stats_log_path(cwd);
            let manager = StatsCacheManager::new(&cache_path, &log_path);
            manager.load_or_rebuild().ok()
        })
    };

    let cmd = ListCommand::with_stats(backend, config, stats_cache);
    let options = ListOptions {
        json,
        quiet,
        limit,
        stale,
        include_archived,
        stale_days,
        no_stats,
    };

    let output = cmd.run(&options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

fn run_stats(
    json: bool,
    quiet: bool,
    detailed: bool,
    rebuild: bool,
    cwd: &Path,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::stats::{StatsCommand, StatsOptions};

    let config = Config::load();

    let cmd = StatsCommand::new(config, cwd);
    let options = StatsOptions {
        json,
        quiet,
        detailed,
        rebuild,
    };

    let output = cmd.run(&options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

fn run_maintain(
    action: MaintainAction,
    json: bool,
    quiet: bool,
    stale_days: Option<u32>,
    auto_archive: bool,
    dry_run: bool,
    cwd: &Path,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::maintain::{
        MaintainAction as MaintainActionLib, MaintainCommand, MaintainInput, MaintainOptions,
    };
    use grove::create_primary_backend;

    let config = Config::load();
    let backend = create_primary_backend(cwd, Some(&config));

    let cmd = MaintainCommand::new(backend, config);
    let options = MaintainOptions {
        json,
        quiet,
        stale_days,
        auto_archive,
        dry_run,
    };

    let (lib_action, learning_ids) = match action {
        MaintainAction::List => (MaintainActionLib::List, vec![]),
        MaintainAction::Archive { learning_ids } => (MaintainActionLib::Archive, learning_ids),
        MaintainAction::Restore { learning_ids } => (MaintainActionLib::Restore, learning_ids),
    };

    let input = MaintainInput {
        action: lib_action,
        learning_ids,
    };

    let output = cmd.run_with_input(&input, &options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

fn run_init(
    json: bool,
    quiet: bool,
    force: bool,
    cwd: &Path,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::init::{InitCommand, InitOptions};

    let cmd = InitCommand::new(cwd.to_string_lossy().to_string());
    let options = InitOptions { json, quiet, force };

    let output = cmd.run(&options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

fn run_backends(
    json: bool,
    quiet: bool,
    cwd: &Path,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::backends_cmd::{BackendsCommand, BackendsOptions};

    let config = Config::load();

    let cmd = BackendsCommand::new(cwd.to_string_lossy().to_string(), config);
    let options = BackendsOptions { json, quiet };

    let output = cmd.run(&options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

fn run_tickets(
    json: bool,
    quiet: bool,
    cwd: &Path,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::tickets_cmd::{TicketsCommand, TicketsOptions};

    let config = Config::load();

    let cmd = TicketsCommand::new(cwd.to_string_lossy().to_string(), config);
    let options = TicketsOptions { json, quiet };

    let output = cmd.run(&options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

fn run_sessions(
    json: bool,
    quiet: bool,
    limit: usize,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::sessions::{SessionsCommand, SessionsOptions};

    let store = FileSessionStore::new()?;

    let cmd = SessionsCommand::new(store);
    let options = SessionsOptions { json, quiet, limit };

    let output = cmd.run(&options);

    if !quiet {
        if json {
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("{}", output.format_text());
        }
    }

    Ok(success_to_exit_code(output.success))
}

fn run_debug(
    session_id: &str,
    quiet: bool,
    set_gate: Option<String>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::debug::{DebugCommand, DebugOptions};

    let store = FileSessionStore::new()?;

    let cmd = DebugCommand::new(store);
    let options = DebugOptions {
        json: true, // Debug always uses JSON
        quiet,
        set_gate,
    };

    let output = cmd.run(session_id, &options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

fn run_trace(
    session_id: &str,
    json: bool,
    quiet: bool,
    limit: Option<usize>,
    event_type: Option<String>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::trace::{TraceCommand, TraceOptions};

    let store = FileSessionStore::new()?;

    let cmd = TraceCommand::new(store);
    let options = TraceOptions {
        json,
        quiet,
        limit,
        event_type,
    };

    let output = cmd.run(session_id, &options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

fn run_clean(
    json: bool,
    quiet: bool,
    before: Option<String>,
    orphans: bool,
    dry_run: bool,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use grove::cli::clean::{CleanCommand, CleanOptions};

    let cmd = match CleanCommand::new() {
        Some(cmd) => cmd,
        None => {
            eprintln!("grove error: could not determine sessions directory");
            return Ok(ExitCode::from(exit_codes::APPROVE as u8));
        }
    };

    let options = CleanOptions {
        json,
        quiet,
        before,
        orphans,
        dry_run,
    };

    let output = cmd.run(&options);
    let formatted = cmd.format_output(&output, &options);

    if !formatted.is_empty() {
        println!("{}", formatted);
    }

    Ok(success_to_exit_code(output.success))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_codes() {
        assert_eq!(exit_codes::APPROVE, 0);
        assert_eq!(exit_codes::ERROR, 1);
        assert_eq!(exit_codes::BLOCK, 2);
        assert_eq!(exit_codes::CRASH, 3);
    }

    #[test]
    fn test_success_to_exit_code() {
        assert_eq!(
            success_to_exit_code(true),
            ExitCode::from(exit_codes::APPROVE as u8)
        );
        assert_eq!(
            success_to_exit_code(false),
            ExitCode::from(exit_codes::ERROR as u8)
        );
    }

    #[test]
    fn test_hook_event_conversion() {
        assert_eq!(
            HookType::from(HookEvent::SessionStart),
            HookType::SessionStart
        );
        assert_eq!(HookType::from(HookEvent::PreToolUse), HookType::PreToolUse);
        assert_eq!(
            HookType::from(HookEvent::PostToolUse),
            HookType::PostToolUse
        );
        assert_eq!(HookType::from(HookEvent::Stop), HookType::Stop);
        assert_eq!(HookType::from(HookEvent::SessionEnd), HookType::SessionEnd);
    }

    #[test]
    fn test_cli_parse_hook() {
        let cli = Cli::parse_from(["grove", "hook", "session-start"]);
        match cli.command {
            Commands::Hook { event } => {
                assert!(matches!(event, HookEvent::SessionStart));
            }
            _ => panic!("Expected Hook command"),
        }
    }

    #[test]
    fn test_cli_parse_skip() {
        let cli = Cli::parse_from(["grove", "skip", "minor change", "--session-id", "test-123"]);
        match cli.command {
            Commands::Skip {
                reason, session_id, ..
            } => {
                assert_eq!(reason, "minor change");
                assert_eq!(session_id, "test-123");
            }
            _ => panic!("Expected Skip command"),
        }
    }

    #[test]
    fn test_cli_parse_search() {
        let cli = Cli::parse_from([
            "grove",
            "search",
            "test query",
            "--limit",
            "10",
            "--include-archived",
        ]);
        match cli.command {
            Commands::Search {
                query,
                limit,
                include_archived,
                ..
            } => {
                assert_eq!(query, "test query");
                assert_eq!(limit, Some(10));
                assert!(include_archived);
            }
            _ => panic!("Expected Search command"),
        }
    }

    #[test]
    fn test_cli_parse_init() {
        let cli = Cli::parse_from(["grove", "init", "--force", "--json"]);
        match cli.command {
            Commands::Init { force, json, .. } => {
                assert!(force);
                assert!(json);
            }
            _ => panic!("Expected Init command"),
        }
    }

    #[test]
    fn test_cli_parse_sessions() {
        let cli = Cli::parse_from(["grove", "sessions", "--limit", "50", "--json"]);
        match cli.command {
            Commands::Sessions { json, limit, .. } => {
                assert!(json);
                assert_eq!(limit, 50);
            }
            _ => panic!("Expected Sessions command"),
        }
    }

    #[test]
    fn test_cli_parse_debug() {
        let cli = Cli::parse_from(["grove", "debug", "session-123", "--set-gate", "reflected"]);
        match cli.command {
            Commands::Debug {
                session_id,
                set_gate,
                ..
            } => {
                assert_eq!(session_id, "session-123");
                assert_eq!(set_gate, Some("reflected".to_string()));
            }
            _ => panic!("Expected Debug command"),
        }
    }

    #[test]
    fn test_cli_parse_clean() {
        let cli = Cli::parse_from(["grove", "clean", "--before", "7d", "--dry-run"]);
        match cli.command {
            Commands::Clean {
                before, dry_run, ..
            } => {
                assert_eq!(before, Some("7d".to_string()));
                assert!(dry_run);
            }
            _ => panic!("Expected Clean command"),
        }
    }

    #[test]
    fn test_cli_parse_maintain_list() {
        let cli = Cli::parse_from(["grove", "maintain", "list"]);
        match cli.command {
            Commands::Maintain { action, .. } => {
                assert!(matches!(action, MaintainAction::List));
            }
            _ => panic!("Expected Maintain command"),
        }
    }

    #[test]
    fn test_cli_parse_maintain_archive() {
        let cli = Cli::parse_from(["grove", "maintain", "archive", "id1", "id2"]);
        match cli.command {
            Commands::Maintain { action, .. } => {
                if let MaintainAction::Archive { learning_ids } = action {
                    assert_eq!(learning_ids, vec!["id1", "id2"]);
                } else {
                    panic!("Expected Archive action");
                }
            }
            _ => panic!("Expected Maintain command"),
        }
    }

    #[test]
    fn test_cli_parse_observe() {
        let cli = Cli::parse_from([
            "grove",
            "observe",
            "important note",
            "--session-id",
            "sess-1",
        ]);
        match cli.command {
            Commands::Observe {
                note, session_id, ..
            } => {
                assert_eq!(note, "important note");
                assert_eq!(session_id, "sess-1");
            }
            _ => panic!("Expected Observe command"),
        }
    }

    #[test]
    fn test_cli_parse_trace() {
        let cli = Cli::parse_from([
            "grove",
            "trace",
            "sess-1",
            "--limit",
            "5",
            "--event-type",
            "GateBlocked",
        ]);
        match cli.command {
            Commands::Trace {
                session_id,
                limit,
                event_type,
                ..
            } => {
                assert_eq!(session_id, "sess-1");
                assert_eq!(limit, Some(5));
                assert_eq!(event_type, Some("GateBlocked".to_string()));
            }
            _ => panic!("Expected Trace command"),
        }
    }

    #[test]
    fn test_cli_parse_list() {
        let cli = Cli::parse_from(["grove", "list", "--stale", "--limit", "20"]);
        match cli.command {
            Commands::List { stale, limit, .. } => {
                assert!(stale);
                assert_eq!(limit, Some(20));
            }
            _ => panic!("Expected List command"),
        }
    }

    #[test]
    fn test_cli_parse_stats() {
        let cli = Cli::parse_from(["grove", "stats", "--detailed", "--rebuild"]);
        match cli.command {
            Commands::Stats {
                detailed, rebuild, ..
            } => {
                assert!(detailed);
                assert!(rebuild);
            }
            _ => panic!("Expected Stats command"),
        }
    }
}
