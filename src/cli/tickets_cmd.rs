//! Tickets command for Grove.
//!
//! Shows discovered ticketing system and its status.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::config::Config;
use crate::discovery::{detect_ticketing_system, TicketingInfo, TicketingSystem};

/// Options for the tickets command.
#[derive(Debug, Clone, Default)]
pub struct TicketsOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
}

/// Output format for the tickets command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketsOutput {
    /// Whether the command was successful.
    pub success: bool,
    /// Whether a ticketing system was detected (non-session).
    pub detected: bool,
    /// Ticketing system details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticketing: Option<TicketingDetail>,
    /// Error message if command failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Detailed ticketing system information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketingDetail {
    /// System name.
    pub system: String,
    /// System type.
    pub system_type: String,
    /// Example close command.
    pub close_command: String,
    /// Path to the ticketing system config/data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl From<&TicketingInfo> for TicketingDetail {
    fn from(info: &TicketingInfo) -> Self {
        let close_example = match info.system {
            TicketingSystem::Tissue => "tissue status <id> closed",
            TicketingSystem::Beads => "beads close <id>",
            TicketingSystem::Tasks => "tasks complete <id>",
            TicketingSystem::Session => "(end of session)",
        };

        Self {
            system: info.system.as_str().to_string(),
            system_type: format!("{:?}", info.system),
            close_command: close_example.to_string(),
            path: info.marker_path.as_ref().map(|p| p.display().to_string()),
        }
    }
}

impl TicketsOutput {
    /// Create a successful output with a detected ticketing system.
    pub fn success_detected(ticketing: TicketingDetail) -> Self {
        Self {
            success: true,
            detected: true,
            ticketing: Some(ticketing),
            error: None,
        }
    }

    /// Create a successful output with no detected ticketing system.
    pub fn success_none() -> Self {
        Self {
            success: true,
            detected: false,
            ticketing: None,
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            detected: false,
            ticketing: None,
            error: Some(error.into()),
        }
    }
}

/// The tickets command implementation.
pub struct TicketsCommand {
    cwd: String,
    config: Config,
}

impl TicketsCommand {
    /// Create a new tickets command.
    pub fn new(cwd: impl Into<String>, config: Config) -> Self {
        Self {
            cwd: cwd.into(),
            config,
        }
    }

