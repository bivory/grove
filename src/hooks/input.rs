//! Hook input types for Claude Code integration.
//!
//! These types represent the JSON input that Claude Code passes to grove hooks.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Common input fields shared by all hooks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HookInput {
    /// Unique session identifier.
    pub session_id: String,
    /// Path to the conversation transcript.
    pub transcript_path: PathBuf,
    /// Current working directory.
    pub cwd: PathBuf,
}

impl HookInput {
    /// Create a new hook input.
    pub fn new(
        session_id: impl Into<String>,
        transcript_path: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            transcript_path: transcript_path.into(),
            cwd: cwd.into(),
        }
    }
}

/// Input for session-start hook.
///
/// Contains common fields only - no additional data needed.
pub type SessionStartInput = HookInput;

/// Input for pre-tool-use hook.
///
/// Contains common fields plus tool information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreToolUseInput {
    /// Common hook input fields.
    #[serde(flatten)]
    pub common: HookInput,
    /// The name of the tool being invoked.
    pub tool_name: String,
    /// The tool input (as JSON value).
    #[serde(default)]
    pub tool_input: serde_json::Value,
}

impl PreToolUseInput {
    /// Create a new pre-tool-use input.
    pub fn new(
        common: HookInput,
        tool_name: impl Into<String>,
        tool_input: serde_json::Value,
    ) -> Self {
        Self {
            common,
            tool_name: tool_name.into(),
            tool_input,
        }
    }
}

/// Input for post-tool-use hook.
///
/// Contains common fields plus tool response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostToolUseInput {
    /// Common hook input fields.
    #[serde(flatten)]
    pub common: HookInput,
    /// The name of the tool that was invoked.
    pub tool_name: String,
    /// The tool input (as JSON value).
    #[serde(default)]
    pub tool_input: serde_json::Value,
    /// The tool response/output.
    pub tool_response: String,
}

impl PostToolUseInput {
    /// Create a new post-tool-use input.
    pub fn new(
        common: HookInput,
        tool_name: impl Into<String>,
        tool_input: serde_json::Value,
        tool_response: impl Into<String>,
    ) -> Self {
        Self {
            common,
            tool_name: tool_name.into(),
            tool_input,
            tool_response: tool_response.into(),
        }
    }
}

/// Input for stop hook.
///
/// Contains common fields only.
pub type StopInput = HookInput;

/// Reason for session ending.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionEndReason {
    /// User initiated exit.
    UserExit,
    /// Session timeout.
    Timeout,
    /// Conversation limit reached.
    LimitReached,
    /// Error occurred.
    Error,
    /// Unknown reason.
    #[default]
    Unknown,
}

/// Input for session-end hook.
///
/// Contains common fields plus end reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionEndInput {
    /// Common hook input fields.
    #[serde(flatten)]
    pub common: HookInput,
    /// Reason the session ended.
    #[serde(default)]
    pub reason: SessionEndReason,
}

impl SessionEndInput {
    /// Create a new session-end input.
    pub fn new(common: HookInput, reason: SessionEndReason) -> Self {
        Self { common, reason }
    }
}

