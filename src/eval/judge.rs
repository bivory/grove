//! LLM judge: cache management, prompt building, CLI/API backends.
//!
//! Scores (session, learning) pairs for retrieval relevance using an LLM
//! as judge, with disk-based caching to avoid redundant API calls.

use super::corpus::SessionContext;
use crate::core::learning::CompoundLearning;
use crate::llm;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// LLM judge configuration and cached prompts.
pub struct JudgeContext {
    /// Backend: "cli" or "api".
    pub backend: String,
    /// Model ID.
    pub model: String,
    /// API base URL (only used by "api" backend).
    pub api_url: String,
    /// Pre-built system prompt (same for all calls).
    pub system_prompt: String,
}

/// Result of a single judge call.
pub struct JudgeResult {
    /// Relevance score (1.0-5.0).
    pub score: f64,
    /// Whether the result came from cache.
    pub cached: bool,
}

impl JudgeContext {
    /// Create a new JudgeContext from config.
    pub fn from_config(config: &crate::config::JudgeConfig) -> Self {
        Self {
            backend: config.backend.clone(),
            model: config.model.clone(),
            api_url: config.api_url.clone(),
            system_prompt: build_judge_system_prompt(),
        }
    }
}

/// Load the judge cache from disk. Returns an empty map if the file doesn't
/// exist or can't be parsed.
pub fn load_judge_cache(path: &Path) -> BTreeMap<String, f64> {
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            eprintln!(
                "Warning: failed to parse judge cache at {}: {}",
                path.display(),
                e
            );
            BTreeMap::new()
        }),
        Err(_) => BTreeMap::new(),
    }
}

/// Save the judge cache to disk. Creates parent directories if needed.
pub fn save_judge_cache(cache: &BTreeMap<String, f64>, path: &Path) {
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!(
                "Warning: failed to create cache directory {}: {}",
                parent.display(),
                e
            );
            return;
        }
    }
    match serde_json::to_string_pretty(cache) {
        Ok(json) => {
            if let Err(e) = fs::write(path, json) {
                eprintln!(
                    "Warning: failed to write judge cache to {}: {}",
                    path.display(),
                    e
                );
            }
        }
        Err(e) => {
            eprintln!("Warning: failed to serialize judge cache: {}", e);
        }
    }
}

/// Build the composite cache key for a (session, learning) pair.
pub fn judge_cache_key(session_file: &str, learning_id: &str) -> String {
    let session_id = session_file.strip_suffix(".jsonl").unwrap_or(session_file);
    format!("{}:{}", session_id, learning_id)
}

/// Score a (session, learning) pair using the LLM judge, with caching.
///
/// Checks the disk cache first. On cache miss, dispatches to the configured
/// backend, caches the result, and returns it.
pub fn judge_relevance(
    session_file: &str,
    learning: &CompoundLearning,
    ctx: &SessionContext,
    cache: &mut BTreeMap<String, f64>,
    judge: &JudgeContext,
) -> Option<JudgeResult> {
    let key = judge_cache_key(session_file, &learning.id);

    // Cache hit
    if let Some(&score) = cache.get(&key) {
        return Some(JudgeResult {
            score,
            cached: true,
        });
    }

    // Cache miss -- call the configured backend
    let learning_block = build_judge_learning_block(learning);
    let session_block = build_judge_session_block(ctx);
    let score = match judge.backend.as_str() {
        "cli" => call_llm_judge_cli(
            &judge.model,
            &judge.system_prompt,
            &learning_block,
            &session_block,
        ),
        "api" => call_llm_judge_api(
            &judge.model,
            &judge.api_url,
            &judge.system_prompt,
            &learning_block,
            &session_block,
        ),
        other => {
            eprintln!(
                "Warning: unknown judge backend '{}', skipping (fail-open)",
                other
            );
            return None;
        }
    }?;

    // Cache the result
    cache.insert(key, score);

    Some(JudgeResult {
        score,
        cached: false,
    })
}

/// Resolve judge cache path from CLI flag, config, or env var.
pub fn resolve_cache_path(cli_path: Option<&str>, config_path: &str) -> std::path::PathBuf {
    if let Some(p) = cli_path {
        return std::path::PathBuf::from(p);
    }
    if !config_path.is_empty() {
        return std::path::PathBuf::from(config_path);
    }
    if let Ok(p) = std::env::var("GROVE_BENCH_JUDGE_CACHE_PATH") {
        return std::path::PathBuf::from(p);
    }
    // Default: ~/.grove/judge_cache.json
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(format!("{}/.grove/judge_cache.json", home))
}

