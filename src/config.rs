//! Configuration loading for Grove.
//!
//! Configuration follows a precedence chain:
//! 1. Environment variables (highest priority)
//! 2. Project config (`.grove/config.toml`)
//! 3. User config (`~/.grove/config.toml`)
//! 4. Defaults (lowest priority)
//!
//! All configuration is optional. The system runs with sensible defaults
//! when no config exists.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{FailOpen, GroveError, Result};

/// Main configuration struct for Grove.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Ticketing system discovery configuration.
    pub ticketing: TicketingConfig,
    /// Memory backend configuration.
    pub backends: BackendsConfig,
    /// Gate behavior configuration.
    pub gate: GateConfig,
    /// Passive decay configuration.
    pub decay: DecayConfig,
    /// Learning retrieval configuration.
    pub retrieval: RetrievalConfig,
    /// Circuit breaker configuration.
    pub circuit_breaker: CircuitBreakerConfig,
}

/// Ticketing system discovery configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TicketingConfig {
    /// Ordered list of ticketing systems to probe.
    pub discovery: Vec<String>,
    /// Per-system enable/disable overrides.
    pub overrides: HashMap<String, bool>,
}

impl Default for TicketingConfig {
    fn default() -> Self {
        Self {
            discovery: vec![
                "tissue".to_string(),
                "beads".to_string(),
                "tasks".to_string(),
                "session".to_string(),
            ],
            overrides: HashMap::new(),
        }
    }
}

/// Memory backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BackendsConfig {
    /// Ordered list of backends to probe.
    pub discovery: Vec<String>,
    /// Per-backend enable/disable overrides.
    pub overrides: HashMap<String, bool>,
}

impl Default for BackendsConfig {
    fn default() -> Self {
        Self {
            discovery: vec![
                "total-recall".to_string(),
                "mcp".to_string(),
                "markdown".to_string(),
            ],
            overrides: HashMap::new(),
        }
    }
}

/// Gate behavior configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GateConfig {
    /// Auto-skip configuration for trivial changes.
    pub auto_skip: AutoSkipConfig,
}

/// Auto-skip configuration for trivial changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AutoSkipConfig {
    /// Whether auto-skip is enabled.
    pub enabled: bool,
    /// Diff size (in lines) below which auto-skip applies.
    pub line_threshold: u32,
    /// Who decides whether to skip: "agent", "always", or "never".
    pub decider: String,
}

impl Default for AutoSkipConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            line_threshold: 5,
            decider: "agent".to_string(),
        }
    }
}

/// Passive decay configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DecayConfig {
    /// Days without reference before a learning is archived.
    pub passive_duration_days: u32,
    /// Hit rate above which decay is skipped.
    pub immunity_hit_rate: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            passive_duration_days: 90,
            immunity_hit_rate: 0.8,
        }
    }
}

/// Learning retrieval configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RetrievalConfig {
    /// Maximum learnings to inject per session.
    pub max_injections: u32,
    /// Retrieval strategy: "conservative", "moderate", or "aggressive".
    pub strategy: String,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            max_injections: 5,
            strategy: "moderate".to_string(),
        }
    }
}

/// Circuit breaker configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CircuitBreakerConfig {
    /// Maximum blocks before forced approve.
    pub max_blocks: u32,
    /// Cooldown in seconds before breaker resets.
    pub cooldown_seconds: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            max_blocks: 3,
            cooldown_seconds: 300,
        }
    }
}

impl Config {
    /// Load configuration with full precedence chain.
    ///
    /// Precedence (highest to lowest):
    /// 1. Environment variables
    /// 2. Project config (`.grove/config.toml` in cwd)
    /// 3. User config (`~/.grove/config.toml`)
    /// 4. Defaults
    pub fn load() -> Self {
        Self::load_from_cwd(&env::current_dir().unwrap_or_default())
    }

    /// Load configuration with a specific working directory.
    pub fn load_from_cwd(cwd: &Path) -> Self {
        // Start with defaults
        let mut config = Config::default();

        // Layer 4 → 3: Apply user config
        if let Some(user_config) = Self::load_user_config() {
            config = config.merge(user_config);
        }

        // Layer 3 → 2: Apply project config
        if let Some(project_config) = Self::load_project_config(cwd) {
            config = config.merge(project_config);
        }

        // Layer 2 → 1: Apply environment variables
        config.apply_env_overrides();

        config
    }

