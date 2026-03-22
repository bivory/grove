//! Shared LLM call infrastructure for CLI and API backends.
//!
//! Provides generic functions that return raw response text.
//! Callers are responsible for parsing the response as needed.

pub mod batch;

/// Truncate a string to at most `max_bytes` bytes without splitting UTF-8.
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Call the `claude` CLI with a system prompt and user prompt.
///
/// Returns the raw response text, or `None` on any error (fail-open).
pub fn call_llm_cli(model: &str, system_prompt: &str, user_prompt: &str) -> Option<String> {
    let prompt = format!("{}\n\n{}", system_prompt, user_prompt);

    let output = match std::process::Command::new("claude")
        .args(["-p", &prompt, "--model", model, "--output-format", "json"])
        .env_remove("CLAUDECODE")
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            eprintln!("Warning: failed to invoke claude CLI: {}", e);
            return None;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if stderr.trim().is_empty() {
            &stdout
        } else {
            &stderr
        };
        eprintln!(
            "Warning: claude CLI exited with status {}: {}",
            output.status,
            truncate_str(detail, 200)
        );
        return None;
    }

    let response_text = if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
        json.get("result")
            .and_then(|r| r.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| stdout.trim().to_string())
    } else {
        stdout.trim().to_string()
    };

    Some(response_text)
}

/// Call the Anthropic Messages API via curl with a system prompt and user prompt.
///
/// The system prompt uses `cache_control` for prompt caching.
/// Returns the raw response text, or `None` on any error (fail-open).
pub fn call_llm_api(
    model: &str,
    api_url: &str,
    system_prompt: &str,
    user_prompt: &str,
    max_tokens: u32,
) -> Option<String> {
    let api_key = match std::env::var("ANTHROPIC_API_KEY") {
        Ok(key) if !key.is_empty() => key,
        _ => {
            eprintln!("Warning: ANTHROPIC_API_KEY not set, skipping LLM API call");
            return None;
        }
    };

    let request_body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": [{
            "type": "text",
            "text": system_prompt,
            "cache_control": { "type": "ephemeral" }
        }],
        "messages": [{
            "role": "user",
            "content": user_prompt
        }]
    });
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
            truncate_str(&stderr, 200)
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

    let stop_reason = json
        .get("stop_reason")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    if stop_reason == "max_tokens" {
        eprintln!("Warning: API response truncated (hit max_tokens limit)");
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

    Some(response_text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_str_short_string() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_exact_length() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_cuts_at_boundary() {
        assert_eq!(truncate_str("hello world", 5), "hello");
    }

    #[test]
    fn truncate_str_handles_multibyte() {
        // "cafe\u0301" is 'e' + combining accent = 6 bytes
        let s = "caf\u{00e9}!"; // 'cafe\u0301!' = "caf" (3) + e-acute (2) + "!" (1) = 6
                                // Truncating at 4 should cut inside the 2-byte char, back up to 3
        assert_eq!(truncate_str(s, 4), "caf");
        // Truncating at 5 includes the full 2-byte char
        assert_eq!(truncate_str(s, 5), "caf\u{00e9}");
    }
}
