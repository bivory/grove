//! Corpus loading: transcripts, learnings, and session contexts.
//!
//! Parses Claude Code transcript JSONL files to extract tool calls and
//! build session context for offline evaluation.

use crate::backends::MarkdownBackend;
use crate::core::learning::CompoundLearning;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// A parsed tool call from a transcript.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Tool name (e.g., "Read", "Bash", "Grep").
    pub tool_name: String,
    /// Tool input as JSON.
    pub tool_input: serde_json::Value,
}

/// Session context built from ALL tool calls in a transcript.
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// Session file name.
    pub session_file: String,
    /// All file paths touched (from Read, Edit, Write, Grep path, Glob path).
    pub file_paths: Vec<String>,
    /// All search patterns used (from Grep pattern fields).
    pub grep_patterns: Vec<String>,
    /// All bash commands run.
    pub bash_commands: Vec<String>,
    /// All tool calls in the session.
    pub all_tool_calls: Vec<ToolCall>,
}

/// A high-level summary of a session transcript for retroflect analysis.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    /// JSONL filename (UUID, without .jsonl extension).
    pub session_id: String,
    /// From cwd field in first JSONL entry.
    pub project_cwd: PathBuf,
    /// First user message timestamp (if available).
    pub timestamp: Option<DateTime<Utc>>,
    /// Number of user turns in the session.
    pub user_turns: usize,
    /// Number of tool calls made by the assistant.
    pub tool_calls: usize,
    /// File paths touched during the session.
    pub file_paths: Vec<String>,
    /// Condensed user/assistant transcript text.
    pub condensed_transcript: String,
}

/// Corpus configuration specifying where to find transcripts and learnings.
#[derive(Debug, Clone)]
pub struct CorpusConfig {
    /// Path to directory containing transcript JSONL files.
    pub transcript_dir: std::path::PathBuf,
    /// Path to the learnings markdown file.
    pub learnings_path: std::path::PathBuf,
    /// Human-readable name for this corpus (e.g., "my-project").
    pub name: String,
}

/// A loaded corpus ready for evaluation.
pub struct Corpus {
    /// Loaded learnings.
    pub learnings: Vec<CompoundLearning>,
    /// Session contexts built from transcripts.
    pub contexts: Vec<SessionContext>,
    /// Lookup map from learning ID to index in learnings vec.
    pub learning_map: BTreeMap<String, usize>,
    /// Lookup map from session file name to index in contexts vec.
    pub context_map: BTreeMap<String, usize>,
    /// Corpus name.
    pub name: String,
}

/// Load a complete corpus from the given configuration.
pub fn load_corpus(config: &CorpusConfig) -> crate::Result<Corpus> {
    let learnings = load_learnings(&config.learnings_path);
    if learnings.is_empty() {
        return Err(crate::GroveError::config(format!(
            "No learnings loaded from {}",
            config.learnings_path.display()
        )));
    }

    let contexts = build_session_contexts(&config.transcript_dir);
    if contexts.is_empty() {
        return Err(crate::GroveError::config(format!(
            "No session contexts built from {}",
            config.transcript_dir.display()
        )));
    }

    let learning_map: BTreeMap<String, usize> = learnings
        .iter()
        .enumerate()
        .map(|(i, l)| (l.id.clone(), i))
        .collect();

    let context_map: BTreeMap<String, usize> = contexts
        .iter()
        .enumerate()
        .map(|(i, ctx)| (ctx.session_file.clone(), i))
        .collect();

    Ok(Corpus {
        learnings,
        contexts,
        learning_map,
        context_map,
        name: config.name.clone(),
    })
}

/// Parse ALL tool calls from a single JSONL transcript.
pub fn parse_all_tool_calls(content: &str) -> Vec<ToolCall> {
    let mut results = Vec::new();

    for line in content.lines() {
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if obj.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }

        let content_blocks = obj
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array());

        let blocks = match content_blocks {
            Some(b) => b,
            None => continue,
        };

        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                let tool_name = block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let tool_input = block
                    .get("input")
                    .cloned()
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                results.push(ToolCall {
                    tool_name,
                    tool_input,
                });
            }
        }
    }

    results
}

