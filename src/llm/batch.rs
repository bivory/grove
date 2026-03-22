//! Batch API module for Anthropic Message Batches API.
//!
//! Provides batch request submission, polling, result retrieval, and cancellation.
//! All functions use curl subprocess (consistent with existing `call_llm_api`).
//! All failures return `None` (fail-open).

use serde::{Deserialize, Serialize};

/// A single request to include in a batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequest {
    /// Unique identifier to match results back to callers.
    pub custom_id: String,
    /// Standard Messages API params as a serde_json::Value.
    pub params: serde_json::Value,
}

/// The result of a single request within a completed batch.
#[derive(Debug, Clone)]
pub struct BatchResult {
    pub custom_id: String,
    pub result_type: BatchResultType,
}

/// Result type for a single batch request.
#[derive(Debug, Clone)]
pub enum BatchResultType {
    /// Request succeeded; contains the text response.
    Succeeded(String),
    /// Request failed (errored, canceled, or expired).
    Failed(String),
}

/// Tracks the state of an in-flight batch.
#[derive(Debug, Clone)]
pub struct BatchState {
    pub batch_id: String,
    pub created_at: String,
    pub total_requests: usize,
}

/// Submit a batch of requests to the Anthropic Message Batches API.
///
/// Returns a `BatchState` for polling, or `None` on failure (fail-open).
pub fn create_batch(api_url: &str, requests: Vec<BatchRequest>) -> Option<BatchState> {
    let api_key = get_api_key()?;
    let total_requests = requests.len();

    // Build the request body: {"requests": [{"custom_id": ..., "params": ...}, ...]}
    let body = serde_json::json!({
        "requests": requests.iter().map(|r| {
            serde_json::json!({
                "custom_id": r.custom_id,
                "params": r.params,
            })
        }).collect::<Vec<_>>()
    });
    let body_str = match serde_json::to_string(&body) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Warning: failed to serialize batch request: {}", e);
            return None;
        }
    };

    // The batches endpoint is at the base URL + /batches
    // e.g. https://api.anthropic.com/v1/messages -> https://api.anthropic.com/v1/messages/batches
    let batches_url = format!("{}/batches", api_url.trim_end_matches('/'));

    let output = match std::process::Command::new("curl")
        .args([
            "-s",
            "-X",
            "POST",
            &batches_url,
            "-H",
            &format!("x-api-key: {}", api_key),
            "-H",
            "content-type: application/json",
            "-H",
            "anthropic-version: 2023-06-01",
            "-H",
            "anthropic-beta: message-batches-2024-09-24",
            "-d",
            &body_str,
        ])
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            eprintln!("Warning: failed to invoke curl for batch creation: {}", e);
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "Warning: curl exited with status {} during batch creation: {}",
            output.status,
            super::truncate_str(&stderr, 200)
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Warning: failed to parse batch creation response: {}", e);
            return None;
        }
    };

    if let Some(err) = json.get("error") {
        let err_msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown");
        eprintln!("Warning: batch creation API error: {}", err_msg);
        return None;
    }

    let batch_id = json.get("id").and_then(|v| v.as_str())?.to_string();
    let created_at = json
        .get("created_at")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Some(BatchState {
        batch_id,
        created_at,
        total_requests,
    })
}

