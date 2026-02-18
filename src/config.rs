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

use crate::core::LearningCategory;
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
            discovery: vec!["total-recall".to_string(), "markdown".to_string()],
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
    /// Whether to count skipped sessions as dismissals for unreferenced learnings.
    /// Default is false (skip is "no signal" - doesn't affect learning quality tracking).
    #[serde(default)]
    pub skip_counts_as_dismissal: bool,
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

/// Valid values for the auto-skip decider field.
pub const VALID_DECIDERS: &[&str] = &["agent", "always", "never"];

/// Valid values for the retrieval strategy field.
pub const VALID_STRATEGIES: &[&str] = &["conservative", "moderate", "aggressive"];

impl AutoSkipConfig {
    /// Check if a decider value is valid.
    pub fn is_valid_decider(value: &str) -> bool {
        VALID_DECIDERS.contains(&value)
    }
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
    /// Hit rate above which decay is skipped (conservative: 0.3 instead of 0.8).
    pub immunity_hit_rate: f64,
    /// Minimum number of dismissals required before decay can occur.
    /// Prevents decay based on single dismissals which may be false negatives.
    #[serde(default = "default_min_dismissals")]
    pub min_dismissals_for_decay: u32,
    /// Whether to apply category-aware decay thresholds.
    /// Debugging/domain learnings expect lower hit rates than patterns.
    #[serde(default = "default_category_aware")]
    pub category_aware: bool,
}

fn default_min_dismissals() -> u32 {
    3
}

fn default_category_aware() -> bool {
    true
}

impl DecayConfig {
    /// Check if an immunity_hit_rate value is valid (must be in [0.0, 1.0] and finite).
    pub fn is_valid_immunity_rate(value: f64) -> bool {
        value.is_finite() && (0.0..=1.0).contains(&value)
    }

    /// Get the immunity hit rate for a specific category.
    /// Some categories (like debugging) are niche and expected to have lower hit rates.
    /// Get the immunity rate for a specific learning category.
    ///
    /// Category-specific thresholds are more generous for niche categories
    /// that are expected to have lower hit rates.
    pub fn immunity_rate_for_category(&self, category: &LearningCategory) -> f64 {
        if !self.category_aware {
            return self.immunity_hit_rate;
        }

        // Category-specific thresholds per design doc (03-stats-and-quality.md)
        // More generous for niche categories that naturally have lower hit rates
        match category {
            LearningCategory::Debugging => 0.2,  // Very situational
            LearningCategory::Dependency => 0.2, // Version-specific
            LearningCategory::Process => 0.2,    // Workflow-specific
            LearningCategory::Domain => 0.3,     // May not apply often
            LearningCategory::Convention => 0.3, // Project-specific
            LearningCategory::Pitfall => 0.4,    // Warnings, may not surface often
            LearningCategory::Pattern => 0.4,    // General patterns
        }
    }
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            passive_duration_days: 90,
            // Lowered from 0.8 to 0.3 - be generous since hit rate is a lower bound
            immunity_hit_rate: 0.3,
            // Require 3 dismissals before counting against a learning
            min_dismissals_for_decay: 3,
            // Apply category-specific thresholds
            category_aware: true,
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

impl RetrievalConfig {
    /// Check if a strategy value is valid.
    pub fn is_valid_strategy(value: &str) -> bool {
        VALID_STRATEGIES.contains(&value)
    }
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

/// Minimum valid max_blocks value (must be at least 1 to allow any blocks).
pub const MIN_MAX_BLOCKS: u32 = 1;

/// Minimum valid cooldown_seconds value (must be at least 1 second).
pub const MIN_COOLDOWN_SECONDS: u32 = 1;

impl CircuitBreakerConfig {
    /// Check if max_blocks is valid (must be >= 1).
    ///
    /// A max_blocks of 0 would cause immediate circuit breaker trip, effectively
    /// disabling the gate entirely.
    pub fn is_valid_max_blocks(value: u32) -> bool {
        value >= MIN_MAX_BLOCKS
    }

    /// Check if cooldown_seconds is valid (must be >= 1).
    ///
    /// A cooldown of 0 would cause immediate reset, making timing comparisons
    /// behave unexpectedly.
    pub fn is_valid_cooldown_seconds(value: u32) -> bool {
        value >= MIN_COOLDOWN_SECONDS
    }
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
        // Fail-open: if cwd is unavailable, return defaults with env overrides
        // rather than trying path operations with an empty PathBuf
        match env::current_dir() {
            Ok(cwd) => Self::load_from_cwd(&cwd),
            Err(_) => {
                let mut config = Config::default();
                // Still apply user config and env overrides
                if let Some(user_config) = Self::load_user_config() {
                    config = config.merge(user_config);
                }
                config.apply_env_overrides();
                config
            }
        }
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
            match val.parse::<u32>() {
                Ok(n) => {
                    if CircuitBreakerConfig::is_valid_max_blocks(n) {
                        self.circuit_breaker.max_blocks = n;
                    } else {
                        eprintln!(
                            "Warning: Invalid GROVE_MAX_BLOCKS value '{}'. \
                            Must be >= {}. Using default '{}'.",
                            n, MIN_MAX_BLOCKS, self.circuit_breaker.max_blocks
                        );
                    }
                }
                Err(_) => eprintln!(
                    "Warning: Invalid GROVE_MAX_BLOCKS value '{}'. \
                    Expected a positive integer. Using default '{}'.",
                    val, self.circuit_breaker.max_blocks
                ),
            }
        }