/// Parse hook input from JSON.
///
/// This is a generic parser that handles common error cases.
pub fn parse_input<T: for<'de> Deserialize<'de>>(json: &str) -> crate::error::Result<T> {
    serde_json::from_str(json)
        .map_err(|e| crate::error::GroveError::serde(format!("Failed to parse hook input: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_common_input() -> HookInput {
        HookInput::new("session-123", "/tmp/transcript.jsonl", "/home/user/project")
    }

    // HookInput tests

    #[test]
    fn test_hook_input_new() {
        let input = sample_common_input();

        assert_eq!(input.session_id, "session-123");
        assert_eq!(
            input.transcript_path,
            PathBuf::from("/tmp/transcript.jsonl")
        );
        assert_eq!(input.cwd, PathBuf::from("/home/user/project"));
    }

    #[test]
    fn test_hook_input_serialization() {
        let input = sample_common_input();
        let json = serde_json::to_string(&input).unwrap();
        let parsed: HookInput = serde_json::from_str(&json).unwrap();

        assert_eq!(input, parsed);
    }

    #[test]
    fn test_hook_input_from_json() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir"
        }"#;

        let input: HookInput = parse_input(json).unwrap();

        assert_eq!(input.session_id, "test-session");
    }

    #[test]
    fn test_hook_input_missing_field() {
        let json = r#"{
            "session_id": "test-session"
        }"#;

        let result: crate::error::Result<HookInput> = parse_input(json);
        assert!(result.is_err());
    }

    // PreToolUseInput tests

    #[test]
    fn test_pre_tool_use_input_new() {
        let common = sample_common_input();
        let tool_input = serde_json::json!({"command": "echo hello"});

        let input = PreToolUseInput::new(common.clone(), "Bash", tool_input.clone());

        assert_eq!(input.common, common);
        assert_eq!(input.tool_name, "Bash");
        assert_eq!(input.tool_input, tool_input);
    }

    #[test]
    fn test_pre_tool_use_input_serialization() {
        let input = PreToolUseInput::new(
            sample_common_input(),
            "Bash",
            serde_json::json!({"command": "ls"}),
        );

        let json = serde_json::to_string(&input).unwrap();
        let parsed: PreToolUseInput = serde_json::from_str(&json).unwrap();

        assert_eq!(input, parsed);
    }

    #[test]
    fn test_pre_tool_use_input_flattened() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir",
            "tool_name": "Bash",
            "tool_input": {"command": "pwd"}
        }"#;

        let input: PreToolUseInput = serde_json::from_str(json).unwrap();

        assert_eq!(input.common.session_id, "test-session");
        assert_eq!(input.tool_name, "Bash");
    }

    #[test]
    fn test_pre_tool_use_input_default_tool_input() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir",
            "tool_name": "Bash"
        }"#;

        let input: PreToolUseInput = serde_json::from_str(json).unwrap();

        assert_eq!(input.tool_input, serde_json::Value::Null);
    }

    // PostToolUseInput tests

    #[test]
    fn test_post_tool_use_input_new() {
        let common = sample_common_input();
        let tool_input = serde_json::json!({"command": "echo hello"});

        let input = PostToolUseInput::new(common.clone(), "Bash", tool_input.clone(), "hello\n");

        assert_eq!(input.common, common);
        assert_eq!(input.tool_name, "Bash");
        assert_eq!(input.tool_input, tool_input);
        assert_eq!(input.tool_response, "hello\n");
    }

    #[test]
    fn test_post_tool_use_input_serialization() {
        let input = PostToolUseInput::new(
            sample_common_input(),
            "Bash",
            serde_json::json!({"command": "ls"}),
            "file1.txt\nfile2.txt",
        );

        let json = serde_json::to_string(&input).unwrap();
        let parsed: PostToolUseInput = serde_json::from_str(&json).unwrap();

        assert_eq!(input, parsed);
    }

    #[test]
    fn test_post_tool_use_input_flattened() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir",
            "tool_name": "Bash",
            "tool_input": {"command": "pwd"},
            "tool_response": "/home/user"
        }"#;

        let input: PostToolUseInput = serde_json::from_str(json).unwrap();

        assert_eq!(input.common.session_id, "test-session");
        assert_eq!(input.tool_response, "/home/user");
    }

    // SessionEndInput tests

    #[test]
    fn test_session_end_input_new() {
        let common = sample_common_input();
        let input = SessionEndInput::new(common.clone(), SessionEndReason::UserExit);

        assert_eq!(input.common, common);
        assert_eq!(input.reason, SessionEndReason::UserExit);
    }

    #[test]
    fn test_session_end_input_serialization() {
        let input = SessionEndInput::new(sample_common_input(), SessionEndReason::Timeout);

        let json = serde_json::to_string(&input).unwrap();
        let parsed: SessionEndInput = serde_json::from_str(&json).unwrap();

        assert_eq!(input, parsed);
    }

    #[test]
    fn test_session_end_input_flattened() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir",
            "reason": "user_exit"
        }"#;

        let input: SessionEndInput = serde_json::from_str(json).unwrap();

        assert_eq!(input.common.session_id, "test-session");
        assert_eq!(input.reason, SessionEndReason::UserExit);
    }

    #[test]
    fn test_session_end_input_default_reason() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir"
        }"#;

        let input: SessionEndInput = serde_json::from_str(json).unwrap();

        assert_eq!(input.reason, SessionEndReason::Unknown);
    }

    // SessionEndReason tests

    #[test]
    fn test_session_end_reason_serialization() {
        let reasons = [
            (SessionEndReason::UserExit, "\"user_exit\""),
            (SessionEndReason::Timeout, "\"timeout\""),
            (SessionEndReason::LimitReached, "\"limit_reached\""),
            (SessionEndReason::Error, "\"error\""),
            (SessionEndReason::Unknown, "\"unknown\""),
        ];

        for (reason, expected) in reasons {
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(json, expected);

            let parsed: SessionEndReason = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, reason);
        }
    }

    // parse_input tests

    #[test]
    fn test_parse_input_valid() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir"
        }"#;

        let result: crate::error::Result<HookInput> = parse_input(json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_input_invalid_json() {
        let json = "not valid json";

        let result: crate::error::Result<HookInput> = parse_input(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_input_malformed() {
        let json = r#"{ "session_id": 123 }"#;

        let result: crate::error::Result<HookInput> = parse_input(json);
        assert!(result.is_err());
    }
}
