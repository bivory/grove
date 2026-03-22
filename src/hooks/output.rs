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
    /// For Stop hooks, Claude Code reads the JSON `decision` field on exit 0.
    /// This method is retained for TaskCompleted, which uses exit code 2 + stderr.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Approve => 0,
            Self::Block => 2,
        }
    }
}

/// Output for the stop hook.
///
/// Claude Code expects `reason` (not `message`) in the JSON output.
/// The decision is communicated via the `decision` field; exit code is always 0.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StopOutput {
    /// The decision: approve or block.
    pub decision: StopDecision,
    /// Optional reason explaining the decision (Claude Code reads this field).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl StopOutput {
    /// Create an approve output.
    pub fn approve() -> Self {
        Self {
            decision: StopDecision::Approve,
            reason: None,
        }
    }

    /// Create an approve output with a reason.
    pub fn approve_with_reason(reason: impl Into<String>) -> Self {
        Self {
            decision: StopDecision::Approve,
            reason: Some(reason.into()),
        }
    }

    /// Create a block output.
    pub fn block() -> Self {
        Self {
            decision: StopDecision::Block,
            reason: None,
        }
    }

    /// Create a block output with a reason.
    pub fn block_with_reason(reason: impl Into<String>) -> Self {
        Self {
            decision: StopDecision::Block,
            reason: Some(reason.into()),
        }
    }
}

/// Hook-specific output for PreToolUse.
///
/// Claude Code expects `hookSpecificOutput` with `permissionDecision` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreToolUseHookOutput {
    /// The hook event name (always "PreToolUse").
    #[serde(rename = "hookEventName")]
    pub hook_event_name: String,
    /// Permission decision: "allow" or "deny".
    #[serde(rename = "permissionDecision")]
    pub permission_decision: String,
    /// Reason for the permission decision.
    #[serde(
        rename = "permissionDecisionReason",
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_decision_reason: Option<String>,
    /// Additional context to inject.
    #[serde(rename = "additionalContext", skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// Output for the pre-tool-use hook.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PreToolUseOutput {
    /// Hook-specific output following Claude Code's specification.
    #[serde(rename = "hookSpecificOutput", skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<PreToolUseHookOutput>,
}

impl PreToolUseOutput {
    /// Create an allow output.
    pub fn allow() -> Self {
        Self {
            hook_specific_output: Some(PreToolUseHookOutput {
                hook_event_name: "PreToolUse".to_string(),
                permission_decision: "allow".to_string(),
                permission_decision_reason: None,
                additional_context: None,
            }),
        }
    }

    /// Create an allow output with additional context.
    pub fn allow_with_context(context: impl Into<String>) -> Self {
        Self {
            hook_specific_output: Some(PreToolUseHookOutput {
                hook_event_name: "PreToolUse".to_string(),
                permission_decision: "allow".to_string(),
                permission_decision_reason: None,
                additional_context: Some(context.into()),
            }),
        }
    }

    /// Create a deny output.
    pub fn deny() -> Self {
        Self {
            hook_specific_output: Some(PreToolUseHookOutput {
                hook_event_name: "PreToolUse".to_string(),
                permission_decision: "deny".to_string(),
                permission_decision_reason: None,
                additional_context: None,
            }),
        }
    }

    /// Create a deny output with a reason.
    pub fn deny_with_reason(reason: impl Into<String>) -> Self {
        Self {
            hook_specific_output: Some(PreToolUseHookOutput {
                hook_event_name: "PreToolUse".to_string(),
                permission_decision: "deny".to_string(),
                permission_decision_reason: Some(reason.into()),
                additional_context: None,
            }),
        }
    }

    /// Helper: check if this output allows the tool.
    pub fn is_allowed(&self) -> bool {
        self.hook_specific_output
            .as_ref()
            .map(|h| h.permission_decision == "allow")
            .unwrap_or(true)
    }
}