    /// Run the tickets command.
    pub fn run(&self, _options: &TicketsOptions) -> TicketsOutput {
        let cwd = Path::new(&self.cwd);

        // Detect ticketing system
        let info = detect_ticketing_system(cwd, Some(&self.config));

        // Session-based is always available but counts as "not detected" for display
        if info.system == TicketingSystem::Session {
            TicketsOutput::success_none()
        } else {
            let detail = TicketingDetail::from(&info);
            TicketsOutput::success_detected(detail)
        }
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &TicketsOutput, options: &TicketsOptions) -> String {
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
    fn format_human_readable(&self, output: &TicketsOutput) -> String {
        if !output.success {
            return format!(
                "Tickets command failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            );
        }

        if !output.detected {
            return "No ticketing system detected.\n\nGrove will operate in session-based mode (gate triggers at end of session).\n".to_string();
        }

        // Defensive: handle case where detected=true but ticketing is None
        let Some(ticketing) = output.ticketing.as_ref() else {
            return "Error: ticketing system detected but details unavailable.\n".to_string();
        };

        let mut lines = Vec::new();
        lines.push(format!("Ticketing system detected: {}\n", ticketing.system));
        lines.push(format!("Type: {}", ticketing.system_type));
        lines.push(format!("Close command: {}", ticketing.close_command));

        if let Some(path) = &ticketing.path {
            lines.push(format!("Path: {}", path));
        }

        lines.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_with_tissue() -> (TempDir, Config) {
        let temp = TempDir::new().unwrap();
        let tissue_dir = temp.path().join(".tissue");
        fs::create_dir_all(&tissue_dir).unwrap();

        // Create a minimal tissue config
        fs::write(tissue_dir.join("config.toml"), "[tissue]\n").unwrap();

        (temp, Config::default())
    }

    #[test]
    fn test_tickets_output_success_detected() {
        let detail = TicketingDetail {
            system: "tissue".to_string(),
            system_type: "Tissue".to_string(),
            close_command: "tissue status <id> closed".to_string(),
            path: Some("/path/to/.tissue".to_string()),
        };
        let output = TicketsOutput::success_detected(detail);

        assert!(output.success);
        assert!(output.detected);
        assert!(output.ticketing.is_some());
        assert!(output.error.is_none());
    }

    #[test]
    fn test_tickets_output_success_none() {
        let output = TicketsOutput::success_none();

        assert!(output.success);
        assert!(!output.detected);
        assert!(output.ticketing.is_none());
        assert!(output.error.is_none());
    }

    #[test]
    fn test_tickets_output_failure() {
        let output = TicketsOutput::failure("test error");

        assert!(!output.success);
        assert!(!output.detected);
        assert!(output.ticketing.is_none());
        assert_eq!(output.error, Some("test error".to_string()));
    }

    #[test]
    fn test_tickets_detects_tissue() {
        let (temp, config) = setup_with_tissue();
        let cmd = TicketsCommand::new(temp.path().to_string_lossy().to_string(), config);
        let options = TicketsOptions::default();

        let output = cmd.run(&options);

        assert!(output.success);
        assert!(output.detected);
        assert!(output.ticketing.is_some());
        let ticketing = output.ticketing.unwrap();
        assert!(ticketing.system_type.contains("Tissue"));
    }

    #[test]
    fn test_tickets_without_system() {
        let temp = TempDir::new().unwrap();
        let config = Config::default();
        let cmd = TicketsCommand::new(temp.path().to_string_lossy().to_string(), config);
        let options = TicketsOptions::default();

        let output = cmd.run(&options);

        assert!(output.success);
        // Without tissue/beads, should return not detected (session-based fallback)
        assert!(!output.detected);
    }

    #[test]
    fn test_format_output_json() {
        let (temp, config) = setup_with_tissue();
        let cmd = TicketsCommand::new(temp.path().to_string_lossy().to_string(), config);

        let detail = TicketingDetail {
            system: "tissue".to_string(),
            system_type: "Tissue".to_string(),
            close_command: "tissue status <id> closed".to_string(),
            path: None,
        };
        let output = TicketsOutput::success_detected(detail);
        let options = TicketsOptions {
            json: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
        assert!(formatted.contains("\"detected\": true"));
    }

    #[test]
    fn test_format_output_quiet() {
        let (temp, config) = setup_with_tissue();
        let cmd = TicketsCommand::new(temp.path().to_string_lossy().to_string(), config);

        let output = TicketsOutput::success_none();
        let options = TicketsOptions {
            quiet: true,
            ..Default::default()
        };

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_output_human_readable_detected() {
        let (temp, config) = setup_with_tissue();
        let cmd = TicketsCommand::new(temp.path().to_string_lossy().to_string(), config);

        let detail = TicketingDetail {
            system: "tissue".to_string(),
            system_type: "Tissue".to_string(),
            close_command: "tissue status <id> closed".to_string(),
            path: Some("/path/to/.tissue".to_string()),
        };
        let output = TicketsOutput::success_detected(detail);
        let options = TicketsOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("Ticketing system detected: tissue"));
        assert!(formatted.contains("Type: Tissue"));
        assert!(formatted.contains("Close command:"));
        assert!(formatted.contains("Path:"));
    }

    #[test]
    fn test_format_output_human_readable_none() {
        let (temp, config) = setup_with_tissue();
        let cmd = TicketsCommand::new(temp.path().to_string_lossy().to_string(), config);

        let output = TicketsOutput::success_none();
        let options = TicketsOptions::default();

        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("No ticketing system detected"));
        assert!(formatted.contains("session-based mode"));
    }

    #[test]
    fn test_ticketing_detail_from_info() {
        let info = TicketingInfo::new(TicketingSystem::Tissue, Some("/path/to/.tissue".into()));

        let detail = TicketingDetail::from(&info);
        assert_eq!(detail.system, "tissue");
        assert!(detail.system_type.contains("Tissue"));
        assert!(detail.close_command.contains("tissue status"));
        assert!(detail.path.is_some());
    }

    #[test]
    fn test_format_human_readable_detected_none_ticketing() {
        // Test defensive handling when detected=true but ticketing=None
        let temp = TempDir::new().unwrap();
        let config = Config::default();
        let cmd = TicketsCommand::new(temp.path().to_string_lossy().to_string(), config);

        // Manually construct an inconsistent state
        let output = TicketsOutput {
            success: true,
            detected: true,
            ticketing: None, // Inconsistent: detected but no ticketing detail
            error: None,
        };
        let options = TicketsOptions::default();

        let formatted = cmd.format_output(&output, &options);
        // Should not panic, should return error message
        assert!(formatted.contains("Error:") || formatted.contains("unavailable"));
    }
}
