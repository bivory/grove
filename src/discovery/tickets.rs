//! Ticketing system detection for Grove.
//!
//! Probes the project directory to detect which ticketing system is active.
//! The discovery order is configurable via the Grove config file.
//!
//! Supported ticketing systems:
//! - **tissue**: Check for `.tissue/` directory
//! - **beads**: Check for `.beads/` directory
//! - **tasks**: Claude Code tasks (opt-in via config)
//! - **session**: Always available (fallback)

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::{Config, TicketingConfig};

/// Ticketing system type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketingSystem {
    /// tissue ticketing system (`.tissue/` directory).
    Tissue,
    /// beads ticketing system (`.beads/` directory).
    Beads,
    /// Claude Code tasks (opt-in via config).
    Tasks,
    /// Session fallback (always available).
    Session,
}

impl TicketingSystem {
    /// Get the system name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tissue => "tissue",
            Self::Beads => "beads",
            Self::Tasks => "tasks",
            Self::Session => "session",
        }
    }

    /// Parse a system name from a string.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "tissue" => Some(Self::Tissue),
            "beads" => Some(Self::Beads),
            "tasks" => Some(Self::Tasks),
            "session" => Some(Self::Session),
            _ => None,
        }
    }
}

impl std::fmt::Display for TicketingSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Information about a detected ticketing system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketingInfo {
    /// The detected ticketing system.
    pub system: TicketingSystem,
    /// The directory where the system was detected (if applicable).
    pub marker_path: Option<std::path::PathBuf>,
}

impl TicketingInfo {
    /// Create a new ticketing info.
    pub fn new(system: TicketingSystem, marker_path: Option<std::path::PathBuf>) -> Self {
        Self {
            system,
            marker_path,
        }
    }
}

/// Close pattern for a ticketing system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosePattern {
    /// tissue close: `tissue status <id> closed`
    TissueClose,
    /// beads close: `beads close <id>`
    BeadsClose,
    /// beads complete: `beads complete <id>`
    BeadsComplete,
}

impl ClosePattern {
    /// Get the ticketing system for this close pattern.
    pub fn system(&self) -> TicketingSystem {
        match self {
            Self::TissueClose => TicketingSystem::Tissue,
            Self::BeadsClose | Self::BeadsComplete => TicketingSystem::Beads,
        }
    }
}

/// Detect the active ticketing system.
///
/// Probes in the configured order, returning the first detected system.
/// If no system is detected, falls back to Session mode.
///
/// # Arguments
///
/// * `cwd` - The project's working directory to probe.
/// * `config` - Optional configuration for discovery order and overrides.
///
/// # Returns
///
/// Information about the detected ticketing system.
pub fn detect_ticketing_system(cwd: &Path, config: Option<&Config>) -> TicketingInfo {
    let ticketing_config = config.map(|c| &c.ticketing).cloned().unwrap_or_default();

    detect_with_config(cwd, &ticketing_config)
}

/// Detect ticketing system with specific config.
fn detect_with_config(cwd: &Path, config: &TicketingConfig) -> TicketingInfo {
    for system_name in &config.discovery {
        // Check if the system is disabled via overrides
        if let Some(false) = config.overrides.get(system_name) {
            continue;
        }

        // Parse the system name
        let Some(system) = TicketingSystem::parse(system_name) else {
            continue;
        };

        // Probe for the system
        if let Some(info) = probe_system(cwd, system, config) {
            return info;
        }
    }

    // Fallback to session if nothing else detected, unless explicitly disabled
    if config.overrides.get("session") == Some(&false) {
        tracing::warn!(
            "no ticketing system detected and session fallback is disabled; using session anyway"
        );
    }
    TicketingInfo::new(TicketingSystem::Session, None)
}

/// Probe for a specific ticketing system.
fn probe_system(
    cwd: &Path,
    system: TicketingSystem,
    config: &TicketingConfig,
) -> Option<TicketingInfo> {
    match system {
        TicketingSystem::Tissue => probe_tissue(cwd),
        TicketingSystem::Beads => probe_beads(cwd),
        TicketingSystem::Tasks => probe_tasks(config),
        TicketingSystem::Session => Some(TicketingInfo::new(TicketingSystem::Session, None)),
    }
}

