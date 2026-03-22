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
    to_json, to_json_pretty, PostToolUseOutput, PreToolUseHookOutput, PreToolUseOutput,
    SessionEndOutput, SessionStartHookOutput, SessionStartOutput, StopDecision, StopOutput,
};
pub use runner::{
    adaptive_dk_ratio, apply_adaptive_threshold, apply_dynamic_k, extract_tool_input_keywords,
    extract_tool_input_keywords_v2, extract_tool_input_keywords_v2_with_options,
    extract_user_intent_keywords, learning_matches_intent, HookRunner, HookType,
};

// Re-export benchmark-only functions within the crate
#[cfg(feature = "tantivy-search")]
pub(crate) use runner::{
    build_tantivy_query_string_boosted, build_tantivy_query_string_boosted_with_params,
    enrich_query_with_corpus_vocabulary, extract_corpus_vocabulary, rerank_with_llm,
};