        // GROVE_COOLDOWN_SECONDS
        if let Ok(val) = env::var("GROVE_COOLDOWN_SECONDS") {
            match val.parse::<u32>() {
                Ok(n) => {
                    if CircuitBreakerConfig::is_valid_cooldown_seconds(n) {
                        self.circuit_breaker.cooldown_seconds = n;
                    } else {
                        eprintln!(
                            "Warning: Invalid GROVE_COOLDOWN_SECONDS value '{}'. \
                            Must be >= {}. Using default '{}'.",
                            n, MIN_COOLDOWN_SECONDS, self.circuit_breaker.cooldown_seconds
                        );
                    }
                }
                Err(_) => eprintln!(
                    "Warning: Invalid GROVE_COOLDOWN_SECONDS value '{}'. \
                    Expected a positive integer. Using default '{}'.",
                    val, self.circuit_breaker.cooldown_seconds
                ),
            }
        }

        // GROVE_MAX_INJECTIONS
        if let Ok(val) = env::var("GROVE_MAX_INJECTIONS") {
            match val.parse::<u32>() {
                Ok(n) => self.retrieval.max_injections = n,
                Err(_) => eprintln!(
                    "Warning: Invalid GROVE_MAX_INJECTIONS value '{}'. \
                    Expected a positive integer. Using default '{}'.",
                    val, self.retrieval.max_injections
                ),
            }
        }

        // GROVE_RETRIEVAL_STRATEGY
        if let Ok(val) = env::var("GROVE_RETRIEVAL_STRATEGY") {
            if RetrievalConfig::is_valid_strategy(&val) {
                self.retrieval.strategy = val;
            } else {
                eprintln!(
                    "Warning: Invalid GROVE_RETRIEVAL_STRATEGY value '{}'. \
                    Valid values: {:?}. Using default '{}'.",
                    val, VALID_STRATEGIES, self.retrieval.strategy
                );
            }
        }

        // GROVE_AUTO_SKIP_ENABLED
        if let Ok(val) = env::var("GROVE_AUTO_SKIP_ENABLED") {
            self.gate.auto_skip.enabled = val == "true" || val == "1";
        }

        // GROVE_AUTO_SKIP_THRESHOLD
        if let Ok(val) = env::var("GROVE_AUTO_SKIP_THRESHOLD") {
            match val.parse::<u32>() {
                Ok(n) => self.gate.auto_skip.line_threshold = n,
                Err(_) => eprintln!(
                    "Warning: Invalid GROVE_AUTO_SKIP_THRESHOLD value '{}'. \
                    Expected a positive integer. Using default '{}'.",
                    val, self.gate.auto_skip.line_threshold
                ),
            }
        }

        // GROVE_AUTO_SKIP_DECIDER
        if let Ok(val) = env::var("GROVE_AUTO_SKIP_DECIDER") {
            if AutoSkipConfig::is_valid_decider(&val) {
                self.gate.auto_skip.decider = val;
            } else {
                eprintln!(
                    "Warning: Invalid GROVE_AUTO_SKIP_DECIDER value '{}'. \
                    Valid values: {:?}. Using default '{}'.",
                    val, VALID_DECIDERS, self.gate.auto_skip.decider
                );
            }
        }

        // GROVE_DECAY_DAYS
        if let Ok(val) = env::var("GROVE_DECAY_DAYS") {
            match val.parse::<u32>() {
                Ok(n) => self.decay.passive_duration_days = n,
                Err(_) => eprintln!(
                    "Warning: Invalid GROVE_DECAY_DAYS value '{}'. \
                    Expected a positive integer. Using default '{}'.",
                    val, self.decay.passive_duration_days
                ),
            }
        }

        // GROVE_DECAY_IMMUNITY_RATE
        if let Ok(val) = env::var("GROVE_DECAY_IMMUNITY_RATE") {
            match val.parse::<f64>() {
                Ok(n) => {
                    if DecayConfig::is_valid_immunity_rate(n) {
                        self.decay.immunity_hit_rate = n;
                    } else {
                        eprintln!(
                            "Warning: Invalid GROVE_DECAY_IMMUNITY_RATE value '{}'. \
                            Must be in range [0.0, 1.0]. Using default '{}'.",
                            n, self.decay.immunity_hit_rate
                        );
                    }
                }
                Err(_) => eprintln!(
                    "Warning: Invalid GROVE_DECAY_IMMUNITY_RATE value '{}'. \
                    Expected a decimal number. Using default '{}'.",
                    val, self.decay.immunity_hit_rate
                ),
            }
        }
    }