/// Probe for tissue ticketing system.
///
/// Checks for the presence of a `.tissue/` directory in the working directory.
pub fn probe_tissue(cwd: &Path) -> Option<TicketingInfo> {
    let tissue_dir = cwd.join(".tissue");
    if tissue_dir.is_dir() {
        Some(TicketingInfo::new(
            TicketingSystem::Tissue,
            Some(tissue_dir),
        ))
    } else {
        None
    }
}

/// Probe for beads ticketing system.
///
/// Checks for the presence of a `.beads/` directory in the working directory.
pub fn probe_beads(cwd: &Path) -> Option<TicketingInfo> {
    let beads_dir = cwd.join(".beads");
    if beads_dir.is_dir() {
        Some(TicketingInfo::new(TicketingSystem::Beads, Some(beads_dir)))
    } else {
        None
    }
}

/// Probe for Claude Code tasks.
///
/// Tasks mode uses config-based opt-in since there's no filesystem marker.
/// Enable via `[ticketing.overrides] tasks = true` in config.
pub fn probe_tasks(config: &TicketingConfig) -> Option<TicketingInfo> {
    // Tasks mode requires explicit opt-in via config
    if config.overrides.get("tasks") == Some(&true) {
        Some(TicketingInfo::new(TicketingSystem::Tasks, None))
    } else {
        None
    }
}

/// Match a command against ticket close patterns.
///
/// # Arguments
///
/// * `tool_name` - The name of the tool being invoked (e.g., "Bash").
/// * `command` - The command string to match.
///
/// # Returns
///
/// The matched close pattern if found, None otherwise.
pub fn match_close_command(tool_name: &str, command: &str) -> Option<ClosePattern> {
    // Only match Bash tool commands
    if tool_name != "Bash" {
        return None;
    }

    let command = command.trim();

    // Match tissue close pattern: tissue status <id> closed
    if is_tissue_close_command(command) {
        return Some(ClosePattern::TissueClose);
    }

    // Match beads close pattern: beads close <id>
    if is_beads_close_command(command) {
        return Some(ClosePattern::BeadsClose);
    }

    // Match beads complete pattern: beads complete <id>
    if is_beads_complete_command(command) {
        return Some(ClosePattern::BeadsComplete);
    }

    None
}

/// Shell operators that indicate compound commands.
/// These should prevent matching to avoid security issues.
const SHELL_OPERATORS: [&str; 5] = ["&&", "||", "|", ";", "&"];

/// Check if a command contains shell operators that make it a compound command.
fn contains_shell_operator(command: &str) -> bool {
    // Check for operators as separate tokens
    for part in command.split_whitespace() {
        if SHELL_OPERATORS.contains(&part) {
            return true;
        }
    }

    // Check for operators attached to tokens (e.g., "closed;")
    for op in SHELL_OPERATORS {
        if command.contains(op) {
            return true;
        }
    }

    false
}

/// Check if a command matches the tissue close pattern.
///
/// Pattern: `tissue status <id> closed [extra args...]`
fn is_tissue_close_command(command: &str) -> bool {
    // Reject compound commands for security
    if contains_shell_operator(command) {
        return false;
    }

    let parts: Vec<&str> = command.split_whitespace().collect();

    // Need at least 4 parts: tissue status <id> closed
    if parts.len() < 4 {
        return false;
    }

    // Check structure: tissue status <anything> closed [extra args...]
    // Note: "closed" must be at position 3, extra args after are ignored
    parts[0] == "tissue" && parts[1] == "status" && parts[3] == "closed"
}

/// Check if a command matches the beads close pattern.
///
/// Pattern: `beads close <id> [extra args...]`
fn is_beads_close_command(command: &str) -> bool {
    // Reject compound commands for security
    if contains_shell_operator(command) {
        return false;
    }

    let parts: Vec<&str> = command.split_whitespace().collect();

    // Need at least 3 parts: beads close <id>
    if parts.len() < 3 {
        return false;
    }

    parts[0] == "beads" && parts[1] == "close"
}

