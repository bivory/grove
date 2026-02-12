//! Memory backend detection for Grove.
//!
//! Probes the project directory to detect available memory backends.
//! The discovery order is configurable via the Grove config file.
//!
//! Supported backends (Stage 1):
//! - **config**: Explicit backend declared in `.grove/config.toml`
//! - **markdown**: Built-in fallback (always available)
//!
//! Stage 2 backends (not yet implemented):
//! - **total-recall**: Total Recall memory system
//! - **mcp**: MCP memory server

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::backends::{FallbackBackend, MarkdownBackend, MemoryBackend, TotalRecallBackend};
use crate::config::{project_grove_dir, project_learnings_path, BackendsConfig, Config};
use crate::error::Result;

/// Backend type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendType {
    /// Explicit configuration from `.grove/config.toml`.
    Config,
    /// Total Recall memory system (Stage 2).
    TotalRecall,
    /// MCP memory server (Stage 2).
    Mcp,
    /// Built-in markdown backend (always available).
    Markdown,
}

impl BackendType {
    /// Get the backend name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::TotalRecall => "total-recall",
            Self::Mcp => "mcp",
            Self::Markdown => "markdown",
        }
    }

    /// Parse a backend name from a string.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "config" => Some(Self::Config),
            "total-recall" | "totalrecall" | "total_recall" => Some(Self::TotalRecall),
            "mcp" => Some(Self::Mcp),
            "markdown" | "md" => Some(Self::Markdown),
            _ => None,
        }
    }
}

impl std::fmt::Display for BackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Information about a detected backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    /// The detected backend type.
    pub backend_type: BackendType,
    /// Path to the backend's storage location (if applicable).
    pub path: Option<PathBuf>,
    /// Whether the backend is the primary (first detected) backend.
    pub is_primary: bool,
}

impl BackendInfo {
    /// Create a new backend info.
    pub fn new(backend_type: BackendType, path: Option<PathBuf>, is_primary: bool) -> Self {
        Self {
            backend_type,
            path,
            is_primary,
        }
    }
}

/// Detect available memory backends.
///
/// Probes in the configured order, returning all detected backends.
/// The first detected backend is marked as primary.
///
/// # Arguments
///
/// * `cwd` - The project's working directory to probe.
/// * `config` - Optional configuration for discovery order and overrides.
///
/// # Returns
///
/// A vector of detected backends, with the primary backend first.
pub fn detect_backends(cwd: &Path, config: Option<&Config>) -> Vec<BackendInfo> {
    let backends_config = config.map(|c| &c.backends).cloned().unwrap_or_default();

    detect_with_config(cwd, &backends_config)
}

/// Detect backends with specific config.
fn detect_with_config(cwd: &Path, config: &BackendsConfig) -> Vec<BackendInfo> {
    let mut backends = Vec::new();

    for backend_name in &config.discovery {
        // Check if the backend is disabled via overrides
        if let Some(false) = config.overrides.get(backend_name) {
            continue;
        }

        // Parse the backend name
        let Some(backend_type) = BackendType::parse(backend_name) else {
            continue;
        };

        // Probe for the backend
        if let Some(mut info) = probe_backend(cwd, backend_type) {
            info.is_primary = backends.is_empty();
            backends.push(info);
        }
    }

    // Ensure at least markdown is available as fallback
    if backends.is_empty() {
        let info = BackendInfo::new(
            BackendType::Markdown,
            Some(project_learnings_path(cwd)),
            true,
        );
        backends.push(info);
    }

    backends
}

/// Probe for a specific backend type.
fn probe_backend(cwd: &Path, backend_type: BackendType) -> Option<BackendInfo> {
    match backend_type {
        BackendType::Config => probe_config(cwd),
        BackendType::TotalRecall => probe_total_recall(cwd),
        BackendType::Mcp => probe_mcp(cwd),
        BackendType::Markdown => probe_markdown(cwd),
    }
}