    /// Merge another config into this one.
    ///
    /// The `other` config takes precedence. All non-default fields from `other`
    /// are applied to `self`, enabling proper layering of the precedence chain.
    /// This is field-by-field merging, not section-by-section, which ensures
    /// that explicit defaults in one config do not block overrides from another.
    ///
    /// # Limitation
    ///
    /// A config cannot explicitly set a value back to the default to override a
    /// non-default value from a lower-precedence config. For example:
    /// - User config sets `max_blocks = 10`
    /// - Project config sets `max_blocks = 3` (the default)
    /// - Result: `max_blocks = 10` because the project value equals default
    ///
    /// This limitation exists because we cannot distinguish between "not set in
    /// file" and "explicitly set to default value" without using `Option<T>` for
    /// all config fields. The current approach enables additive config layering
    /// where each layer only needs to specify its customizations.
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

    /// Save configuration to the project config file.
    ///
    /// Writes to `.grove/config.toml` in the given directory.
    /// Creates the `.grove` directory if it doesn't exist.
    /// Uses atomic write (write to temp file, then rename) for safety.
    pub fn save_project(&self, cwd: &Path) -> Result<()> {
        let grove_dir = cwd.join(".grove");

        // Create .grove directory if it doesn't exist
        if !grove_dir.exists() {
            fs::create_dir_all(&grove_dir).map_err(|e| GroveError::storage(&grove_dir, e))?;
        }

        let config_path = grove_dir.join("config.toml");

        // Serialize to TOML
        let content =
            toml::to_string_pretty(self).map_err(|e| GroveError::config(e.to_string()))?;

        // Atomic write: write to temp file, then rename
        let temp_path = grove_dir.join(".config.toml.tmp");
        fs::write(&temp_path, &content).map_err(|e| GroveError::storage(&temp_path, e))?;

        // Sync the file to disk
        let file = fs::File::open(&temp_path).map_err(|e| GroveError::storage(&temp_path, e))?;
        file.sync_all()
            .map_err(|e| GroveError::storage(&temp_path, e))?;
        drop(file);

        // Rename temp to final (atomic on most filesystems)
        fs::rename(&temp_path, &config_path).map_err(|e| GroveError::storage(&config_path, e))?;

        Ok(())
    }

    /// Generate a diff of changed values between two configs.
    ///
    /// Returns a list of (key, old_value, new_value) tuples for changed fields.
    pub fn diff(&self, other: &Config) -> Vec<(String, String, String)> {
        let mut changes = Vec::new();

        // Retrieval strategy
        if self.retrieval.strategy != other.retrieval.strategy {
            changes.push((
                "retrieval.strategy".to_string(),
                self.retrieval.strategy.clone(),
                other.retrieval.strategy.clone(),
            ));
        }

        // Auto-skip threshold
        if self.gate.auto_skip.line_threshold != other.gate.auto_skip.line_threshold {
            changes.push((
                "gate.auto_skip.line_threshold".to_string(),
                self.gate.auto_skip.line_threshold.to_string(),
                other.gate.auto_skip.line_threshold.to_string(),
            ));
        }

        // Auto-skip enabled
        if self.gate.auto_skip.enabled != other.gate.auto_skip.enabled {
            changes.push((
                "gate.auto_skip.enabled".to_string(),
                self.gate.auto_skip.enabled.to_string(),
                other.gate.auto_skip.enabled.to_string(),
            ));
        }

        // Auto-skip decider
        if self.gate.auto_skip.decider != other.gate.auto_skip.decider {
            changes.push((
                "gate.auto_skip.decider".to_string(),
                self.gate.auto_skip.decider.clone(),
                other.gate.auto_skip.decider.clone(),
            ));
        }

        // Decay days
        if self.decay.passive_duration_days != other.decay.passive_duration_days {
            changes.push((
                "decay.passive_duration_days".to_string(),
                self.decay.passive_duration_days.to_string(),
                other.decay.passive_duration_days.to_string(),
            ));
        }

        // Decay immunity rate
        if (self.decay.immunity_hit_rate - other.decay.immunity_hit_rate).abs() > f64::EPSILON {
            changes.push((
                "decay.immunity_hit_rate".to_string(),
                format!("{:.2}", self.decay.immunity_hit_rate),
                format!("{:.2}", other.decay.immunity_hit_rate),
            ));
        }

        // Circuit breaker max_blocks
        if self.circuit_breaker.max_blocks != other.circuit_breaker.max_blocks {
            changes.push((
                "circuit_breaker.max_blocks".to_string(),
                self.circuit_breaker.max_blocks.to_string(),
                other.circuit_breaker.max_blocks.to_string(),
            ));
        }

        // Circuit breaker cooldown
        if self.circuit_breaker.cooldown_seconds != other.circuit_breaker.cooldown_seconds {
            changes.push((
                "circuit_breaker.cooldown_seconds".to_string(),
                self.circuit_breaker.cooldown_seconds.to_string(),
                other.circuit_breaker.cooldown_seconds.to_string(),
            ));
        }

        // Max injections
        if self.retrieval.max_injections != other.retrieval.max_injections {
            changes.push((
                "retrieval.max_injections".to_string(),
                self.retrieval.max_injections.to_string(),
                other.retrieval.max_injections.to_string(),
            ));
        }

        changes
    }
}