/// Check if a command matches the beads complete pattern.
///
/// Pattern: `beads complete <id> [extra args...]`
fn is_beads_complete_command(command: &str) -> bool {
    // Reject compound commands for security
    if contains_shell_operator(command) {
        return false;
    }

    let parts: Vec<&str> = command.split_whitespace().collect();

    // Need at least 3 parts: beads complete <id>
    if parts.len() < 3 {
        return false;
    }

    parts[0] == "beads" && parts[1] == "complete"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    // TicketingSystem tests

    #[test]
    fn test_ticketing_system_as_str() {
        assert_eq!(TicketingSystem::Tissue.as_str(), "tissue");
        assert_eq!(TicketingSystem::Beads.as_str(), "beads");
        assert_eq!(TicketingSystem::Tasks.as_str(), "tasks");
        assert_eq!(TicketingSystem::Session.as_str(), "session");
    }

    #[test]
    fn test_ticketing_system_parse() {
        assert_eq!(
            TicketingSystem::parse("tissue"),
            Some(TicketingSystem::Tissue)
        );
        assert_eq!(
            TicketingSystem::parse("TISSUE"),
            Some(TicketingSystem::Tissue)
        );
        assert_eq!(
            TicketingSystem::parse("beads"),
            Some(TicketingSystem::Beads)
        );
        assert_eq!(
            TicketingSystem::parse("tasks"),
            Some(TicketingSystem::Tasks)
        );
        assert_eq!(
            TicketingSystem::parse("session"),
            Some(TicketingSystem::Session)
        );
        assert_eq!(TicketingSystem::parse("unknown"), None);
    }

    #[test]
    fn test_ticketing_system_display() {
        assert_eq!(format!("{}", TicketingSystem::Tissue), "tissue");
        assert_eq!(format!("{}", TicketingSystem::Beads), "beads");
    }

    #[test]
    fn test_ticketing_system_serialization() {
        let system = TicketingSystem::Tissue;
        let json = serde_json::to_string(&system).unwrap();
        assert_eq!(json, "\"tissue\"");

        let parsed: TicketingSystem = serde_json::from_str("\"beads\"").unwrap();
        assert_eq!(parsed, TicketingSystem::Beads);
    }

    // TicketingInfo tests

    #[test]
    fn test_ticketing_info_new() {
        let info = TicketingInfo::new(TicketingSystem::Tissue, Some("/path/.tissue".into()));
        assert_eq!(info.system, TicketingSystem::Tissue);
        assert_eq!(
            info.marker_path,
            Some(std::path::PathBuf::from("/path/.tissue"))
        );
    }

    #[test]
    fn test_ticketing_info_no_marker() {
        let info = TicketingInfo::new(TicketingSystem::Session, None);
        assert_eq!(info.system, TicketingSystem::Session);
        assert!(info.marker_path.is_none());
    }

    // ClosePattern tests

    #[test]
    fn test_close_pattern_system() {
        assert_eq!(ClosePattern::TissueClose.system(), TicketingSystem::Tissue);
        assert_eq!(ClosePattern::BeadsClose.system(), TicketingSystem::Beads);
        assert_eq!(ClosePattern::BeadsComplete.system(), TicketingSystem::Beads);
    }

    // probe_tissue tests

    #[test]
    fn test_probe_tissue_found() {
        let dir = TempDir::new().unwrap();
        let tissue_dir = dir.path().join(".tissue");
        std::fs::create_dir(&tissue_dir).unwrap();

        let result = probe_tissue(dir.path());

        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.system, TicketingSystem::Tissue);
        assert_eq!(info.marker_path, Some(tissue_dir));
    }

    #[test]
    fn test_probe_tissue_not_found() {
        let dir = TempDir::new().unwrap();

        let result = probe_tissue(dir.path());

        assert!(result.is_none());
    }

    #[test]
    fn test_probe_tissue_file_not_dir() {
        let dir = TempDir::new().unwrap();
        let tissue_path = dir.path().join(".tissue");
        std::fs::write(&tissue_path, "not a directory").unwrap();

        let result = probe_tissue(dir.path());

        assert!(result.is_none());
    }

    // probe_beads tests

    #[test]
    fn test_probe_beads_found() {
        let dir = TempDir::new().unwrap();
        let beads_dir = dir.path().join(".beads");
        std::fs::create_dir(&beads_dir).unwrap();

        let result = probe_beads(dir.path());

        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.system, TicketingSystem::Beads);
        assert_eq!(info.marker_path, Some(beads_dir));
    }

    #[test]
    fn test_probe_beads_not_found() {
        let dir = TempDir::new().unwrap();

        let result = probe_beads(dir.path());

        assert!(result.is_none());
    }

    // probe_tasks tests

    #[test]
    fn test_probe_tasks_returns_none_without_opt_in() {
        // Without explicit opt-in, returns None
        let config = TicketingConfig::default();
        let result = probe_tasks(&config);
        assert!(result.is_none());
    }

    #[test]
    fn test_probe_tasks_returns_some_with_opt_in() {
        let mut config = TicketingConfig::default();
        config.overrides.insert("tasks".to_string(), true);

        let result = probe_tasks(&config);

        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.system, TicketingSystem::Tasks);
        assert!(info.marker_path.is_none());
    }

    #[test]
    fn test_probe_tasks_disabled_override() {
        let mut config = TicketingConfig::default();
        config.overrides.insert("tasks".to_string(), false);

        let result = probe_tasks(&config);
        assert!(result.is_none());
    }

    // detect_ticketing_system tests

    #[test]
    fn test_detect_tissue_first() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".tissue")).unwrap();
        std::fs::create_dir(dir.path().join(".beads")).unwrap();

        let result = detect_ticketing_system(dir.path(), None);

        // tissue should be detected first (default order)
        assert_eq!(result.system, TicketingSystem::Tissue);
    }

    #[test]
    fn test_detect_beads_when_tissue_disabled() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".tissue")).unwrap();
        std::fs::create_dir(dir.path().join(".beads")).unwrap();

        let mut config = Config::default();
        config
            .ticketing
            .overrides
            .insert("tissue".to_string(), false);

        let result = detect_ticketing_system(dir.path(), Some(&config));

        // tissue disabled, should detect beads
        assert_eq!(result.system, TicketingSystem::Beads);
    }

    #[test]
    fn test_detect_custom_order() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".tissue")).unwrap();
        std::fs::create_dir(dir.path().join(".beads")).unwrap();

        let mut config = Config::default();
        config.ticketing.discovery = vec!["beads".to_string(), "tissue".to_string()];

        let result = detect_ticketing_system(dir.path(), Some(&config));

        // beads should be detected first (custom order)
        assert_eq!(result.system, TicketingSystem::Beads);
    }

    #[test]
    fn test_detect_fallback_to_session() {
        let dir = TempDir::new().unwrap();
        // No ticketing system directories

        let result = detect_ticketing_system(dir.path(), None);

        assert_eq!(result.system, TicketingSystem::Session);
        assert!(result.marker_path.is_none());
    }

    #[test]
    fn test_detect_ignores_unknown_systems() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".tissue")).unwrap();

        let config = Config {
            ticketing: TicketingConfig {
                discovery: vec!["unknown".to_string(), "tissue".to_string()],
                overrides: HashMap::new(),
            },
            ..Config::default()
        };

        let result = detect_ticketing_system(dir.path(), Some(&config));

        // Should skip unknown and find tissue
        assert_eq!(result.system, TicketingSystem::Tissue);
    }

    #[test]
    fn test_detect_all_disabled_falls_back_to_session() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".tissue")).unwrap();
        std::fs::create_dir(dir.path().join(".beads")).unwrap();

        let config = Config {
            ticketing: TicketingConfig {
                discovery: vec!["tissue".to_string(), "beads".to_string()],
                overrides: {
                    let mut m = HashMap::new();
                    m.insert("tissue".to_string(), false);
                    m.insert("beads".to_string(), false);
                    m
                },
            },
            ..Config::default()
        };

        let result = detect_ticketing_system(dir.path(), Some(&config));

        // All disabled, fallback to session
        assert_eq!(result.system, TicketingSystem::Session);
    }

    #[test]
    fn test_detect_tasks_with_opt_in() {
        let dir = TempDir::new().unwrap();

        let mut config = Config::default();
        config.ticketing.overrides.insert("tasks".to_string(), true);

        let result = detect_ticketing_system(dir.path(), Some(&config));

        // Tasks should be detected (enabled via config)
        assert_eq!(result.system, TicketingSystem::Tasks);
        assert!(result.marker_path.is_none());
    }

    #[test]
    fn test_detect_tasks_requires_opt_in() {
        let dir = TempDir::new().unwrap();
        // No config overrides - tasks should not be detected
        let result = detect_ticketing_system(dir.path(), None);

        // Should fall back to session (tasks not enabled)
        assert_eq!(result.system, TicketingSystem::Session);
    }

    #[test]
    fn test_detect_tasks_priority_over_session() {
        let dir = TempDir::new().unwrap();

        let mut config = Config::default();
        // Enable tasks - it should be found before session fallback
        config.ticketing.overrides.insert("tasks".to_string(), true);

        let result = detect_ticketing_system(dir.path(), Some(&config));

        assert_eq!(result.system, TicketingSystem::Tasks);
    }

    #[test]
    fn test_detect_tissue_over_tasks() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".tissue")).unwrap();

        let mut config = Config::default();
        config.ticketing.overrides.insert("tasks".to_string(), true);

        let result = detect_ticketing_system(dir.path(), Some(&config));

        // tissue has higher priority by default
        assert_eq!(result.system, TicketingSystem::Tissue);
    }

    #[test]
    fn test_detect_tasks_with_custom_priority() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".tissue")).unwrap();

        let mut config = Config::default();
        // Put tasks first in discovery order
        config.ticketing.discovery = vec![
            "tasks".to_string(),
            "tissue".to_string(),
            "session".to_string(),
        ];
        config.ticketing.overrides.insert("tasks".to_string(), true);

        let result = detect_ticketing_system(dir.path(), Some(&config));

        // tasks should be detected first now
        assert_eq!(result.system, TicketingSystem::Tasks);
    }

    // match_close_command tests

    #[test]
    fn test_match_tissue_close() {
        let result = match_close_command("Bash", "tissue status grove-123 closed");
        assert_eq!(result, Some(ClosePattern::TissueClose));
    }

    #[test]
    fn test_match_tissue_close_with_whitespace() {
        let result = match_close_command("Bash", "  tissue status my-ticket closed  ");
        assert_eq!(result, Some(ClosePattern::TissueClose));
    }

    #[test]
    fn test_match_tissue_close_long_id() {
        let result = match_close_command("Bash", "tissue status grove-abc-123-def closed");
        assert_eq!(result, Some(ClosePattern::TissueClose));
    }

    #[test]
    fn test_match_beads_close() {
        let result = match_close_command("Bash", "beads close issue-456");
        assert_eq!(result, Some(ClosePattern::BeadsClose));
    }

    #[test]
    fn test_match_beads_complete() {
        let result = match_close_command("Bash", "beads complete task-789");
        assert_eq!(result, Some(ClosePattern::BeadsComplete));
    }

    #[test]
    fn test_match_no_match_wrong_tool() {
        let result = match_close_command("Read", "tissue status grove-123 closed");
        assert!(result.is_none());
    }

    #[test]
    fn test_match_no_match_wrong_command() {
        let result = match_close_command("Bash", "tissue list");
        assert!(result.is_none());
    }

    #[test]
    fn test_match_no_match_tissue_incomplete() {
        // Missing "closed" at the end
        let result = match_close_command("Bash", "tissue status grove-123");
        assert!(result.is_none());
    }

    #[test]
    fn test_match_no_match_tissue_wrong_subcommand() {
        let result = match_close_command("Bash", "tissue list grove-123 closed");
        assert!(result.is_none());
    }

    #[test]
    fn test_match_no_match_beads_incomplete() {
        // Missing ticket ID
        let result = match_close_command("Bash", "beads close");
        assert!(result.is_none());
    }

    #[test]
    fn test_match_no_match_empty_command() {
        let result = match_close_command("Bash", "");
        assert!(result.is_none());
    }

    #[test]
    fn test_match_no_match_whitespace_only() {
        let result = match_close_command("Bash", "   ");
        assert!(result.is_none());
    }

    // Edge cases

    #[test]
    fn test_detect_nonexistent_directory() {
        let result = detect_ticketing_system(Path::new("/nonexistent/path"), None);
        // Should fall back to session mode
        assert_eq!(result.system, TicketingSystem::Session);
    }

    #[test]
    fn test_match_tissue_with_extra_args() {
        // Extra arguments after "closed" - should now match (consistent with beads)
        let result = match_close_command("Bash", "tissue status grove-123 closed --verbose");
        // The pattern now checks that "closed" is at position 3, extra args are ignored
        assert_eq!(result, Some(ClosePattern::TissueClose));
    }

    #[test]
    fn test_match_beads_close_with_flags() {
        // beads close with extra arguments - still matches
        let result = match_close_command("Bash", "beads close issue-456 --force");
        assert_eq!(result, Some(ClosePattern::BeadsClose));
    }

    #[test]
    fn test_match_case_sensitive() {
        // Commands are case-sensitive
        let result = match_close_command("Bash", "TISSUE status grove-123 closed");
        assert!(result.is_none());

        let result = match_close_command("Bash", "Beads close issue-456");
        assert!(result.is_none());
    }

    // =========================================================================
    // Compound command security tests (grove-2zw7d3hz)
    // =========================================================================

    #[test]
    fn test_compound_command_and_operator() {
        // Commands joined with && should not match
        let result = match_close_command("Bash", "tissue status ticket closed && echo done");
        assert!(result.is_none(), "&& operator should prevent match");
    }

    #[test]
    fn test_compound_command_or_operator() {
        // Commands joined with || should not match
        let result = match_close_command("Bash", "tissue status ticket closed || echo failed");
        assert!(result.is_none(), "|| operator should prevent match");
    }

    #[test]
    fn test_compound_command_semicolon() {
        // Commands joined with ; should not match (closed is not last token)
        let result = match_close_command("Bash", "tissue status ticket closed; echo done");
        // Note: split_whitespace treats "closed;" as one token, so closed != "closed;"
        assert!(result.is_none(), "; operator should prevent match");
    }

    #[test]
    fn test_compound_command_pipe() {
        // Commands joined with | should not match
        let result = match_close_command("Bash", "tissue status ticket closed | tee log.txt");
        assert!(result.is_none(), "| operator should prevent match");
    }

    #[test]
    fn test_compound_command_prefix() {
        // Command prefixed with another command should not match
        let result = match_close_command("Bash", "echo hello && tissue status ticket closed");
        assert!(
            result.is_none(),
            "prefixed compound command should not match"
        );
    }

    #[test]
    fn test_subshell_command() {
        // Commands in subshell should not match (parentheses become part of token)
        let result = match_close_command("Bash", "(tissue status ticket closed)");
        assert!(
            result.is_none(),
            "subshell command should not match due to parentheses"
        );
    }

    #[test]
    fn test_simple_command_still_matches() {
        // Verify simple command still works after security checks
        let result = match_close_command("Bash", "tissue status grove-123 closed");
        assert_eq!(result, Some(ClosePattern::TissueClose));
    }

    // Beads compound command tests (consistency with tissue security checks)

    #[test]
    fn test_beads_compound_command_and_operator() {
        let result = match_close_command("Bash", "beads close issue-456 && echo done");
        assert!(result.is_none(), "&& operator should prevent beads match");
    }

    #[test]
    fn test_beads_compound_command_or_operator() {
        let result = match_close_command("Bash", "beads complete task-789 || echo failed");
        assert!(result.is_none(), "|| operator should prevent beads match");
    }

    #[test]
    fn test_beads_compound_command_pipe() {
        let result = match_close_command("Bash", "beads close issue-456 | tee log.txt");
        assert!(result.is_none(), "| operator should prevent beads match");
    }
}
