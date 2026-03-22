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
    /// The hook event name (e.g., "Stop", "PreToolUse").
    #[serde(default)]
    pub hook_event_name: Option<String>,
    /// Current permission mode (e.g., "default", "plan").
    #[serde(default)]
    pub permission_mode: Option<String>,
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
            hook_event_name: None,
            permission_mode: None,
        }
    }
}

/// Input for session-start hook.
///
/// Contains common fields plus optional session metadata from Claude Code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionStartInput {
    /// Common hook input fields.
    #[serde(flatten)]
    pub common: HookInput,
    /// Source of the session (e.g., "cli", "ide").
    #[serde(default)]
    pub source: Option<String>,
    /// The model being used (e.g., "claude-sonnet-4-6").
    #[serde(default)]
    pub model: Option<String>,
}

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
    /// Unique identifier for this tool use.
    #[serde(default)]
    pub tool_use_id: Option<String>,
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
            tool_use_id: None,
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
    /// Unique identifier for this tool use.
    #[serde(default)]
    pub tool_use_id: Option<String>,
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
            tool_use_id: None,
        }
    }
}

/// Input for stop hook.
///
/// Contains common fields plus stop-hook-specific fields from Claude Code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StopInput {
    /// Common hook input fields.
    #[serde(flatten)]
    pub common: HookInput,
    /// Whether the agent is already in a stop-hook-triggered continuation.
    /// When true, the agent is trying to resolve a previous block — don't block again.
    #[serde(default)]
    pub stop_hook_active: bool,
    /// The last assistant message before the stop hook was triggered.
    #[serde(default)]
    pub last_assistant_message: Option<String>,
}

impl StopInput {
    /// Create a new stop input.
    pub fn new(common: HookInput) -> Self {
        Self {
            common,
            stop_hook_active: false,
            last_assistant_message: None,
        }
    }
}

/// Reason for session ending.
///
/// Values match Claude Code's actual reason strings.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionEndReason {
    /// User cleared the conversation.
    Clear,
    /// User logged out.
    Logout,
    /// User exited at the prompt input.
    PromptInputExit,
    /// Bypass permissions was disabled.
    BypassPermissionsDisabled,
    /// User initiated exit (legacy, may still appear).
    UserExit,
    /// Session timeout (legacy, may still appear).
    Timeout,
    /// Conversation limit reached (legacy, may still appear).
    LimitReached,
    /// Error occurred (legacy, may still appear).
    Error,
    /// Unknown or unrecognized reason.
    #[default]
    #[serde(other)]
    Other,
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

/// Input for TaskCompleted hook.
///
/// Fired when a Claude Code task is marked as completed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskCompletedInput {
    /// Common hook input fields.
    #[serde(flatten)]
    pub common: HookInput,
    /// The task's unique identifier.
    pub task_id: String,
    /// The task's subject/title.
    pub task_subject: String,
    /// Optional task description.
    #[serde(default)]
    pub task_description: Option<String>,
    /// Optional teammate name (for agent teams).
    #[serde(default)]
    pub teammate_name: Option<String>,
    /// Optional team name (for agent teams).
    #[serde(default)]
    pub team_name: Option<String>,
}

impl TaskCompletedInput {
    /// Create a new task-completed input.
    pub fn new(
        common: HookInput,
        task_id: impl Into<String>,
        task_subject: impl Into<String>,
    ) -> Self {
        Self {
            common,
            task_id: task_id.into(),
            task_subject: task_subject.into(),
            task_description: None,
            teammate_name: None,
            team_name: None,
        }
    }

    /// Create a task-completed input with description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.task_description = Some(description.into());
        self
    }
}

/// Input for user-prompt-submit hook.
///
/// Fires when the user submits a prompt, before Claude processes it.
/// Contains the user's prompt text for keyword extraction and re-retrieval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserPromptSubmitInput {
    /// Common hook input fields.
    #[serde(flatten)]
    pub common: HookInput,
    /// The user's submitted prompt text.
    pub prompt: String,
}

impl UserPromptSubmitInput {
    /// Create a new user-prompt-submit input.
    pub fn new(common: HookInput, prompt: impl Into<String>) -> Self {
        Self {
            common,
            prompt: prompt.into(),
        }
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

        assert_eq!(input.reason, SessionEndReason::Other);
    }

    // SessionEndReason tests