/// Poll a batch until it reaches "ended" status or timeout.
///
/// Uses exponential backoff: 10s, 20s, 40s, 60s, 60s, ...
/// Calls `progress_callback` with (status, processing, succeeded, errored, expired) on each poll.
/// Returns `true` if ended, `false` if timed out, `None` on error.
pub fn poll_batch_until_ended(
    api_url: &str,
    batch_id: &str,
    timeout_seconds: u64,
    progress_callback: &dyn Fn(&str, usize, usize, usize, usize),
) -> Option<bool> {
    let api_key = get_api_key()?;
    let batch_url = format!("{}/batches/{}", api_url.trim_end_matches('/'), batch_id);

    let start = std::time::Instant::now();
    let mut backoff_secs: u64 = 10;

    loop {
        let output = match std::process::Command::new("curl")
            .args([
                "-s",
                &batch_url,
                "-H",
                &format!("x-api-key: {}", api_key),
                "-H",
                "anthropic-version: 2023-06-01",
                "-H",
                "anthropic-beta: message-batches-2024-09-24",
            ])
            .output()
        {
            Ok(output) => output,
            Err(e) => {
                eprintln!("Warning: failed to invoke curl for batch polling: {}", e);
                return None;
            }
        };

        if !output.status.success() {
            eprintln!(
                "Warning: curl exited with status {} during batch poll",
                output.status
            );
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = match serde_json::from_str(&stdout) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Warning: failed to parse batch poll response: {}", e);
                return None;
            }
        };

        let status = json
            .get("processing_status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Extract request_counts
        let counts = json.get("request_counts");
        let processing = counts
            .and_then(|c| c.get("processing"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let succeeded = counts
            .and_then(|c| c.get("succeeded"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let errored = counts
            .and_then(|c| c.get("errored"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let expired = counts
            .and_then(|c| c.get("expired"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        progress_callback(status, processing, succeeded, errored, expired);

        if status == "ended" {
            return Some(true);
        }

        // Check timeout
        if start.elapsed().as_secs() >= timeout_seconds {
            eprintln!(
                "Warning: batch polling timed out after {}s",
                timeout_seconds
            );
            return Some(false);
        }

        // Sleep with exponential backoff
        std::thread::sleep(std::time::Duration::from_secs(backoff_secs));
        backoff_secs = (backoff_secs * 2).min(60);
    }
}

/// Retrieve results for a completed batch as a `Vec<BatchResult>`.
///
/// Streams the JSONL response and parses each line.
pub fn retrieve_batch_results(api_url: &str, batch_id: &str) -> Option<Vec<BatchResult>> {
    let api_key = get_api_key()?;
    let results_url = format!(
        "{}/batches/{}/results",
        api_url.trim_end_matches('/'),
        batch_id
    );

    let output = match std::process::Command::new("curl")
        .args([
            "-s",
            &results_url,
            "-H",
            &format!("x-api-key: {}", api_key),
            "-H",
            "anthropic-version: 2023-06-01",
            "-H",
            "anthropic-beta: message-batches-2024-09-24",
        ])
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            eprintln!(
                "Warning: failed to invoke curl for batch results retrieval: {}",
                e
            );
            return None;
        }
    };

    if !output.status.success() {
        eprintln!(
            "Warning: curl exited with status {} during batch results retrieval",
            output.status
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(parse_batch_results_jsonl(&stdout))
}

/// Cancel a batch (best-effort, used for Ctrl+C handling).
pub fn cancel_batch(api_url: &str, batch_id: &str) {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => return,
    };
    let cancel_url = format!(
        "{}/batches/{}/cancel",
        api_url.trim_end_matches('/'),
        batch_id
    );

    let result = std::process::Command::new("curl")
        .args([
            "-s",
            "-X",
            "POST",
            &cancel_url,
            "-H",
            &format!("x-api-key: {}", api_key),
            "-H",
            "anthropic-version: 2023-06-01",
            "-H",
            "anthropic-beta: message-batches-2024-09-24",
        ])
        .output();

    if let Err(e) = result {
        eprintln!("Warning: failed to cancel batch: {}", e);
    }
}

// ---- Internal helpers ----

fn get_api_key() -> Option<String> {
    match std::env::var("ANTHROPIC_API_KEY") {
        Ok(key) if !key.is_empty() => Some(key),
        _ => {
            eprintln!("Warning: ANTHROPIC_API_KEY not set, skipping batch API call");
            None
        }
    }
}

/// Parse JSONL batch results into Vec<BatchResult>.
///
/// Each line is a JSON object with:
/// - `custom_id`: string
/// - `result.type`: "succeeded" | "errored" | "expired" | "canceled"
/// - For succeeded: `result.message.content[0].text`
/// - For failed: `result.error.message` or type description
pub fn parse_batch_results_jsonl(jsonl: &str) -> Vec<BatchResult> {
    let mut results = Vec::new();

    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Warning: failed to parse batch result line: {}", e);
                continue;
            }
        };

        let custom_id = match json.get("custom_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };

        let result = json.get("result");
        let result_type = result
            .and_then(|r| r.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let batch_result_type = if result_type == "succeeded" {
            // Extract text from result.message.content[0].text
            let text = result
                .and_then(|r| r.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|block| block.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            BatchResultType::Succeeded(text)
        } else {
            // errored, expired, canceled
            let error_msg = result
                .and_then(|r| r.get("error"))
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or(result_type)
                .to_string();
            BatchResultType::Failed(error_msg)
        };

        results.push(BatchResult {
            custom_id,
            result_type: batch_result_type,
        });
    }

    results
}

/// Encode a string as a valid batch API custom_id.
///
/// The Anthropic API requires custom_id to match `^[a-zA-Z0-9_-]{1,64}$`.
/// This function replaces `:` (used in cache keys) with `--` (valid in custom_id).
/// Safe because neither UUIDs nor learning IDs contain consecutive dashes.
pub fn encode_custom_id(key: &str) -> String {
    key.replace(':', "--")
}

/// Decode a batch API custom_id back to the original key format.
///
/// Reverses the encoding done by `encode_custom_id` (replaces first `--` with `:`).
pub fn decode_custom_id(custom_id: &str) -> String {
    custom_id.replacen("--", ":", 1)
}

/// Compute the next backoff duration given the current one.
///
/// Doubles the interval up to a max of 60 seconds.
/// Starts at 10s: 10 → 20 → 40 → 60 → 60 → ...
pub fn next_backoff(current_secs: u64) -> u64 {
    (current_secs * 2).min(60)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- JSON construction tests ----

    #[test]
    fn batch_request_serializes_correctly() {
        let req = BatchRequest {
            custom_id: "retroflect--abc-123".to_string(),
            params: serde_json::json!({
                "model": "claude-sonnet-4-20250514",
                "max_tokens": 1024,
                "messages": [{"role": "user", "content": "hello"}]
            }),
        };

        let json_str = serde_json::to_string(&req).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(parsed["custom_id"], "retroflect--abc-123");
        assert_eq!(parsed["params"]["model"], "claude-sonnet-4-20250514");
        assert_eq!(parsed["params"]["max_tokens"], 1024);
    }

    #[test]
    fn batch_request_body_format() {
        let requests = [
            BatchRequest {
                custom_id: "req-1".to_string(),
                params: serde_json::json!({"model": "test", "messages": []}),
            },
            BatchRequest {
                custom_id: "req-2".to_string(),
                params: serde_json::json!({"model": "test", "messages": []}),
            },
        ];

        let body = serde_json::json!({
            "requests": requests.iter().map(|r| {
                serde_json::json!({
                    "custom_id": r.custom_id,
                    "params": r.params,
                })
            }).collect::<Vec<_>>()
        });

        let arr = body["requests"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["custom_id"], "req-1");
        assert_eq!(arr[1]["custom_id"], "req-2");
    }

    // ---- JSONL parsing tests ----

    #[test]
    fn parse_succeeded_result() {
        let jsonl = r#"{"custom_id":"abc--learn-001","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"4"}]}}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].custom_id, "abc--learn-001");
        match &results[0].result_type {
            BatchResultType::Succeeded(text) => assert_eq!(text, "4"),
            BatchResultType::Failed(_) => panic!("Expected Succeeded"),
        }
    }

    #[test]
    fn parse_errored_result() {
        let jsonl = r#"{"custom_id":"abc--learn-002","result":{"type":"errored","error":{"type":"server_error","message":"Internal server error"}}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].custom_id, "abc--learn-002");
        match &results[0].result_type {
            BatchResultType::Failed(msg) => assert_eq!(msg, "Internal server error"),
            BatchResultType::Succeeded(_) => panic!("Expected Failed"),
        }
    }

    #[test]
    fn parse_expired_result() {
        let jsonl = r#"{"custom_id":"abc--learn-003","result":{"type":"expired"}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 1);
        match &results[0].result_type {
            BatchResultType::Failed(msg) => assert_eq!(msg, "expired"),
            BatchResultType::Succeeded(_) => panic!("Expected Failed"),
        }
    }

    #[test]
    fn parse_canceled_result() {
        let jsonl = r#"{"custom_id":"abc--learn-004","result":{"type":"canceled"}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 1);
        match &results[0].result_type {
            BatchResultType::Failed(msg) => assert_eq!(msg, "canceled"),
            BatchResultType::Succeeded(_) => panic!("Expected Failed"),
        }
    }

    #[test]
    fn parse_multiple_results() {
        let jsonl = concat!(
            r#"{"custom_id":"r1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"3"}]}}}"#,
            "\n",
            r#"{"custom_id":"r2","result":{"type":"errored","error":{"type":"server_error","message":"fail"}}}"#,
            "\n",
            r#"{"custom_id":"r3","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"5"}]}}}"#,
        );
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].custom_id, "r1");
        assert_eq!(results[1].custom_id, "r2");
        assert_eq!(results[2].custom_id, "r3");
        assert!(matches!(
            results[0].result_type,
            BatchResultType::Succeeded(_)
        ));
        assert!(matches!(results[1].result_type, BatchResultType::Failed(_)));
        assert!(matches!(
            results[2].result_type,
            BatchResultType::Succeeded(_)
        ));
    }

    #[test]
    fn parse_empty_and_whitespace_lines() {
        let jsonl = "\n  \n";
        let results = parse_batch_results_jsonl(jsonl);
        assert!(results.is_empty());
    }

    #[test]
    fn parse_malformed_line_skipped() {
        let jsonl = "not json\n{\"custom_id\":\"r1\",\"result\":{\"type\":\"succeeded\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}}\n";
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].custom_id, "r1");
    }

    // ---- custom_id encoding/decoding tests ----

    #[test]
    fn encode_custom_id_replaces_colon() {
        assert_eq!(encode_custom_id("abc:learn-001"), "abc--learn-001");
        assert_eq!(
            encode_custom_id("retroflect:550e8400-e29b-41d4"),
            "retroflect--550e8400-e29b-41d4"
        );
    }

    #[test]
    fn decode_custom_id_restores_colon() {
        assert_eq!(decode_custom_id("abc--learn-001"), "abc:learn-001");
        assert_eq!(
            decode_custom_id("retroflect--550e8400-e29b-41d4"),
            "retroflect:550e8400-e29b-41d4"
        );
    }

    #[test]
    fn encode_decode_roundtrip() {
        let keys = [
            "session-abc:learn-001",
            "retroflect:550e8400-e29b-41d4-a716-446655440000",
            "test-session:cl_007",
        ];
        for key in keys {
            assert_eq!(decode_custom_id(&encode_custom_id(key)), key);
        }
    }

    #[test]
    fn encoded_custom_id_matches_api_pattern() {
        fn matches_api_pattern(s: &str) -> bool {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        }

        let test_cases = [
            "session-abc:learn-001",
            "retroflect:550e8400-e29b-41d4-a716-446655440000",
            "test-session:cl_007",
        ];
        for key in test_cases {
            let encoded = encode_custom_id(key);
            assert!(
                matches_api_pattern(&encoded),
                "encoded custom_id '{}' does not match API pattern",
                encoded
            );
        }
    }

    #[test]
    fn custom_id_roundtrip_retroflect() {
        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let cache_key = format!("retroflect:{}", session_id);
        let custom_id = encode_custom_id(&cache_key);

        let jsonl = format!(
            r#"{{"custom_id":"{}","result":{{"type":"succeeded","message":{{"content":[{{"type":"text","text":"test"}}]}}}}}}"#,
            custom_id
        );
        let results = parse_batch_results_jsonl(&jsonl);
        assert_eq!(results.len(), 1);

        // Decode back to original key and extract session_id
        let decoded = decode_custom_id(&results[0].custom_id);
        let parsed_id = decoded.strip_prefix("retroflect:").unwrap();
        assert_eq!(parsed_id, session_id);
    }

    #[test]
    fn custom_id_roundtrip_judge() {
        let session_file = "abc123";
        let learning_id = "cl_007";
        let cache_key = format!("{}:{}", session_file, learning_id);
        let custom_id = encode_custom_id(&cache_key);

        let jsonl = format!(
            r#"{{"custom_id":"{}","result":{{"type":"succeeded","message":{{"content":[{{"type":"text","text":"4"}}]}}}}}}"#,
            custom_id
        );
        let results = parse_batch_results_jsonl(&jsonl);
        assert_eq!(results.len(), 1);

        // Decode back and split into components
        let decoded = decode_custom_id(&results[0].custom_id);
        let parts: Vec<&str> = decoded.splitn(2, ':').collect();
        assert_eq!(parts[0], session_file);
        assert_eq!(parts[1], learning_id);
    }

    // ---- Backoff logic tests ----

    #[test]
    fn backoff_sequence() {
        assert_eq!(next_backoff(10), 20);
        assert_eq!(next_backoff(20), 40);
        assert_eq!(next_backoff(40), 60);
        assert_eq!(next_backoff(60), 60); // capped at 60
        assert_eq!(next_backoff(100), 60); // still capped
    }

    #[test]
    fn backoff_starts_at_10() {
        let initial = 10u64;
        let mut current = initial;
        let expected = vec![20, 40, 60, 60, 60];
        for expected_val in expected {
            current = next_backoff(current);
            assert_eq!(current, expected_val);
        }
    }

    // ---- Succeeded text extraction ----

    #[test]
    fn succeeded_extracts_multiline_text() {
        let jsonl = r#"{"custom_id":"test","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"[\n  {\"category\": \"pattern\"}\n]"}]}}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 1);
        match &results[0].result_type {
            BatchResultType::Succeeded(text) => {
                assert!(text.contains("pattern"));
            }
            BatchResultType::Failed(_) => panic!("Expected Succeeded"),
        }
    }

    #[test]
    fn succeeded_with_empty_content() {
        let jsonl =
            r#"{"custom_id":"test","result":{"type":"succeeded","message":{"content":[]}}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 1);
        match &results[0].result_type {
            BatchResultType::Succeeded(text) => assert_eq!(text, ""),
            BatchResultType::Failed(_) => panic!("Expected Succeeded"),
        }
    }

    // ---- Partial failure handling ----

    #[test]
    fn partial_failure_preserves_successes() {
        // Simulate a batch where some requests succeeded and others failed
        let jsonl = concat!(
            r#"{"custom_id":"retroflect--session-1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"[{\"category\":\"pattern\",\"summary\":\"test\"}]"}]}}}"#,
            "\n",
            r#"{"custom_id":"retroflect--session-2","result":{"type":"errored","error":{"type":"server_error","message":"overloaded"}}}"#,
            "\n",
            r#"{"custom_id":"retroflect--session-3","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"[{\"category\":\"pitfall\",\"summary\":\"avoid\"}]"}]}}}"#,
            "\n",
            r#"{"custom_id":"retroflect--session-4","result":{"type":"expired"}}"#,
        );

        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 4);

        let succeeded: Vec<_> = results
            .iter()
            .filter(|r| matches!(r.result_type, BatchResultType::Succeeded(_)))
            .collect();
        let failed: Vec<_> = results
            .iter()
            .filter(|r| matches!(r.result_type, BatchResultType::Failed(_)))
            .collect();

        assert_eq!(succeeded.len(), 2, "two requests should succeed");
        assert_eq!(failed.len(), 2, "two requests should fail");

        // Verify the succeeded ones have correct content
        assert_eq!(succeeded[0].custom_id, "retroflect--session-1");
        assert_eq!(succeeded[1].custom_id, "retroflect--session-3");

        // Verify failed ones have error info
        match &failed[0].result_type {
            BatchResultType::Failed(msg) => assert_eq!(msg, "overloaded"),
            _ => panic!("Expected Failed"),
        }
        match &failed[1].result_type {
            BatchResultType::Failed(msg) => assert_eq!(msg, "expired"),
            _ => panic!("Expected Failed"),
        }
    }

    #[test]
    fn batch_result_ordering_independent_of_input_order() {
        // Results may arrive in different order than requests were submitted
        let jsonl = concat!(
            r#"{"custom_id":"retroflect--session-3","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"c"}]}}}"#,
            "\n",
            r#"{"custom_id":"retroflect--session-1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"a"}]}}}"#,
            "\n",
            r#"{"custom_id":"retroflect--session-2","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"b"}]}}}"#,
        );

        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 3);

        // custom_id allows re-ordering by caller
        let mut sorted: Vec<_> = results.iter().collect();
        sorted.sort_by_key(|r| &r.custom_id);

        assert_eq!(sorted[0].custom_id, "retroflect--session-1");
        assert_eq!(sorted[1].custom_id, "retroflect--session-2");
        assert_eq!(sorted[2].custom_id, "retroflect--session-3");
    }

    #[test]
    fn large_batch_result_parsing() {
        // Simulate a larger batch with 100 results
        let mut lines = Vec::new();
        for i in 0..100 {
            lines.push(format!(
                r#"{{"custom_id":"retroflect--session-{:03}","result":{{"type":"succeeded","message":{{"content":[{{"type":"text","text":"{}"}}]}}}}}}"#,
                i, i
            ));
        }
        let jsonl = lines.join("\n");

        let results = parse_batch_results_jsonl(&jsonl);
        assert_eq!(results.len(), 100);

        // Spot-check first and last
        assert_eq!(results[0].custom_id, "retroflect--session-000");
        assert_eq!(results[99].custom_id, "retroflect--session-099");
    }
}