    /// Load user config from `~/.grove/config.toml`.
    fn load_user_config() -> Option<Config> {
        let home = grove_home()?;
        let config_path = home.join("config.toml");
        Self::load_from_file(&config_path).ok()
    }

    /// Load project config from `.grove/config.toml` in the given directory.
    fn load_project_config(cwd: &Path) -> Option<Config> {
        let config_path = cwd.join(".grove").join("config.toml");
        Self::load_from_file(&config_path).ok()
    }

    /// Load config from a specific file path.
    fn load_from_file(path: &Path) -> Result<Config> {
        let content = fs::read_to_string(path).map_err(|e| GroveError::storage(path, e))?;
        toml::from_str(&content).map_err(|e| GroveError::config(e.to_string()))
    }

    /// Apply environment variable overrides.
    fn apply_env_overrides(&mut self) {
        // GROVE_MAX_BLOCKS
        if let Ok(val) = env::var("GROVE_MAX_BLOCKS") {
            if let Ok(n) = val.parse() {
                self.circuit_breaker.max_blocks = n;
            }
        }

        // GROVE_COOLDOWN_SECONDS
        if let Ok(val) = env::var("GROVE_COOLDOWN_SECONDS") {
            if let Ok(n) = val.parse() {
                self.circuit_breaker.cooldown_seconds = n;
            }
        }

        // GROVE_MAX_INJECTIONS
        if let Ok(val) = env::var("GROVE_MAX_INJECTIONS") {
            if let Ok(n) = val.parse() {
                self.retrieval.max_injections = n;
            }
        }

        // GROVE_RETRIEVAL_STRATEGY
        if let Ok(val) = env::var("GROVE_RETRIEVAL_STRATEGY") {
            self.retrieval.strategy = val;
        }

        // GROVE_AUTO_SKIP_ENABLED
        if let Ok(val) = env::var("GROVE_AUTO_SKIP_ENABLED") {
            self.gate.auto_skip.enabled = val == "true" || val == "1";
        }

        // GROVE_AUTO_SKIP_THRESHOLD
        if let Ok(val) = env::var("GROVE_AUTO_SKIP_THRESHOLD") {
            if let Ok(n) = val.parse() {
                self.gate.auto_skip.line_threshold = n;
            }
        }

        // GROVE_AUTO_SKIP_DECIDER
        if let Ok(val) = env::var("GROVE_AUTO_SKIP_DECIDER") {
            self.gate.auto_skip.decider = val;
        }

        // GROVE_DECAY_DAYS
        if let Ok(val) = env::var("GROVE_DECAY_DAYS") {
            if let Ok(n) = val.parse() {
                self.decay.passive_duration_days = n;
            }
        }

        // GROVE_DECAY_IMMUNITY_RATE
        if let Ok(val) = env::var("GROVE_DECAY_IMMUNITY_RATE") {
            if let Ok(n) = val.parse() {
                self.decay.immunity_hit_rate = n;
            }
        }
    }

    /// Merge another config into this one.
    ///
    /// The `other` config takes precedence. All non-default fields from `other`
    /// are applied to `self`, enabling proper layering of the precedence chain.
    /// This is field-by-field merging, not section-by-section, which ensures
    /// that explicit defaults in one config do not block overrides from another.
    fn merge(mut self, other: Config) -> Self {
        // Ticketing: merge discovery list and overrides
        // Discovery list: take from other if it was customized
        if other.ticketing.discovery != TicketingConfig::default().discovery {
            self.ticketing.discovery = other.ticketing.discovery;
        }
        // Overrides: always merge (additively)
        for (k, v) in other.ticketing.overrides {
            self.ticketing.overrides.insert(k, v);
        }

        // Backends: merge discovery list and overrides
        if other.backends.discovery != BackendsConfig::default().discovery {
            self.backends.discovery = other.backends.discovery;
        }
        for (k, v) in other.backends.overrides {
            self.backends.overrides.insert(k, v);
        }

        // Gate: merge auto_skip settings field by field
        // Take each non-default value from other
        let default_auto_skip = AutoSkipConfig::default();
        if other.gate.auto_skip.enabled != default_auto_skip.enabled {
            self.gate.auto_skip.enabled = other.gate.auto_skip.enabled;
        }
        if other.gate.auto_skip.line_threshold != default_auto_skip.line_threshold {
            self.gate.auto_skip.line_threshold = other.gate.auto_skip.line_threshold;
        }
        if other.gate.auto_skip.decider != default_auto_skip.decider {
            self.gate.auto_skip.decider = other.gate.auto_skip.decider;
        }

        // Decay: merge field by field
        let default_decay = DecayConfig::default();
        if other.decay.passive_duration_days != default_decay.passive_duration_days {
            self.decay.passive_duration_days = other.decay.passive_duration_days;
        }
        if other.decay.immunity_hit_rate != default_decay.immunity_hit_rate {
            self.decay.immunity_hit_rate = other.decay.immunity_hit_rate;
        }

        // Retrieval: merge field by field
        let default_retrieval = RetrievalConfig::default();
        if other.retrieval.max_injections != default_retrieval.max_injections {
            self.retrieval.max_injections = other.retrieval.max_injections;
        }
        if other.retrieval.strategy != default_retrieval.strategy {
            self.retrieval.strategy = other.retrieval.strategy;
        }

        // Circuit breaker: merge field by field
        let default_cb = CircuitBreakerConfig::default();
        if other.circuit_breaker.max_blocks != default_cb.max_blocks {
            self.circuit_breaker.max_blocks = other.circuit_breaker.max_blocks;
        }
        if other.circuit_breaker.cooldown_seconds != default_cb.cooldown_seconds {
            self.circuit_breaker.cooldown_seconds = other.circuit_breaker.cooldown_seconds;
        }

        self
    }