    #[test]
    fn test_session_end_reason_serialization() {
        let reasons = [
            (SessionEndReason::Clear, "\"clear\""),
            (SessionEndReason::Logout, "\"logout\""),
            (SessionEndReason::PromptInputExit, "\"prompt_input_exit\""),
            (
                SessionEndReason::BypassPermissionsDisabled,
                "\"bypass_permissions_disabled\"",
            ),
            (SessionEndReason::UserExit, "\"user_exit\""),
            (SessionEndReason::Timeout, "\"timeout\""),
            (SessionEndReason::LimitReached, "\"limit_reached\""),
            (SessionEndReason::Error, "\"error\""),
        ];

        for (reason, expected) in reasons {
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(json, expected);

            let parsed: SessionEndReason = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, reason);
        }
    }

    #[test]
    fn test_session_end_reason_unknown_falls_back_to_other() {
        // Unrecognized strings should deserialize to Other via #[serde(other)]
        let parsed: SessionEndReason = serde_json::from_str("\"some_future_reason\"").unwrap();
        assert_eq!(parsed, SessionEndReason::Other);
    }

    // parse_input tests

    // StopInput tests

    #[test]
    fn test_stop_input_new() {
        let common = sample_common_input();
        let input = StopInput::new(common.clone());

        assert_eq!(input.common, common);
        assert!(!input.stop_hook_active);
    }

    #[test]
    fn test_stop_input_with_stop_hook_active() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir",
            "stop_hook_active": true
        }"#;

        let input: StopInput = serde_json::from_str(json).unwrap();

        assert_eq!(input.common.session_id, "test-session");
        assert!(input.stop_hook_active);
    }

    #[test]
    fn test_stop_input_defaults_stop_hook_active() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir"
        }"#;

        let input: StopInput = serde_json::from_str(json).unwrap();
        assert!(!input.stop_hook_active);
        assert!(input.last_assistant_message.is_none());
    }

    #[test]
    fn test_stop_input_with_last_assistant_message() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir",
            "stop_hook_active": true,
            "last_assistant_message": "I've completed the task."
        }"#;

        let input: StopInput = serde_json::from_str(json).unwrap();
        assert!(input.stop_hook_active);
        assert_eq!(
            input.last_assistant_message,
            Some("I've completed the task.".to_string())
        );
    }

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

    // TaskCompletedInput tests

    #[test]
    fn test_task_completed_input_new() {
        let common = sample_common_input();
        let input = TaskCompletedInput::new(common.clone(), "task-001", "Implement feature");

        assert_eq!(input.common, common);
        assert_eq!(input.task_id, "task-001");
        assert_eq!(input.task_subject, "Implement feature");
        assert!(input.task_description.is_none());
        assert!(input.teammate_name.is_none());
        assert!(input.team_name.is_none());
    }

    #[test]
    fn test_task_completed_input_with_description() {
        let common = sample_common_input();
        let input = TaskCompletedInput::new(common, "task-001", "Implement feature")
            .with_description("Add login and signup endpoints");

        assert_eq!(
            input.task_description,
            Some("Add login and signup endpoints".to_string())
        );
    }

    #[test]
    fn test_task_completed_input_serialization() {
        let input = TaskCompletedInput::new(sample_common_input(), "task-001", "Test task")
            .with_description("Task description");

        let json = serde_json::to_string(&input).unwrap();
        let parsed: TaskCompletedInput = serde_json::from_str(&json).unwrap();

        assert_eq!(input, parsed);
    }

    #[test]
    fn test_task_completed_input_flattened() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir",
            "task_id": "task-123",
            "task_subject": "Implement authentication",
            "task_description": "Add JWT auth",
            "teammate_name": "implementer",
            "team_name": "backend"
        }"#;

        let input: TaskCompletedInput = serde_json::from_str(json).unwrap();

        assert_eq!(input.common.session_id, "test-session");
        assert_eq!(input.task_id, "task-123");
        assert_eq!(input.task_subject, "Implement authentication");
        assert_eq!(input.task_description, Some("Add JWT auth".to_string()));
        assert_eq!(input.teammate_name, Some("implementer".to_string()));
        assert_eq!(input.team_name, Some("backend".to_string()));
    }

    #[test]
    fn test_task_completed_input_minimal() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir",
            "task_id": "task-456",
            "task_subject": "Fix bug"
        }"#;

        let input: TaskCompletedInput = serde_json::from_str(json).unwrap();

        assert_eq!(input.task_id, "task-456");
        assert_eq!(input.task_subject, "Fix bug");
        assert!(input.task_description.is_none());
        assert!(input.teammate_name.is_none());
        assert!(input.team_name.is_none());
    }

    #[test]
    fn test_task_completed_input_missing_required() {
        // Missing task_subject
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir",
            "task_id": "task-456"
        }"#;

        let result: Result<TaskCompletedInput, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // UserPromptSubmitInput tests

    #[test]
    fn test_user_prompt_submit_input_new() {
        let common = sample_common_input();
        let input = UserPromptSubmitInput::new(common.clone(), "Help me fix authentication");

        assert_eq!(input.common, common);
        assert_eq!(input.prompt, "Help me fix authentication");
    }

    #[test]
    fn test_user_prompt_submit_input_serialization() {
        let input =
            UserPromptSubmitInput::new(sample_common_input(), "Help me fix the database query");

        let json = serde_json::to_string(&input).unwrap();
        let parsed: UserPromptSubmitInput = serde_json::from_str(&json).unwrap();

        assert_eq!(input, parsed);
    }

    #[test]
    fn test_user_prompt_submit_input_flattened() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir",
            "prompt": "Implement OAuth login"
        }"#;

        let input: UserPromptSubmitInput = serde_json::from_str(json).unwrap();

        assert_eq!(input.common.session_id, "test-session");
        assert_eq!(input.prompt, "Implement OAuth login");
    }

    #[test]
    fn test_user_prompt_submit_input_missing_prompt() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir"
        }"#;

        let result: Result<UserPromptSubmitInput, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_user_prompt_submit_input_empty_prompt() {
        let json = r#"{
            "session_id": "test-session",
            "transcript_path": "/path/to/transcript.jsonl",
            "cwd": "/working/dir",
            "prompt": ""
        }"#;

        let input: UserPromptSubmitInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.prompt, "");
    }
}
