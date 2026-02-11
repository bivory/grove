//! Init command for Grove.
//!
//! Scaffolds the Grove configuration files and directories.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::config::{grove_home, project_grove_dir, project_learnings_path, sessions_dir};

/// Options for the init command.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Force overwrite existing files.
    pub force: bool,
}

/// Output format for the init command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitOutput {
    /// Whether initialization was successful.
    pub success: bool,
    /// Files and directories created.
    pub created: Vec<String>,
    /// Files that already existed (skipped).
    pub skipped: Vec<String>,
    /// Error message if initialization failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl InitOutput {
    /// Create a successful output.
    pub fn success(created: Vec<String>, skipped: Vec<String>) -> Self {
        Self {
            success: true,
            created,
            skipped,
            error: None,
        }
    }

    /// Create a failed output with partial success information.
    ///
    /// This reports what was created before the failure occurred, so the user
    /// knows what partial state may have been left behind.
    pub fn failure(error: impl Into<String>, created: Vec<String>, skipped: Vec<String>) -> Self {
        Self {
            success: false,
            created,
            skipped,
            error: Some(error.into()),
        }
    }
}

/// Default config.toml content.
const DEFAULT_CONFIG: &str = r#"# Grove Configuration
#
# This file configures the Grove compound learning gate.
# See https://github.com/bivory/grove for documentation.

# Ticketing system discovery order
# Options: tissue, beads, tasks, session
[ticketing]
discovery = ["tissue", "beads", "tasks", "session"]

# Memory backend discovery order
# Options: total-recall, mcp, markdown
[backends]
discovery = ["total-recall", "mcp", "markdown"]

# Auto-skip settings for trivial changes
# decider: "agent" (default), "always", or "never"
[gate.auto_skip]
enabled = true
line_threshold = 5
decider = "agent"

# Passive decay settings
[decay]
passive_duration_days = 90

# Retrieval settings
# strategy: "conservative", "moderate" (default), or "aggressive"
[retrieval]
max_injections = 5
strategy = "moderate"

# Circuit breaker settings
[circuit_breaker]
max_blocks = 3
cooldown_seconds = 300
"#;

/// Default learnings.md header.
const DEFAULT_LEARNINGS: &str = r#"# Project Learnings

This file contains structured learnings captured by Grove.
Each learning is stored as a markdown section with metadata.

---
"#;

/// The init command implementation.
pub struct InitCommand {
    cwd: String,
}