/// Get the Grove home directory.
///
/// Checks `GROVE_HOME` environment variable first, then falls back to
/// `~/.grove`.
///
/// # Validation
///
/// If `GROVE_HOME` is set, it must be:
/// - Non-empty
/// - An absolute path (or we canonicalize it)
///
/// Invalid values are ignored and we fall back to the default.
pub fn grove_home() -> Option<PathBuf> {
    // Check GROVE_HOME env var first
    if let Ok(home) = env::var("GROVE_HOME") {
        // Validate: must be non-empty
        if home.is_empty() {
            tracing::warn!("GROVE_HOME is empty, using default");
        } else {
            let path = PathBuf::from(&home);
            // If it's an absolute path, use it directly
            if path.is_absolute() {
                return Some(path);
            }
            // For relative paths, try to canonicalize it
            if let Ok(canonical) = path.canonicalize() {
                return Some(canonical);
            }
            // If canonicalization fails (path doesn't exist), use as-is but warn
            tracing::warn!("GROVE_HOME is relative and doesn't exist, using as-is");
            return Some(path);
        }
    }

    // Fall back to ~/.grove
    if let Some(home) = dirs::home_dir() {
        return Some(home.join(".grove"));
    }

    // Fallback for containerized/minimal environments without HOME
    let fallback_path = fallback_grove_home();
    tracing::warn!(
        "HOME not set, using fallback location: {}",
        fallback_path.display()
    );
    Some(fallback_path)
}

/// Get fallback grove home path when HOME is unavailable.
#[cfg(unix)]
fn fallback_grove_home() -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    // Get UID for unique temp directory
    let uid = std::fs::metadata("/").map(|m| m.uid()).unwrap_or(0);
    PathBuf::from(format!("/tmp/grove-{}", uid))
}

/// Get fallback grove home path when HOME is unavailable.
#[cfg(not(unix))]
fn fallback_grove_home() -> PathBuf {
    std::env::temp_dir().join("grove")
}

