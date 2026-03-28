//! LLM judge for borderline learnings at capture time.
//!
//! When a learning's composite specificity score falls in the borderline zone
//! (default 1.5–2.5), the LLM judge evaluates whether it contains genuinely
//! project-specific, actionable knowledge vs generic advice.
//!
//! The judge is opt-in (`judge_enabled = false` by default) and follows
//! fail-open philosophy: any error → accept the borderline learning.

use crate::config::JudgeConfig;
use crate::core::learning::CompoundLearning;

// Re-export for crate-internal callers that historically used judge::truncate_str
pub(crate) use crate::llm::truncate_str;

/// Build the system prompt for the specificity judge.
///
/// This is a different rubric from the retrieval relevance judge in
/// `replay_harness.rs` — this one evaluates whether the learning contains
/// project-specific, actionable knowledge vs generic advice.
pub fn build_specificity_judge_prompt() -> &'static str {
    r#"You are evaluating whether a captured learning is genuinely project-specific and actionable, or just generic advice that any developer would already know.

Score the learning from 1 to 5:
1 = Generic platitude. Could appear in any "best practices" blog post. Example: "Always write tests."
2 = Vaguely technical. Mentions a technology but lacks specific context. Example: "Use caching for better performance."
3 = Somewhat specific. References a particular tool/framework with some context, but still broadly applicable. Example: "Configure ESLint with the recommended rules."
4 = Clearly project-specific. Contains concrete details tied to a particular codebase, workflow, or environment. Example: "Always validate Vector VRL transforms locally with vector validate before deploying to production."
5 = Highly specific. Includes file paths, function names, config keys, error messages, or other concrete identifiers unique to the project. Example: "The LiveView.mount/3 callback in router.ex must set all assigns used by render/1."

Respond with ONLY a single integer (1-5). No explanation."#
}

/// Format a learning into a prompt block for the judge.
fn format_learning_block(learning: &CompoundLearning) -> String {
    let mut block = format!(
        "Summary: {}\nDetail: {}\nCategory: {:?}\nTags: {}",
        learning.summary,
        learning.detail,
        learning.category,
        learning.tags.join(", "),
    );
    if let Some(ref files) = learning.context_files {
        if !files.is_empty() {
            block.push_str(&format!("\nFiles: {}", files.join(", ")));
        }
    }
    block
}

/// Call the LLM judge to assess a borderline learning's specificity.
///
/// Dispatches to CLI or API backend based on `config.backend`.
/// Returns `None` on any error (fail-open).
pub fn call_judge(config: &JudgeConfig, learning: &CompoundLearning) -> Option<f64> {
    let system_prompt = build_specificity_judge_prompt();
    let learning_block = format_learning_block(learning);

    match config.backend.as_str() {
        "cli" => call_judge_cli(&config.model, system_prompt, &learning_block),
        "api" => call_judge_api(
            &config.model,
            &config.api_url,
            system_prompt,
            &learning_block,
        ),
        other => {
            eprintln!(
                "Warning: unknown judge backend '{}', skipping (fail-open)",
                other
            );
            None
        }
    }
}

/// Call the LLM judge via the `claude` CLI.
///
/// Uses `timeout` to enforce a 10s limit. Returns `None` on failure.
fn call_judge_cli(model: &str, system_prompt: &str, learning_block: &str) -> Option<f64> {
    let prompt = format!("{}\n\n{}", system_prompt, learning_block);

    let output = match std::process::Command::new("timeout")
        .args([
            "10",
            "claude",
            "-p",
            &prompt,
            "--model",
            model,
            "--output-format",
            "json",
        ])
        .env_remove("CLAUDECODE")
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            eprintln!("Warning: failed to invoke claude CLI for judge: {}", e);
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
            "Warning: judge CLI exited with status {}: {}",
            output.status,
            truncate_str(detail, 200)
        );
        return None;
    }

    // --output-format json wraps the response in {"result": "..."}
    let response_text = if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
        json.get("result")
            .and_then(|r| r.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| stdout.trim().to_string())
    } else {
        stdout.trim().to_string()
    };

    parse_judge_score(&response_text)
}