// ---- Internal functions ----

/// Build the system prompt for the LLM judge.
fn build_judge_system_prompt() -> String {
    r#"You are a relevance judge for a developer learning system. You evaluate whether a stored learning would be useful to an AI agent working on a specific coding session.

SCORING CRITERIA:
  1 = Completely irrelevant. The learning has nothing to do with this session's work.
  2 = Tangentially related. Shares a technology or broad domain but not the specific task.
  3 = Somewhat relevant. Touches on a related subsystem or concept the session works with.
  4 = Relevant. Directly addresses a technology, pattern, or component the session uses.
  5 = Highly relevant. The learning is precisely about what this session is doing and would meaningfully help.

Respond with ONLY a single integer from 1 to 5. No explanation, no other text."#
        .to_string()
}

/// Build the learning block for the LLM judge user message.
fn build_judge_learning_block(learning: &CompoundLearning) -> String {
    format!(
        "LEARNING:\n  ID: {}\n  Category: {:?}\n  Summary: {}\n  Detail: {}\n  Tags: [{}]",
        learning.id,
        learning.category,
        learning.summary,
        llm::truncate_str(&learning.detail, 500),
        learning.tags.join(", "),
    )
}

/// Build the session context block for the LLM judge user message.
fn build_judge_session_block(ctx: &SessionContext) -> String {
    let file_paths: Vec<&str> = ctx.file_paths.iter().take(30).map(|s| s.as_str()).collect();
    let grep_patterns: Vec<&str> = ctx
        .grep_patterns
        .iter()
        .take(15)
        .map(|s| s.as_str())
        .collect();
    let bash_commands: Vec<&str> = ctx
        .bash_commands
        .iter()
        .take(15)
        .map(|s| s.as_str())
        .collect();

    format!(
        "SESSION ACTIVITY:\n  File paths touched (up to 30):\n    {}\n  Grep patterns used (up to 15):\n    {}\n  Bash commands run (up to 15):\n    {}\n  Total tool calls in session: {}",
        if file_paths.is_empty() { "(none)".to_string() } else { file_paths.join("\n    ") },
        if grep_patterns.is_empty() { "(none)".to_string() } else { grep_patterns.join("\n    ") },
        if bash_commands.is_empty() { "(none)".to_string() } else { bash_commands.join("\n    ") },
        ctx.all_tool_calls.len(),
    )
}

/// Query the LLM judge via the `claude` CLI.
///
/// Combines the learning and session blocks into a user prompt, calls the
/// shared CLI function, and parses the score from the response.
fn call_llm_judge_cli(
    model: &str,
    system_prompt: &str,
    learning_block: &str,
    session_block: &str,
) -> Option<f64> {
    let user_prompt = format!("{}\n\n{}", learning_block, session_block);
    let response_text = llm::call_llm_cli(model, system_prompt, &user_prompt)?;
    parse_judge_score(&response_text)
}

/// Query the LLM judge via the Anthropic Messages API.
///
/// Builds a multi-block user message with cache_control on the learning block,
/// calls the shared API function with the combined prompt, and parses the score.
fn call_llm_judge_api(
    model: &str,
    api_url: &str,
    system_prompt: &str,
    learning_block: &str,
    session_block: &str,
) -> Option<f64> {
    // The judge needs a specific API request format with multi-block user messages
    // and cache_control on the learning block, so we call the API directly.
    let api_key = match std::env::var("ANTHROPIC_API_KEY") {
        Ok(key) if !key.is_empty() => key,
        _ => {
            eprintln!("Warning: ANTHROPIC_API_KEY not set, skipping LLM judge call");
            return None;
        }
    };

    let request_body = build_api_request(model, system_prompt, learning_block, session_block);
    let body_str = serde_json::to_string(&request_body).ok()?;

    let output = match std::process::Command::new("curl")
        .args([
            "-s",
            "-X",
            "POST",
            api_url,
            "-H",
            &format!("x-api-key: {}", api_key),
            "-H",
            "content-type: application/json",
            "-H",
            "anthropic-version: 2023-06-01",
            "-d",
            &body_str,
        ])
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            eprintln!("Warning: failed to invoke curl: {}", e);
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "Warning: curl exited with status {}: {}",
            output.status,
            llm::truncate_str(&stderr, 200)
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    let json: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Warning: failed to parse API response: {}", e);
            return None;
        }
    };

    if let Some(err) = json.get("error") {
        let err_msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .or_else(|| err.as_str())
            .unwrap_or("unknown");
        let err_type = err
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown");
        eprintln!("Warning: API error {}: {}", err_type, err_msg);
        return None;
    }

    let response_text = json
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|block| block.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");

    if let Some(usage) = json.get("usage") {
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_create = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if cache_read > 0 || cache_create > 0 {
            eprintln!("      cache: read={} create={}", cache_read, cache_create);
        }
    }

    parse_judge_score(response_text)
}

