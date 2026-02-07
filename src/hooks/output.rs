//! Hook output types for Claude Code integration.
//!
//! These types represent the JSON output that grove returns to Claude Code hooks.

use serde::{Deserialize, Serialize};

/// Decision for the stop hook.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopDecision {
    /// Allow the session to end.
    Approve,
    /// Block the session from ending.
    Block,
}

impl StopDecision {
    /// Get the exit code for this decision.
    ///
    /// - 0 = approve (session can end)
    /// - 2 = block (gate requires reflection)
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Approve => 0,
            Self::Block => 2,
        }
    }
}

/// Output for the stop hook.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StopOutput {
    /// The decision: approve or block.
    pub decision: StopDecision,
    /// Optional message explaining the decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl StopOutput {
    /// Create an approve output.
    pub fn approve() -> Self {
        Self {
            decision: StopDecision::Approve,
            message: None,
        }
    }

    /// Create an approve output with a message.
    pub fn approve_with_message(message: impl Into<String>) -> Self {
        Self {
            decision: StopDecision::Approve,
            message: Some(message.into()),
        }
    }

    /// Create a block output.
    pub fn block() -> Self {
        Self {
            decision: StopDecision::Block,
            message: None,
        }
    }

    /// Create a block output with a message.
    pub fn block_with_message(message: impl Into<String>) -> Self {
        Self {
            decision: StopDecision::Block,
            message: Some(message.into()),
        }
    }
}

/// Output for the pre-tool-use hook.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PreToolUseOutput {
    /// Whether to allow the tool invocation.
    #[serde(default = "default_true")]
    pub allow: bool,
    /// Optional message (e.g., warning or modification note).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn default_true() -> bool {
    true
}

impl PreToolUseOutput {
    /// Create an allow output.
    pub fn allow() -> Self {
        Self {
            allow: true,
            message: None,
        }
    }

    /// Create an allow output with a message.
    pub fn allow_with_message(message: impl Into<String>) -> Self {
        Self {
            allow: true,
            message: Some(message.into()),
        }
    }

    /// Create a block output.
    pub fn deny() -> Self {
        Self {
            allow: false,
            message: None,
        }
    }

    /// Create a block output with a reason.
    pub fn deny_with_reason(reason: impl Into<String>) -> Self {
        Self {
            allow: false,
            message: Some(reason.into()),
        }
    }
}

/// Output for the session-start hook.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionStartOutput {
    /// Additional context to inject into the session.
    #[serde(rename = "additionalContext", skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
    /// Optional message for logging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl SessionStartOutput {
    /// Create an empty output (no context injection).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create an output with additional context.
    pub fn with_context(context: impl Into<String>) -> Self {
        Self {
            additional_context: Some(context.into()),
            message: None,
        }
    }

    /// Create an output with context and a message.
    pub fn with_context_and_message(
        context: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            additional_context: Some(context.into()),
            message: Some(message.into()),
        }
    }
}

/// Output for the post-tool-use hook.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PostToolUseOutput {
    /// Optional message for logging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl PostToolUseOutput {
    /// Create an empty output.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create an output with a message.
    pub fn with_message(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
        }
    }
}

/// Output for the session-end hook.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionEndOutput {
    /// Optional message for logging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl SessionEndOutput {
    /// Create an empty output.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create an output with a message.
    pub fn with_message(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
        }
    }
}

/// Serialize output to JSON.
pub fn to_json<T: Serialize>(output: &T) -> crate::error::Result<String> {
    serde_json::to_string(output)
        .map_err(|e| crate::error::GroveError::serde(format!("Failed to serialize output: {}", e)))
}