/// Find the project root for a given working directory.
///
/// This function walks up the directory tree to find the appropriate project root,
/// using the following precedence:
///
/// 1. **Existing `.grove/` directory** - If a `.grove/` directory exists in the current
///    directory or any ancestor, that directory is used. This allows explicit placement
///    of the Grove directory.
///
/// 2. **Git repository root** - If no `.grove/` is found, we ask git for the repository
///    root via `git rev-parse --show-toplevel`. This handles all git edge cases including
///    worktrees and submodules.
///
/// 3. **Fallback to cwd** - If neither is found (e.g., not a git repo, git not installed),
///    the original working directory is used.
///
/// # Arguments
///
/// * `cwd` - The current working directory to start searching from
///
/// # Examples
///
/// ```ignore
/// // If cwd is /project/src/module and .grove exists at /project/.grove:
/// let root = find_project_root(Path::new("/project/src/module"));
/// assert_eq!(root, PathBuf::from("/project"));
///
/// // If no .grove but inside git repo rooted at /project:
/// let root = find_project_root(Path::new("/project/src/module"));
/// assert_eq!(root, PathBuf::from("/project"));
/// ```
pub fn find_project_root(cwd: &Path) -> PathBuf {
    // 1. Walk up looking for existing .grove/ (explicit placement wins)
    for ancestor in cwd.ancestors() {
        if ancestor.join(".grove").is_dir() {
            return ancestor.to_path_buf();
        }
    }

    // 2. Ask git for the repo root
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
    {
        if output.status.success() {
            if let Ok(path) = String::from_utf8(output.stdout) {
                let trimmed = path.trim();
                if !trimmed.is_empty() {
                    return PathBuf::from(trimmed);
                }
            }
        }
    }

    // 3. Fall back to cwd
    cwd.to_path_buf()
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
/// This function first finds the project root (by looking for an existing `.grove/`
/// directory or the git repository root), then returns the `.grove/` subdirectory.
///
/// See [`find_project_root`] for details on how the project root is determined.
///
/// # Arguments
///
/// * `cwd` - The current working directory to start searching from
///
/// # Returns
///
/// The path to the `.grove/` directory, e.g., `<project_root>/.grove/`
pub fn project_grove_dir(cwd: &Path) -> PathBuf {
    find_project_root(cwd).join(".grove")
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
        assert_eq!(config.backends.discovery, vec!["total-recall", "markdown"]);
        assert!(config.backends.overrides.is_empty());

        // Gate defaults
        assert!(config.gate.auto_skip.enabled);
        assert_eq!(config.gate.auto_skip.line_threshold, 5);
        assert_eq!(config.gate.auto_skip.decider, "agent");
        assert!(!config.gate.skip_counts_as_dismissal); // Default: skip is no-signal

        // Decay defaults (conservative: 0.3 instead of 0.8)
        assert_eq!(config.decay.passive_duration_days, 90);
        assert!((config.decay.immunity_hit_rate - 0.3).abs() < f64::EPSILON);
        assert_eq!(config.decay.min_dismissals_for_decay, 3);
        assert!(config.decay.category_aware);

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
total-recall = false
"#;
        fs::write(&config_path, toml_content).unwrap();

        let config = Config::load_from_cwd(dir.path());

        assert_eq!(config.backends.discovery, vec!["markdown"]);
        assert_eq!(config.backends.overrides.get("total-recall"), Some(&false));
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
    fn test_grove_home_empty_env() {
        // Empty GROVE_HOME should fall back to default
        env::set_var("GROVE_HOME", "");

        let home = grove_home();
        // Should fall back to ~/.grove
        assert!(home.is_some());
        assert!(home.unwrap().ends_with(".grove"));

        env::remove_var("GROVE_HOME");
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
                skip_counts_as_dismissal: false,
            },
            decay: DecayConfig {
                passive_duration_days: 60,
                immunity_hit_rate: 0.9,
                min_dismissals_for_decay: 3,
                category_aware: true,
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
                skip_counts_as_dismissal: false,
            },
            decay: DecayConfig {
                passive_duration_days: 90, // same as default
                immunity_hit_rate: 0.3,    // same as default
                min_dismissals_for_decay: 3,
                category_aware: true,
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
                skip_counts_as_dismissal: false,
            },
            decay: DecayConfig {
                passive_duration_days: 180, // different from default
                immunity_hit_rate: 0.3,     // same as default
                min_dismissals_for_decay: 3,
                category_aware: true,
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

        // immunity_hit_rate: both have default (0.3), base's value should remain
        assert!((merged.decay.immunity_hit_rate - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_merge_with_explicit_defaults_does_not_block_overrides() {
        // This tests the specific bug: if user config sets values to defaults,
        // project config overrides should still apply.

        // Simulate user config that explicitly sets everything to defaults
        let user_config = Config {
            gate: GateConfig {
                auto_skip: AutoSkipConfig::default(),
                skip_counts_as_dismissal: false,
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
                skip_counts_as_dismissal: false,
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

    #[test]
    fn test_is_valid_decider_accepts_valid_values() {
        assert!(AutoSkipConfig::is_valid_decider("agent"));
        assert!(AutoSkipConfig::is_valid_decider("always"));
        assert!(AutoSkipConfig::is_valid_decider("never"));
    }

    #[test]
    fn test_is_valid_decider_rejects_invalid_values() {
        assert!(!AutoSkipConfig::is_valid_decider("invalid"));
        assert!(!AutoSkipConfig::is_valid_decider(""));
        assert!(!AutoSkipConfig::is_valid_decider("AGENT")); // Case sensitive
        assert!(!AutoSkipConfig::is_valid_decider("Agent"));
        assert!(!AutoSkipConfig::is_valid_decider("sometimes"));
    }

    #[test]
    fn test_env_var_invalid_decider_ignored() {
        // Set an invalid decider value
        env::set_var("GROVE_AUTO_SKIP_DECIDER", "invalid_value");

        let mut config = Config::default();
        config.apply_env_overrides();

        // Should keep the default value, not the invalid one
        assert_eq!(config.gate.auto_skip.decider, "agent");

        env::remove_var("GROVE_AUTO_SKIP_DECIDER");
    }

    #[test]
    fn test_env_var_valid_decider_applied() {
        // Set valid decider values
        for valid in VALID_DECIDERS {
            env::set_var("GROVE_AUTO_SKIP_DECIDER", valid);

            let mut config = Config::default();
            config.apply_env_overrides();

            assert_eq!(config.gate.auto_skip.decider, *valid);

            env::remove_var("GROVE_AUTO_SKIP_DECIDER");
        }
    }

    #[test]
    fn test_env_var_invalid_strategy_ignored() {
        // Clean up first (in case previous test didn't clean up)
        env::remove_var("GROVE_RETRIEVAL_STRATEGY");

        // Get the default strategy
        let default_strategy = Config::default().retrieval.strategy.clone();

        // Set an invalid strategy value
        env::set_var("GROVE_RETRIEVAL_STRATEGY", "invalid_value");

        let mut config = Config::default();
        config.apply_env_overrides();

        // Should keep the default value, not the invalid one
        assert_eq!(config.retrieval.strategy, default_strategy);

        env::remove_var("GROVE_RETRIEVAL_STRATEGY");
    }

    #[test]
    fn test_env_var_valid_strategy_applied() {
        // Set valid strategy values
        for valid in VALID_STRATEGIES {
            env::set_var("GROVE_RETRIEVAL_STRATEGY", valid);

            let mut config = Config::default();
            config.apply_env_overrides();

            assert_eq!(config.retrieval.strategy, *valid);

            env::remove_var("GROVE_RETRIEVAL_STRATEGY");
        }
    }

    #[test]
    fn test_is_valid_strategy() {
        // Valid strategies
        assert!(RetrievalConfig::is_valid_strategy("conservative"));
        assert!(RetrievalConfig::is_valid_strategy("moderate"));
        assert!(RetrievalConfig::is_valid_strategy("aggressive"));

        // Invalid strategies
        assert!(!RetrievalConfig::is_valid_strategy("invalid"));
        assert!(!RetrievalConfig::is_valid_strategy(""));
        assert!(!RetrievalConfig::is_valid_strategy("MODERATE")); // Case sensitive
    }

    #[test]
    fn test_is_valid_immunity_rate() {
        // Valid rates (within [0.0, 1.0])
        assert!(DecayConfig::is_valid_immunity_rate(0.0));
        assert!(DecayConfig::is_valid_immunity_rate(0.5));
        assert!(DecayConfig::is_valid_immunity_rate(1.0));
        assert!(DecayConfig::is_valid_immunity_rate(0.001));
        assert!(DecayConfig::is_valid_immunity_rate(0.999));

        // Invalid rates (outside [0.0, 1.0])
        assert!(!DecayConfig::is_valid_immunity_rate(-0.1));
        assert!(!DecayConfig::is_valid_immunity_rate(-1.0));
        assert!(!DecayConfig::is_valid_immunity_rate(1.1));
        assert!(!DecayConfig::is_valid_immunity_rate(2.0));

        // Invalid special values
        assert!(!DecayConfig::is_valid_immunity_rate(f64::NAN));
        assert!(!DecayConfig::is_valid_immunity_rate(f64::INFINITY));
        assert!(!DecayConfig::is_valid_immunity_rate(f64::NEG_INFINITY));
    }

    #[test]
    fn test_env_var_invalid_immunity_rate_ignored() {
        // Clean up first
        env::remove_var("GROVE_DECAY_IMMUNITY_RATE");

        // Get the default rate
        let default_rate = Config::default().decay.immunity_hit_rate;

        // Test out-of-range values
        env::set_var("GROVE_DECAY_IMMUNITY_RATE", "-0.5");
        let mut config = Config::default();
        config.apply_env_overrides();
        assert_eq!(config.decay.immunity_hit_rate, default_rate);

        env::set_var("GROVE_DECAY_IMMUNITY_RATE", "1.5");
        let mut config = Config::default();
        config.apply_env_overrides();
        assert_eq!(config.decay.immunity_hit_rate, default_rate);

        // Test non-numeric values (these just fail to parse, which is fine)
        env::set_var("GROVE_DECAY_IMMUNITY_RATE", "invalid");
        let mut config = Config::default();
        config.apply_env_overrides();
        assert_eq!(config.decay.immunity_hit_rate, default_rate);

        env::remove_var("GROVE_DECAY_IMMUNITY_RATE");
    }

    #[test]
    fn test_env_var_valid_immunity_rate_applied() {
        // Clean up first
        env::remove_var("GROVE_DECAY_IMMUNITY_RATE");

        // Test valid values
        let valid_values = [0.0, 0.5, 1.0, 0.75, 0.25];
        for value in valid_values {
            env::set_var("GROVE_DECAY_IMMUNITY_RATE", value.to_string());

            let mut config = Config::default();
            config.apply_env_overrides();

            assert_eq!(config.decay.immunity_hit_rate, value);
        }

        env::remove_var("GROVE_DECAY_IMMUNITY_RATE");
    }

    #[test]
    fn test_is_valid_max_blocks() {
        // Valid values (>= 1)
        assert!(CircuitBreakerConfig::is_valid_max_blocks(1));
        assert!(CircuitBreakerConfig::is_valid_max_blocks(3));
        assert!(CircuitBreakerConfig::is_valid_max_blocks(100));

        // Invalid values (0)
        assert!(!CircuitBreakerConfig::is_valid_max_blocks(0));
    }

    #[test]
    fn test_is_valid_cooldown_seconds() {
        // Valid values (>= 1)
        assert!(CircuitBreakerConfig::is_valid_cooldown_seconds(1));
        assert!(CircuitBreakerConfig::is_valid_cooldown_seconds(300));
        assert!(CircuitBreakerConfig::is_valid_cooldown_seconds(3600));

        // Invalid values (0)
        assert!(!CircuitBreakerConfig::is_valid_cooldown_seconds(0));
    }

    #[test]
    fn test_env_var_invalid_max_blocks_ignored() {
        // Clean up first
        env::remove_var("GROVE_MAX_BLOCKS");

        let default_max_blocks = Config::default().circuit_breaker.max_blocks;

        // Set invalid max_blocks (0)
        env::set_var("GROVE_MAX_BLOCKS", "0");

        let mut config = Config::default();
        config.apply_env_overrides();

        // Should keep the default value
        assert_eq!(config.circuit_breaker.max_blocks, default_max_blocks);

        env::remove_var("GROVE_MAX_BLOCKS");
    }

    #[test]
    fn test_env_var_valid_max_blocks_applied() {
        // Clean up first
        env::remove_var("GROVE_MAX_BLOCKS");

        // Set valid max_blocks
        env::set_var("GROVE_MAX_BLOCKS", "5");

        let mut config = Config::default();
        config.apply_env_overrides();

        assert_eq!(config.circuit_breaker.max_blocks, 5);

        env::remove_var("GROVE_MAX_BLOCKS");
    }

    #[test]
    fn test_env_var_invalid_cooldown_seconds_ignored() {
        // Clean up first
        env::remove_var("GROVE_COOLDOWN_SECONDS");

        let default_cooldown = Config::default().circuit_breaker.cooldown_seconds;

        // Set invalid cooldown (0)
        env::set_var("GROVE_COOLDOWN_SECONDS", "0");

        let mut config = Config::default();
        config.apply_env_overrides();

        // Should keep the default value
        assert_eq!(config.circuit_breaker.cooldown_seconds, default_cooldown);

        env::remove_var("GROVE_COOLDOWN_SECONDS");
    }

    #[test]
    fn test_env_var_valid_cooldown_seconds_applied() {
        // Clean up first
        env::remove_var("GROVE_COOLDOWN_SECONDS");

        // Set valid cooldown
        env::set_var("GROVE_COOLDOWN_SECONDS", "600");

        let mut config = Config::default();
        config.apply_env_overrides();

        assert_eq!(config.circuit_breaker.cooldown_seconds, 600);

        env::remove_var("GROVE_COOLDOWN_SECONDS");
    }

    // Category-aware decay threshold tests

    #[test]
    fn test_immunity_rate_for_category_debugging() {
        let config = DecayConfig::default();
        assert!(
            (config.immunity_rate_for_category(&LearningCategory::Debugging) - 0.2).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn test_immunity_rate_for_category_dependency() {
        let config = DecayConfig::default();
        assert!(
            (config.immunity_rate_for_category(&LearningCategory::Dependency) - 0.2).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn test_immunity_rate_for_category_process() {
        let config = DecayConfig::default();
        assert!(
            (config.immunity_rate_for_category(&LearningCategory::Process) - 0.2).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn test_immunity_rate_for_category_domain() {
        let config = DecayConfig::default();
        assert!(
            (config.immunity_rate_for_category(&LearningCategory::Domain) - 0.3).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn test_immunity_rate_for_category_convention() {
        let config = DecayConfig::default();
        assert!(
            (config.immunity_rate_for_category(&LearningCategory::Convention) - 0.3).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn test_immunity_rate_for_category_pitfall() {
        let config = DecayConfig::default();
        assert!(
            (config.immunity_rate_for_category(&LearningCategory::Pitfall) - 0.4).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn test_immunity_rate_for_category_pattern() {
        let config = DecayConfig::default();
        assert!(
            (config.immunity_rate_for_category(&LearningCategory::Pattern) - 0.4).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn test_immunity_rate_category_aware_disabled() {
        let config = DecayConfig {
            category_aware: false,
            ..Default::default()
        };

        // All categories should return the default rate when category_aware is false
        assert!(
            (config.immunity_rate_for_category(&LearningCategory::Debugging) - 0.3).abs()
                < f64::EPSILON
        );
        assert!(
            (config.immunity_rate_for_category(&LearningCategory::Pattern) - 0.3).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn test_save_project_creates_config_file() {
        let dir = TempDir::new().unwrap();
        let config = Config {
            retrieval: RetrievalConfig {
                max_injections: 10,
                strategy: "aggressive".to_string(),
            },
            ..Config::default()
        };

        config.save_project(dir.path()).unwrap();

        let config_path = dir.path().join(".grove").join("config.toml");
        assert!(config_path.exists());

        // Verify content
        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("aggressive"));
        assert!(content.contains("10"));
    }

    #[test]
    fn test_save_project_creates_grove_dir() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");

        // Ensure .grove doesn't exist initially
        assert!(!grove_dir.exists());

        let config = Config::default();
        config.save_project(dir.path()).unwrap();

        // .grove should now exist
        assert!(grove_dir.exists());
    }

    #[test]
    fn test_save_project_overwrites_existing() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        // Write initial config
        let config1 = Config {
            retrieval: RetrievalConfig {
                strategy: "conservative".to_string(),
                ..Default::default()
            },
            ..Config::default()
        };
        config1.save_project(dir.path()).unwrap();

        // Write updated config
        let config2 = Config {
            retrieval: RetrievalConfig {
                strategy: "aggressive".to_string(),
                ..Default::default()
            },
            ..Config::default()
        };
        config2.save_project(dir.path()).unwrap();

        // Verify the file was updated
        let config_path = dir.path().join(".grove").join("config.toml");
        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("aggressive"));
        assert!(!content.contains("conservative"));
    }

    #[test]
    fn test_diff_no_changes() {
        let config = Config::default();
        let changes = config.diff(&config);
        assert!(changes.is_empty());
    }

    #[test]
    fn test_diff_retrieval_strategy() {
        let config1 = Config::default();
        let mut config2 = Config::default();
        config2.retrieval.strategy = "aggressive".to_string();

        let changes = config1.diff(&config2);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].0, "retrieval.strategy");
        assert_eq!(changes[0].1, "moderate");
        assert_eq!(changes[0].2, "aggressive");
    }

    #[test]
    fn test_diff_auto_skip_threshold() {
        let config1 = Config::default();
        let mut config2 = Config::default();
        config2.gate.auto_skip.line_threshold = 10;

        let changes = config1.diff(&config2);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].0, "gate.auto_skip.line_threshold");
        assert_eq!(changes[0].1, "5");
        assert_eq!(changes[0].2, "10");
    }

    #[test]
    fn test_diff_multiple_changes() {
        let config1 = Config::default();
        let mut config2 = Config::default();
        config2.retrieval.strategy = "aggressive".to_string();
        config2.gate.auto_skip.line_threshold = 3;
        config2.circuit_breaker.max_blocks = 5;

        let changes = config1.diff(&config2);
        assert_eq!(changes.len(), 3);

        // Check that all expected changes are present
        let keys: Vec<_> = changes.iter().map(|(k, _, _)| k.as_str()).collect();
        assert!(keys.contains(&"retrieval.strategy"));
        assert!(keys.contains(&"gate.auto_skip.line_threshold"));
        assert!(keys.contains(&"circuit_breaker.max_blocks"));
    }

    // Tests for find_project_root

    #[test]
    fn test_find_project_root_existing_grove_in_cwd() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        let result = find_project_root(dir.path());
        assert_eq!(result, dir.path());
    }

    #[test]
    fn test_find_project_root_existing_grove_in_parent() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        // Create a subdirectory
        let subdir = dir.path().join("src").join("module");
        fs::create_dir_all(&subdir).unwrap();

        // find_project_root from subdirectory should find parent's .grove
        let result = find_project_root(&subdir);
        assert_eq!(result, dir.path());
    }

    #[test]
    fn test_find_project_root_existing_grove_in_grandparent() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        // Create nested subdirectories
        let deep_subdir = dir.path().join("a").join("b").join("c").join("d");
        fs::create_dir_all(&deep_subdir).unwrap();

        // find_project_root from deep subdirectory should find root's .grove
        let result = find_project_root(&deep_subdir);
        assert_eq!(result, dir.path());
    }

    #[test]
    fn test_find_project_root_fallback_behavior() {
        // Test that when a parent has .grove/, we find it (not the cwd)
        // This exercises the fallback path indirectly
        let outer = TempDir::new().unwrap();
        let outer_grove = outer.path().join(".grove");
        fs::create_dir_all(&outer_grove).unwrap();

        // Create an inner directory without .grove
        let inner = outer.path().join("inner_project");
        fs::create_dir_all(&inner).unwrap();

        // find_project_root from inner should find outer (where .grove exists)
        let result = find_project_root(&inner);
        assert_eq!(result, outer.path());
    }

    #[test]
    fn test_find_project_root_git_repo() {
        let dir = TempDir::new().unwrap();

        // Check if any ancestor has .grove/ - skip test if so (environment issue)
        for ancestor in dir.path().ancestors() {
            if ancestor.join(".grove").is_dir() {
                // Skip test - can't reliably test git detection with .grove/ in ancestors
                return;
            }
        }

        // Initialize a git repo
        let git_init = std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output();

        // Skip test if git is not available
        if git_init.is_err() || !git_init.unwrap().status.success() {
            return;
        }

        // Create a subdirectory
        let subdir = dir.path().join("src");
        fs::create_dir_all(&subdir).unwrap();

        // find_project_root from subdirectory should find git root
        let result = find_project_root(&subdir);
        // Canonicalize both to handle symlinks (e.g., /tmp -> /private/tmp on macOS)
        let expected = dir
            .path()
            .canonicalize()
            .unwrap_or_else(|_| dir.path().to_path_buf());
        let actual = result.canonicalize().unwrap_or(result);
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_find_project_root_grove_takes_precedence_over_git() {
        let dir = TempDir::new().unwrap();

        // Initialize a git repo
        let git_init = std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output();

        // Skip test if git is not available
        if git_init.is_err() || !git_init.unwrap().status.success() {
            return;
        }

        // Create .grove in a subdirectory (not at git root)
        let subproject = dir.path().join("packages").join("my-package");
        let grove_dir = subproject.join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        // Create a deeper subdirectory
        let deep_subdir = subproject.join("src").join("lib");
        fs::create_dir_all(&deep_subdir).unwrap();

        // find_project_root should find .grove in packages/my-package, not git root
        let result = find_project_root(&deep_subdir);
        assert_eq!(result, subproject);
    }

    #[test]
    fn test_project_grove_dir_uses_find_project_root() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        // Create a subdirectory
        let subdir = dir.path().join("src");
        fs::create_dir_all(&subdir).unwrap();

        // project_grove_dir from subdirectory should return parent's .grove
        let result = project_grove_dir(&subdir);
        assert_eq!(result, grove_dir);
    }

    #[test]
    fn test_project_learnings_path_uses_find_project_root() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        // Create a subdirectory
        let subdir = dir.path().join("src");
        fs::create_dir_all(&subdir).unwrap();

        // project_learnings_path from subdirectory should return parent's path
        let result = project_learnings_path(&subdir);
        assert_eq!(result, grove_dir.join("learnings.md"));
    }

    #[test]
    fn test_project_stats_log_path_uses_find_project_root() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        // Create a subdirectory
        let subdir = dir.path().join("src");
        fs::create_dir_all(&subdir).unwrap();

        // project_stats_log_path from subdirectory should return parent's path
        let result = project_stats_log_path(&subdir);
        assert_eq!(result, grove_dir.join("stats.log"));
    }
}