/// Build the JSON request body for the Anthropic Messages API with prompt caching.
fn build_api_request(
    model: &str,
    system_prompt: &str,
    learning_block: &str,
    session_block: &str,
) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": 16,
        "system": [{
            "type": "text",
            "text": system_prompt,
            "cache_control": { "type": "ephemeral" }
        }],
        "messages": [{
            "role": "user",
            "content": [
                {
                    "type": "text",
                    "text": learning_block,
                    "cache_control": { "type": "ephemeral" }
                },
                {
                    "type": "text",
                    "text": session_block
                }
            ]
        }]
    })
}

// Re-export shared parse_judge_score from core::judge
pub use crate::core::judge::parse_judge_score;

use crate::llm::batch::{BatchRequest, BatchResult, BatchResultType};

/// Build a BatchRequest for a (session, learning) pair.
///
/// Returns `None` if the pair is already cached (cache hit → skip).
pub fn build_judge_batch_request(
    session_file: &str,
    learning: &CompoundLearning,
    ctx: &SessionContext,
    cache: &BTreeMap<String, f64>,
    judge: &JudgeContext,
) -> Option<BatchRequest> {
    let key = judge_cache_key(session_file, &learning.id);

    // Cache hit → skip
    if cache.contains_key(&key) {
        return None;
    }

    let learning_block = build_judge_learning_block(learning);
    let session_block = build_judge_session_block(ctx);
    let params = build_api_request(
        &judge.model,
        &judge.system_prompt,
        &learning_block,
        &session_block,
    );

    Some(BatchRequest {
        custom_id: crate::llm::batch::encode_custom_id(&key),
        params,
    })
}