/// Hook-specific output for SessionStart.
///
/// Claude Code expects `hookSpecificOutput` containing `additionalContext`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionStartHookOutput {
    /// The hook event name (always "SessionStart").
    #[serde(rename = "hookEventName")]
    pub hook_event_name: String,
    /// Additional context to inject into the session.
    #[serde(rename = "additionalContext", skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// Output for the session-start hook.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionStartOutput {
    /// Hook-specific output following Claude Code's specification.
    #[serde(rename = "hookSpecificOutput", skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<SessionStartHookOutput>,
}

impl SessionStartOutput {
    /// Create an empty output (no context injection).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create an output with additional context.
    pub fn with_context(context: impl Into<String>) -> Self {
        Self {
            hook_specific_output: Some(SessionStartHookOutput {
                hook_event_name: "SessionStart".to_string(),
                additional_context: Some(context.into()),
            }),
        }
    }

    /// Helper: get the additional context string (if any).
    pub fn additional_context(&self) -> Option<&str> {
        self.hook_specific_output
            .as_ref()
            .and_then(|h| h.additional_context.as_deref())
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

/// Hook-specific output for the user-prompt-submit hook.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserPromptSubmitHookOutput {
    /// The hook event name (always "UserPromptSubmit").
    #[serde(rename = "hookEventName")]
    pub hook_event_name: String,
    /// Additional context to inject into the session.
    #[serde(rename = "additionalContext", skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// Output for the user-prompt-submit hook.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UserPromptSubmitOutput {
    /// Hook-specific output with context injection.
    #[serde(rename = "hookSpecificOutput", skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<UserPromptSubmitHookOutput>,
}

impl UserPromptSubmitOutput {
    /// Create an empty output (no context to inject).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create an output with additional context.
    pub fn with_context(context: impl Into<String>) -> Self {
        Self {
            hook_specific_output: Some(UserPromptSubmitHookOutput {
                hook_event_name: "UserPromptSubmit".to_string(),
                additional_context: Some(context.into()),
            }),
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
        assert!(output.reason.is_none());
    }

    #[test]
    fn test_stop_output_approve_with_reason() {
        let output = StopOutput::approve_with_reason("Session approved");

        assert_eq!(output.decision, StopDecision::Approve);
        assert_eq!(output.reason, Some("Session approved".to_string()));
    }

    #[test]
    fn test_stop_output_block() {
        let output = StopOutput::block();

        assert_eq!(output.decision, StopDecision::Block);
        assert!(output.reason.is_none());
    }

    #[test]
    fn test_stop_output_block_with_reason() {
        let output = StopOutput::block_with_reason("Reflection required");

        assert_eq!(output.decision, StopDecision::Block);
        assert_eq!(output.reason, Some("Reflection required".to_string()));
    }

    #[test]
    fn test_stop_output_serialization() {
        let output = StopOutput::approve();
        let json = serde_json::to_string(&output).unwrap();
        assert_eq!(json, r#"{"decision":"approve"}"#);

        let output = StopOutput::block_with_reason("test");
        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"decision\":\"block\""));
        assert!(json.contains("\"reason\":\"test\""));
    }

    // PreToolUseOutput tests

    #[test]
    fn test_pre_tool_use_output_allow() {
        let output = PreToolUseOutput::allow();

        assert!(output.is_allowed());
        let hso = output.hook_specific_output.unwrap();
        assert_eq!(hso.permission_decision, "allow");
        assert_eq!(hso.hook_event_name, "PreToolUse");
    }

    #[test]
    fn test_pre_tool_use_output_allow_with_context() {
        let output = PreToolUseOutput::allow_with_context("Detected ticket close");

        assert!(output.is_allowed());
        let hso = output.hook_specific_output.unwrap();
        assert_eq!(
            hso.additional_context,
            Some("Detected ticket close".to_string())
        );
    }