impl InitCommand {
    /// Create a new init command.
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into() }
    }

    /// Run the init command.
    pub fn run(&self, options: &InitOptions) -> InitOutput {
        let cwd = Path::new(&self.cwd);
        let mut created = Vec::new();
        let mut skipped = Vec::new();

        // Create project .grove directory
        let grove_dir = project_grove_dir(cwd);
        match self.ensure_dir(&grove_dir, options.force) {
            Ok(true) => created.push(grove_dir.display().to_string()),
            Ok(false) => skipped.push(grove_dir.display().to_string()),
            Err(e) => return InitOutput::failure(e, created, skipped),
        }

        // Create project config.toml
        let config_path = grove_dir.join("config.toml");
        match self.ensure_file(&config_path, DEFAULT_CONFIG, options.force) {
            Ok(true) => created.push(config_path.display().to_string()),
            Ok(false) => skipped.push(config_path.display().to_string()),
            Err(e) => return InitOutput::failure(e, created, skipped),
        }

        // Create project learnings.md
        let learnings_path = project_learnings_path(cwd);
        match self.ensure_file(&learnings_path, DEFAULT_LEARNINGS, options.force) {
            Ok(true) => created.push(learnings_path.display().to_string()),
            Ok(false) => skipped.push(learnings_path.display().to_string()),
            Err(e) => return InitOutput::failure(e, created, skipped),
        }

        // Create user-level ~/.grove directory
        if let Some(home) = grove_home() {
            match self.ensure_dir(&home, options.force) {
                Ok(true) => created.push(home.display().to_string()),
                Ok(false) => skipped.push(home.display().to_string()),
                Err(e) => return InitOutput::failure(e, created, skipped),
            }
        }

        // Create sessions directory
        if let Some(sessions) = sessions_dir() {
            match self.ensure_dir(&sessions, options.force) {
                Ok(true) => created.push(sessions.display().to_string()),
                Ok(false) => skipped.push(sessions.display().to_string()),
                Err(e) => return InitOutput::failure(e, created, skipped),
            }
        }

        InitOutput::success(created, skipped)
    }

    /// Ensure a directory exists.
    /// Returns Ok(true) if created, Ok(false) if already exists.
    fn ensure_dir(&self, path: &Path, _force: bool) -> Result<bool, String> {
        if path.exists() {
            if path.is_dir() {
                return Ok(false);
            } else {
                return Err(format!("{} exists but is not a directory", path.display()));
            }
        }

        fs::create_dir_all(path)
            .map_err(|e| format!("Failed to create directory {}: {}", path.display(), e))?;

        Ok(true)
    }

    /// Ensure a file exists with the given content.
    /// Returns Ok(true) if created, Ok(false) if already exists.
    fn ensure_file(&self, path: &Path, content: &str, force: bool) -> Result<bool, String> {
        if path.exists() && !force {
            return Ok(false);
        }

        fs::write(path, content)
            .map_err(|e| format!("Failed to write file {}: {}", path.display(), e))?;

        Ok(true)
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &InitOutput, options: &InitOptions) -> String {
        if options.quiet {
            return String::new();
        }

        if options.json {
            serde_json::to_string_pretty(output).unwrap_or_else(|_| "{}".to_string())
        } else {
            self.format_human_readable(output)
        }
    }

    /// Format output as human-readable text.
    fn format_human_readable(&self, output: &InitOutput) -> String {
        if !output.success {
            let mut lines = Vec::new();
            lines.push(format!(
                "Init failed: {}",
                output.error.as_deref().unwrap_or("unknown error")
            ));

            // Report what was partially created before the failure
            if !output.created.is_empty() {
                lines.push(String::new());
                lines.push("Partially created before failure:".to_string());
                for path in &output.created {
                    lines.push(format!("  {}", path));
                }
            }

            if !output.skipped.is_empty() {
                lines.push(String::new());
                lines.push("Already existed (skipped):".to_string());
                for path in &output.skipped {
                    lines.push(format!("  {}", path));
                }
            }

            return lines.join("\n") + "\n";
        }

        let mut lines = Vec::new();

        if output.created.is_empty() && output.skipped.is_empty() {
            return "Grove already initialized.\n".to_string();
        }

        if !output.created.is_empty() {
            lines.push("Created:".to_string());
            for path in &output.created {
                lines.push(format!("  {}", path));
            }
        }

        if !output.skipped.is_empty() {
            lines.push("Already exists (skipped):".to_string());
            for path in &output.skipped {
                lines.push(format!("  {}", path));
            }
        }

        lines.push(String::new());
        lines.push("Grove initialized successfully.".to_string());

        lines.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_init_output_success() {
        let output = InitOutput::success(vec!["file1".to_string()], vec!["file2".to_string()]);

        assert!(output.success);
        assert_eq!(output.created.len(), 1);
        assert_eq!(output.skipped.len(), 1);
        assert!(output.error.is_none());
    }

    #[test]
    fn test_init_output_failure() {
        let output = InitOutput::failure("test error", vec![], vec![]);

        assert!(!output.success);
        assert!(output.created.is_empty());
        assert!(output.skipped.is_empty());
        assert_eq!(output.error, Some("test error".to_string()));
    }

    #[test]
    fn test_init_output_failure_with_partial_state() {
        let output = InitOutput::failure(
            "permission denied",
            vec!["created_dir".to_string()],
            vec!["skipped_file".to_string()],
        );

        assert!(!output.success);
        assert_eq!(output.created.len(), 1);
        assert_eq!(output.created[0], "created_dir");
        assert_eq!(output.skipped.len(), 1);
        assert_eq!(output.skipped[0], "skipped_file");
        assert_eq!(output.error, Some("permission denied".to_string()));
    }

    #[test]
    fn test_init_creates_directories() {
        let temp = TempDir::new().unwrap();
        let cwd = temp.path();

        let cmd = InitCommand::new(cwd.to_string_lossy().to_string());
        let options = InitOptions::default();
        let output = cmd.run(&options);

        assert!(output.success);
        assert!(cwd.join(".grove").exists());
        assert!(cwd.join(".grove").join("config.toml").exists());
        assert!(cwd.join(".grove").join("learnings.md").exists());
    }

    #[test]
    fn test_init_idempotent() {
        let temp = TempDir::new().unwrap();
        let cwd = temp.path();

        let cmd = InitCommand::new(cwd.to_string_lossy().to_string());
        let options = InitOptions::default();

        // First run creates files
        let output1 = cmd.run(&options);
        assert!(output1.success);
        let created_count = output1.created.len();

        // Second run skips existing files
        let output2 = cmd.run(&options);
        assert!(output2.success);
        assert!(output2.created.is_empty() || output2.created.len() < created_count);
    }

    #[test]
    fn test_init_with_force() {
        let temp = TempDir::new().unwrap();
        let cwd = temp.path();

        let cmd = InitCommand::new(cwd.to_string_lossy().to_string());
        let options = InitOptions::default();

        // First run
        cmd.run(&options);

        // Modify config
        let config_path = cwd.join(".grove").join("config.toml");
        fs::write(&config_path, "# modified").unwrap();

        // Run with force
        let force_options = InitOptions {
            force: true,
            ..Default::default()
        };
        let output = cmd.run(&force_options);

        assert!(output.success);
        // Should have re-created the config file
        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("Grove Configuration"));
    }

    #[test]
    fn test_format_output_json() {
        let temp = TempDir::new().unwrap();
        let cmd = InitCommand::new(temp.path().to_string_lossy().to_string());

        let output = InitOutput::success(vec!["test".to_string()], vec![]);
        let options = InitOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
    }

    #[test]
    fn test_format_output_quiet() {
        let temp = TempDir::new().unwrap();
        let cmd = InitCommand::new(temp.path().to_string_lossy().to_string());

        let output = InitOutput::success(vec!["test".to_string()], vec![]);
        let options = InitOptions {
            quiet: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_output_human_readable() {
        let temp = TempDir::new().unwrap();
        let cmd = InitCommand::new(temp.path().to_string_lossy().to_string());

        let output = InitOutput::success(
            vec!["created.txt".to_string()],
            vec!["skipped.txt".to_string()],
        );
        let options = InitOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Created:"));
        assert!(formatted.contains("created.txt"));
        assert!(formatted.contains("Already exists"));
        assert!(formatted.contains("skipped.txt"));
    }

    #[test]
    fn test_default_config_valid_toml() {
        // Verify the default config is valid TOML
        let _: toml::Value = toml::from_str(DEFAULT_CONFIG).unwrap();
    }

    #[test]
    fn test_learnings_content() {
        let temp = TempDir::new().unwrap();
        let cwd = temp.path();

        let cmd = InitCommand::new(cwd.to_string_lossy().to_string());
        let options = InitOptions::default();
        cmd.run(&options);

        let content = fs::read_to_string(cwd.join(".grove").join("learnings.md")).unwrap();
        assert!(content.contains("# Project Learnings"));
    }

    #[test]
    fn test_format_output_partial_failure() {
        let temp = TempDir::new().unwrap();
        let cmd = InitCommand::new(temp.path().to_string_lossy().to_string());

        let output = InitOutput::failure(
            "permission denied",
            vec!["created_dir".to_string()],
            vec!["skipped_file".to_string()],
        );
        let options = InitOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Init failed: permission denied"));
        assert!(formatted.contains("Partially created before failure:"));
        assert!(formatted.contains("created_dir"));
        assert!(formatted.contains("Already existed (skipped):"));
        assert!(formatted.contains("skipped_file"));
    }

    #[test]
    fn test_format_output_failure_json_includes_partial_state() {
        let temp = TempDir::new().unwrap();
        let cmd = InitCommand::new(temp.path().to_string_lossy().to_string());

        let output =
            InitOutput::failure("permission denied", vec!["created_dir".to_string()], vec![]);
        let options = InitOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": false"));
        assert!(formatted.contains("\"created\""));
        assert!(formatted.contains("created_dir"));
        assert!(formatted.contains("\"error\""));
        assert!(formatted.contains("permission denied"));
    }
}