/// Apply a batch result to the judge cache.
///
/// Parses the score from the response text and inserts into cache.
/// Returns the `JudgeResult` if successful, `None` on failure.
/// Failed results don't corrupt the cache.
pub fn apply_judge_batch_result(
    result: &BatchResult,
    cache: &mut BTreeMap<String, f64>,
) -> Option<JudgeResult> {
    match &result.result_type {
        BatchResultType::Succeeded(text) => {
            let score = parse_judge_score(text)?;
            let cache_key = crate::llm::batch::decode_custom_id(&result.custom_id);
            cache.insert(cache_key, score);
            Some(JudgeResult {
                score,
                cached: false,
            })
        }
        BatchResultType::Failed(reason) => {
            eprintln!(
                "Warning: batch judge request failed for {}: {}",
                result.custom_id, reason
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judge_cache_key_strips_jsonl() {
        assert_eq!(
            judge_cache_key("abc123.jsonl", "learn-001"),
            "abc123:learn-001"
        );
    }

    #[test]
    fn judge_cache_key_handles_no_extension() {
        assert_eq!(judge_cache_key("abc123", "learn-001"), "abc123:learn-001");
    }

    #[test]
    fn load_judge_cache_missing_file() {
        let cache = load_judge_cache(Path::new("/nonexistent/path/cache.json"));
        assert!(cache.is_empty());
    }

    #[test]
    fn resolve_cache_path_cli_flag() {
        let path = resolve_cache_path(Some("/tmp/cache.json"), "");
        assert_eq!(path, std::path::PathBuf::from("/tmp/cache.json"));
    }

    #[test]
    fn resolve_cache_path_config() {
        let path = resolve_cache_path(None, "/config/cache.json");
        assert_eq!(path, std::path::PathBuf::from("/config/cache.json"));
    }

    #[test]
    fn resolve_cache_path_cli_overrides_config() {
        let path = resolve_cache_path(Some("/cli/cache.json"), "/config/cache.json");
        assert_eq!(
            path,
            std::path::PathBuf::from("/cli/cache.json"),
            "CLI flag should take priority over config"
        );
    }

    #[test]
    fn resolve_cache_path_empty_config_falls_through() {
        // With empty config and no env var, should use default ~/.grove/judge_cache.json
        let path = resolve_cache_path(None, "");
        // Can't assert exact path (depends on HOME), but should end with judge_cache.json
        assert!(
            path.to_string_lossy().ends_with("judge_cache.json"),
            "Expected default judge_cache.json, got: {}",
            path.display()
        );
    }

    #[test]
    fn resolve_cache_path_sweep_per_corpus_derivation() {
        // Simulates the sweep pattern: CLI --cache-path as base, per-corpus name appended
        let base = resolve_cache_path(Some("/tmp/reliability/cache.json"), "");
        let parent = base.parent().unwrap();
        let corpus_cache = parent.join("judge_cache_grove.json");
        assert_eq!(
            corpus_cache,
            std::path::PathBuf::from("/tmp/reliability/judge_cache_grove.json"),
            "Per-corpus cache should be derived from CLI base path parent"
        );
    }

    // ---- Batch helper tests ----

    fn make_test_learning(id: &str) -> CompoundLearning {
        use crate::core::learning::*;
        let mut l = CompoundLearning::new(
            LearningCategory::Pattern,
            "Test summary for batch",
            "Detailed explanation for batch testing that is long enough.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["test".to_string()],
            "session-1",
        );
        l.id = id.to_string();
        l
    }

    fn make_test_session_context() -> SessionContext {
        SessionContext {
            session_file: "test-session.jsonl".to_string(),
            file_paths: vec!["src/main.rs".to_string()],
            grep_patterns: vec!["error".to_string()],
            bash_commands: vec!["cargo test".to_string()],
            all_tool_calls: Vec::new(),
        }
    }

    #[test]
    fn build_judge_batch_request_skips_cached() {
        let learning = make_test_learning("cl_001");
        let ctx = make_test_session_context();
        let judge = JudgeContext {
            backend: "api".to_string(),
            model: "test-model".to_string(),
            api_url: "https://api.anthropic.com/v1/messages".to_string(),
            system_prompt: "test prompt".to_string(),
        };
        let mut cache = BTreeMap::new();
        // Pre-populate cache for this pair
        cache.insert("test-session:cl_001".to_string(), 4.0);

        let result =
            build_judge_batch_request("test-session.jsonl", &learning, &ctx, &cache, &judge);
        assert!(result.is_none(), "should skip cached pair");
    }

    #[test]
    fn build_judge_batch_request_returns_for_uncached() {
        let learning = make_test_learning("cl_002");
        let ctx = make_test_session_context();
        let judge = JudgeContext {
            backend: "api".to_string(),
            model: "test-model".to_string(),
            api_url: "https://api.anthropic.com/v1/messages".to_string(),
            system_prompt: "test prompt".to_string(),
        };
        let cache = BTreeMap::new();

        let result =
            build_judge_batch_request("test-session.jsonl", &learning, &ctx, &cache, &judge);
        assert!(result.is_some(), "should return request for uncached pair");

        let req = result.unwrap();
        assert_eq!(req.custom_id, "test-session--cl_002");
        assert!(req.params["model"].as_str().unwrap() == "test-model");
    }

    #[test]
    fn apply_judge_batch_result_succeeded() {
        let result = BatchResult {
            custom_id: "session1--learn1".to_string(),
            result_type: BatchResultType::Succeeded("4".to_string()),
        };
        let mut cache = BTreeMap::new();

        let judge_result = apply_judge_batch_result(&result, &mut cache);
        assert!(judge_result.is_some());
        let jr = judge_result.unwrap();
        assert_eq!(jr.score, 4.0);
        assert!(!jr.cached);
        assert_eq!(cache.get("session1:learn1"), Some(&4.0));
    }

    #[test]
    fn apply_judge_batch_result_failed_doesnt_corrupt_cache() {
        let result = BatchResult {
            custom_id: "session1--learn1".to_string(),
            result_type: BatchResultType::Failed("server error".to_string()),
        };
        let mut cache = BTreeMap::new();
        cache.insert("other:key".to_string(), 3.0);

        let judge_result = apply_judge_batch_result(&result, &mut cache);
        assert!(judge_result.is_none());
        // Cache should not be modified
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get("other:key"), Some(&3.0));
    }

    #[test]
    fn apply_judge_batch_result_unparsable_score() {
        let result = BatchResult {
            custom_id: "session1--learn1".to_string(),
            result_type: BatchResultType::Succeeded("not a number".to_string()),
        };
        let mut cache = BTreeMap::new();

        let judge_result = apply_judge_batch_result(&result, &mut cache);
        assert!(judge_result.is_none());
        assert!(cache.is_empty(), "unparsable score should not enter cache");
    }
}