/// Probe for an explicitly configured backend.
///
/// Checks for `[backends] primary` in `.grove/config.toml` and returns
/// the corresponding backend type if valid.
fn probe_config(cwd: &Path) -> Option<BackendInfo> {
    let config_path = project_grove_dir(cwd).join("config.toml");

    if !config_path.is_file() {
        return None;
    }

    // Read and parse the config to check for explicit backend
    let content = fs::read_to_string(&config_path).ok()?;
    let config: toml::Value = toml::from_str(&content).ok()?;

    // Check for backends.primary key
    let backends = config.get("backends")?;
    let primary = backends.get("primary")?;

    // Parse the primary value as a backend type
    let backend_name = primary.as_str()?;
    let backend_type = BackendType::parse(backend_name)?;

    // Return the actual configured backend type, not BackendType::Config
    // This allows users to explicitly configure their backend
    Some(BackendInfo::new(backend_type, Some(config_path), false))
}

/// Probe for Total Recall memory system.
///
/// Detects Total Recall by checking for:
/// - `memory/` directory exists
/// - AND either `rules/total-recall.md` OR `.claude/rules/total-recall.md` exists
fn probe_total_recall(cwd: &Path) -> Option<BackendInfo> {
    let memory_dir = cwd.join("memory");
    let rules_v1 = cwd.join("rules/total-recall.md");
    let rules_v2 = cwd.join(".claude/rules/total-recall.md");

    if memory_dir.is_dir() && (rules_v1.is_file() || rules_v2.is_file()) {
        Some(BackendInfo::new(
            BackendType::TotalRecall,
            Some(memory_dir),
            false,
        ))
    } else {
        None
    }
}

/// Probe for MCP memory server.
///
/// Stage 2 implementation - currently returns None.
fn probe_mcp(_cwd: &Path) -> Option<BackendInfo> {
    // MCP detection deferred to Stage 2
    // Would check for MCP server registration
    None
}

/// Probe for the built-in markdown backend.
///
/// The markdown backend is always available. This function checks whether
/// the learnings file already exists.
pub fn probe_markdown(cwd: &Path) -> Option<BackendInfo> {
    let learnings_path = project_learnings_path(cwd);

    // Markdown is always available, but we note if file exists
    Some(BackendInfo::new(
        BackendType::Markdown,
        Some(learnings_path),
        false,
    ))
}

/// Create the default backend structure.
///
/// Scaffolds the `.grove/` directory with:
/// - `learnings.md` - Empty learnings file
/// - `stats.log` - Empty stats log
///
/// # Arguments
///
/// * `cwd` - The project's working directory.
///
/// # Returns
///
/// The path to the created learnings file.
pub fn create_default_backend(cwd: &Path) -> Result<PathBuf> {
    let grove_dir = project_grove_dir(cwd);
    let learnings_path = project_learnings_path(cwd);
    let stats_path = crate::config::project_stats_log_path(cwd);

    // Create .grove directory if it doesn't exist
    if !grove_dir.exists() {
        fs::create_dir_all(&grove_dir)
            .map_err(|e| crate::error::GroveError::storage(&grove_dir, e))?;
    }

    // Create learnings.md if it doesn't exist
    if !learnings_path.exists() {
        let header = "# Grove Learnings\n\n\
            This file contains compound learnings captured during development.\n\
            Each learning is a structured reflection on patterns, pitfalls,\n\
            conventions, and insights discovered while working on this project.\n\n\
            ---\n\n";
        fs::write(&learnings_path, header)
            .map_err(|e| crate::error::GroveError::storage(&learnings_path, e))?;
    }

    // Create stats.log if it doesn't exist
    if !stats_path.exists() {
        fs::write(&stats_path, "")
            .map_err(|e| crate::error::GroveError::storage(&stats_path, e))?;
    }

    Ok(learnings_path)
}