    /// Load config with fail-open behavior.
    ///
    /// If loading fails for any reason, returns defaults.
    pub fn load_fail_open() -> Self {
        let result: Result<Self> = Ok(Self::load());
        result.fail_open_default("loading config")
    }
}

/// Get the Grove home directory.
///
/// Checks `GROVE_HOME` environment variable first, then falls back to
/// `~/.grove`.
pub fn grove_home() -> Option<PathBuf> {
    // Check GROVE_HOME env var first
    if let Ok(home) = env::var("GROVE_HOME") {
        return Some(PathBuf::from(home));
    }

    // Fall back to ~/.grove
    dirs::home_dir().map(|h| h.join(".grove"))
}

/// Get the sessions directory.
///
/// Returns `<grove_home>/sessions/`.
pub fn sessions_dir() -> Option<PathBuf> {
    grove_home().map(|h| h.join("sessions"))
}

/// Get the stats cache path.
///
/// Returns `<grove_home>/stats-cache.json`.
pub fn stats_cache_path() -> Option<PathBuf> {
    grove_home().map(|h| h.join("stats-cache.json"))
}

/// Get the project grove directory for a given working directory.
///
/// Returns `<cwd>/.grove/`.
pub fn project_grove_dir(cwd: &Path) -> PathBuf {
    cwd.join(".grove")
}

/// Get the project learnings file path.
///
/// Returns `<cwd>/.grove/learnings.md`.
pub fn project_learnings_path(cwd: &Path) -> PathBuf {
    project_grove_dir(cwd).join("learnings.md")
}