    #[test]
    fn test_pre_tool_use_output_deny() {
        let output = PreToolUseOutput::deny();

        assert!(!output.is_allowed());
        let hso = output.hook_specific_output.unwrap();
        assert_eq!(hso.permission_decision, "deny");
    }

    #[test]
    fn test_pre_tool_use_output_deny_with_reason() {
        let output = PreToolUseOutput::deny_with_reason("Blocked");

        assert!(!output.is_allowed());
        let hso = output.hook_specific_output.unwrap();
        assert_eq!(hso.permission_decision_reason, Some("Blocked".to_string()));
    }

    #[test]
    fn test_pre_tool_use_output_serialization() {
        let output = PreToolUseOutput::allow();
        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"hookSpecificOutput\""));
        assert!(json.contains("\"permissionDecision\":\"allow\""));
        assert!(json.contains("\"hookEventName\":\"PreToolUse\""));
    }

    #[test]
    fn test_pre_tool_use_output_deserialization_defaults() {
        // Empty object should default to no hook_specific_output (allowed)
        let json = r#"{}"#;
        let output: PreToolUseOutput = serde_json::from_str(json).unwrap();
        assert!(output.is_allowed());
    }

    // SessionStartOutput tests

    #[test]
    fn test_session_start_output_empty() {
        let output = SessionStartOutput::empty();

        assert!(output.hook_specific_output.is_none());
    }

    #[test]
    fn test_session_start_output_with_context() {
        let output = SessionStartOutput::with_context("# Relevant learnings\n- Learning 1");

        let hso = output.hook_specific_output.unwrap();
        assert_eq!(hso.hook_event_name, "SessionStart");
        assert_eq!(
            hso.additional_context,
            Some("# Relevant learnings\n- Learning 1".to_string())
        );
    }

    #[test]
    fn test_session_start_output_serialization() {
        let output = SessionStartOutput::with_context("Test context");
        let json = serde_json::to_string(&output).unwrap();

        assert!(json.contains("\"hookSpecificOutput\""));
        assert!(json.contains("\"additionalContext\":\"Test context\""));
        assert!(json.contains("\"hookEventName\":\"SessionStart\""));
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
        let output = StopOutput::block_with_reason("test");
        let json = to_json_pretty(&output).unwrap();

        assert!(json.contains('\n'));
        assert!(json.contains("\"decision\": \"block\""));
    }

    // UserPromptSubmitOutput tests

    #[test]
    fn test_user_prompt_submit_output_empty() {
        let output = UserPromptSubmitOutput::empty();
        assert!(output.hook_specific_output.is_none());

        let json = to_json(&output).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn test_user_prompt_submit_output_with_context() {
        let output = UserPromptSubmitOutput::with_context("relevant learning context");
        let hook_output = output.hook_specific_output.as_ref().unwrap();

        assert_eq!(hook_output.hook_event_name, "UserPromptSubmit");
        assert_eq!(
            hook_output.additional_context.as_ref().unwrap(),
            "relevant learning context"
        );
    }

    #[test]
    fn test_user_prompt_submit_output_serialization() {
        let output = UserPromptSubmitOutput::with_context("test context");
        let json = to_json(&output).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"],
            "UserPromptSubmit"
        );
        assert_eq!(
            parsed["hookSpecificOutput"]["additionalContext"],
            "test context"
        );
    }

    #[test]
    fn test_user_prompt_submit_output_round_trip() {
        let output = UserPromptSubmitOutput::with_context("round trip context");
        let json = to_json(&output).unwrap();
        let parsed: UserPromptSubmitOutput = serde_json::from_str(&json).unwrap();

        assert_eq!(output, parsed);
    }

    #[test]
    fn test_user_prompt_submit_output_empty_serializes_no_null() {
        let output = UserPromptSubmitOutput::empty();
        let json = to_json(&output).unwrap();
        // skip_serializing_if = "Option::is_none" should omit the field
        assert!(!json.contains("hookSpecificOutput"));
    }
}
