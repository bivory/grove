//! Hook integration for Claude Code.
//!
//! This module provides types and handlers for integrating with Claude Code
//! hooks. Hooks are invoked at key points in the session lifecycle:
//!
//! - **session-start**: Session initialization, context injection
//! - **pre-tool-use**: Tool invocation interception
//! - **post-tool-use**: Tool result processing
//! - **stop**: Session exit gate
//! - **session-end**: Session cleanup

pub mod input;
pub mod output;
pub mod runner;

pub use input::{
    parse_input, HookInput, PostToolUseInput, PreToolUseInput, SessionEndInput, SessionEndReason,
    SessionStartInput, StopInput,
};
pub use output::{
    to_json, to_json_pretty, PostToolUseOutput, PreToolUseOutput, SessionEndOutput,
    SessionStartOutput, StopDecision, StopOutput,
};
pub use runner::{HookRunner, HookType};
