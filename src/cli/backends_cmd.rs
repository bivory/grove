//! Backends command for Grove.
//!
//! Shows discovered memory backends and their status.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::config::Config;
use crate::discovery::{detect_backends, BackendInfo};

/// Options for the backends command.
#[derive(Debug, Clone, Default)]
pub struct BackendsOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
}

/// Output format for the backends command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendsOutput {
    /// Whether the command was successful.
    pub success: bool,
    /// Discovered backends.
    pub backends: Vec<BackendDetail>,
    /// The active backend (first available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// Error message if command failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Detailed backend information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendDetail {
    /// Backend name.
    pub name: String,
    /// Backend type.
    pub backend_type: String,
    /// Whether the backend is available (has a path).
    pub available: bool,
    /// Whether this is the primary backend.
    pub is_primary: bool,
    /// Backend path or endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Health check result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
}

impl From<&BackendInfo> for BackendDetail {
    fn from(info: &BackendInfo) -> Self {
        let available = info.path.is_some();
        Self {
            name: info.backend_type.as_str().to_string(),
            backend_type: format!("{:?}", info.backend_type),
            available,
            is_primary: info.is_primary,
            path: info.path.as_ref().map(|p| p.display().to_string()),
            health: if available {
                Some("ok".to_string())
            } else {
                Some("unavailable".to_string())
            },
        }
    }
}

impl BackendsOutput {
    /// Create a successful output.
    pub fn success(backends: Vec<BackendDetail>, active: Option<String>) -> Self {
        Self {
            success: true,
            backends,
            active,
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            backends: Vec::new(),
            active: None,
            error: Some(error.into()),
        }
    }
}

/// The backends command implementation.
pub struct BackendsCommand {
    cwd: String,
    config: Config,
}

impl BackendsCommand {
    /// Create a new backends command.
    pub fn new(cwd: impl Into<String>, config: Config) -> Self {
        Self {
            cwd: cwd.into(),
            config,
        }
    }