/// Serialize output to pretty JSON.
pub fn to_json_pretty<T: Serialize>(output: &T) -> crate::error::Result<String> {
    serde_json::to_string_pretty(output)
        .map_err(|e| crate::error::GroveError::serde(format!("Failed to serialize output: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // StopDecision tests

    #[test]
    fn test_stop_decision_exit_codes() {
        assert_eq!(StopDecision::Approve.exit_code(), 0);
        assert_eq!(StopDecision::Block.exit_code(), 2);
    }

    #[test]
    fn test_stop_decision_serialization() {
        let approve = serde_json::to_string(&StopDecision::Approve).unwrap();
        assert_eq!(approve, "\"approve\"");

        let block = serde_json::to_string(&StopDecision::Block).unwrap();
        assert_eq!(block, "\"block\"");

        let parsed: StopDecision = serde_json::from_str("\"approve\"").unwrap();
        assert_eq!(parsed, StopDecision::Approve);
    }

    // StopOutput tests

    #[test]
    fn test_stop_output_approve() {
        let output = StopOutput::approve();

        assert_eq!(output.decision, StopDecision::Approve);
        assert!(output.message.is_none());
    }

    #[test]
    fn test_stop_output_approve_with_message() {
        let output = StopOutput::approve_with_message("Session approved");

        assert_eq!(output.decision, StopDecision::Approve);
        assert_eq!(output.message, Some("Session approved".to_string()));
    }

    #[test]
    fn test_stop_output_block() {
        let output = StopOutput::block();

        assert_eq!(output.decision, StopDecision::Block);
        assert!(output.message.is_none());
    }

    #[test]
    fn test_stop_output_block_with_message() {
        let output = StopOutput::block_with_message("Reflection required");

        assert_eq!(output.decision, StopDecision::Block);
        assert_eq!(output.message, Some("Reflection required".to_string()));
    }

    #[test]
    fn test_stop_output_serialization() {
        let output = StopOutput::approve();
        let json = serde_json::to_string(&output).unwrap();
        assert_eq!(json, r#"{"decision":"approve"}"#);

        let output = StopOutput::block_with_message("test");
        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"decision\":\"block\""));
        assert!(json.contains("\"message\":\"test\""));
    }

    // PreToolUseOutput tests

    #[test]
    fn test_pre_tool_use_output_allow() {
        let output = PreToolUseOutput::allow();

        assert!(output.allow);
        assert!(output.message.is_none());
    }

    #[test]
    fn test_pre_tool_use_output_allow_with_message() {
        let output = PreToolUseOutput::allow_with_message("Detected ticket close");

        assert!(output.allow);
        assert_eq!(output.message, Some("Detected ticket close".to_string()));
    }

    #[test]
    fn test_pre_tool_use_output_deny() {
        let output = PreToolUseOutput::deny();

        assert!(!output.allow);
        assert!(output.message.is_none());
    }

    #[test]
    fn test_pre_tool_use_output_deny_with_reason() {
        let output = PreToolUseOutput::deny_with_reason("Blocked");

        assert!(!output.allow);
        assert_eq!(output.message, Some("Blocked".to_string()));
    }

    #[test]
    fn test_pre_tool_use_output_serialization() {
        let output = PreToolUseOutput::allow();
        let json = serde_json::to_string(&output).unwrap();
        assert_eq!(json, r#"{"allow":true}"#);
    }

    #[test]
    fn test_pre_tool_use_output_deserialization_defaults() {
        // Missing 'allow' should default to true
        let json = r#"{}"#;
        let output: PreToolUseOutput = serde_json::from_str(json).unwrap();
        assert!(output.allow);
    }

    // SessionStartOutput tests

    #[test]
    fn test_session_start_output_empty() {
        let output = SessionStartOutput::empty();

        assert!(output.additional_context.is_none());
        assert!(output.message.is_none());
    }

    #[test]
    fn test_session_start_output_with_context() {
        let output = SessionStartOutput::with_context("# Relevant learnings\n- Learning 1");

        assert_eq!(
            output.additional_context,
            Some("# Relevant learnings\n- Learning 1".to_string())
        );
        assert!(output.message.is_none());
    }

    #[test]
    fn test_session_start_output_with_context_and_message() {
        let output =
            SessionStartOutput::with_context_and_message("# Context", "Injected 3 learnings");

        assert_eq!(output.additional_context, Some("# Context".to_string()));
        assert_eq!(output.message, Some("Injected 3 learnings".to_string()));
    }

    #[test]
    fn test_session_start_output_serialization() {
        let output = SessionStartOutput::with_context("Test context");
        let json = serde_json::to_string(&output).unwrap();

        assert!(json.contains("\"additionalContext\":\"Test context\""));
    }

    #[test]
    fn test_session_start_output_empty_serialization() {
        let output = SessionStartOutput::empty();
        let json = serde_json::to_string(&output).unwrap();

        assert_eq!(json, "{}");
    }

    // PostToolUseOutput tests

    #[test]
    fn test_post_tool_use_output_empty() {
        let output = PostToolUseOutput::empty();
        assert!(output.message.is_none());
    }

    #[test]
    fn test_post_tool_use_output_with_message() {
        let output = PostToolUseOutput::with_message("Ticket closed");
        assert_eq!(output.message, Some("Ticket closed".to_string()));
    }

    #[test]
    fn test_post_tool_use_output_serialization() {
        let output = PostToolUseOutput::empty();
        let json = serde_json::to_string(&output).unwrap();
        assert_eq!(json, "{}");

        let output = PostToolUseOutput::with_message("test");
        let json = serde_json::to_string(&output).unwrap();
        assert_eq!(json, r#"{"message":"test"}"#);
    }

    // SessionEndOutput tests

    #[test]
    fn test_session_end_output_empty() {
        let output = SessionEndOutput::empty();
        assert!(output.message.is_none());
    }

    #[test]
    fn test_session_end_output_with_message() {
        let output = SessionEndOutput::with_message("Session cleanup complete");
        assert_eq!(output.message, Some("Session cleanup complete".to_string()));
    }

    #[test]
    fn test_session_end_output_serialization() {
        let output = SessionEndOutput::empty();
        let json = serde_json::to_string(&output).unwrap();
        assert_eq!(json, "{}");
    }

    // to_json tests

    #[test]
    fn test_to_json() {
        let output = StopOutput::approve();
        let json = to_json(&output).unwrap();
        assert_eq!(json, r#"{"decision":"approve"}"#);
    }

    #[test]
    fn test_to_json_pretty() {
        let output = StopOutput::block_with_message("test");
        let json = to_json_pretty(&output).unwrap();

        assert!(json.contains('\n'));
        assert!(json.contains("\"decision\": \"block\""));
    }
}