/// Get the project stats log path.
///
/// Returns `<cwd>/.grove/stats.log`.
pub fn project_stats_log_path(cwd: &Path) -> PathBuf {
    project_grove_dir(cwd).join("stats.log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let config = Config::default();

        // Ticketing defaults
        assert_eq!(
            config.ticketing.discovery,
            vec!["tissue", "beads", "tasks", "session"]
        );
        assert!(config.ticketing.overrides.is_empty());

        // Backends defaults
        assert_eq!(
            config.backends.discovery,
            vec!["total-recall", "mcp", "markdown"]
        );
        assert!(config.backends.overrides.is_empty());

        // Gate defaults
        assert!(config.gate.auto_skip.enabled);
        assert_eq!(config.gate.auto_skip.line_threshold, 5);
        assert_eq!(config.gate.auto_skip.decider, "agent");

        // Decay defaults
        assert_eq!(config.decay.passive_duration_days, 90);
        assert!((config.decay.immunity_hit_rate - 0.8).abs() < f64::EPSILON);

        // Retrieval defaults
        assert_eq!(config.retrieval.max_injections, 5);
        assert_eq!(config.retrieval.strategy, "moderate");

        // Circuit breaker defaults
        assert_eq!(config.circuit_breaker.max_blocks, 3);
        assert_eq!(config.circuit_breaker.cooldown_seconds, 300);
    }

    #[test]
    fn test_load_from_file() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");

        let toml_content = r#"
[circuit_breaker]
max_blocks = 5
cooldown_seconds = 600

[retrieval]
max_injections = 10
strategy = "aggressive"
"#;

        fs::write(&config_path, toml_content).unwrap();

        let config = Config::load_from_file(&config_path).unwrap();

        assert_eq!(config.circuit_breaker.max_blocks, 5);
        assert_eq!(config.circuit_breaker.cooldown_seconds, 600);
        assert_eq!(config.retrieval.max_injections, 10);
        assert_eq!(config.retrieval.strategy, "aggressive");

        // Other fields should be defaults
        assert!(config.gate.auto_skip.enabled);
        assert_eq!(config.decay.passive_duration_days, 90);
    }

    #[test]
    fn test_load_from_file_missing() {
        let result = Config::load_from_file(Path::new("/nonexistent/config.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_load_from_file_invalid_toml() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, "this is not valid toml [[[").unwrap();

        let result = Config::load_from_file(&config_path);
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn test_project_config_precedence() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        let config_path = grove_dir.join("config.toml");
        let toml_content = r#"
[circuit_breaker]
max_blocks = 7
"#;
        fs::write(&config_path, toml_content).unwrap();

        let config = Config::load_from_cwd(dir.path());

        // Project config overrides default
        assert_eq!(config.circuit_breaker.max_blocks, 7);
        // Other defaults still apply
        assert_eq!(config.circuit_breaker.cooldown_seconds, 300);
    }

    #[test]
    #[serial]
    fn test_env_var_precedence() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        let config_path = grove_dir.join("config.toml");
        let toml_content = r#"
[circuit_breaker]
max_blocks = 7
"#;
        fs::write(&config_path, toml_content).unwrap();

        // Set env var to override
        env::set_var("GROVE_MAX_BLOCKS", "10");

        let config = Config::load_from_cwd(dir.path());

        // Env var takes precedence over project config
        assert_eq!(config.circuit_breaker.max_blocks, 10);

        // Clean up
        env::remove_var("GROVE_MAX_BLOCKS");
    }

    #[test]
    #[serial]
    fn test_env_var_overrides() {
        env::set_var("GROVE_MAX_BLOCKS", "15");
        env::set_var("GROVE_COOLDOWN_SECONDS", "600");
        env::set_var("GROVE_MAX_INJECTIONS", "20");
        env::set_var("GROVE_RETRIEVAL_STRATEGY", "aggressive");
        env::set_var("GROVE_AUTO_SKIP_ENABLED", "false");
        env::set_var("GROVE_AUTO_SKIP_THRESHOLD", "10");
        env::set_var("GROVE_AUTO_SKIP_DECIDER", "never");
        env::set_var("GROVE_DECAY_DAYS", "180");
        env::set_var("GROVE_DECAY_IMMUNITY_RATE", "0.9");

        let mut config = Config::default();
        config.apply_env_overrides();

        assert_eq!(config.circuit_breaker.max_blocks, 15);
        assert_eq!(config.circuit_breaker.cooldown_seconds, 600);
        assert_eq!(config.retrieval.max_injections, 20);
        assert_eq!(config.retrieval.strategy, "aggressive");
        assert!(!config.gate.auto_skip.enabled);
        assert_eq!(config.gate.auto_skip.line_threshold, 10);
        assert_eq!(config.gate.auto_skip.decider, "never");
        assert_eq!(config.decay.passive_duration_days, 180);
        assert!((config.decay.immunity_hit_rate - 0.9).abs() < f64::EPSILON);

        // Clean up
        env::remove_var("GROVE_MAX_BLOCKS");
        env::remove_var("GROVE_COOLDOWN_SECONDS");
        env::remove_var("GROVE_MAX_INJECTIONS");
        env::remove_var("GROVE_RETRIEVAL_STRATEGY");
        env::remove_var("GROVE_AUTO_SKIP_ENABLED");
        env::remove_var("GROVE_AUTO_SKIP_THRESHOLD");
        env::remove_var("GROVE_AUTO_SKIP_DECIDER");
        env::remove_var("GROVE_DECAY_DAYS");
        env::remove_var("GROVE_DECAY_IMMUNITY_RATE");
    }

    #[test]
    fn test_merge_configs() {
        let base = Config::default();

        let override_config = Config {
            circuit_breaker: CircuitBreakerConfig {
                max_blocks: 10,
                cooldown_seconds: 600,
            },
            ..Config::default()
        };

        let merged = base.merge(override_config);

        assert_eq!(merged.circuit_breaker.max_blocks, 10);
        assert_eq!(merged.circuit_breaker.cooldown_seconds, 600);
        // Other sections unchanged
        assert!(merged.gate.auto_skip.enabled);
    }

    #[test]
    fn test_ticketing_overrides() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        let config_path = grove_dir.join("config.toml");
        let toml_content = r#"
[ticketing]
discovery = ["tissue", "session"]

[ticketing.overrides]
beads = false
"#;
        fs::write(&config_path, toml_content).unwrap();

        let config = Config::load_from_cwd(dir.path());

        assert_eq!(config.ticketing.discovery, vec!["tissue", "session"]);
        assert_eq!(config.ticketing.overrides.get("beads"), Some(&false));
    }

    #[test]
    fn test_backends_overrides() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        let config_path = grove_dir.join("config.toml");
        let toml_content = r#"
[backends]
discovery = ["markdown"]

[backends.overrides]
mcp = false
"#;
        fs::write(&config_path, toml_content).unwrap();

        let config = Config::load_from_cwd(dir.path());

        assert_eq!(config.backends.discovery, vec!["markdown"]);
        assert_eq!(config.backends.overrides.get("mcp"), Some(&false));
    }

    #[test]
    #[serial]
    fn test_grove_home_with_env() {
        let dir = TempDir::new().unwrap();
        env::set_var("GROVE_HOME", dir.path().to_str().unwrap());

        let home = grove_home().unwrap();
        assert_eq!(home, dir.path());

        env::remove_var("GROVE_HOME");
    }

    #[test]
    #[serial]
    fn test_grove_home_fallback() {
        env::remove_var("GROVE_HOME");

        let home = grove_home();
        // Should return Some(~/.grove) in most environments
        assert!(home.is_some());
        assert!(home.unwrap().ends_with(".grove"));
    }

    #[test]
    #[serial]
    fn test_sessions_dir() {
        let dir = TempDir::new().unwrap();
        env::set_var("GROVE_HOME", dir.path().to_str().unwrap());

        let sessions = sessions_dir().unwrap();
        assert_eq!(sessions, dir.path().join("sessions"));

        env::remove_var("GROVE_HOME");
    }

    #[test]
    #[serial]
    fn test_stats_cache_path() {
        let dir = TempDir::new().unwrap();
        env::set_var("GROVE_HOME", dir.path().to_str().unwrap());

        let cache = stats_cache_path().unwrap();
        assert_eq!(cache, dir.path().join("stats-cache.json"));

        env::remove_var("GROVE_HOME");
    }

    #[test]
    fn test_project_paths() {
        let cwd = Path::new("/some/project");

        assert_eq!(
            project_grove_dir(cwd),
            PathBuf::from("/some/project/.grove")
        );
        assert_eq!(
            project_learnings_path(cwd),
            PathBuf::from("/some/project/.grove/learnings.md")
        );
        assert_eq!(
            project_stats_log_path(cwd),
            PathBuf::from("/some/project/.grove/stats.log")
        );
    }

    #[test]
    #[serial]
    fn test_load_fail_open() {
        // Even with no config files, should return defaults
        let config = Config::load_fail_open();
        assert_eq!(config.circuit_breaker.max_blocks, 3);
    }

    #[test]
    fn test_full_toml_roundtrip() {
        let config = Config {
            ticketing: TicketingConfig {
                discovery: vec!["tissue".to_string(), "session".to_string()],
                overrides: {
                    let mut m = HashMap::new();
                    m.insert("beads".to_string(), false);
                    m
                },
            },
            backends: BackendsConfig {
                discovery: vec!["markdown".to_string()],
                overrides: HashMap::new(),
            },
            gate: GateConfig {
                auto_skip: AutoSkipConfig {
                    enabled: false,
                    line_threshold: 10,
                    decider: "never".to_string(),
                },
            },
            decay: DecayConfig {
                passive_duration_days: 60,
                immunity_hit_rate: 0.9,
            },
            retrieval: RetrievalConfig {
                max_injections: 10,
                strategy: "conservative".to_string(),
            },
            circuit_breaker: CircuitBreakerConfig {
                max_blocks: 5,
                cooldown_seconds: 120,
            },
        };

        let toml_str = toml::to_string(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(config, parsed);
    }

    #[test]
    fn test_partial_toml_uses_defaults() {
        let toml_content = r#"
[circuit_breaker]
max_blocks = 10
"#;

        let config: Config = toml::from_str(toml_content).unwrap();

        // Specified value
        assert_eq!(config.circuit_breaker.max_blocks, 10);
        // Default for unspecified field in same section
        assert_eq!(config.circuit_breaker.cooldown_seconds, 300);
        // Defaults for unspecified sections
        assert!(config.gate.auto_skip.enabled);
        assert_eq!(config.decay.passive_duration_days, 90);
    }

    #[test]
    #[serial]
    fn test_auto_skip_enabled_parsing() {
        // Test "true" string
        env::set_var("GROVE_AUTO_SKIP_ENABLED", "true");
        let mut config = Config::default();
        config.gate.auto_skip.enabled = false; // Start with false
        config.apply_env_overrides();
        assert!(config.gate.auto_skip.enabled);
        env::remove_var("GROVE_AUTO_SKIP_ENABLED");

        // Test "1" string
        env::set_var("GROVE_AUTO_SKIP_ENABLED", "1");
        let mut config = Config::default();
        config.gate.auto_skip.enabled = false;
        config.apply_env_overrides();
        assert!(config.gate.auto_skip.enabled);
        env::remove_var("GROVE_AUTO_SKIP_ENABLED");

        // Test "false" string
        env::set_var("GROVE_AUTO_SKIP_ENABLED", "false");
        let mut config = Config::default();
        config.apply_env_overrides();
        assert!(!config.gate.auto_skip.enabled);
        env::remove_var("GROVE_AUTO_SKIP_ENABLED");
    }

    #[test]
    fn test_merge_field_by_field_preserves_non_default_values() {
        // This test verifies that the merge function properly merges field-by-field,
        // ensuring that non-default values from either config are preserved.

        // Create a "base" config with some non-default values
        let base = Config {
            gate: GateConfig {
                auto_skip: AutoSkipConfig {
                    enabled: true,                // same as default
                    line_threshold: 20,           // different from default (5)
                    decider: "agent".to_string(), // same as default
                },
            },
            decay: DecayConfig {
                passive_duration_days: 90, // same as default
                immunity_hit_rate: 0.8,    // same as default
            },
            ..Config::default()
        };

        // Create an "override" config with different non-default values
        let override_config = Config {
            gate: GateConfig {
                auto_skip: AutoSkipConfig {
                    enabled: false,               // different from default
                    line_threshold: 5,            // same as default
                    decider: "never".to_string(), // different from default
                },
            },
            decay: DecayConfig {
                passive_duration_days: 180, // different from default
                immunity_hit_rate: 0.8,     // same as default
            },
            ..Config::default()
        };

        let merged = base.merge(override_config);

        // enabled: override_config has false (non-default), should take precedence
        assert!(!merged.gate.auto_skip.enabled);

        // line_threshold: override_config has default (5), so base's 20 should be preserved
        assert_eq!(merged.gate.auto_skip.line_threshold, 20);

        // decider: override_config has "never" (non-default), should take precedence
        assert_eq!(merged.gate.auto_skip.decider, "never");

        // passive_duration_days: override_config has 180 (non-default), should take precedence
        assert_eq!(merged.decay.passive_duration_days, 180);

        // immunity_hit_rate: both have default, base's value should remain
        assert!((merged.decay.immunity_hit_rate - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn test_merge_with_explicit_defaults_does_not_block_overrides() {
        // This tests the specific bug: if user config sets values to defaults,
        // project config overrides should still apply.

        // Simulate user config that explicitly sets everything to defaults
        let user_config = Config {
            gate: GateConfig {
                auto_skip: AutoSkipConfig::default(),
            },
            ..Config::default()
        };

        // Simulate project config that only sets line_threshold
        let project_config = Config {
            gate: GateConfig {
                auto_skip: AutoSkipConfig {
                    enabled: true,                // same as default
                    line_threshold: 10,           // different from default
                    decider: "agent".to_string(), // same as default
                },
            },
            ..Config::default()
        };

        // Start with defaults, merge user, then project
        let mut config = Config::default();
        config = config.merge(user_config);
        config = config.merge(project_config);

        // Project's line_threshold should have been applied
        assert_eq!(config.gate.auto_skip.line_threshold, 10);
        // Other defaults should remain
        assert!(config.gate.auto_skip.enabled);
        assert_eq!(config.gate.auto_skip.decider, "agent");
    }
}