    /// Run the backends command.
    pub fn run(&self, _options: &BackendsOptions) -> BackendsOutput {
        let cwd = Path::new(&self.cwd);

        // Detect backends
        let backends = detect_backends(cwd, Some(&self.config));

        // Convert to detail format
        let details: Vec<BackendDetail> = backends.iter().map(BackendDetail::from).collect();

        // Find the active (first available/primary) backend
        let active = backends
            .iter()
            .find(|b| b.is_primary)
            .map(|b| b.backend_type.as_str().to_string());

        BackendsOutput::success(details, active)
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &BackendsOutput, options: &BackendsOptions) -> String {
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
    fn format_human_readable(&self, output: &BackendsOutput) -> String {
        if !output.success {
            return format!(
                "Backends command failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            );
        }

        let mut lines = Vec::new();

        if output.backends.is_empty() {
            return "No backends discovered.\n".to_string();
        }

        lines.push("Discovered backends:\n".to_string());

        for backend in &output.backends {
            let status = if backend.available { "+" } else { "-" };
            let path_info = backend
                .path
                .as_ref()
                .map(|p| format!(" ({})", p))
                .unwrap_or_default();

            lines.push(format!(
                "  [{}] {}: {}{}",
                status, backend.name, backend.backend_type, path_info
            ));
        }

        lines.push(String::new());

        if let Some(active) = &output.active {
            lines.push(format!("Active backend: {}", active));
        } else {
            lines.push("No active backend available.".to_string());
        }

        lines.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::BackendType;
    use std::fs;
    use tempfile::TempDir;

    fn setup_with_learnings() -> (TempDir, Config) {
        let temp = TempDir::new().unwrap();
        let grove_dir = temp.path().join(".grove");
        fs::create_dir_all(&grove_dir).unwrap();

        let learnings_content = r#"# Project Learnings

---
"#;
        fs::write(grove_dir.join("learnings.md"), learnings_content).unwrap();

        (temp, Config::default())
    }

    #[test]
    fn test_backends_output_success() {
        let backends = vec![BackendDetail {
            name: "test".to_string(),
            backend_type: "Markdown".to_string(),
            available: true,
            is_primary: true,
            path: Some("/path/to/learnings.md".to_string()),
            health: Some("ok".to_string()),
        }];
        let output = BackendsOutput::success(backends, Some("test".to_string()));

        assert!(output.success);
        assert_eq!(output.backends.len(), 1);
        assert_eq!(output.active, Some("test".to_string()));
        assert!(output.error.is_none());
    }

    #[test]
    fn test_backends_output_failure() {
        let output = BackendsOutput::failure("test error");

        assert!(!output.success);
        assert!(output.backends.is_empty());
        assert!(output.active.is_none());
        assert_eq!(output.error, Some("test error".to_string()));
    }

    #[test]
    fn test_backends_discovers_markdown() {
        let (temp, config) = setup_with_learnings();
        let cmd = BackendsCommand::new(temp.path().to_string_lossy().to_string(), config);
        let options = BackendsOptions::default();

        let output = cmd.run(&options);

        assert!(output.success);
        // Should find markdown backend
        assert!(output
            .backends
            .iter()
            .any(|b| b.backend_type.contains("Markdown")));
    }

    #[test]
    fn test_backends_without_learnings() {
        let temp = TempDir::new().unwrap();
        let config = Config::default();
        let cmd = BackendsCommand::new(temp.path().to_string_lossy().to_string(), config);
        let options = BackendsOptions::default();

        let output = cmd.run(&options);

        assert!(output.success);
        // May still list backends, but they might be unavailable
    }

    #[test]
    fn test_format_output_json() {
        let (temp, config) = setup_with_learnings();
        let cmd = BackendsCommand::new(temp.path().to_string_lossy().to_string(), config);

        let backends = vec![BackendDetail {
            name: "test".to_string(),
            backend_type: "Markdown".to_string(),
            available: true,
            is_primary: true,
            path: None,
            health: None,
        }];
        let output = BackendsOutput::success(backends, Some("test".to_string()));
        let options = BackendsOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"active\": \"test\""));
    }

    #[test]
    fn test_format_output_quiet() {
        let (temp, config) = setup_with_learnings();
        let cmd = BackendsCommand::new(temp.path().to_string_lossy().to_string(), config);

        let output = BackendsOutput::success(vec![], None);
        let options = BackendsOptions {
            quiet: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_output_human_readable() {
        let (temp, config) = setup_with_learnings();
        let cmd = BackendsCommand::new(temp.path().to_string_lossy().to_string(), config);

        let backends = vec![
            BackendDetail {
                name: "markdown".to_string(),
                backend_type: "Markdown".to_string(),
                available: true,
                is_primary: true,
                path: Some("/path/to/learnings.md".to_string()),
                health: Some("ok".to_string()),
            },
            BackendDetail {
                name: "total-recall".to_string(),
                backend_type: "TotalRecall".to_string(),
                available: false,
                is_primary: false,
                path: None,
                health: Some("unavailable".to_string()),
            },
        ];
        let output = BackendsOutput::success(backends, Some("markdown".to_string()));
        let options = BackendsOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Discovered backends:"));
        assert!(formatted.contains("[+] markdown"));
        assert!(formatted.contains("[-] total-recall"));
        assert!(formatted.contains("Active backend: markdown"));
    }

    #[test]
    fn test_format_output_no_backends() {
        let (temp, config) = setup_with_learnings();
        let cmd = BackendsCommand::new(temp.path().to_string_lossy().to_string(), config);

        let output = BackendsOutput::success(vec![], None);
        let options = BackendsOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("No backends discovered"));
    }

    #[test]
    fn test_backend_detail_from_info() {
        let info = BackendInfo::new(BackendType::Markdown, Some("/path/to/file".into()), true);

        let detail = BackendDetail::from(&info);
        assert_eq!(detail.name, "markdown");
        assert!(detail.backend_type.contains("Markdown"));
        assert!(detail.available);
        assert!(detail.is_primary);
        assert!(detail.path.is_some());
        assert_eq!(detail.health, Some("ok".to_string()));
    }

    #[test]
    fn test_backend_detail_unavailable() {
        let info = BackendInfo::new(BackendType::TotalRecall, None, false);

        let detail = BackendDetail::from(&info);
        assert!(!detail.available);
        assert!(!detail.is_primary);
        assert_eq!(detail.health, Some("unavailable".to_string()));
    }
}