/// Create the primary memory backend based on detected backends.
///
/// This is the main entry point for multi-backend routing. It:
/// 1. Detects available backends in the project
/// 2. Selects the primary backend (first detected in discovery order)
/// 3. Creates and returns the appropriate backend instance
///
/// **Scope routing** happens inside each backend:
/// - Project/Team → configured storage location
/// - Personal → `~/.grove/personal-learnings.md`
/// - Ephemeral → daily log (Total Recall) or discarded (Markdown)
///
/// # Arguments
///
/// * `cwd` - The project's working directory
/// * `config` - Optional configuration for discovery and overrides
///
/// # Returns
///
/// A boxed memory backend ready for use.
pub fn create_primary_backend(cwd: &Path, config: Option<&Config>) -> Box<dyn MemoryBackend> {
    let backends = detect_backends(cwd, config);

    // Find the primary backend
    let primary = backends.into_iter().find(|b| b.is_primary);

    match primary {
        Some(info) => match info.backend_type {
            BackendType::TotalRecall => {
                // Total Recall backend with markdown fallback
                // If Total Recall write fails, fall back to markdown
                let memory_dir = info.path.unwrap_or_else(|| cwd.join("memory"));
                let tr_backend = Box::new(TotalRecallBackend::new(&memory_dir, cwd));
                let md_path = project_learnings_path(cwd);
                let md_backend = Box::new(MarkdownBackend::new(&md_path));
                Box::new(FallbackBackend::new(tr_backend, md_backend))
            }
            BackendType::Config | BackendType::Mcp => {
                // MCP support is Stage 2, falls back to markdown
                // Config should no longer be returned by probe_config (it returns the actual type)
                // but kept for exhaustiveness and future compatibility
                let path = project_learnings_path(cwd);
                Box::new(MarkdownBackend::new(&path))
            }
            BackendType::Markdown => {
                let path = info.path.unwrap_or_else(|| project_learnings_path(cwd));
                Box::new(MarkdownBackend::new(&path))
            }
        },
        None => {
            // No backends detected (shouldn't happen, but fallback to markdown)
            let path = project_learnings_path(cwd);
            Box::new(MarkdownBackend::new(&path))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    // BackendType tests

    #[test]
    fn test_backend_type_as_str() {
        assert_eq!(BackendType::Config.as_str(), "config");
        assert_eq!(BackendType::TotalRecall.as_str(), "total-recall");
        assert_eq!(BackendType::Mcp.as_str(), "mcp");
        assert_eq!(BackendType::Markdown.as_str(), "markdown");
    }

    #[test]
    fn test_backend_type_parse() {
        assert_eq!(BackendType::parse("config"), Some(BackendType::Config));
        assert_eq!(
            BackendType::parse("total-recall"),
            Some(BackendType::TotalRecall)
        );
        assert_eq!(
            BackendType::parse("totalrecall"),
            Some(BackendType::TotalRecall)
        );
        assert_eq!(
            BackendType::parse("total_recall"),
            Some(BackendType::TotalRecall)
        );
        assert_eq!(BackendType::parse("mcp"), Some(BackendType::Mcp));
        assert_eq!(BackendType::parse("markdown"), Some(BackendType::Markdown));
        assert_eq!(BackendType::parse("md"), Some(BackendType::Markdown));
        assert_eq!(BackendType::parse("unknown"), None);
    }

    #[test]
    fn test_backend_type_parse_case_insensitive() {
        assert_eq!(BackendType::parse("CONFIG"), Some(BackendType::Config));
        assert_eq!(BackendType::parse("MARKDOWN"), Some(BackendType::Markdown));
        assert_eq!(
            BackendType::parse("Total-Recall"),
            Some(BackendType::TotalRecall)
        );
    }

    #[test]
    fn test_backend_type_display() {
        assert_eq!(format!("{}", BackendType::Config), "config");
        assert_eq!(format!("{}", BackendType::Markdown), "markdown");
    }

    #[test]
    fn test_backend_type_serialization() {
        let backend = BackendType::Markdown;
        let json = serde_json::to_string(&backend).unwrap();
        assert_eq!(json, "\"markdown\"");

        let parsed: BackendType = serde_json::from_str("\"total_recall\"").unwrap();
        assert_eq!(parsed, BackendType::TotalRecall);
    }

    // BackendInfo tests

    #[test]
    fn test_backend_info_new() {
        let info = BackendInfo::new(
            BackendType::Markdown,
            Some("/path/learnings.md".into()),
            true,
        );
        assert_eq!(info.backend_type, BackendType::Markdown);
        assert_eq!(info.path, Some(PathBuf::from("/path/learnings.md")));
        assert!(info.is_primary);
    }

    #[test]
    fn test_backend_info_no_path() {
        let info = BackendInfo::new(BackendType::Mcp, None, false);
        assert_eq!(info.backend_type, BackendType::Mcp);
        assert!(info.path.is_none());
        assert!(!info.is_primary);
    }

    // probe_markdown tests

    #[test]
    fn test_probe_markdown_always_available() {
        let dir = TempDir::new().unwrap();

        let result = probe_markdown(dir.path());

        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.backend_type, BackendType::Markdown);
        assert!(info.path.is_some());
        assert!(info.path.unwrap().ends_with("learnings.md"));
    }

    #[test]
    fn test_probe_markdown_with_existing_file() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();
        fs::write(grove_dir.join("learnings.md"), "# Learnings").unwrap();

        let result = probe_markdown(dir.path());

        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.backend_type, BackendType::Markdown);
    }

    // probe_total_recall tests

    #[test]
    fn test_probe_total_recall_with_memory_and_rules_v1() {
        let dir = TempDir::new().unwrap();

        // Create memory/ directory and rules/total-recall.md
        fs::create_dir_all(dir.path().join("memory")).unwrap();
        fs::create_dir_all(dir.path().join("rules")).unwrap();
        fs::write(dir.path().join("rules/total-recall.md"), "# Total Recall").unwrap();

        let result = probe_total_recall(dir.path());

        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.backend_type, BackendType::TotalRecall);
        assert!(info.path.unwrap().ends_with("memory"));
    }

    #[test]
    fn test_probe_total_recall_with_memory_and_rules_v2() {
        let dir = TempDir::new().unwrap();

        // Create memory/ directory and .claude/rules/total-recall.md
        fs::create_dir_all(dir.path().join("memory")).unwrap();
        fs::create_dir_all(dir.path().join(".claude/rules")).unwrap();
        fs::write(
            dir.path().join(".claude/rules/total-recall.md"),
            "# Total Recall",
        )
        .unwrap();

        let result = probe_total_recall(dir.path());

        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.backend_type, BackendType::TotalRecall);
    }

    #[test]
    fn test_probe_total_recall_memory_only() {
        let dir = TempDir::new().unwrap();

        // Create only memory/ directory (no rules file)
        fs::create_dir_all(dir.path().join("memory")).unwrap();

        let result = probe_total_recall(dir.path());

        assert!(result.is_none());
    }

    #[test]
    fn test_probe_total_recall_rules_only() {
        let dir = TempDir::new().unwrap();

        // Create only rules file (no memory/ directory)
        fs::create_dir_all(dir.path().join("rules")).unwrap();
        fs::write(dir.path().join("rules/total-recall.md"), "# Total Recall").unwrap();

        let result = probe_total_recall(dir.path());

        assert!(result.is_none());
    }

    #[test]
    fn test_probe_total_recall_empty_dir() {
        let dir = TempDir::new().unwrap();

        let result = probe_total_recall(dir.path());

        assert!(result.is_none());
    }

    // probe_mcp tests

    #[test]
    fn test_probe_mcp_returns_none() {
        let dir = TempDir::new().unwrap();

        // Stage 2: always returns None
        let result = probe_mcp(dir.path());

        assert!(result.is_none());
    }

    // probe_config tests

    #[test]
    fn test_probe_config_no_config_file() {
        let dir = TempDir::new().unwrap();

        let result = probe_config(dir.path());

        assert!(result.is_none());
    }

    #[test]
    fn test_probe_config_no_backends_section() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();
        fs::write(grove_dir.join("config.toml"), "[gate]\nauto_skip = true\n").unwrap();

        let result = probe_config(dir.path());

        assert!(result.is_none());
    }

    #[test]
    fn test_probe_config_with_primary_total_recall() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();
        fs::write(
            grove_dir.join("config.toml"),
            "[backends]\nprimary = \"total-recall\"\n",
        )
        .unwrap();

        let result = probe_config(dir.path());

        assert!(result.is_some());
        let info = result.unwrap();
        // Should return the actual backend type, not BackendType::Config
        assert_eq!(info.backend_type, BackendType::TotalRecall);
    }

    #[test]
    fn test_probe_config_with_primary_markdown() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();
        fs::write(
            grove_dir.join("config.toml"),
            "[backends]\nprimary = \"markdown\"\n",
        )
        .unwrap();

        let result = probe_config(dir.path());

        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.backend_type, BackendType::Markdown);
    }

    #[test]
    fn test_probe_config_with_invalid_primary() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();
        fs::write(
            grove_dir.join("config.toml"),
            "[backends]\nprimary = \"invalid-backend\"\n",
        )
        .unwrap();

        // Invalid backend name should return None
        let result = probe_config(dir.path());
        assert!(result.is_none());
    }

    // detect_backends tests

    #[test]
    fn test_detect_backends_default() {
        let dir = TempDir::new().unwrap();

        let backends = detect_backends(dir.path(), None);

        // Should have at least markdown as fallback
        assert!(!backends.is_empty());
        assert!(backends
            .iter()
            .any(|b| b.backend_type == BackendType::Markdown));
    }

    #[test]
    fn test_detect_backends_markdown_is_primary() {
        let dir = TempDir::new().unwrap();

        let backends = detect_backends(dir.path(), None);

        // First (primary) backend should be markdown
        let primary = backends.iter().find(|b| b.is_primary);
        assert!(primary.is_some());
        assert_eq!(primary.unwrap().backend_type, BackendType::Markdown);
    }

    #[test]
    fn test_detect_backends_custom_order() {
        let dir = TempDir::new().unwrap();

        let config = Config {
            backends: BackendsConfig {
                discovery: vec!["markdown".to_string()],
                overrides: HashMap::new(),
            },
            ..Config::default()
        };

        let backends = detect_backends(dir.path(), Some(&config));

        assert_eq!(backends.len(), 1);
        assert_eq!(backends[0].backend_type, BackendType::Markdown);
        assert!(backends[0].is_primary);
    }

    #[test]
    fn test_detect_backends_disabled_markdown() {
        let dir = TempDir::new().unwrap();

        let config = Config {
            backends: BackendsConfig {
                discovery: vec!["markdown".to_string()],
                overrides: {
                    let mut m = HashMap::new();
                    m.insert("markdown".to_string(), false);
                    m
                },
            },
            ..Config::default()
        };

        let backends = detect_backends(dir.path(), Some(&config));

        // Should still have markdown as emergency fallback
        assert!(!backends.is_empty());
        assert!(backends[0].is_primary);
    }

    #[test]
    fn test_detect_backends_ignores_unknown() {
        let dir = TempDir::new().unwrap();

        let config = Config {
            backends: BackendsConfig {
                discovery: vec!["unknown".to_string(), "markdown".to_string()],
                overrides: HashMap::new(),
            },
            ..Config::default()
        };

        let backends = detect_backends(dir.path(), Some(&config));

        // Should skip unknown and find markdown
        assert_eq!(backends.len(), 1);
        assert_eq!(backends[0].backend_type, BackendType::Markdown);
    }

    // create_default_backend tests

    #[test]
    fn test_create_default_backend() {
        let dir = TempDir::new().unwrap();

        let result = create_default_backend(dir.path());

        assert!(result.is_ok());
        let path = result.unwrap();

        // Check that files were created
        assert!(path.exists());
        assert!(dir.path().join(".grove").exists());
        assert!(dir.path().join(".grove/learnings.md").exists());
        assert!(dir.path().join(".grove/stats.log").exists());
    }

    #[test]
    fn test_create_default_backend_existing_grove_dir() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        let result = create_default_backend(dir.path());

        assert!(result.is_ok());
        assert!(dir.path().join(".grove/learnings.md").exists());
    }

    #[test]
    fn test_create_default_backend_existing_files() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();
        fs::write(grove_dir.join("learnings.md"), "# My Learnings").unwrap();
        fs::write(grove_dir.join("stats.log"), "existing content").unwrap();

        let result = create_default_backend(dir.path());

        assert!(result.is_ok());

        // Existing files should not be overwritten
        let content = fs::read_to_string(grove_dir.join("learnings.md")).unwrap();
        assert_eq!(content, "# My Learnings");

        let stats_content = fs::read_to_string(grove_dir.join("stats.log")).unwrap();
        assert_eq!(stats_content, "existing content");
    }

    #[test]
    fn test_create_default_backend_learnings_header() {
        let dir = TempDir::new().unwrap();

        create_default_backend(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".grove/learnings.md")).unwrap();
        assert!(content.contains("# Grove Learnings"));
        assert!(content.contains("compound learnings"));
    }

    // Edge cases

    #[test]
    fn test_detect_backends_empty_discovery_list() {
        let dir = TempDir::new().unwrap();

        let config = Config {
            backends: BackendsConfig {
                discovery: vec![],
                overrides: HashMap::new(),
            },
            ..Config::default()
        };

        let backends = detect_backends(dir.path(), Some(&config));

        // Should still have markdown as emergency fallback
        assert!(!backends.is_empty());
        assert_eq!(backends[0].backend_type, BackendType::Markdown);
        assert!(backends[0].is_primary);
    }

    #[test]
    fn test_probe_config_invalid_toml() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();
        fs::write(grove_dir.join("config.toml"), "this is not [valid toml").unwrap();

        let result = probe_config(dir.path());

        // Should gracefully return None
        assert!(result.is_none());
    }

    // create_primary_backend tests

    #[test]
    fn test_create_primary_backend_markdown_default() {
        let dir = TempDir::new().unwrap();

        let backend = create_primary_backend(dir.path(), None);

        // Should create a working backend
        assert_eq!(backend.name(), "markdown");
        assert!(backend.ping());
    }

    #[test]
    fn test_create_primary_backend_detects_total_recall() {
        let dir = TempDir::new().unwrap();

        // Create Total Recall structure
        fs::create_dir_all(dir.path().join("memory")).unwrap();
        fs::create_dir_all(dir.path().join("rules")).unwrap();
        fs::write(dir.path().join("rules/total-recall.md"), "# Total Recall").unwrap();

        let backend = create_primary_backend(dir.path(), None);

        // Should detect and create Total Recall backend
        assert_eq!(backend.name(), "total-recall");
    }

    #[test]
    fn test_create_primary_backend_with_config_override() {
        let dir = TempDir::new().unwrap();

        // Create Total Recall structure
        fs::create_dir_all(dir.path().join("memory")).unwrap();
        fs::create_dir_all(dir.path().join("rules")).unwrap();
        fs::write(dir.path().join("rules/total-recall.md"), "# Total Recall").unwrap();

        // But disable total-recall in config
        let config = Config {
            backends: BackendsConfig {
                discovery: vec!["total-recall".to_string(), "markdown".to_string()],
                overrides: {
                    let mut m = HashMap::new();
                    m.insert("total-recall".to_string(), false);
                    m
                },
            },
            ..Config::default()
        };

        let backend = create_primary_backend(dir.path(), Some(&config));

        // Should fall back to markdown because TR is disabled
        assert_eq!(backend.name(), "markdown");
    }

    #[test]
    fn test_create_primary_backend_markdown_only_config() {
        let dir = TempDir::new().unwrap();

        // Create Total Recall structure (would normally be detected)
        fs::create_dir_all(dir.path().join("memory")).unwrap();
        fs::create_dir_all(dir.path().join("rules")).unwrap();
        fs::write(dir.path().join("rules/total-recall.md"), "# Total Recall").unwrap();

        // Config only includes markdown in discovery
        let config = Config {
            backends: BackendsConfig {
                discovery: vec!["markdown".to_string()],
                overrides: HashMap::new(),
            },
            ..Config::default()
        };

        let backend = create_primary_backend(dir.path(), Some(&config));

        // Should use markdown because it's the only one in discovery list
        assert_eq!(backend.name(), "markdown");
    }

    // Integration tests: verify backend operations work through discovery

    mod integration_tests {
        use super::*;
        use crate::backends::{SearchFilters, SearchQuery};
        use crate::core::{Confidence, LearningCategory, LearningScope, WriteGateCriterion};

        fn sample_learning() -> crate::core::CompoundLearning {
            crate::core::CompoundLearning::new(
                LearningCategory::Pattern,
                "Integration test learning",
                "This tests that discovered backends work end-to-end",
                LearningScope::Project,
                Confidence::High,
                vec![WriteGateCriterion::BehaviorChanging],
                vec!["integration".to_string(), "test".to_string()],
                "integration-test-session",
            )
        }

        #[test]
        fn test_discovered_backend_write_and_search() {
            let dir = TempDir::new().unwrap();

            // No Total Recall structure, so markdown should be used
            let config = Config::default();
            let backend = create_primary_backend(dir.path(), Some(&config));

            assert_eq!(backend.name(), "markdown");

            // Write a learning through the discovered backend
            let learning = sample_learning();
            let write_result = backend.write(&learning).unwrap();
            assert!(write_result.success);

            // Search should find it
            let results = backend
                .search(&SearchQuery::new(), &SearchFilters::default())
                .unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].learning.summary, "Integration test learning");
        }

        #[test]
        fn test_discovered_backend_list_all() {
            let dir = TempDir::new().unwrap();

            let config = Config::default();
            let backend = create_primary_backend(dir.path(), Some(&config));

            // Write two learnings
            let learning1 = sample_learning();
            backend.write(&learning1).unwrap();

            let mut learning2 = sample_learning();
            learning2.summary = "Second integration test learning".to_string();
            backend.write(&learning2).unwrap();

            // list_all should return both
            let learnings = backend.list_all().unwrap();
            assert_eq!(learnings.len(), 2);
        }

        #[test]
        fn test_discovered_backend_archive_restore() {
            let dir = TempDir::new().unwrap();

            let config = Config::default();
            let backend = create_primary_backend(dir.path(), Some(&config));

            // Write a learning
            let learning = sample_learning();
            backend.write(&learning).unwrap();

            // Archive it
            backend.archive(&learning.id).unwrap();

            // Should not appear in active-only search
            let active_results = backend
                .search(&SearchQuery::new(), &SearchFilters::active_only())
                .unwrap();
            assert_eq!(active_results.len(), 0);

            // Should appear in all search
            let all_results = backend
                .search(&SearchQuery::new(), &SearchFilters::all())
                .unwrap();
            assert_eq!(all_results.len(), 1);

            // Restore it
            backend.restore(&learning.id).unwrap();

            // Should appear in active search again
            let active_results = backend
                .search(&SearchQuery::new(), &SearchFilters::active_only())
                .unwrap();
            assert_eq!(active_results.len(), 1);
        }

        #[test]
        fn test_total_recall_backend_detected_and_functional() {
            let dir = TempDir::new().unwrap();

            // Create Total Recall structure
            fs::create_dir_all(dir.path().join("memory")).unwrap();
            fs::create_dir_all(dir.path().join("rules")).unwrap();
            fs::write(dir.path().join("rules/total-recall.md"), "# Total Recall").unwrap();

            let config = Config::default();
            let backend = create_primary_backend(dir.path(), Some(&config));

            // Should detect Total Recall
            assert_eq!(backend.name(), "total-recall");
            assert!(backend.ping());

            // Write should work (may fail-open if claude CLI unavailable)
            let learning = sample_learning();
            let write_result = backend.write(&learning);
            assert!(write_result.is_ok());
        }

        #[test]
        fn test_config_discovery_order_respected() {
            let dir = TempDir::new().unwrap();

            // Create Total Recall structure
            fs::create_dir_all(dir.path().join("memory")).unwrap();
            fs::create_dir_all(dir.path().join("rules")).unwrap();
            fs::write(dir.path().join("rules/total-recall.md"), "# Total Recall").unwrap();

            // Config puts markdown first
            let config = Config {
                backends: BackendsConfig {
                    discovery: vec!["markdown".to_string(), "total-recall".to_string()],
                    overrides: HashMap::new(),
                },
                ..Config::default()
            };

            let backend = create_primary_backend(dir.path(), Some(&config));

            // Markdown should be selected because it's first in discovery order
            assert_eq!(backend.name(), "markdown");
        }
    }
}