/// Build session contexts by parsing all transcripts and extracting all tool calls.
pub fn build_session_contexts(dir: &Path) -> Vec<SessionContext> {
    let mut contexts = Vec::new();

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("Cannot read transcript directory {}: {}", dir.display(), e);
            return contexts;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }

        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let all_tool_calls = parse_all_tool_calls(&content);
        if all_tool_calls.is_empty() {
            continue;
        }

        let mut file_paths = Vec::new();
        let mut grep_patterns = Vec::new();
        let mut bash_commands = Vec::new();

        for tc in &all_tool_calls {
            match tc.tool_name.as_str() {
                "Read" | "Write" | "Edit" => {
                    if let Some(fp) = tc.tool_input.get("file_path").and_then(|v| v.as_str()) {
                        file_paths.push(fp.to_string());
                    }
                }
                "Grep" => {
                    if let Some(pat) = tc.tool_input.get("pattern").and_then(|v| v.as_str()) {
                        grep_patterns.push(pat.to_string());
                    }
                    if let Some(fp) = tc.tool_input.get("path").and_then(|v| v.as_str()) {
                        file_paths.push(fp.to_string());
                    }
                }
                "Glob" => {
                    if let Some(pat) = tc.tool_input.get("pattern").and_then(|v| v.as_str()) {
                        file_paths.push(pat.to_string());
                    }
                    if let Some(fp) = tc.tool_input.get("path").and_then(|v| v.as_str()) {
                        file_paths.push(fp.to_string());
                    }
                }
                "Bash" => {
                    if let Some(cmd) = tc.tool_input.get("command").and_then(|v| v.as_str()) {
                        bash_commands.push(cmd.to_string());
                    }
                }
                _ => {}
            }
        }

        contexts.push(SessionContext {
            session_file: file_name,
            file_paths,
            grep_patterns,
            bash_commands,
            all_tool_calls,
        });
    }

    contexts.sort_by(|a, b| a.session_file.cmp(&b.session_file));
    contexts
}

/// Load learnings from a markdown file.
pub fn load_learnings(path: &Path) -> Vec<CompoundLearning> {
    let backend = MarkdownBackend::with_paths(path, path);
    match backend.parse_file(path) {
        Ok(learnings) => learnings,
        Err(e) => {
            eprintln!("Failed to parse learnings from {}: {}", path.display(), e);
            Vec::new()
        }
    }
}

/// Resolve corpus config from CLI flags, env vars, and config file.
///
/// Priority: CLI flags > env vars > error (no hardcoded defaults).
pub fn resolve_corpus_config(
    transcript_dir: Option<&str>,
    learnings_path: Option<&str>,
) -> crate::Result<CorpusConfig> {
    let transcript_dir = transcript_dir
        .map(|s| s.to_string())
        .or_else(|| std::env::var("GROVE_BENCH_TRANSCRIPT_DIR").ok())
        .ok_or_else(|| {
            crate::GroveError::config(
                "No transcript directory specified. Use --transcript-dir or set GROVE_BENCH_TRANSCRIPT_DIR"
                    .to_string(),
            )
        })?;

    let learnings_path = learnings_path
        .map(|s| s.to_string())
        .or_else(|| std::env::var("GROVE_BENCH_LEARNINGS_PATH").ok())
        .ok_or_else(|| {
            crate::GroveError::config(
                "No learnings path specified. Use --learnings-path or set GROVE_BENCH_LEARNINGS_PATH"
                    .to_string(),
            )
        })?;

    // Derive corpus name from transcript directory
    let dir_path = std::path::PathBuf::from(&transcript_dir);
    let name = dir_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed".to_string());

    Ok(CorpusConfig {
        transcript_dir: dir_path,
        learnings_path: std::path::PathBuf::from(learnings_path),
        name,
    })
}

/// A corpus entry in the manifest file.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CorpusEntry {
    /// Human-readable name (e.g., "my-project").
    pub name: String,
    /// Path to directory containing transcript JSONL files.
    pub transcript_dir: String,
    /// Path to the learnings markdown file.
    pub learnings_path: String,
    /// Per-corpus judge cache path (optional; derived from name if absent).
    pub cache_path: Option<String>,
}

/// Multi-corpus manifest.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CorpusManifest {
    /// List of corpus entries to evaluate.
    pub corpus: Vec<CorpusEntry>,
}