/// Call the LLM judge via the Anthropic Messages API (curl + ANTHROPIC_API_KEY).
///
/// Returns `None` on failure.
fn call_judge_api(
    model: &str,
    api_url: &str,
    system_prompt: &str,
    learning_block: &str,
) -> Option<f64> {
    let api_key = match std::env::var("ANTHROPIC_API_KEY") {
        Ok(key) if !key.is_empty() => key,
        _ => {
            eprintln!("Warning: ANTHROPIC_API_KEY not set, skipping judge (fail-open)");
            return None;
        }
    };

    let body = serde_json::json!({
        "model": model,
        "max_tokens": 16,
        "system": system_prompt,
        "messages": [{
            "role": "user",
            "content": learning_block
        }]
    });

    let output = match std::process::Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "10",
            "-X",
            "POST",
            api_url,
            "-H",
            "Content-Type: application/json",
            "-H",
            &format!("x-api-key: {}", api_key),
            "-H",
            "anthropic-version: 2023-06-01",
            "-d",
            &body.to_string(),
        ])
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            eprintln!("Warning: failed to invoke curl for judge API: {}", e);
            return None;
        }
    };

    if !output.status.success() {
        eprintln!(
            "Warning: judge API curl failed with status {}",
            output.status
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Warning: judge API returned invalid JSON: {}", e);
            return None;
        }
    };

    // Extract text from the Messages API response
    let response_text = json
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|block| block.get("text"))
        .and_then(|t| t.as_str())?;

    parse_judge_score(response_text)
}

/// Parse a 1-5 score from LLM response text.
///
/// Finds the first ASCII digit and validates it's in range [1, 5].
/// Returns `None` if no valid score is found.
///
/// This function is shared between the capture-time judge (`core::judge`)
/// and the replay harness judge (`hooks::replay_harness`).
pub fn parse_judge_score(response_text: &str) -> Option<f64> {
    let score = response_text
        .chars()
        .find(|c| c.is_ascii_digit())
        .and_then(|c| c.to_digit(10))
        .map(|d| d as f64);

    match score {
        Some(s) if (1.0..=5.0).contains(&s) => Some(s),
        Some(s) => {
            eprintln!(
                "Warning: LLM judge returned out-of-range score {}: {:?}",
                s,
                truncate_str(response_text, 100)
            );
            None
        }
        None => {
            eprintln!(
                "Warning: LLM judge returned unparsable response: {:?}",
                truncate_str(response_text, 100)
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_specificity_judge_prompt() {
        let prompt = build_specificity_judge_prompt();
        assert!(prompt.contains("project-specific"));
        assert!(prompt.contains("1 ="));
        assert!(prompt.contains("5 ="));
        assert!(prompt.contains("single integer"));
    }

    #[test]
    fn test_parse_judge_score_valid() {
        assert_eq!(parse_judge_score("3"), Some(3.0));
        assert_eq!(parse_judge_score("Score: 4\n"), Some(4.0));
        assert_eq!(parse_judge_score("1"), Some(1.0));
        assert_eq!(parse_judge_score("5"), Some(5.0));
        assert_eq!(parse_judge_score("  2  "), Some(2.0));
        assert_eq!(parse_judge_score("The score is 3."), Some(3.0));
    }

    #[test]
    fn test_parse_judge_score_invalid() {
        assert_eq!(parse_judge_score(""), None);
        assert_eq!(parse_judge_score("abc"), None);
        assert_eq!(parse_judge_score("0"), None);
        assert_eq!(parse_judge_score("6"), None);
        assert_eq!(parse_judge_score("no digits here"), None);
    }

    #[test]
    fn test_format_learning_block() {
        use crate::core::learning::*;

        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            "Always validate VRL transforms locally",
            "Use vector validate to check VRL transforms before deploying to production.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["vector".to_string(), "vrl".to_string()],
            "session-1",
        );

        let block = format_learning_block(&learning);
        assert!(block.contains("Always validate VRL transforms locally"));
        assert!(block.contains("vector validate"));
        assert!(block.contains("vector, vrl"));
        assert!(block.contains("Pattern"));
    }

    #[test]
    fn test_format_learning_block_with_files() {
        use crate::core::learning::*;

        let mut learning = CompoundLearning::new(
            LearningCategory::Convention,
            "Config must be loaded from project root",
            "The config loader searches for .grove/config.toml starting from cwd upward.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::StableFact],
            vec!["config".to_string()],
            "session-1",
        );
        learning.context_files = Some(vec!["src/config.rs".to_string()]);

        let block = format_learning_block(&learning);
        assert!(block.contains("Files: src/config.rs"));
    }
}