/// Load a corpus manifest from a TOML file.
///
/// The manifest lists all known corpora with their paths. Example:
///
/// ```toml
/// [[corpus]]
/// name = "my-project"
/// transcript_dir = "~/.claude/projects/-Users-dev-my-project"
/// learnings_path = "/home/dev/my-project/.grove/learnings.md"
/// ```
pub fn load_corpus_manifest(path: &Path) -> crate::Result<CorpusManifest> {
    let content = fs::read_to_string(path).map_err(|e| {
        crate::GroveError::config(format!(
            "Failed to read corpus manifest at {}: {}",
            path.display(),
            e
        ))
    })?;

    let manifest: CorpusManifest = toml::from_str(&content).map_err(|e| {
        crate::GroveError::config(format!("Failed to parse corpus manifest: {}", e))
    })?;

    if manifest.corpus.is_empty() {
        return Err(crate::GroveError::config(
            "Corpus manifest contains no corpus entries".to_string(),
        ));
    }

    Ok(manifest)
}

/// Parse a session transcript JSONL file into a `SessionSummary`.
///
/// Returns `None` for empty files or files with no parseable content.
pub fn parse_session_transcript(path: &Path) -> Option<SessionSummary> {
    let content = fs::read_to_string(path).ok()?;
    if content.trim().is_empty() {
        return None;
    }

    let session_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut project_cwd = PathBuf::new();
    let mut cwd_found = false;
    let mut timestamp: Option<DateTime<Utc>> = None;
    let mut user_turns: usize = 0;
    let mut tool_calls: usize = 0;
    let mut file_paths = Vec::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut current_user_text = String::new();
    let mut current_assistant_text = String::new();

    for line in content.lines() {
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Extract cwd from the first entry that has it
        if !cwd_found {
            if let Some(cwd) = obj.get("cwd").and_then(|v| v.as_str()) {
                project_cwd = PathBuf::from(cwd);
                cwd_found = true;
            }
        }

        let entry_type = obj
            .get("type")
            .and_then(|t| t.as_str())
            .or_else(|| obj.get("role").and_then(|r| r.as_str()));

        match entry_type {
            Some("user") => {
                // Flush previous pair if we have assistant text
                if !current_user_text.is_empty() || !current_assistant_text.is_empty() {
                    pairs.push((
                        std::mem::take(&mut current_user_text),
                        std::mem::take(&mut current_assistant_text),
                    ));
                }

                user_turns += 1;

                // Extract timestamp from first user message
                if timestamp.is_none() {
                    if let Some(ts) = obj.get("timestamp").and_then(|v| v.as_str()) {
                        timestamp = ts.parse::<DateTime<Utc>>().ok();
                    }
                }

                // Extract text content blocks (skip tool_result blocks)
                let blocks = obj
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .or_else(|| obj.get("content"));

                if let Some(blocks) = blocks {
                    if let Some(text) = blocks.as_str() {
                        current_user_text.push_str(text);
                    } else if let Some(arr) = blocks.as_array() {
                        for block in arr {
                            let block_type = block.get("type").and_then(|t| t.as_str());
                            if block_type == Some("text") {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    if !current_user_text.is_empty() {
                                        current_user_text.push('\n');
                                    }
                                    current_user_text.push_str(text);
                                }
                            }
                            // Skip tool_result blocks
                        }
                    }
                }
            }
            Some("assistant") => {
                let blocks = obj
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .or_else(|| obj.get("content"));

                if let Some(blocks) = blocks {
                    if let Some(text) = blocks.as_str() {
                        current_assistant_text.push_str(text);
                    } else if let Some(arr) = blocks.as_array() {
                        for block in arr {
                            let block_type = block.get("type").and_then(|t| t.as_str());
                            match block_type {
                                Some("text") => {
                                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                        if !current_assistant_text.is_empty() {
                                            current_assistant_text.push('\n');
                                        }
                                        current_assistant_text.push_str(text);
                                    }
                                }
                                Some("tool_use") => {
                                    tool_calls += 1;
                                    // Extract file paths from tool inputs
                                    if let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                                        let input = block.get("input");
                                        match name {
                                            "Read" | "Write" | "Edit" => {
                                                if let Some(fp) = input
                                                    .and_then(|i| i.get("file_path"))
                                                    .and_then(|v| v.as_str())
                                                {
                                                    file_paths.push(fp.to_string());
                                                }
                                            }
                                            "Grep" | "Glob" => {
                                                if let Some(fp) = input
                                                    .and_then(|i| i.get("path"))
                                                    .and_then(|v| v.as_str())
                                                {
                                                    file_paths.push(fp.to_string());
                                                }
                                                if let Some(pat) = input
                                                    .and_then(|i| i.get("pattern"))
                                                    .and_then(|v| v.as_str())
                                                {
                                                    // For Glob, the pattern is often a path
                                                    if name == "Glob" {
                                                        file_paths.push(pat.to_string());
                                                    }
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                // Skip "thinking" and other block types
                                _ => {}
                            }
                        }
                    }
                }
            }
            // Skip queue-operation, progress, system, file-history-snapshot
            _ => {}
        }
    }

    // Flush final pair
    if !current_user_text.is_empty() || !current_assistant_text.is_empty() {
        pairs.push((current_user_text, current_assistant_text));
    }

    if pairs.is_empty() && user_turns == 0 {
        return None;
    }

    // Deduplicate file paths while preserving order
    let mut seen = std::collections::HashSet::new();
    file_paths.retain(|p| seen.insert(p.clone()));

    let condensed_transcript = condense_transcript(&pairs, 32000);

    Some(SessionSummary {
        session_id,
        project_cwd,
        timestamp,
        user_turns,
        tool_calls,
        file_paths,
        condensed_transcript,
    })
}

/// Condense user-assistant pairs into a single transcript string.
///
/// If the total text fits within `max_chars`, returns everything.
/// Otherwise, uses a sliding window approach: sorts pairs by assistant text
/// length (descending) and takes the longest responses first without splitting
/// pairs.
pub fn condense_transcript(pairs: &[(String, String)], max_chars: usize) -> String {
    let format_pair = |user: &str, assistant: &str| -> String {
        format!("User: {}\n\nAssistant: {}\n\n---\n\n", user, assistant)
    };

    // Calculate total size
    let total: usize = pairs.iter().map(|(u, a)| format_pair(u, a).len()).sum();

    if total <= max_chars {
        return pairs
            .iter()
            .map(|(u, a)| format_pair(u, a))
            .collect::<String>()
            .trim_end()
            .to_string();
    }

    // Sort indices by assistant text length (descending), keeping original indices
    // to reconstruct chronological order later
    let mut indexed: Vec<(usize, usize)> = pairs
        .iter()
        .enumerate()
        .map(|(i, (_, a))| (i, a.len()))
        .collect();
    indexed.sort_by(|a, b| b.1.cmp(&a.1));

    // Greedily select pairs that fit within budget
    let mut selected_indices = Vec::new();
    let mut budget = max_chars;

    for (idx, _) in &indexed {
        let formatted = format_pair(&pairs[*idx].0, &pairs[*idx].1);
        if formatted.len() <= budget {
            selected_indices.push(*idx);
            budget -= formatted.len();
        }
    }

    // Sort selected indices to restore chronological order
    selected_indices.sort();

    selected_indices
        .iter()
        .map(|&i| format_pair(&pairs[i].0, &pairs[i].1))
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Convert a corpus entry into a CorpusConfig, expanding ~ in paths.
pub fn entry_to_config(entry: &CorpusEntry) -> CorpusConfig {
    let expand = |s: &str| -> std::path::PathBuf {
        if let Some(rest) = s.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(rest);
            }
        }
        std::path::PathBuf::from(s)
    };

    CorpusConfig {
        transcript_dir: expand(&entry.transcript_dir),
        learnings_path: expand(&entry.learnings_path),
        name: entry.name.clone(),
    }
}

/// Build a synthetic negative corpus by pairing learnings from one corpus
/// with sessions from another. Used for cross-project false positive rate testing.
///
/// The resulting corpus has learnings from `learnings_corpus` and session contexts
/// from `sessions_corpus`. Since the learnings and sessions come from different
/// projects, a good retrieval system should score these pairs low (1-2).
pub fn build_negative_corpus(learnings_corpus: &Corpus, sessions_corpus: &Corpus) -> Corpus {
    let learnings = learnings_corpus.learnings.clone();
    let contexts = sessions_corpus.contexts.clone();

    let learning_map: BTreeMap<String, usize> = learnings
        .iter()
        .enumerate()
        .map(|(i, l)| (l.id.clone(), i))
        .collect();

    let context_map: BTreeMap<String, usize> = contexts
        .iter()
        .enumerate()
        .map(|(i, ctx)| (ctx.session_file.clone(), i))
        .collect();

    Corpus {
        learnings,
        contexts,
        learning_map,
        context_map,
        name: format!("{}-x-{}", learnings_corpus.name, sessions_corpus.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_tool_calls_empty_input() {
        let calls = parse_all_tool_calls("");
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_all_tool_calls_extracts_tool_use() {
        let jsonl = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/test.rs"}}]}}"#;
        let calls = parse_all_tool_calls(jsonl);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "Read");
        assert_eq!(
            calls[0].tool_input.get("file_path").unwrap().as_str(),
            Some("/tmp/test.rs")
        );
    }

    #[test]
    fn parse_all_tool_calls_skips_non_assistant() {
        let jsonl = r#"{"type":"user","message":{"content":"hello"}}"#;
        let calls = parse_all_tool_calls(jsonl);
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_all_tool_calls_handles_malformed_lines() {
        let jsonl = "not json\n{\"type\":\"user\"}\n";
        let calls = parse_all_tool_calls(jsonl);
        assert!(calls.is_empty());
    }

    #[test]
    fn resolve_corpus_config_from_flags() {
        let config = resolve_corpus_config(Some("/tmp/transcripts"), Some("/tmp/learnings.md"));
        assert!(config.is_ok());
        let config = config.unwrap();
        assert_eq!(
            config.transcript_dir,
            std::path::PathBuf::from("/tmp/transcripts")
        );
        assert_eq!(
            config.learnings_path,
            std::path::PathBuf::from("/tmp/learnings.md")
        );
        assert_eq!(config.name, "transcripts");
    }

    #[test]
    fn resolve_corpus_config_missing_flags_and_env() {
        // Clear env vars for this test
        std::env::remove_var("GROVE_BENCH_TRANSCRIPT_DIR");
        std::env::remove_var("GROVE_BENCH_LEARNINGS_PATH");

        let config = resolve_corpus_config(None, None);
        assert!(config.is_err());
    }

    // Corpus manifest tests

    #[test]
    fn load_corpus_manifest_valid() {
        let dir = tempfile::TempDir::new().unwrap();
        let manifest_path = dir.path().join("corpora.toml");
        std::fs::write(
            &manifest_path,
            r#"
[[corpus]]
name = "test-corpus"
transcript_dir = "/tmp/transcripts"
learnings_path = "/tmp/learnings.md"

[[corpus]]
name = "other-corpus"
transcript_dir = "/tmp/other-transcripts"
learnings_path = "/tmp/other-learnings.md"
cache_path = "/tmp/other-cache.json"
"#,
        )
        .unwrap();

        let manifest = load_corpus_manifest(&manifest_path).unwrap();
        assert_eq!(manifest.corpus.len(), 2);
        assert_eq!(manifest.corpus[0].name, "test-corpus");
        assert_eq!(manifest.corpus[1].name, "other-corpus");
        assert!(manifest.corpus[0].cache_path.is_none());
        assert_eq!(
            manifest.corpus[1].cache_path.as_deref(),
            Some("/tmp/other-cache.json")
        );
    }

    #[test]
    fn load_corpus_manifest_empty_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let manifest_path = dir.path().join("empty.toml");
        std::fs::write(&manifest_path, "corpus = []\n").unwrap();

        let result = load_corpus_manifest(&manifest_path);
        assert!(result.is_err());
    }

    #[test]
    fn load_corpus_manifest_missing_file_errors() {
        let result = load_corpus_manifest(Path::new("/nonexistent/corpora.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn load_corpus_manifest_invalid_toml_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let manifest_path = dir.path().join("bad.toml");
        std::fs::write(&manifest_path, "not valid toml {{{\n").unwrap();

        let result = load_corpus_manifest(&manifest_path);
        assert!(result.is_err());
    }

    #[test]
    fn entry_to_config_absolute_paths() {
        let entry = CorpusEntry {
            name: "test".to_string(),
            transcript_dir: "/tmp/transcripts".to_string(),
            learnings_path: "/tmp/learnings.md".to_string(),
            cache_path: None,
        };

        let config = entry_to_config(&entry);
        assert_eq!(config.name, "test");
        assert_eq!(
            config.transcript_dir,
            std::path::PathBuf::from("/tmp/transcripts")
        );
        assert_eq!(
            config.learnings_path,
            std::path::PathBuf::from("/tmp/learnings.md")
        );
    }

    // Session transcript parser tests

    #[test]
    fn parse_session_transcript_empty_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, "").unwrap();
        assert!(parse_session_transcript(&path).is_none());
    }

    #[test]
    fn parse_session_transcript_whitespace_only() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("blank.jsonl");
        std::fs::write(&path, "  \n  \n").unwrap();
        assert!(parse_session_transcript(&path).is_none());
    }

    #[test]
    fn parse_session_transcript_basic() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("abc-123.jsonl");
        let lines = [
            r#"{"type":"user","cwd":"/home/dev/project","message":{"content":[{"type":"text","text":"Fix the bug"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"I'll fix it."},{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/foo.rs"}}]}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let summary = parse_session_transcript(&path).unwrap();
        assert_eq!(summary.session_id, "abc-123");
        assert_eq!(summary.project_cwd, PathBuf::from("/home/dev/project"));
        assert_eq!(summary.user_turns, 1);
        assert_eq!(summary.tool_calls, 1);
        assert_eq!(summary.file_paths, vec!["/tmp/foo.rs"]);
        assert!(summary.condensed_transcript.contains("Fix the bug"));
        assert!(summary.condensed_transcript.contains("I'll fix it."));
    }

    #[test]
    fn parse_session_transcript_skips_tool_result_blocks() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let lines = [
            r#"{"type":"user","cwd":"/dev","message":{"content":[{"type":"tool_result","content":"file contents here"},{"type":"text","text":"What does this do?"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"It does X."}]}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let summary = parse_session_transcript(&path).unwrap();
        assert!(summary.condensed_transcript.contains("What does this do?"));
        assert!(!summary.condensed_transcript.contains("file contents here"));
    }

    #[test]
    fn parse_session_transcript_skips_thinking_blocks() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let lines = [
            r#"{"type":"user","cwd":"/dev","message":{"content":[{"type":"text","text":"Hello"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","text":"Let me think..."},{"type":"text","text":"Hi there!"}]}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let summary = parse_session_transcript(&path).unwrap();
        assert!(!summary.condensed_transcript.contains("Let me think"));
        assert!(summary.condensed_transcript.contains("Hi there!"));
    }

    #[test]
    fn parse_session_transcript_skips_non_message_types() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let lines = [
            r#"{"type":"system","message":"init"}"#,
            r#"{"type":"progress","data":"50%"}"#,
            r#"{"type":"queue-operation","action":"enqueue"}"#,
            r#"{"type":"file-history-snapshot","files":[]}"#,
            r#"{"type":"user","cwd":"/dev","message":{"content":"Hi"}}"#,
            r#"{"type":"assistant","message":{"content":"Hello!"}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let summary = parse_session_transcript(&path).unwrap();
        assert_eq!(summary.user_turns, 1);
        assert!(summary.condensed_transcript.contains("Hi"));
    }

    #[test]
    fn parse_session_transcript_extracts_file_paths_from_tools() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let lines = [
            r#"{"type":"user","cwd":"/dev","message":{"content":"check files"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a.rs"}},{"type":"tool_use","name":"Edit","input":{"file_path":"/b.rs"}},{"type":"tool_use","name":"Grep","input":{"pattern":"foo","path":"/src"}},{"type":"tool_use","name":"Glob","input":{"pattern":"*.rs","path":"/lib"}},{"type":"tool_use","name":"Write","input":{"file_path":"/c.rs"}},{"type":"text","text":"Done."}]}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let summary = parse_session_transcript(&path).unwrap();
        assert_eq!(summary.tool_calls, 5);
        assert!(summary.file_paths.contains(&"/a.rs".to_string()));
        assert!(summary.file_paths.contains(&"/b.rs".to_string()));
        assert!(summary.file_paths.contains(&"/src".to_string()));
        assert!(summary.file_paths.contains(&"*.rs".to_string()));
        assert!(summary.file_paths.contains(&"/lib".to_string()));
        assert!(summary.file_paths.contains(&"/c.rs".to_string()));
    }

    #[test]
    fn parse_session_transcript_deduplicates_file_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let lines = [
            r#"{"type":"user","cwd":"/dev","message":{"content":"check"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a.rs"}},{"type":"tool_use","name":"Read","input":{"file_path":"/a.rs"}},{"type":"text","text":"ok"}]}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let summary = parse_session_transcript(&path).unwrap();
        assert_eq!(summary.file_paths.len(), 1);
    }

    #[test]
    fn parse_session_transcript_handles_malformed_lines() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let lines = [
            "not valid json",
            r#"{"type":"user","cwd":"/dev","message":{"content":"Hi"}}"#,
            "{broken",
            r#"{"type":"assistant","message":{"content":"Hello"}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let summary = parse_session_transcript(&path).unwrap();
        assert_eq!(summary.user_turns, 1);
    }

    #[test]
    fn parse_session_transcript_extracts_timestamp() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let lines = [
            r#"{"type":"user","cwd":"/dev","timestamp":"2024-01-15T10:30:00Z","message":{"content":"Hi"}}"#,
            r#"{"type":"assistant","message":{"content":"Hello"}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let summary = parse_session_transcript(&path).unwrap();
        assert!(summary.timestamp.is_some());
    }

    #[test]
    fn parse_session_transcript_missing_file() {
        let result = parse_session_transcript(Path::new("/nonexistent/session.jsonl"));
        assert!(result.is_none());
    }

    // Condense transcript tests

    #[test]
    fn condense_transcript_fits_within_budget() {
        let pairs = vec![
            ("Q1".to_string(), "A1".to_string()),
            ("Q2".to_string(), "A2".to_string()),
        ];
        let result = condense_transcript(&pairs, 100_000);
        assert!(result.contains("User: Q1"));
        assert!(result.contains("Assistant: A1"));
        assert!(result.contains("User: Q2"));
        assert!(result.contains("Assistant: A2"));
    }

    #[test]
    fn condense_transcript_truncates_when_over_budget() {
        let short = ("short".to_string(), "s".to_string());
        let long = ("long".to_string(), "x".repeat(100));
        let pairs = vec![short.clone(), long.clone()];

        // Budget too small for both, should keep the longer assistant response
        let result = condense_transcript(&pairs, 150);
        assert!(result.contains("User: long"));
        assert!(result.contains(&"x".repeat(100)));
    }

    #[test]
    fn condense_transcript_empty_pairs() {
        let result = condense_transcript(&[], 1000);
        assert!(result.is_empty());
    }

    #[test]
    fn condense_transcript_preserves_chronological_order() {
        let pairs = vec![
            ("first".to_string(), "a".repeat(50)),
            ("second".to_string(), "b".repeat(10)),
            ("third".to_string(), "c".repeat(50)),
        ];

        // Budget enough for pairs 0 and 2 (longest assistant text), but not all three
        // The selected pairs should be in chronological order
        let result = condense_transcript(&pairs, 200);
        if result.contains("first") && result.contains("third") {
            let first_pos = result.find("first").unwrap();
            let third_pos = result.find("third").unwrap();
            assert!(
                first_pos < third_pos,
                "chronological order should be preserved"
            );
        }
    }

    #[test]
    fn entry_to_config_tilde_expansion() {
        let entry = CorpusEntry {
            name: "tilde-test".to_string(),
            transcript_dir: "~/transcripts".to_string(),
            learnings_path: "~/learnings.md".to_string(),
            cache_path: None,
        };

        let config = entry_to_config(&entry);
        // Should expand ~ to home dir
        assert!(
            !config.transcript_dir.to_string_lossy().starts_with('~'),
            "tilde should be expanded"
        );
        assert!(
            config
                .transcript_dir
                .to_string_lossy()
                .contains("transcripts"),
            "path tail should be preserved"
        );
    }

    // Helper to build a test corpus
    fn make_test_corpus(name: &str, learning_ids: &[&str], session_files: &[&str]) -> Corpus {
        use crate::core::learning::{
            CompoundLearning, Confidence, LearningCategory, LearningScope, LearningStatus,
        };

        let learnings: Vec<CompoundLearning> = learning_ids
            .iter()
            .map(|id| CompoundLearning {
                id: id.to_string(),
                schema_version: 1,
                category: LearningCategory::Pattern,
                summary: format!("Learning {}", id),
                detail: format!("Detail for {}", id),
                scope: LearningScope::Project,
                confidence: Confidence::High,
                criteria_met: vec![],
                tags: vec![],
                session_id: "test-session".to_string(),
                ticket_id: None,
                timestamp: chrono::Utc::now(),
                context_files: None,
                relevance_context: None,
                status: LearningStatus::Active,
            })
            .collect();

        let contexts: Vec<SessionContext> = session_files
            .iter()
            .map(|f| SessionContext {
                session_file: f.to_string(),
                file_paths: vec![],
                grep_patterns: vec![],
                bash_commands: vec![],
                all_tool_calls: vec![],
            })
            .collect();

        let learning_map: BTreeMap<String, usize> = learnings
            .iter()
            .enumerate()
            .map(|(i, l)| (l.id.clone(), i))
            .collect();

        let context_map: BTreeMap<String, usize> = contexts
            .iter()
            .enumerate()
            .map(|(i, ctx)| (ctx.session_file.clone(), i))
            .collect();

        Corpus {
            learnings,
            contexts,
            learning_map,
            context_map,
            name: name.to_string(),
        }
    }

    #[test]
    fn build_negative_corpus_combines_correctly() {
        let a = make_test_corpus("alpha", &["l1", "l2"], &["s1.jsonl"]);
        let b = make_test_corpus("beta", &["l3"], &["s2.jsonl", "s3.jsonl"]);

        let neg = build_negative_corpus(&a, &b);
        assert_eq!(neg.name, "alpha-x-beta");
        assert_eq!(neg.learnings.len(), 2); // from alpha
        assert_eq!(neg.contexts.len(), 2); // from beta
    }

    #[test]
    fn build_negative_corpus_preserves_learning_map() {
        let a = make_test_corpus("alpha", &["l1", "l2", "l3"], &["s1.jsonl"]);
        let b = make_test_corpus("beta", &["lx"], &["s2.jsonl"]);

        let neg = build_negative_corpus(&a, &b);
        assert_eq!(neg.learning_map.len(), 3);
        assert_eq!(neg.learning_map["l1"], 0);
        assert_eq!(neg.learning_map["l2"], 1);
        assert_eq!(neg.learning_map["l3"], 2);
        // Verify map indices point to correct learnings
        assert_eq!(neg.learnings[neg.learning_map["l1"]].id, "l1");
        assert_eq!(neg.learnings[neg.learning_map["l2"]].id, "l2");
    }

    #[test]
    fn build_negative_corpus_preserves_context_map() {
        let a = make_test_corpus("alpha", &["l1"], &["s1.jsonl"]);
        let b = make_test_corpus("beta", &["lx"], &["s2.jsonl", "s3.jsonl"]);

        let neg = build_negative_corpus(&a, &b);
        assert_eq!(neg.context_map.len(), 2);
        assert_eq!(neg.context_map["s2.jsonl"], 0);
        assert_eq!(neg.context_map["s3.jsonl"], 1);
        // Verify map indices point to correct contexts
        assert_eq!(
            neg.contexts[neg.context_map["s2.jsonl"]].session_file,
            "s2.jsonl"
        );
    }

    #[test]
    fn build_negative_corpus_empty_learnings() {
        let a = make_test_corpus("alpha", &[], &["s1.jsonl"]);
        let b = make_test_corpus("beta", &["l1"], &["s2.jsonl"]);

        let neg = build_negative_corpus(&a, &b);
        assert_eq!(neg.learnings.len(), 0);
        assert_eq!(neg.learning_map.len(), 0);
        assert_eq!(neg.contexts.len(), 1);
    }

    #[test]
    fn build_negative_corpus_empty_sessions() {
        let a = make_test_corpus("alpha", &["l1", "l2"], &["s1.jsonl"]);
        let b = make_test_corpus("beta", &["l3"], &[]);

        let neg = build_negative_corpus(&a, &b);
        assert_eq!(neg.learnings.len(), 2);
        assert_eq!(neg.contexts.len(), 0);
        assert_eq!(neg.context_map.len(), 0);
    }
}
