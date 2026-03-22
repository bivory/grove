//! Offline replay harness for keyword extraction quality evaluation.
//!
//! Parses real Claude Code transcript JSONL files, extracts the first tool call
//! from each session, runs both v1 and v2 keyword extractors, matches keywords
//! against real learnings, and compares retrieval results.
//!
//! Tests are `#[ignore]`d so they don't run in CI (they require real data files
//! on disk). Run manually with:
//!
//! ```bash
//! cargo test -- --ignored replay --nocapture
//! ```

use super::*;
use crate::core::learning::CompoundLearning;
use crate::eval::corpus::{self as eval_corpus, SessionContext};
use crate::eval::judge::{self as eval_judge, JudgeContext};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

/// Default directory containing Claude Code transcript JSONL files.
/// Override with GROVE_BENCH_TRANSCRIPT_DIR env var to use a different corpus.
const DEFAULT_TRANSCRIPT_DIR: &str =
    concat!(env!("HOME"), "/.claude/projects/-Users-dev-my-project");

/// Default path to the real learnings file.
/// Override with GROVE_BENCH_LEARNINGS_PATH env var to use a different corpus.
const DEFAULT_LEARNINGS_PATH: &str = concat!(env!("HOME"), "/my-project/.grove/learnings.md");

/// Resolve transcript directory from env var or default.
fn transcript_dir() -> String {
    std::env::var("GROVE_BENCH_TRANSCRIPT_DIR")
        .unwrap_or_else(|_| DEFAULT_TRANSCRIPT_DIR.to_string())
}

/// Resolve learnings path from env var or default.
fn learnings_path() -> String {
    std::env::var("GROVE_BENCH_LEARNINGS_PATH")
        .unwrap_or_else(|_| DEFAULT_LEARNINGS_PATH.to_string())
}

/// A parsed first tool call from a session transcript.
#[derive(Debug, Clone)]
struct FirstToolCall {
    /// Session file name (UUID.jsonl).
    session_file: String,
    /// Tool name (e.g., "Read", "Bash", "Grep").
    tool_name: String,
    /// Tool input as JSON.
    tool_input: serde_json::Value,
}

/// Result of running keyword extraction on a single tool call.
#[derive(Debug)]
struct ExtractionResult {
    session_file: String,
    tool_name: String,
    v1_keywords: Vec<String>,
    v2_keywords: Vec<String>,
    v1_matched_learnings: Vec<MatchedLearning>,
    v2_matched_learnings: Vec<MatchedLearning>,
}

/// A learning that was matched by keywords.
#[derive(Debug, Clone)]
struct MatchedLearning {
    id: String,
    summary: String,
    matched_keywords: Vec<String>,
}

/// Parse all JSONL transcript files and extract the first tool_use block from each.
fn parse_transcripts(dir: &Path) -> Vec<FirstToolCall> {
    let mut results = Vec::new();

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("Cannot read transcript directory {}: {}", dir.display(), e);
            return results;
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

        // Find the first tool_use block in this session
        if let Some(first_tool) = find_first_tool_call(&content, &file_name) {
            results.push(first_tool);
        }
    }

    results.sort_by(|a, b| a.session_file.cmp(&b.session_file));
    results
}

/// Find the first tool_use content block in a JSONL transcript.
fn find_first_tool_call(content: &str, file_name: &str) -> Option<FirstToolCall> {
    for line in content.lines() {
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Look for assistant messages
        if obj.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }

        // Navigate to message.content array
        let content_blocks = obj
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array());

        let blocks = match content_blocks {
            Some(b) => b,
            None => continue,
        };

        // Find first tool_use block
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

                return Some(FirstToolCall {
                    session_file: file_name.to_string(),
                    tool_name,
                    tool_input,
                });
            }
        }
    }

    None
}

/// Load learnings from the markdown file.
fn load_learnings(path: &Path) -> Vec<CompoundLearning> {
    eval_corpus::load_learnings(path)
}

/// Match keywords against learnings using the same whole-word matching
/// approach as `compute_relevance` in the markdown backend.
///
/// For each keyword, check if it appears as a whole word in the learning's
/// summary or detail text. This mirrors the KEYWORD scoring path in
/// `MarkdownBackend::compute_relevance`.
fn match_keywords_to_learnings(
    keywords: &[String],
    learnings: &[CompoundLearning],
) -> Vec<MatchedLearning> {
    let mut matched = Vec::new();

    for learning in learnings {
        // Tokenize summary and detail into words (same approach as compute_relevance)
        let summary_words: HashSet<String> = learning
            .summary
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .filter(|w| !w.is_empty())
            .map(|w| w.to_string())
            .collect();

        let detail_words: HashSet<String> = learning
            .detail
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .filter(|w| !w.is_empty())
            .map(|w| w.to_string())
            .collect();

        let mut matched_kws = Vec::new();
        for keyword in keywords {
            let kw_lower = keyword.to_lowercase();
            // Check whole-word match in summary or detail
            if summary_words.contains(&kw_lower) || detail_words.contains(&kw_lower) {
                matched_kws.push(keyword.clone());
            }
        }

        if !matched_kws.is_empty() {
            matched.push(MatchedLearning {
                id: learning.id.clone(),
                summary: learning.summary.clone(),
                matched_keywords: matched_kws,
            });
        }
    }

    matched
}

/// Run the full replay harness: extract keywords, match learnings, compare v1 vs v2.
fn run_replay(
    tool_calls: &[FirstToolCall],
    learnings: &[CompoundLearning],
) -> Vec<ExtractionResult> {
    tool_calls
        .iter()
        .map(|tc| {
            let v1_keywords = extract_tool_input_keywords(&tc.tool_name, &tc.tool_input);
            let v2_keywords = extract_tool_input_keywords_v2(&tc.tool_name, &tc.tool_input);

            let v1_matched = match_keywords_to_learnings(&v1_keywords, learnings);
            let v2_matched = match_keywords_to_learnings(&v2_keywords, learnings);

            ExtractionResult {
                session_file: tc.session_file.clone(),
                tool_name: tc.tool_name.clone(),
                v1_keywords,
                v2_keywords,
                v1_matched_learnings: v1_matched,
                v2_matched_learnings: v2_matched,
            }
        })
        .collect()
}

/// Print a detailed comparison report.
fn print_report(results: &[ExtractionResult], total_learnings: usize) {
    eprintln!("\n{}", "=".repeat(80));
    eprintln!("OFFLINE REPLAY HARNESS -- v1 vs v2 KEYWORD EXTRACTION COMPARISON");
    eprintln!("{}", "=".repeat(80));
    eprintln!("Sessions analyzed: {}", results.len());
    eprintln!("Total learnings in corpus: {}", total_learnings);

    // Aggregate metrics
    let mut total_v1_keywords = 0usize;
    let mut total_v2_keywords = 0usize;
    let mut total_v1_matches = 0usize;
    let mut total_v2_matches = 0usize;
    let mut v1_learning_ids: HashSet<String> = HashSet::new();
    let mut v2_learning_ids: HashSet<String> = HashSet::new();
    let mut v1_only_learnings: BTreeMap<String, Vec<String>> = BTreeMap::new(); // id -> sessions
    let mut v2_only_learnings: BTreeMap<String, Vec<String>> = BTreeMap::new(); // id -> sessions
    let mut tool_distribution: BTreeMap<String, usize> = BTreeMap::new();

    eprintln!("\n{}", "-".repeat(80));
    eprintln!("PER-SESSION DETAILS");
    eprintln!("{}", "-".repeat(80));

    for result in results {
        *tool_distribution
            .entry(result.tool_name.clone())
            .or_insert(0) += 1;

        total_v1_keywords += result.v1_keywords.len();
        total_v2_keywords += result.v2_keywords.len();
        total_v1_matches += result.v1_matched_learnings.len();
        total_v2_matches += result.v2_matched_learnings.len();

        let v1_ids: HashSet<String> = result
            .v1_matched_learnings
            .iter()
            .map(|m| m.id.clone())
            .collect();
        let v2_ids: HashSet<String> = result
            .v2_matched_learnings
            .iter()
            .map(|m| m.id.clone())
            .collect();

        v1_learning_ids.extend(v1_ids.iter().cloned());
        v2_learning_ids.extend(v2_ids.iter().cloned());

        // Track learnings unique to each version
        for id in v1_ids.difference(&v2_ids) {
            v1_only_learnings
                .entry(id.clone())
                .or_default()
                .push(result.session_file.clone());
        }
        for id in v2_ids.difference(&v1_ids) {
            v2_only_learnings
                .entry(id.clone())
                .or_default()
                .push(result.session_file.clone());
        }

        // Print per-session detail
        let session_short = &result.session_file[..8.min(result.session_file.len())];
        eprintln!(
            "\n  Session: {} | Tool: {}",
            session_short, result.tool_name
        );
        eprintln!(
            "    v1 keywords ({}): {:?}",
            result.v1_keywords.len(),
            result.v1_keywords
        );
        eprintln!(
            "    v2 keywords ({}): {:?}",
            result.v2_keywords.len(),
            result.v2_keywords
        );

        let v1_only_kw: Vec<&String> = result
            .v1_keywords
            .iter()
            .filter(|k| !result.v2_keywords.contains(k))
            .collect();
        let v2_only_kw: Vec<&String> = result
            .v2_keywords
            .iter()
            .filter(|k| !result.v1_keywords.contains(k))
            .collect();
        if !v1_only_kw.is_empty() {
            eprintln!("    v1-only keywords (removed by v2): {:?}", v1_only_kw);
        }
        if !v2_only_kw.is_empty() {
            eprintln!("    v2-only keywords (added by v2): {:?}", v2_only_kw);
        }

        eprintln!(
            "    v1 matched learnings: {}",
            result.v1_matched_learnings.len()
        );
        for m in &result.v1_matched_learnings {
            eprintln!(
                "      - {} | {} [via: {}]",
                m.id,
                m.summary,
                m.matched_keywords.join(", ")
            );
        }
        eprintln!(
            "    v2 matched learnings: {}",
            result.v2_matched_learnings.len()
        );
        for m in &result.v2_matched_learnings {
            eprintln!(
                "      - {} | {} [via: {}]",
                m.id,
                m.summary,
                m.matched_keywords.join(", ")
            );
        }
    }

    // Summary report
    eprintln!("\n{}", "=".repeat(80));
    eprintln!("AGGREGATE SUMMARY");
    eprintln!("{}", "=".repeat(80));

    eprintln!("\n  First tool call distribution:");
    for (tool, count) in &tool_distribution {
        eprintln!("    {}: {} sessions", tool, count);
    }

    eprintln!("\n  Keyword extraction:");
    eprintln!("    v1 total keywords: {}", total_v1_keywords);
    eprintln!("    v2 total keywords: {}", total_v2_keywords);
    let kw_reduction = if total_v1_keywords > 0 {
        ((total_v1_keywords as f64 - total_v2_keywords as f64) / total_v1_keywords as f64) * 100.0
    } else {
        0.0
    };
    eprintln!(
        "    Keyword reduction: {:.1}% ({} fewer)",
        kw_reduction,
        total_v1_keywords.saturating_sub(total_v2_keywords)
    );
    let avg_v1 = if results.is_empty() {
        0.0
    } else {
        total_v1_keywords as f64 / results.len() as f64
    };
    let avg_v2 = if results.is_empty() {
        0.0
    } else {
        total_v2_keywords as f64 / results.len() as f64
    };
    eprintln!(
        "    Avg keywords per session: v1={:.1}, v2={:.1}",
        avg_v1, avg_v2
    );

    eprintln!("\n  Learning matches:");
    eprintln!(
        "    v1 total matches (across all sessions): {}",
        total_v1_matches
    );
    eprintln!(
        "    v2 total matches (across all sessions): {}",
        total_v2_matches
    );
    let match_reduction = if total_v1_matches > 0 {
        ((total_v1_matches as f64 - total_v2_matches as f64) / total_v1_matches as f64) * 100.0
    } else {
        0.0
    };
    eprintln!(
        "    Match reduction: {:.1}% ({} fewer)",
        match_reduction,
        total_v1_matches.saturating_sub(total_v2_matches)
    );

    eprintln!("\n  Unique learnings surfaced:");
    eprintln!(
        "    v1: {} / {} ({:.1}%)",
        v1_learning_ids.len(),
        total_learnings,
        v1_learning_ids.len() as f64 / total_learnings as f64 * 100.0
    );
    eprintln!(
        "    v2: {} / {} ({:.1}%)",
        v2_learning_ids.len(),
        total_learnings,
        v2_learning_ids.len() as f64 / total_learnings as f64 * 100.0
    );

    // Learnings gained/lost
    let both: HashSet<String> = v1_learning_ids
        .intersection(&v2_learning_ids)
        .cloned()
        .collect();
    let v1_only: HashSet<String> = v1_learning_ids
        .difference(&v2_learning_ids)
        .cloned()
        .collect();
    let v2_only: HashSet<String> = v2_learning_ids
        .difference(&v1_learning_ids)
        .cloned()
        .collect();

    eprintln!("    Shared (both v1 and v2): {}", both.len());
    eprintln!("    v1-only (LOST by switching to v2): {}", v1_only.len());
    eprintln!("    v2-only (GAINED by switching to v2): {}", v2_only.len());

    if !v1_only.is_empty() {
        eprintln!("\n  Learnings LOST by v2 (potential false negatives from noise filtering):");
        for (id, sessions) in &v1_only_learnings {
            eprintln!("    - {} (surfaced in {} sessions)", id, sessions.len());
        }
    }

    if !v2_only.is_empty() {
        eprintln!("\n  Learnings GAINED by v2 (from path stripping / Task tool support):");
        for (id, sessions) in &v2_only_learnings {
            eprintln!("    - {} (surfaced in {} sessions)", id, sessions.len());
        }
    }

    // Precision estimate (rough)
    eprintln!("\n  Precision estimate:");
    let avg_v1_matches = if results.is_empty() {
        0.0
    } else {
        total_v1_matches as f64 / results.len() as f64
    };
    let avg_v2_matches = if results.is_empty() {
        0.0
    } else {
        total_v2_matches as f64 / results.len() as f64
    };
    eprintln!(
        "    Avg learnings matched per session: v1={:.1}, v2={:.1}",
        avg_v1_matches, avg_v2_matches
    );
    eprintln!(
        "    Fewer spurious matches per session: {:.1}",
        avg_v1_matches - avg_v2_matches
    );

    eprintln!("\n{}", "=".repeat(80));
}

// =============================================================================
// Independent classification helpers
// =============================================================================

// AnyToolCall and SessionContext are imported from crate::eval::corpus

/// Build session contexts by parsing all transcripts and extracting all tool calls.
fn build_session_contexts(dir: &Path) -> Vec<SessionContext> {
    eval_corpus::build_session_contexts(dir)
}

/// Extract keywords from session context (file paths, grep patterns, bash commands).
///
/// Uses simple word tokenization to capture all domain terms present in the
/// session's activity. No aggressive noise filtering -- we want a broad
/// context fingerprint to use as an independent relevance signal.
fn extract_session_context_keywords(ctx: &SessionContext) -> HashSet<String> {
    let mut keywords = HashSet::new();

    let tokenize = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| w.len() >= 4 && !w.chars().all(|c| c.is_numeric()))
            .map(|w| w.to_lowercase())
            .collect()
    };

    for path in &ctx.file_paths {
        keywords.extend(tokenize(path));
    }
    for pattern in &ctx.grep_patterns {
        keywords.extend(tokenize(pattern));
    }
    for cmd in &ctx.bash_commands {
        keywords.extend(tokenize(cmd));
    }

    keywords
}

/// Extract keywords from a learning's summary, detail, and tags.
fn extract_learning_keywords(learning: &CompoundLearning) -> HashSet<String> {
    let mut keywords = HashSet::new();

    let tokenize = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| w.len() >= 4 && !w.chars().all(|c| c.is_numeric()))
            .map(|w| w.to_lowercase())
            .collect()
    };

    keywords.extend(tokenize(&learning.summary));
    keywords.extend(tokenize(&learning.detail));
    for tag in &learning.tags {
        keywords.extend(tokenize(tag));
    }

    keywords
}

/// Compute relevance score for a learning against session context.
///
/// Returns the overlap ratio: (shared keywords) / (total learning keywords).
/// Higher overlap means the learning is more relevant to what the session
/// actually worked on. This is an independent signal -- it does not use the
/// keyword extraction being evaluated, only the objective session activity.
fn compute_context_relevance(
    learning: &CompoundLearning,
    session_context_keywords: &HashSet<String>,
) -> f64 {
    let learning_keywords = extract_learning_keywords(learning);
    if learning_keywords.is_empty() {
        return 0.0;
    }

    let shared: usize = learning_keywords
        .intersection(session_context_keywords)
        .count();

    shared as f64 / learning_keywords.len() as f64
}

/// Result of independent classification for a single session.
#[derive(Debug)]
struct IndependentClassResult {
    session_file: String,
    tool_count: usize,
    context_keyword_count: usize,
    v1_matched: Vec<(String, f64)>, // (learning_id, relevance_score)
    v2_matched: Vec<(String, f64)>,
    v1_only: Vec<(String, f64)>, // learnings in v1 but not v2
    v2_only: Vec<(String, f64)>, // learnings in v2 but not v1
    v1_avg_relevance: f64,
    v2_avg_relevance: f64,
}

// =============================================================================
// Strict multi-signal relevance scoring
// =============================================================================

/// Extract file path segments from a learning's context_files and text.
///
/// Looks for path-like tokens in the learning's summary, detail, and
/// explicit context_files field. Returns normalized path segments
/// (lowercase, 4+ chars, no pure numbers).
fn extract_learning_file_signals(learning: &CompoundLearning) -> HashSet<String> {
    let mut signals = HashSet::new();

    let tokenize_path = |path: &str| -> Vec<String> {
        path.split(['/', '\\', '.'])
            .filter(|w| w.len() >= 4 && !w.chars().all(|c| c.is_numeric()))
            .map(|w| w.to_lowercase())
            .collect()
    };

    // From explicit context_files
    if let Some(ref files) = learning.context_files {
        for f in files {
            signals.extend(tokenize_path(f));
        }
    }

    // From text: look for path-like tokens (containing / or .)
    for text in [&learning.summary, &learning.detail] {
        for word in text.split_whitespace() {
            // Heuristic: if a word contains '/' or starts with '.', it's path-like
            if word.contains('/') || word.starts_with('.') {
                // Strip surrounding punctuation like backticks, parens
                let cleaned = word.trim_matches(|c: char| {
                    !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-'
                });
                signals.extend(tokenize_path(cleaned));
            }
        }
    }

    signals
}

/// Extract ONLY the session's file path segments (not grep/bash).
///
/// Returns normalized path segments from file paths touched during the session.
fn extract_session_file_signals(ctx: &SessionContext) -> HashSet<String> {
    let mut signals = HashSet::new();

    for path in &ctx.file_paths {
        for segment in path.split(['/', '\\', '.']) {
            if segment.len() >= 4 && !segment.chars().all(|c| c.is_numeric()) {
                signals.insert(segment.to_lowercase());
            }
        }
    }

    signals
}

/// Extract topic keywords from a learning's SUMMARY ONLY (not detail).
///
/// This is stricter than `extract_learning_keywords` which includes the detail
/// text. By limiting to summary, we get the learning's core topic terms
/// without incidental words from explanatory text.
fn extract_learning_summary_keywords(learning: &CompoundLearning) -> HashSet<String> {
    learning
        .summary
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|w| w.len() >= 4 && !w.chars().all(|c| c.is_numeric()))
        .map(|w| w.to_lowercase())
        .collect()
}

/// Extract session "activity" keywords from grep patterns and bash commands ONLY.
///
/// These represent what the session was actively searching for and doing,
/// rather than just which files it touched.
fn extract_session_activity_keywords(ctx: &SessionContext) -> HashSet<String> {
    let mut keywords = HashSet::new();

    let tokenize = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| w.len() >= 4 && !w.chars().all(|c| c.is_numeric()))
            .map(|w| w.to_lowercase())
            .collect()
    };

    for pattern in &ctx.grep_patterns {
        keywords.extend(tokenize(pattern));
    }
    for cmd in &ctx.bash_commands {
        keywords.extend(tokenize(cmd));
    }

    keywords
}

/// Extract tag keywords from a learning (lowercase, 4+ chars).
fn extract_learning_tag_keywords(learning: &CompoundLearning) -> HashSet<String> {
    let mut keywords = HashSet::new();
    for tag in &learning.tags {
        for word in tag.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
            if word.len() >= 4 && !word.chars().all(|c| c.is_numeric()) {
                keywords.insert(word.to_lowercase());
            }
        }
    }
    keywords
}

/// Compute a strict multi-signal relevance score for a learning against session context.
///
/// Unlike `compute_context_relevance` which uses a single bag-of-words overlap,
/// this scorer separates three independent signals with different weights:
///
/// - **File path overlap** (weight 0.3): Learning's mentioned/context files vs
///   session's touched files. Uses path segments, not full words.
/// - **Topic keyword overlap** (weight 0.4): Learning's SUMMARY keywords (not
///   detail) vs session's grep patterns and bash commands. This tests whether
///   the learning's core topic matches what the session was actively working on.
/// - **Tag overlap** (weight 0.3): Learning's tags vs the full session context
///   keywords. Tags are curated metadata and should match activity signals.
///
/// Each signal is computed as: (intersection size) / (union size) (Jaccard index).
/// Using Jaccard instead of overlap ratio penalizes broad keyword sets more
/// heavily, making it harder for "matches everything" keywords to score high.
///
/// Returns the weighted composite score. A match is considered a true positive
/// only if the composite score exceeds the threshold (default 0.15).
fn compute_strict_relevance(
    learning: &CompoundLearning,
    ctx: &SessionContext,
) -> StrictRelevanceScore {
    // Signal 1: File path overlap (Jaccard)
    let learning_files = extract_learning_file_signals(learning);
    let session_files = extract_session_file_signals(ctx);
    let file_score = if learning_files.is_empty() && session_files.is_empty() {
        0.0
    } else {
        let intersection = learning_files.intersection(&session_files).count();
        let union = learning_files.union(&session_files).count();
        if union == 0 {
            0.0
        } else {
            intersection as f64 / union as f64
        }
    };

    // Signal 2: Topic keyword overlap (summary-only vs activity)
    let learning_topics = extract_learning_summary_keywords(learning);
    let session_activity = extract_session_activity_keywords(ctx);
    let topic_score = if learning_topics.is_empty() && session_activity.is_empty() {
        0.0
    } else {
        let intersection = learning_topics.intersection(&session_activity).count();
        let union = learning_topics.union(&session_activity).count();
        if union == 0 {
            0.0
        } else {
            intersection as f64 / union as f64
        }
    };

    // Signal 3: Tag overlap (tags vs full session context)
    let learning_tags = extract_learning_tag_keywords(learning);
    let session_all = extract_session_context_keywords(ctx);
    let tag_score = if learning_tags.is_empty() && session_all.is_empty() {
        0.0
    } else {
        let intersection = learning_tags.intersection(&session_all).count();
        let union = learning_tags.union(&session_all).count();
        if union == 0 {
            0.0
        } else {
            intersection as f64 / union as f64
        }
    };

    // Weighted composite
    let composite = file_score * 0.3 + topic_score * 0.4 + tag_score * 0.3;

    StrictRelevanceScore {
        file_score,
        topic_score,
        tag_score,
        composite,
    }
}

/// Detailed relevance score breakdown for the strict scorer.
#[derive(Debug, Clone)]
struct StrictRelevanceScore {
    /// File path overlap signal (Jaccard index).
    file_score: f64,
    /// Topic keyword overlap signal (summary-only vs grep/bash).
    topic_score: f64,
    /// Tag overlap signal (tags vs full session context).
    tag_score: f64,
    /// Weighted composite score.
    composite: f64,
}

/// Default threshold for strict TP classification.
///
/// With Jaccard similarity on large keyword sets, scores are naturally low
/// (typically 0.00-0.07). A threshold of 0.01 separates matches with
/// any measurable multi-signal overlap from zero/near-zero overlap matches.
const STRICT_TP_THRESHOLD: f64 = 0.01;

/// Truncate a string to at most `max_bytes` bytes, ensuring we don't split
/// a multi-byte UTF-8 character. Returns a string slice that is always valid.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk backward from max_bytes to find a char boundary
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// =============================================================================
// Tests
// =============================================================================

#[test]
#[ignore]
fn replay_full_comparison() {
    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() {
        eprintln!(
            "SKIPPING: transcript directory does not exist: {}",
            transcript_dir_str
        );
        return;
    }
    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    let tool_calls = parse_transcripts(transcript_dir);
    eprintln!("Parsed {} sessions with first tool calls", tool_calls.len());
    assert!(
        !tool_calls.is_empty(),
        "Expected at least one transcript with a tool call"
    );

    let learnings = load_learnings(learnings_path);
    eprintln!("Loaded {} learnings", learnings.len());
    assert!(
        !learnings.is_empty(),
        "Expected at least one learning in the corpus"
    );

    let results = run_replay(&tool_calls, &learnings);
    print_report(&results, learnings.len());

    // Basic sanity assertions (not correctness assertions -- this is exploratory)
    assert_eq!(
        results.len(),
        tool_calls.len(),
        "Should have one result per tool call"
    );
}

#[test]
#[ignore]
fn replay_keyword_reduction_summary() {
    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() || !learnings_path.exists() {
        eprintln!("SKIPPING: required data files not found");
        return;
    }

    let tool_calls = parse_transcripts(transcript_dir);
    let learnings = load_learnings(learnings_path);
    let results = run_replay(&tool_calls, &learnings);

    // Count sessions where v2 produces strictly fewer keywords
    let fewer_keywords = results
        .iter()
        .filter(|r| r.v2_keywords.len() < r.v1_keywords.len())
        .count();
    let same_keywords = results
        .iter()
        .filter(|r| r.v2_keywords.len() == r.v1_keywords.len())
        .count();
    let more_keywords = results
        .iter()
        .filter(|r| r.v2_keywords.len() > r.v1_keywords.len())
        .count();

    eprintln!(
        "\nKeyword count comparison across {} sessions:",
        results.len()
    );
    eprintln!(
        "  v2 fewer keywords: {} sessions ({:.0}%)",
        fewer_keywords,
        fewer_keywords as f64 / results.len() as f64 * 100.0
    );
    eprintln!(
        "  v2 same keywords:  {} sessions ({:.0}%)",
        same_keywords,
        same_keywords as f64 / results.len() as f64 * 100.0
    );
    eprintln!(
        "  v2 more keywords:  {} sessions ({:.0}%)",
        more_keywords,
        more_keywords as f64 / results.len() as f64 * 100.0
    );

    // Count sessions where v2 matches fewer learnings (desired)
    let fewer_matches = results
        .iter()
        .filter(|r| r.v2_matched_learnings.len() < r.v1_matched_learnings.len())
        .count();
    let same_matches = results
        .iter()
        .filter(|r| r.v2_matched_learnings.len() == r.v1_matched_learnings.len())
        .count();
    let more_matches = results
        .iter()
        .filter(|r| r.v2_matched_learnings.len() > r.v1_matched_learnings.len())
        .count();

    eprintln!(
        "\nLearning match comparison across {} sessions:",
        results.len()
    );
    eprintln!(
        "  v2 fewer matches: {} sessions ({:.0}%)",
        fewer_matches,
        fewer_matches as f64 / results.len() as f64 * 100.0
    );
    eprintln!(
        "  v2 same matches:  {} sessions ({:.0}%)",
        same_matches,
        same_matches as f64 / results.len() as f64 * 100.0
    );
    eprintln!(
        "  v2 more matches:  {} sessions ({:.0}%)",
        more_matches,
        more_matches as f64 / results.len() as f64 * 100.0
    );
}

#[test]
#[ignore]
fn replay_lost_learnings_detail() {
    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() || !learnings_path.exists() {
        eprintln!("SKIPPING: required data files not found");
        return;
    }

    let tool_calls = parse_transcripts(transcript_dir);
    let learnings = load_learnings(learnings_path);
    let results = run_replay(&tool_calls, &learnings);

    // Build a map of learning ID -> summary for lookup
    let learning_map: BTreeMap<String, String> = learnings
        .iter()
        .map(|l| (l.id.clone(), l.summary.clone()))
        .collect();

    // Identify all learnings surfaced by v1 but not v2 across ALL sessions
    let mut v1_only_all: BTreeMap<String, Vec<(String, Vec<String>)>> = BTreeMap::new();
    // session -> (keywords that caused the match)

    for result in &results {
        let v2_ids: HashSet<String> = result
            .v2_matched_learnings
            .iter()
            .map(|m| m.id.clone())
            .collect();

        for m in &result.v1_matched_learnings {
            if !v2_ids.contains(&m.id) {
                v1_only_all
                    .entry(m.id.clone())
                    .or_default()
                    .push((result.session_file.clone(), m.matched_keywords.clone()));
            }
        }
    }

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("LEARNINGS LOST BY v2 -- DETAILED ANALYSIS");
    eprintln!("{}", "=".repeat(80));
    eprintln!(
        "\n{} unique learnings surfaced by v1 but NOT v2:\n",
        v1_only_all.len()
    );

    for (id, occurrences) in &v1_only_all {
        let summary = learning_map
            .get(id)
            .cloned()
            .unwrap_or_else(|| "(unknown)".to_string());
        eprintln!("  {} -- {}", id, summary);
        for (session, keywords) in occurrences {
            let session_short = &session[..8.min(session.len())];
            eprintln!(
                "    Session {}: matched via [{}]",
                session_short,
                keywords.join(", ")
            );
        }
        eprintln!();
    }
}

/// Independent keyword classification using session context as ground truth.
///
/// This test breaks the circularity in the original experiments where the same
/// entity designed the noise list and classified test keywords. Instead of
/// subjective labeling, it uses **objective session context** (all file paths
/// touched, grep patterns, bash commands) to judge whether a matched learning
/// is relevant to what the session actually worked on.
///
/// Methodology:
/// 1. Parse ALL tool calls from each session (not just the first one)
/// 2. Build a "session context" keyword set from file paths, grep patterns,
///    and bash commands across all tool calls
/// 3. Run v1 and v2 keyword extraction on the first tool call
/// 4. Match keywords against all 38 learnings
/// 5. For each matched learning, compute relevance = (shared keywords between
///    learning text and session context) / (total learning keywords)
/// 6. Compare average relevance of v1-matched vs v2-matched learnings
///
/// This is independent because the relevance scoring uses session activity
/// data, not the keyword extraction pipeline being evaluated.
#[test]
#[ignore]
fn replay_independent_classification() {
    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() {
        eprintln!(
            "SKIPPING: transcript directory does not exist: {}",
            transcript_dir_str
        );
        return;
    }
    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    // --- 1. Load data ---

    let learnings = load_learnings(learnings_path);
    eprintln!("Loaded {} learnings", learnings.len());
    if learnings.is_empty() {
        eprintln!("SKIPPING: no learnings loaded");
        return;
    }

    let contexts = build_session_contexts(transcript_dir);
    eprintln!("Built session contexts for {} sessions", contexts.len());
    if contexts.is_empty() {
        eprintln!("SKIPPING: no session contexts built");
        return;
    }

    // Build a learning ID -> CompoundLearning map for quick lookup
    let learning_map: BTreeMap<String, &CompoundLearning> =
        learnings.iter().map(|l| (l.id.clone(), l)).collect();

    // --- 2. Per-session independent classification ---

    let mut all_results: Vec<IndependentClassResult> = Vec::new();

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("INDEPENDENT CLASSIFICATION -- SESSION CONTEXT RELEVANCE");
    eprintln!("{}", "=".repeat(80));

    for ctx in &contexts {
        // Extract session context keywords (independent signal)
        let context_keywords = extract_session_context_keywords(ctx);
        if context_keywords.is_empty() {
            continue;
        }

        // Get the first tool call for v1/v2 keyword extraction
        let first_tc = match ctx.all_tool_calls.first() {
            Some(tc) => tc,
            None => continue,
        };

        // Run both extractors on the first tool call
        let v1_keywords = extract_tool_input_keywords(&first_tc.tool_name, &first_tc.tool_input);
        let v2_keywords = extract_tool_input_keywords_v2(&first_tc.tool_name, &first_tc.tool_input);

        // Match keywords against learnings
        let v1_matched = match_keywords_to_learnings(&v1_keywords, &learnings);
        let v2_matched = match_keywords_to_learnings(&v2_keywords, &learnings);

        // Score each matched learning using session context relevance
        let v1_scored: Vec<(String, f64)> = v1_matched
            .iter()
            .map(|m| {
                let learning = learning_map.get(&m.id).unwrap();
                let relevance = compute_context_relevance(learning, &context_keywords);
                (m.id.clone(), relevance)
            })
            .collect();

        let v2_scored: Vec<(String, f64)> = v2_matched
            .iter()
            .map(|m| {
                let learning = learning_map.get(&m.id).unwrap();
                let relevance = compute_context_relevance(learning, &context_keywords);
                (m.id.clone(), relevance)
            })
            .collect();

        // Compute set differences
        let v1_ids: HashSet<String> = v1_scored.iter().map(|(id, _)| id.clone()).collect();
        let v2_ids: HashSet<String> = v2_scored.iter().map(|(id, _)| id.clone()).collect();

        let v1_only: Vec<(String, f64)> = v1_scored
            .iter()
            .filter(|(id, _)| !v2_ids.contains(id))
            .cloned()
            .collect();
        let v2_only: Vec<(String, f64)> = v2_scored
            .iter()
            .filter(|(id, _)| !v1_ids.contains(id))
            .cloned()
            .collect();

        // Compute averages
        let v1_avg = if v1_scored.is_empty() {
            0.0
        } else {
            v1_scored.iter().map(|(_, r)| r).sum::<f64>() / v1_scored.len() as f64
        };
        let v2_avg = if v2_scored.is_empty() {
            0.0
        } else {
            v2_scored.iter().map(|(_, r)| r).sum::<f64>() / v2_scored.len() as f64
        };

        let result = IndependentClassResult {
            session_file: ctx.session_file.clone(),
            tool_count: ctx.all_tool_calls.len(),
            context_keyword_count: context_keywords.len(),
            v1_matched: v1_scored,
            v2_matched: v2_scored,
            v1_only,
            v2_only,
            v1_avg_relevance: v1_avg,
            v2_avg_relevance: v2_avg,
        };

        // Print per-session detail
        let session_short = &result.session_file[..8.min(result.session_file.len())];
        eprintln!(
            "\n  Session {} | {} tool calls | {} context keywords",
            session_short, result.tool_count, result.context_keyword_count
        );
        eprintln!(
            "    First tool: {} | v1 kws: {:?} | v2 kws: {:?}",
            first_tc.tool_name, v1_keywords, v2_keywords
        );
        eprintln!(
            "    v1: {} matched (avg relevance {:.3}) | v2: {} matched (avg relevance {:.3})",
            result.v1_matched.len(),
            result.v1_avg_relevance,
            result.v2_matched.len(),
            result.v2_avg_relevance
        );

        if !result.v1_only.is_empty() {
            eprintln!("    v1-only learnings (lost by v2):");
            for (id, rel) in &result.v1_only {
                let summary = learning_map
                    .get(id)
                    .map(|l| l.summary.as_str())
                    .unwrap_or("(unknown)");
                eprintln!("      - {} (relevance {:.3}) | {}", id, rel, summary);
            }
        }
        if !result.v2_only.is_empty() {
            eprintln!("    v2-only learnings (gained by v2):");
            for (id, rel) in &result.v2_only {
                let summary = learning_map
                    .get(id)
                    .map(|l| l.summary.as_str())
                    .unwrap_or("(unknown)");
                eprintln!("      - {} (relevance {:.3}) | {}", id, rel, summary);
            }
        }

        all_results.push(result);
    }

    // --- 3. Aggregate metrics ---

    let sessions_with_matches = all_results
        .iter()
        .filter(|r| !r.v1_matched.is_empty() || !r.v2_matched.is_empty())
        .count();

    // Average relevance across all sessions (weighted by number of matches)
    let total_v1_relevance: f64 = all_results
        .iter()
        .flat_map(|r| r.v1_matched.iter().map(|(_, rel)| *rel))
        .sum();
    let total_v1_count: usize = all_results.iter().map(|r| r.v1_matched.len()).sum();

    let total_v2_relevance: f64 = all_results
        .iter()
        .flat_map(|r| r.v2_matched.iter().map(|(_, rel)| *rel))
        .sum();
    let total_v2_count: usize = all_results.iter().map(|r| r.v2_matched.len()).sum();

    let v1_global_avg = if total_v1_count > 0 {
        total_v1_relevance / total_v1_count as f64
    } else {
        0.0
    };
    let v2_global_avg = if total_v2_count > 0 {
        total_v2_relevance / total_v2_count as f64
    } else {
        0.0
    };

    // v2-only and v1-only learnings across all sessions
    let total_v2_only_relevance: f64 = all_results
        .iter()
        .flat_map(|r| r.v2_only.iter().map(|(_, rel)| *rel))
        .sum();
    let total_v2_only_count: usize = all_results.iter().map(|r| r.v2_only.len()).sum();

    let total_v1_only_relevance: f64 = all_results
        .iter()
        .flat_map(|r| r.v1_only.iter().map(|(_, rel)| *rel))
        .sum();
    let total_v1_only_count: usize = all_results.iter().map(|r| r.v1_only.len()).sum();

    let v2_only_avg = if total_v2_only_count > 0 {
        total_v2_only_relevance / total_v2_only_count as f64
    } else {
        0.0
    };
    let v1_only_avg = if total_v1_only_count > 0 {
        total_v1_only_relevance / total_v1_only_count as f64
    } else {
        0.0
    };

    // Sessions where v2 has higher average relevance
    let v2_higher_relevance = all_results
        .iter()
        .filter(|r| {
            !r.v1_matched.is_empty()
                && !r.v2_matched.is_empty()
                && r.v2_avg_relevance > r.v1_avg_relevance
        })
        .count();
    let v1_higher_relevance = all_results
        .iter()
        .filter(|r| {
            !r.v1_matched.is_empty()
                && !r.v2_matched.is_empty()
                && r.v1_avg_relevance > r.v2_avg_relevance
        })
        .count();
    let equal_relevance = all_results
        .iter()
        .filter(|r| {
            !r.v1_matched.is_empty()
                && !r.v2_matched.is_empty()
                && (r.v1_avg_relevance - r.v2_avg_relevance).abs() < f64::EPSILON
        })
        .count();

    // --- 4. Print aggregate report ---

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("AGGREGATE INDEPENDENT CLASSIFICATION RESULTS");
    eprintln!("{}", "=".repeat(80));

    eprintln!("\n  Dataset:");
    eprintln!("    Sessions analyzed:        {}", all_results.len());
    eprintln!("    Sessions with matches:    {}", sessions_with_matches);
    eprintln!("    Total learnings:          {}", learnings.len());

    eprintln!("\n  Match counts:");
    eprintln!(
        "    v1 total matches:         {} across all sessions",
        total_v1_count
    );
    eprintln!(
        "    v2 total matches:         {} across all sessions",
        total_v2_count
    );
    eprintln!(
        "    v2-only (gained):         {} matches",
        total_v2_only_count
    );
    eprintln!(
        "    v1-only (lost):           {} matches",
        total_v1_only_count
    );

    eprintln!("\n  Average context relevance (higher = more relevant to session):");
    eprintln!("    v1-matched learnings:     {:.4}", v1_global_avg);
    eprintln!("    v2-matched learnings:     {:.4}", v2_global_avg);
    let relevance_delta = v2_global_avg - v1_global_avg;
    eprintln!(
        "    Delta (v2 - v1):          {:+.4} ({})",
        relevance_delta,
        if relevance_delta > 0.0 {
            "v2 matches more relevant learnings"
        } else if relevance_delta < 0.0 {
            "v1 matches more relevant learnings"
        } else {
            "equal"
        }
    );

    eprintln!("\n  Gained/lost learning relevance:");
    eprintln!(
        "    v2-only avg relevance:    {:.4} ({} learnings gained)",
        v2_only_avg, total_v2_only_count
    );
    eprintln!(
        "    v1-only avg relevance:    {:.4} ({} learnings lost)",
        v1_only_avg, total_v1_only_count
    );
    if total_v2_only_count > 0 && total_v1_only_count > 0 {
        let gain_loss_delta = v2_only_avg - v1_only_avg;
        eprintln!(
            "    Gain-loss delta:          {:+.4} ({})",
            gain_loss_delta,
            if gain_loss_delta > 0.0 {
                "gained learnings are MORE relevant than lost ones"
            } else if gain_loss_delta < 0.0 {
                "lost learnings are MORE relevant than gained ones"
            } else {
                "equal"
            }
        );
    }

    eprintln!("\n  Per-session relevance comparison:");
    eprintln!(
        "    v2 higher avg relevance:  {} sessions",
        v2_higher_relevance
    );
    eprintln!(
        "    v1 higher avg relevance:  {} sessions",
        v1_higher_relevance
    );
    eprintln!("    Equal avg relevance:      {} sessions", equal_relevance);

    // --- 5. Detailed v2-only and v1-only learning breakdown ---

    // Collect all unique v2-only and v1-only learning IDs with their relevance scores
    let mut v2_only_all: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut v1_only_all: BTreeMap<String, Vec<f64>> = BTreeMap::new();

    for result in &all_results {
        for (id, rel) in &result.v2_only {
            v2_only_all.entry(id.clone()).or_default().push(*rel);
        }
        for (id, rel) in &result.v1_only {
            v1_only_all.entry(id.clone()).or_default().push(*rel);
        }
    }

    if !v2_only_all.is_empty() {
        eprintln!(
            "\n  v2-ONLY learnings (gained) -- {} unique:",
            v2_only_all.len()
        );
        for (id, scores) in &v2_only_all {
            let avg: f64 = scores.iter().sum::<f64>() / scores.len() as f64;
            let summary = learning_map
                .get(id)
                .map(|l| l.summary.as_str())
                .unwrap_or("(unknown)");
            eprintln!(
                "    {} | avg relevance {:.3} ({} sessions) | {}",
                id,
                avg,
                scores.len(),
                summary
            );
        }
    }

    if !v1_only_all.is_empty() {
        eprintln!(
            "\n  v1-ONLY learnings (lost) -- {} unique:",
            v1_only_all.len()
        );
        for (id, scores) in &v1_only_all {
            let avg: f64 = scores.iter().sum::<f64>() / scores.len() as f64;
            let summary = learning_map
                .get(id)
                .map(|l| l.summary.as_str())
                .unwrap_or("(unknown)");
            eprintln!(
                "    {} | avg relevance {:.3} ({} sessions) | {}",
                id,
                avg,
                scores.len(),
                summary
            );
        }
    }

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("INTERPRETATION");
    eprintln!("{}", "=".repeat(80));
    eprintln!("\n  This test uses OBJECTIVE session context to judge relevance,");
    eprintln!("  breaking the circularity where the noise list designer also");
    eprintln!("  classified keywords. Relevance is measured by keyword overlap");
    eprintln!("  between learning text and the full session activity (all file");
    eprintln!("  paths, grep patterns, and bash commands).");
    eprintln!("\n  Key question: Does v2 surface MORE relevant learnings than v1?");
    eprintln!(
        "  Answer: v2 avg relevance = {:.4}, v1 avg relevance = {:.4}, delta = {:+.4}",
        v2_global_avg, v1_global_avg, relevance_delta
    );

    eprintln!("\n{}", "=".repeat(80));
    eprintln!(
        "To reproduce: cargo test -- --ignored replay_independent_classification --nocapture"
    );
    eprintln!("{}", "=".repeat(80));
}

/// Reproduce the headline metrics (15% TP / 60% FP) from the keyword extraction
/// audit using automated transcript parsing and a file-overlap heuristic.
///
/// For each session:
/// 1. Extract first tool call and run v1 keyword extraction
/// 2. Match keywords against 38 learnings
/// 3. For each match, classify as TP or FP using file-overlap heuristic:
///    - Extract ALL file paths from ALL tool calls in the session
///    - Tokenize those paths into context keywords
///    - A match is TP if the learning's keywords overlap with session file-path keywords
///    - A match is FP otherwise
#[test]
#[ignore]
fn replay_reproduce_headline_metrics() {
    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() {
        eprintln!(
            "SKIPPING: transcript directory does not exist: {}",
            transcript_dir_str
        );
        return;
    }
    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    // Load data
    let first_tool_calls = parse_transcripts(transcript_dir);
    let contexts = build_session_contexts(transcript_dir);
    let learnings = load_learnings(learnings_path);

    eprintln!(
        "Parsed {} sessions with first tool calls",
        first_tool_calls.len()
    );
    eprintln!("Built {} session contexts", contexts.len());
    eprintln!("Loaded {} learnings", learnings.len());

    if first_tool_calls.is_empty() || learnings.is_empty() {
        eprintln!("SKIPPING: insufficient data");
        return;
    }

    // Build context lookup: session_file -> context keywords from file paths
    let context_map: BTreeMap<String, HashSet<String>> = contexts
        .iter()
        .map(|ctx| {
            let keywords = extract_session_context_keywords(ctx);
            (ctx.session_file.clone(), keywords)
        })
        .collect();

    // Per-session TP/FP classification
    let mut total_tp = 0usize;
    let mut total_fp = 0usize;
    let mut total_matches = 0usize;
    let mut sessions_with_matches = 0usize;
    let mut all_matched_learning_ids: HashSet<String> = HashSet::new();

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("HEADLINE METRICS REPRODUCTION -- v1 KEYWORD EXTRACTION");
    eprintln!("{}", "=".repeat(80));

    for tc in &first_tool_calls {
        let v1_keywords = extract_tool_input_keywords(&tc.tool_name, &tc.tool_input);
        let matched = match_keywords_to_learnings(&v1_keywords, &learnings);

        if matched.is_empty() {
            continue;
        }

        sessions_with_matches += 1;

        // Get session context keywords (from all file paths in the session)
        let context_keywords = context_map.get(&tc.session_file);

        let mut session_tp = 0usize;
        let mut session_fp = 0usize;

        for m in &matched {
            all_matched_learning_ids.insert(m.id.clone());

            // Classify using file-overlap heuristic
            let is_tp = if let Some(ctx_kws) = context_keywords {
                // Extract keywords from learning text
                let learning = learnings.iter().find(|l| l.id == m.id);
                if let Some(learning) = learning {
                    let learning_kws = extract_learning_keywords(learning);
                    // TP if any learning keyword overlaps with session context
                    learning_kws.intersection(ctx_kws).next().is_some()
                } else {
                    false
                }
            } else {
                false
            };

            if is_tp {
                session_tp += 1;
            } else {
                session_fp += 1;
            }
        }

        total_tp += session_tp;
        total_fp += session_fp;
        total_matches += matched.len();

        let session_short = &tc.session_file[..8.min(tc.session_file.len())];
        eprintln!(
            "  {} | {} {} | {} matches (TP={}, FP={}) | keywords: {:?}",
            session_short,
            tc.tool_name,
            if v1_keywords.is_empty() {
                "(no keywords)"
            } else {
                ""
            },
            matched.len(),
            session_tp,
            session_fp,
            v1_keywords
        );
    }

    // Compute rates
    let tp_rate = if total_matches > 0 {
        total_tp as f64 / total_matches as f64 * 100.0
    } else {
        0.0
    };
    let fp_rate = if total_matches > 0 {
        total_fp as f64 / total_matches as f64 * 100.0
    } else {
        0.0
    };
    let never_matched = learnings.len() - all_matched_learning_ids.len();

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("AGGREGATE RESULTS");
    eprintln!("{}", "=".repeat(80));

    eprintln!("\n  Dataset:");
    eprintln!(
        "    Sessions analyzed:            {}",
        first_tool_calls.len()
    );
    eprintln!(
        "    Sessions with matches:        {}",
        sessions_with_matches
    );
    eprintln!("    Total learnings:              {}", learnings.len());

    eprintln!("\n  Metrics:");
    eprintln!("    Total matches:                {}", total_matches);
    eprintln!("    True positives:               {}", total_tp);
    eprintln!("    False positives:              {}", total_fp);
    eprintln!("    True positive rate:           {:.1}%", tp_rate);
    eprintln!("    False positive rate:          {:.1}%", fp_rate);

    eprintln!("\n  Recall:");
    eprintln!(
        "    Learnings matched:            {}/{}",
        all_matched_learning_ids.len(),
        learnings.len()
    );
    eprintln!(
        "    Learnings never matched:      {}/{} ({:.1}%)",
        never_matched,
        learnings.len(),
        never_matched as f64 / learnings.len() as f64 * 100.0
    );

    eprintln!("\n  Comparison with original claims:");
    eprintln!("    Claimed TP rate:              15%");
    eprintln!("    Computed TP rate:             {:.1}%", tp_rate);
    eprintln!("    Claimed FP rate:              60%");
    eprintln!("    Computed FP rate:             {:.1}%", fp_rate);
    eprintln!("    Claimed never-matched:        23/38 (60.5%)");
    eprintln!(
        "    Computed never-matched:       {}/{} ({:.1}%)",
        never_matched,
        learnings.len(),
        never_matched as f64 / learnings.len() as f64 * 100.0
    );

    eprintln!("\n{}", "=".repeat(80));
    eprintln!(
        "To reproduce: cargo test -- --ignored replay_reproduce_headline_metrics --nocapture"
    );
    eprintln!("{}", "=".repeat(80));
}

/// Strict headline metrics using the multi-signal relevance scorer.
///
/// This test replaces the overly generous file-overlap heuristic (which
/// produced 98.1% TP by classifying almost everything as true positive)
/// with a weighted multi-signal scorer that requires stronger evidence.
///
/// Instead of ANY keyword overlap between learning text and session context,
/// the strict scorer requires evidence from three independent signals:
///
/// 1. **File path overlap** (weight 0.3): Learning's mentioned files vs
///    session's touched files (Jaccard index on path segments).
/// 2. **Topic keyword overlap** (weight 0.4): Learning's SUMMARY keywords
///    vs session's grep patterns and bash commands (Jaccard index).
/// 3. **Tag overlap** (weight 0.3): Learning's tags vs full session context
///    keywords (Jaccard index).
///
/// A match is TP only if the composite score exceeds the threshold (0.15).
///
/// Key design differences from the broad heuristic:
/// - Uses Jaccard index (intersection/union) instead of overlap ratio
///   (intersection/learning_size), which penalizes broad keyword sets
/// - Limits topic matching to SUMMARY only (not detail text), so incidental
///   words in explanatory paragraphs cannot inflate relevance
/// - Separates signals: file paths, activity patterns, and tags must each
///   contribute independently
/// - Requires a composite threshold, so a single weak signal is insufficient
#[test]
#[ignore]
fn replay_strict_headline_metrics() {
    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() {
        eprintln!(
            "SKIPPING: transcript directory does not exist: {}",
            transcript_dir_str
        );
        return;
    }
    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    // Load data
    let first_tool_calls = parse_transcripts(transcript_dir);
    let contexts = build_session_contexts(transcript_dir);
    let learnings = load_learnings(learnings_path);

    eprintln!(
        "Parsed {} sessions with first tool calls",
        first_tool_calls.len()
    );
    eprintln!("Built {} session contexts", contexts.len());
    eprintln!("Loaded {} learnings", learnings.len());

    if first_tool_calls.is_empty() || learnings.is_empty() || contexts.is_empty() {
        eprintln!("SKIPPING: insufficient data");
        return;
    }

    // Build context lookup: session_file -> SessionContext
    let context_map: BTreeMap<String, &SessionContext> = contexts
        .iter()
        .map(|ctx| (ctx.session_file.clone(), ctx))
        .collect();

    // Build learning lookup
    let learning_map: BTreeMap<String, &CompoundLearning> =
        learnings.iter().map(|l| (l.id.clone(), l)).collect();

    // --- Run v1 and v2 through the strict classifier ---

    struct VersionMetrics {
        total_tp: usize,
        total_fp: usize,
        total_matches: usize,
        sessions_with_matches: usize,
        matched_learning_ids: HashSet<String>,
        tp_scores: Vec<f64>,
        fp_scores: Vec<f64>,
    }

    let mut v1_metrics = VersionMetrics {
        total_tp: 0,
        total_fp: 0,
        total_matches: 0,
        sessions_with_matches: 0,
        matched_learning_ids: HashSet::new(),
        tp_scores: Vec::new(),
        fp_scores: Vec::new(),
    };

    let mut v2_metrics = VersionMetrics {
        total_tp: 0,
        total_fp: 0,
        total_matches: 0,
        sessions_with_matches: 0,
        matched_learning_ids: HashSet::new(),
        tp_scores: Vec::new(),
        fp_scores: Vec::new(),
    };

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("STRICT HEADLINE METRICS -- MULTI-SIGNAL RELEVANCE SCORER");
    eprintln!("Threshold: composite >= {:.2}", STRICT_TP_THRESHOLD);
    eprintln!("Weights: file=0.3, topic=0.4, tag=0.3");
    eprintln!("Similarity: Jaccard index (intersection/union)");
    eprintln!("{}", "=".repeat(80));

    for tc in &first_tool_calls {
        let ctx = match context_map.get(&tc.session_file) {
            Some(ctx) => ctx,
            None => continue,
        };

        let v1_keywords = extract_tool_input_keywords(&tc.tool_name, &tc.tool_input);
        let v2_keywords = extract_tool_input_keywords_v2(&tc.tool_name, &tc.tool_input);

        let v1_matched = match_keywords_to_learnings(&v1_keywords, &learnings);
        let v2_matched = match_keywords_to_learnings(&v2_keywords, &learnings);

        let session_short = &tc.session_file[..8.min(tc.session_file.len())];

        // Classify v1 matches
        if !v1_matched.is_empty() {
            v1_metrics.sessions_with_matches += 1;
            eprintln!(
                "\n  Session {} | Tool: {} | v1 keywords: {:?}",
                session_short, tc.tool_name, v1_keywords
            );

            for m in &v1_matched {
                v1_metrics.matched_learning_ids.insert(m.id.clone());
                v1_metrics.total_matches += 1;

                if let Some(learning) = learning_map.get(&m.id) {
                    let score = compute_strict_relevance(learning, ctx);
                    let is_tp = score.composite >= STRICT_TP_THRESHOLD;

                    if is_tp {
                        v1_metrics.total_tp += 1;
                        v1_metrics.tp_scores.push(score.composite);
                    } else {
                        v1_metrics.total_fp += 1;
                        v1_metrics.fp_scores.push(score.composite);
                    }

                    eprintln!(
                        "    v1 {} {} | composite={:.3} (file={:.3} topic={:.3} tag={:.3}) | {}",
                        if is_tp { "TP" } else { "FP" },
                        m.id,
                        score.composite,
                        score.file_score,
                        score.topic_score,
                        score.tag_score,
                        truncate_str(&m.summary, 50)
                    );
                }
            }
        }

        // Classify v2 matches
        if !v2_matched.is_empty() {
            v2_metrics.sessions_with_matches += 1;
            if v1_matched.is_empty() {
                eprintln!(
                    "\n  Session {} | Tool: {} | v2 keywords: {:?}",
                    session_short, tc.tool_name, v2_keywords
                );
            }

            for m in &v2_matched {
                v2_metrics.matched_learning_ids.insert(m.id.clone());
                v2_metrics.total_matches += 1;

                if let Some(learning) = learning_map.get(&m.id) {
                    let score = compute_strict_relevance(learning, ctx);
                    let is_tp = score.composite >= STRICT_TP_THRESHOLD;

                    if is_tp {
                        v2_metrics.total_tp += 1;
                        v2_metrics.tp_scores.push(score.composite);
                    } else {
                        v2_metrics.total_fp += 1;
                        v2_metrics.fp_scores.push(score.composite);
                    }

                    eprintln!(
                        "    v2 {} {} | composite={:.3} (file={:.3} topic={:.3} tag={:.3}) | {}",
                        if is_tp { "TP" } else { "FP" },
                        m.id,
                        score.composite,
                        score.file_score,
                        score.topic_score,
                        score.tag_score,
                        truncate_str(&m.summary, 50)
                    );
                }
            }
        }
    }

    // --- Aggregate report ---

    let v1_tp_rate = if v1_metrics.total_matches > 0 {
        v1_metrics.total_tp as f64 / v1_metrics.total_matches as f64 * 100.0
    } else {
        0.0
    };
    let v1_fp_rate = if v1_metrics.total_matches > 0 {
        v1_metrics.total_fp as f64 / v1_metrics.total_matches as f64 * 100.0
    } else {
        0.0
    };
    let v2_tp_rate = if v2_metrics.total_matches > 0 {
        v2_metrics.total_tp as f64 / v2_metrics.total_matches as f64 * 100.0
    } else {
        0.0
    };
    let v2_fp_rate = if v2_metrics.total_matches > 0 {
        v2_metrics.total_fp as f64 / v2_metrics.total_matches as f64 * 100.0
    } else {
        0.0
    };

    let v1_never_matched = learnings.len() - v1_metrics.matched_learning_ids.len();
    let v2_never_matched = learnings.len() - v2_metrics.matched_learning_ids.len();

    let v1_avg_tp_score = if v1_metrics.tp_scores.is_empty() {
        0.0
    } else {
        v1_metrics.tp_scores.iter().sum::<f64>() / v1_metrics.tp_scores.len() as f64
    };
    let v1_avg_fp_score = if v1_metrics.fp_scores.is_empty() {
        0.0
    } else {
        v1_metrics.fp_scores.iter().sum::<f64>() / v1_metrics.fp_scores.len() as f64
    };
    let v2_avg_tp_score = if v2_metrics.tp_scores.is_empty() {
        0.0
    } else {
        v2_metrics.tp_scores.iter().sum::<f64>() / v2_metrics.tp_scores.len() as f64
    };
    let v2_avg_fp_score = if v2_metrics.fp_scores.is_empty() {
        0.0
    } else {
        v2_metrics.fp_scores.iter().sum::<f64>() / v2_metrics.fp_scores.len() as f64
    };

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("STRICT AGGREGATE RESULTS");
    eprintln!("{}", "=".repeat(80));

    eprintln!("\n  Dataset:");
    eprintln!(
        "    Sessions analyzed:            {}",
        first_tool_calls.len()
    );
    eprintln!("    Total learnings:              {}", learnings.len());

    eprintln!("\n  v1 Metrics:");
    eprintln!(
        "    Sessions with matches:        {}",
        v1_metrics.sessions_with_matches
    );
    eprintln!(
        "    Total matches:                {}",
        v1_metrics.total_matches
    );
    eprintln!("    True positives:               {}", v1_metrics.total_tp);
    eprintln!("    False positives:              {}", v1_metrics.total_fp);
    eprintln!("    True positive rate:           {:.1}%", v1_tp_rate);
    eprintln!("    False positive rate:          {:.1}%", v1_fp_rate);
    eprintln!(
        "    Never matched:               {}/{} ({:.1}%)",
        v1_never_matched,
        learnings.len(),
        v1_never_matched as f64 / learnings.len() as f64 * 100.0
    );
    eprintln!("    Avg TP composite score:       {:.4}", v1_avg_tp_score);
    eprintln!("    Avg FP composite score:       {:.4}", v1_avg_fp_score);

    eprintln!("\n  v2 Metrics:");
    eprintln!(
        "    Sessions with matches:        {}",
        v2_metrics.sessions_with_matches
    );
    eprintln!(
        "    Total matches:                {}",
        v2_metrics.total_matches
    );
    eprintln!("    True positives:               {}", v2_metrics.total_tp);
    eprintln!("    False positives:              {}", v2_metrics.total_fp);
    eprintln!("    True positive rate:           {:.1}%", v2_tp_rate);
    eprintln!("    False positive rate:          {:.1}%", v2_fp_rate);
    eprintln!(
        "    Never matched:               {}/{} ({:.1}%)",
        v2_never_matched,
        learnings.len(),
        v2_never_matched as f64 / learnings.len() as f64 * 100.0
    );
    eprintln!("    Avg TP composite score:       {:.4}", v2_avg_tp_score);
    eprintln!("    Avg FP composite score:       {:.4}", v2_avg_fp_score);

    eprintln!("\n  Comparison with broad heuristic (Section 9):");
    eprintln!("    Broad heuristic v1 TP rate:   98.1%");
    eprintln!("    Strict scorer v1 TP rate:     {:.1}%", v1_tp_rate);
    eprintln!("    Strict scorer v2 TP rate:     {:.1}%", v2_tp_rate);

    eprintln!("\n  Score distribution (TP vs FP separation):");
    eprintln!(
        "    v1 TP avg score: {:.4} | FP avg score: {:.4} | gap: {:.4}",
        v1_avg_tp_score,
        v1_avg_fp_score,
        v1_avg_tp_score - v1_avg_fp_score
    );
    eprintln!(
        "    v2 TP avg score: {:.4} | FP avg score: {:.4} | gap: {:.4}",
        v2_avg_tp_score,
        v2_avg_fp_score,
        v2_avg_tp_score - v2_avg_fp_score
    );

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("INTERPRETATION");
    eprintln!("{}", "=".repeat(80));
    eprintln!("\n  The strict multi-signal scorer separates TP/FP classification");
    eprintln!("  into three independent signals (file paths, topic keywords from");
    eprintln!("  summary-only, and tags), each using Jaccard similarity. This is");
    eprintln!("  substantially more discriminating than the broad heuristic which");
    eprintln!("  classified 98.1% as TP via any-keyword overlap.");
    eprintln!("\n  A useful relevance oracle should produce TP rates in the 30-70%");
    eprintln!("  range, indicating it can discriminate between relevant and");
    eprintln!("  irrelevant matches. Rates near 0% or 100% indicate the oracle");
    eprintln!("  itself is not discriminating.");

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("To reproduce: cargo test -- --ignored replay_strict_headline_metrics --nocapture");
    eprintln!("{}", "=".repeat(80));
}

// =============================================================================
// LLM Judge -- calls Anthropic API via curl with prompt caching
// =============================================================================

// Judge infrastructure is now in crate::eval::judge.
// The following functions delegate to the eval module for backward compatibility
// with the benchmark tests below.

/// Default path to the LLM judge cache file.
const DEFAULT_LLM_JUDGE_CACHE_PATH: &str = concat!(
    env!("HOME"),
    "/.claude/projects/-Users-dev-my-project/.grove/judge_cache.json"
);

/// Resolve judge cache path from env var or default.
fn llm_judge_cache_path() -> String {
    std::env::var("GROVE_BENCH_JUDGE_CACHE_PATH")
        .unwrap_or_else(|_| DEFAULT_LLM_JUDGE_CACHE_PATH.to_string())
}

/// Load the judge cache from disk.
fn load_judge_cache_from(cache_path: &str) -> BTreeMap<String, f64> {
    eval_judge::load_judge_cache(Path::new(cache_path))
}

/// Save the judge cache to disk.
fn save_judge_cache_to(cache: &BTreeMap<String, f64>, cache_path: &str) {
    eval_judge::save_judge_cache(cache, Path::new(cache_path));
}

/// Build the composite cache key for a (session, learning) pair.
fn judge_cache_key(session_file: &str, learning_id: &str) -> String {
    eval_judge::judge_cache_key(session_file, learning_id)
}

/// Score a (session, learning) pair using the LLM judge, with caching.
fn llm_judge_relevance(
    session_file: &str,
    learning: &CompoundLearning,
    ctx: &SessionContext,
    cache: &mut BTreeMap<String, f64>,
    judge: &JudgeContext,
) -> Option<f64> {
    eval_judge::judge_relevance(session_file, learning, ctx, cache, judge).map(|r| r.score)
}

/// LLM judge evaluation of the replay harness.
///
/// For each session, this test:
/// 1. Runs v1 and v2 keyword extraction on the first tool call
/// 2. Matches keywords against learnings
/// 3. For each matched learning, gets an LLM judge relevance score (1-5)
/// 4. Compares average scores for v1-only, v2-only, and shared learnings
///
/// The key question: Are v2-gained learnings scored higher by the LLM judge
/// than v1-lost learnings? If so, v2's noise filtering is improving relevance,
/// not just reducing volume.
///
/// Supports two backends configured via `[judge]` in `.grove/config.toml`:
/// - `backend = "cli"` (default): Uses `claude` CLI with Max plan. No API costs.
/// - `backend = "api"`: Uses curl + `ANTHROPIC_API_KEY` with prompt caching.
///
/// Run with:
/// ```bash
/// cargo test -- --ignored replay_llm_judge --nocapture
/// ```
#[test]
#[ignore]
fn replay_llm_judge() {
    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() {
        eprintln!(
            "SKIPPING: transcript directory does not exist: {}",
            transcript_dir_str
        );
        return;
    }
    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    // --- 0. Load config ---

    let config = crate::config::Config::load();
    let backend = &config.judge.backend;
    let model = &config.judge.model;
    let api_url = &config.judge.api_url;
    let cache_path = if config.judge.cache_path.is_empty() {
        llm_judge_cache_path()
    } else {
        config.judge.cache_path.clone()
    };

    // Check for API key only when using the API backend
    if backend == "api"
        && std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .is_none()
    {
        eprintln!("SKIPPING: backend=api but ANTHROPIC_API_KEY not set");
        return;
    }

    eprintln!(
        "Judge config: backend={}, model={}, api={}",
        backend, model, api_url
    );
    eprintln!("Cache path: {}", cache_path);

    // --- 1. Load data ---

    let learnings = load_learnings(learnings_path);
    eprintln!("Loaded {} learnings", learnings.len());
    if learnings.is_empty() {
        eprintln!("SKIPPING: no learnings loaded");
        return;
    }

    let contexts = build_session_contexts(transcript_dir);
    eprintln!("Built session contexts for {} sessions", contexts.len());
    if contexts.is_empty() {
        eprintln!("SKIPPING: no session contexts built");
        return;
    }

    // Build lookup maps
    let learning_map: BTreeMap<String, &CompoundLearning> =
        learnings.iter().map(|l| (l.id.clone(), l)).collect();
    let context_map: BTreeMap<String, &SessionContext> = contexts
        .iter()
        .map(|ctx| (ctx.session_file.clone(), ctx))
        .collect();

    // Load the judge cache
    let mut cache = load_judge_cache_from(&cache_path);
    let initial_cache_size = cache.len();
    eprintln!("Loaded judge cache with {} entries", initial_cache_size,);

    // Build the judge context (system prompt is built once, cached by API)
    let judge = JudgeContext::from_config(&config.judge);

    // --- 2. Per-session LLM judge scoring ---

    // Accumulators for aggregate stats
    let mut v1_only_scores: Vec<f64> = Vec::new();
    let mut v2_only_scores: Vec<f64> = Vec::new();
    let mut shared_scores: Vec<f64> = Vec::new();
    let mut all_v1_scores: Vec<f64> = Vec::new();
    let mut all_v2_scores: Vec<f64> = Vec::new();
    let mut judge_calls = 0usize;
    let mut judge_failures = 0usize;
    let mut cache_hits = 0usize;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!(
        "LLM JUDGE EVALUATION -- {} backend ({}){}",
        backend,
        model,
        if backend == "api" {
            " with prompt caching"
        } else {
            ""
        }
    );
    eprintln!("{}", "=".repeat(80));

    for ctx in &contexts {
        let session_ctx = match context_map.get(&ctx.session_file) {
            Some(c) => *c,
            None => continue,
        };

        // Get the first tool call for v1/v2 keyword extraction
        let first_tc = match ctx.all_tool_calls.first() {
            Some(tc) => tc,
            None => continue,
        };

        // Run both extractors
        let v1_keywords = extract_tool_input_keywords(&first_tc.tool_name, &first_tc.tool_input);
        let v2_keywords = extract_tool_input_keywords_v2(&first_tc.tool_name, &first_tc.tool_input);

        // Match keywords against learnings
        let v1_matched = match_keywords_to_learnings(&v1_keywords, &learnings);
        let v2_matched = match_keywords_to_learnings(&v2_keywords, &learnings);

        if v1_matched.is_empty() && v2_matched.is_empty() {
            continue;
        }

        let v1_ids: HashSet<String> = v1_matched.iter().map(|m| m.id.clone()).collect();
        let v2_ids: HashSet<String> = v2_matched.iter().map(|m| m.id.clone()).collect();

        // Union of all learning IDs we need to judge for this session
        let all_ids: HashSet<String> = v1_ids.union(&v2_ids).cloned().collect();

        let session_short = &ctx.session_file[..8.min(ctx.session_file.len())];
        eprintln!(
            "\n  Session {} | {} tool calls | v1={} v2={} matched learnings",
            session_short,
            ctx.all_tool_calls.len(),
            v1_matched.len(),
            v2_matched.len(),
        );

        // Score each unique learning in this session
        let mut session_scores: BTreeMap<String, f64> = BTreeMap::new();

        for learning_id in &all_ids {
            let learning = match learning_map.get(learning_id) {
                Some(l) => *l,
                None => continue,
            };

            let cache_key = judge_cache_key(&ctx.session_file, learning_id);
            let was_cached = cache.contains_key(&cache_key);

            match llm_judge_relevance(&ctx.session_file, learning, session_ctx, &mut cache, &judge)
            {
                Some(score) => {
                    session_scores.insert(learning_id.clone(), score);
                    if was_cached {
                        cache_hits += 1;
                    }
                    judge_calls += 1;
                }
                None => {
                    judge_failures += 1;
                    judge_calls += 1;
                }
            }
        }

        // Classify scores into v1-only, v2-only, shared
        for (id, score) in &session_scores {
            let in_v1 = v1_ids.contains(id);
            let in_v2 = v2_ids.contains(id);

            match (in_v1, in_v2) {
                (true, true) => {
                    shared_scores.push(*score);
                    all_v1_scores.push(*score);
                    all_v2_scores.push(*score);
                }
                (true, false) => {
                    v1_only_scores.push(*score);
                    all_v1_scores.push(*score);
                }
                (false, true) => {
                    v2_only_scores.push(*score);
                    all_v2_scores.push(*score);
                }
                (false, false) => {} // shouldn't happen
            }

            let label = match (in_v1, in_v2) {
                (true, true) => "shared",
                (true, false) => "v1-only",
                (false, true) => "v2-only",
                _ => "?",
            };
            let summary = learning_map
                .get(id)
                .map(|l| l.summary.as_str())
                .unwrap_or("(unknown)");
            eprintln!(
                "    {} score={} {} | {}",
                label,
                score,
                id,
                truncate_str(summary, 60)
            );
        }

        // Persist cache after each session in case we're interrupted
        save_judge_cache_to(&cache, &cache_path);
    }

    // --- 3. Aggregate report ---

    let avg = |scores: &[f64]| -> f64 {
        if scores.is_empty() {
            0.0
        } else {
            scores.iter().sum::<f64>() / scores.len() as f64
        }
    };

    let median = |scores: &mut Vec<f64>| -> f64 {
        if scores.is_empty() {
            return 0.0;
        }
        scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = scores.len() / 2;
        if scores.len().is_multiple_of(2) {
            (scores[mid - 1] + scores[mid]) / 2.0
        } else {
            scores[mid]
        }
    };

    let v1_only_avg = avg(&v1_only_scores);
    let v2_only_avg = avg(&v2_only_scores);
    let shared_avg = avg(&shared_scores);
    let all_v1_avg = avg(&all_v1_scores);
    let all_v2_avg = avg(&all_v2_scores);

    let v1_only_median = median(&mut v1_only_scores.clone());
    let v2_only_median = median(&mut v2_only_scores.clone());
    let shared_median = median(&mut shared_scores.clone());

    // Score distribution (count per score value)
    let distribution = |scores: &[f64]| -> String {
        let mut counts = [0usize; 5]; // index 0 = score 1, etc.
        for &s in scores {
            let idx = (s as usize).saturating_sub(1).min(4);
            counts[idx] += 1;
        }
        format!(
            "1={} 2={} 3={} 4={} 5={}",
            counts[0], counts[1], counts[2], counts[3], counts[4]
        )
    };

    let new_cache_entries = cache.len() - initial_cache_size;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("LLM JUDGE AGGREGATE RESULTS");
    eprintln!("{}", "=".repeat(80));

    eprintln!("\n  Execution:");
    eprintln!("    Model:                       {}", model);
    eprintln!("    Total judge calls:           {}", judge_calls);
    eprintln!("    Cache hits:                  {}", cache_hits);
    eprintln!("    New judgments:                {}", new_cache_entries);
    eprintln!("    Failures:                    {}", judge_failures);
    eprintln!("    Total cache entries:          {}", cache.len());

    eprintln!("\n  v1-only learnings (LOST by switching to v2):");
    eprintln!("    Count:                       {}", v1_only_scores.len());
    eprintln!("    Avg score:                   {:.2}", v1_only_avg);
    eprintln!("    Median score:                {:.1}", v1_only_median);
    eprintln!(
        "    Distribution:                {}",
        distribution(&v1_only_scores)
    );

    eprintln!("\n  v2-only learnings (GAINED by switching to v2):");
    eprintln!("    Count:                       {}", v2_only_scores.len());
    eprintln!("    Avg score:                   {:.2}", v2_only_avg);
    eprintln!("    Median score:                {:.1}", v2_only_median);
    eprintln!(
        "    Distribution:                {}",
        distribution(&v2_only_scores)
    );

    eprintln!("\n  Shared learnings (matched by both v1 and v2):");
    eprintln!("    Count:                       {}", shared_scores.len());
    eprintln!("    Avg score:                   {:.2}", shared_avg);
    eprintln!("    Median score:                {:.1}", shared_median);
    eprintln!(
        "    Distribution:                {}",
        distribution(&shared_scores)
    );

    eprintln!("\n  Overall:");
    eprintln!(
        "    All v1-matched avg:          {:.2} ({} learnings)",
        all_v1_avg,
        all_v1_scores.len()
    );
    eprintln!(
        "    All v2-matched avg:          {:.2} ({} learnings)",
        all_v2_avg,
        all_v2_scores.len()
    );

    // Key comparison
    let delta = v2_only_avg - v1_only_avg;
    eprintln!("\n  KEY COMPARISON:");
    eprintln!("    v2-gained avg:               {:.2}", v2_only_avg);
    eprintln!("    v1-lost avg:                 {:.2}", v1_only_avg);
    eprintln!("    Delta (gained - lost):        {:+.2}", delta);
    eprintln!(
        "    Interpretation:              {}",
        if delta > 0.3 {
            "v2 gains are substantially MORE relevant than v1 losses"
        } else if delta > 0.0 {
            "v2 gains are slightly more relevant than v1 losses"
        } else if delta > -0.3 {
            "v2 gains are slightly LESS relevant than v1 losses"
        } else {
            "v2 gains are substantially LESS relevant than v1 losses -- v2 may be dropping good learnings"
        }
    );

    // High-value vs low-value analysis
    let v1_only_high = v1_only_scores.iter().filter(|&&s| s >= 4.0).count();
    let v2_only_high = v2_only_scores.iter().filter(|&&s| s >= 4.0).count();
    let v1_only_low = v1_only_scores.iter().filter(|&&s| s <= 2.0).count();
    let v2_only_low = v2_only_scores.iter().filter(|&&s| s <= 2.0).count();

    eprintln!("\n  HIGH/LOW VALUE BREAKDOWN:");
    eprintln!(
        "    v1-lost high-value (>=4):     {} / {} ({:.0}%)",
        v1_only_high,
        v1_only_scores.len(),
        if v1_only_scores.is_empty() {
            0.0
        } else {
            v1_only_high as f64 / v1_only_scores.len() as f64 * 100.0
        }
    );
    eprintln!(
        "    v1-lost low-value (<=2):      {} / {} ({:.0}%)",
        v1_only_low,
        v1_only_scores.len(),
        if v1_only_scores.is_empty() {
            0.0
        } else {
            v1_only_low as f64 / v1_only_scores.len() as f64 * 100.0
        }
    );
    eprintln!(
        "    v2-gained high-value (>=4):   {} / {} ({:.0}%)",
        v2_only_high,
        v2_only_scores.len(),
        if v2_only_scores.is_empty() {
            0.0
        } else {
            v2_only_high as f64 / v2_only_scores.len() as f64 * 100.0
        }
    );
    eprintln!(
        "    v2-gained low-value (<=2):    {} / {} ({:.0}%)",
        v2_only_low,
        v2_only_scores.len(),
        if v2_only_scores.is_empty() {
            0.0
        } else {
            v2_only_low as f64 / v2_only_scores.len() as f64 * 100.0
        }
    );

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("INTERPRETATION");
    eprintln!("{}", "=".repeat(80));
    eprintln!(
        "\n  This test uses an LLM ({}) as an independent relevance judge",
        model
    );
    eprintln!(
        "  via the {} backend.{}",
        backend,
        if backend == "api" {
            " Prompt caching reduces API costs."
        } else {
            " Uses Max subscription (no API costs)."
        }
    );
    eprintln!("  Each (session, learning) pair gets a 1-5 score based");
    eprintln!("  on the session's full activity context.");
    eprintln!("\n  The critical question: Are the learnings v2 GAINS more relevant");
    eprintln!("  than the learnings v2 LOSES?");
    eprintln!("\n  If v2-gained avg > v1-lost avg, then v2's noise filtering is");
    eprintln!("  correctly dropping low-value matches and (through improved keyword");
    eprintln!("  extraction) picking up higher-value ones.");
    eprintln!("\n  Also check the high/low breakdown: ideally v1-lost learnings");
    eprintln!("  should be mostly low-value (scores 1-2), and v2-gained learnings");
    eprintln!("  should include high-value ones (scores 4-5).");
    eprintln!("\n  Configure backend in .grove/config.toml:");
    eprintln!("    [judge]");
    eprintln!("    backend = \"cli\"   # Max plan (default)");
    eprintln!("    model = \"haiku\"   # or \"claude-haiku-4-5-20251001\" for api");

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("To reproduce: cargo test -- --ignored replay_llm_judge --nocapture");
    eprintln!("{}", "=".repeat(80));
}

// =============================================================================
// Tantivy BM25 replay harness tests
// =============================================================================

/// Compare keyword-overlap vs BM25 scoring on real session data.
///
/// For each session: score learnings with both keyword-overlap and BM25,
/// report top-5 overlap, score distributions, and per-learning frequency
/// to show which learnings BM25 promotes or demotes vs keyword-overlap.
///
/// Run with:
/// ```bash
/// cargo test --features tantivy-search -- --ignored replay_tantivy_comparison --nocapture
/// ```
#[test]
#[ignore]
#[cfg(feature = "tantivy-search")]
fn replay_tantivy_comparison() {
    use crate::search::TantivySearchIndex;

    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() {
        eprintln!(
            "SKIPPING: transcript directory does not exist: {}",
            transcript_dir_str
        );
        return;
    }
    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    // Load data
    let learnings = load_learnings(learnings_path);
    eprintln!("Loaded {} learnings", learnings.len());
    if learnings.is_empty() {
        eprintln!("SKIPPING: no learnings loaded");
        return;
    }

    let contexts = build_session_contexts(transcript_dir);
    eprintln!("Built session contexts for {} sessions", contexts.len());
    if contexts.is_empty() {
        eprintln!("SKIPPING: no session contexts built");
        return;
    }

    // Build Tantivy index once for all sessions
    let index = TantivySearchIndex::in_memory().expect("Failed to create Tantivy index");
    index
        .index_learnings(&learnings)
        .expect("Failed to index learnings");
    eprintln!("Built Tantivy index with {} documents", index.num_docs());

    // Track aggregate stats
    let mut keyword_match_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut bm25_match_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut sessions_compared = 0usize;
    let mut total_keyword_matches = 0usize;
    let mut total_bm25_matches = 0usize;
    let top_n = 5;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("KEYWORD-OVERLAP vs BM25 SCORING COMPARISON");
    eprintln!("{}", "=".repeat(80));

    for ctx in &contexts {
        let first_tc = match ctx.all_tool_calls.first() {
            Some(tc) => tc,
            None => continue,
        };

        // Keyword-overlap scoring (v2 extractor)
        let v2_keywords = extract_tool_input_keywords_v2(&first_tc.tool_name, &first_tc.tool_input);
        let keyword_matched = match_keywords_to_learnings(&v2_keywords, &learnings);

        // BM25 scoring: build query from the same keywords
        let query_string = v2_keywords.join(" ");
        let bm25_results = if query_string.trim().is_empty() {
            Vec::new()
        } else {
            index.search(&query_string, top_n).unwrap_or_default()
        };

        if keyword_matched.is_empty() && bm25_results.is_empty() {
            continue;
        }

        sessions_compared += 1;

        // Track keyword matches (top N by keyword count)
        let mut keyword_sorted = keyword_matched.clone();
        keyword_sorted.sort_by(|a, b| b.matched_keywords.len().cmp(&a.matched_keywords.len()));
        for m in keyword_sorted.iter().take(top_n) {
            *keyword_match_counts.entry(m.id.clone()).or_insert(0) += 1;
            total_keyword_matches += 1;
        }

        // Track BM25 matches
        for r in &bm25_results {
            *bm25_match_counts.entry(r.id.clone()).or_insert(0) += 1;
            total_bm25_matches += 1;
        }

        // Per-session detail (abbreviated)
        let session_short = &ctx.session_file[..8.min(ctx.session_file.len())];
        let keyword_ids: HashSet<String> = keyword_sorted
            .iter()
            .take(top_n)
            .map(|m| m.id.clone())
            .collect();
        let bm25_ids: HashSet<String> = bm25_results.iter().map(|r| r.id.clone()).collect();
        let overlap_count = keyword_ids.intersection(&bm25_ids).count();

        eprintln!(
            "  {} | kw={} bm25={} overlap={}/{}",
            session_short,
            keyword_ids.len(),
            bm25_ids.len(),
            overlap_count,
            keyword_ids.union(&bm25_ids).count()
        );
    }

    // Aggregate report
    eprintln!("\n{}", "=".repeat(80));
    eprintln!("AGGREGATE COMPARISON");
    eprintln!("{}", "=".repeat(80));
    eprintln!("  Sessions compared:              {}", sessions_compared);
    eprintln!(
        "  Total keyword top-{} matches:    {}",
        top_n, total_keyword_matches
    );
    eprintln!(
        "  Total BM25 top-{} matches:       {}",
        top_n, total_bm25_matches
    );

    // Unique learnings surfaced
    eprintln!(
        "  Unique learnings (keyword):      {}",
        keyword_match_counts.len()
    );
    eprintln!(
        "  Unique learnings (BM25):         {}",
        bm25_match_counts.len()
    );

    let kw_only: Vec<&String> = keyword_match_counts
        .keys()
        .filter(|k| !bm25_match_counts.contains_key(*k))
        .collect();
    let bm25_only: Vec<&String> = bm25_match_counts
        .keys()
        .filter(|k| !keyword_match_counts.contains_key(*k))
        .collect();

    eprintln!("  Keyword-only learnings:          {}", kw_only.len());
    eprintln!("  BM25-only learnings:             {}", bm25_only.len());

    // Top surfaced learnings by each method
    let mut kw_sorted: Vec<_> = keyword_match_counts.iter().collect();
    kw_sorted.sort_by(|a, b| b.1.cmp(a.1));
    eprintln!("\n  Top keyword-overlap learnings:");
    for (id, count) in kw_sorted.iter().take(10) {
        eprintln!("    {} (surfaced {} times)", id, count);
    }

    let mut bm25_sorted: Vec<_> = bm25_match_counts.iter().collect();
    bm25_sorted.sort_by(|a, b| b.1.cmp(a.1));
    eprintln!("\n  Top BM25 learnings:");
    for (id, count) in bm25_sorted.iter().take(10) {
        eprintln!("    {} (surfaced {} times)", id, count);
    }

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("To reproduce: cargo test --features tantivy-search -- --ignored replay_tantivy_comparison --nocapture");
    eprintln!("{}", "=".repeat(80));
}

/// LLM judge evaluation using BM25 scoring instead of keyword-overlap.
///
/// Same methodology as `replay_llm_judge` but uses Tantivy BM25 to match
/// learnings to sessions. Reports avg relevance for comparison with the
/// keyword-overlap baseline (2.39/5.0).
///
/// Cache keys are shared: same (session, learning) pair gets the same judge
/// score regardless of matching method.
///
/// Run with:
/// ```bash
/// cargo test --features tantivy-search -- --ignored replay_tantivy_llm_judge --nocapture
/// ```
#[test]
#[ignore]
#[cfg(feature = "tantivy-search")]
fn replay_tantivy_llm_judge() {
    use crate::search::TantivySearchIndex;

    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() {
        eprintln!(
            "SKIPPING: transcript directory does not exist: {}",
            transcript_dir_str
        );
        return;
    }
    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    // Load config
    let config = crate::config::Config::load();
    let backend = &config.judge.backend;
    let model = &config.judge.model;
    let api_url = &config.judge.api_url;
    let cache_path = if config.judge.cache_path.is_empty() {
        llm_judge_cache_path()
    } else {
        config.judge.cache_path.clone()
    };

    if backend == "api"
        && std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .is_none()
    {
        eprintln!("SKIPPING: backend=api but ANTHROPIC_API_KEY not set");
        return;
    }

    eprintln!(
        "Judge config: backend={}, model={}, api={}",
        backend, model, api_url
    );
    eprintln!("Cache path: {}", cache_path);

    // Load data
    let learnings = load_learnings(learnings_path);
    eprintln!("Loaded {} learnings", learnings.len());
    if learnings.is_empty() {
        eprintln!("SKIPPING: no learnings loaded");
        return;
    }

    let contexts = build_session_contexts(transcript_dir);
    eprintln!("Built session contexts for {} sessions", contexts.len());
    if contexts.is_empty() {
        eprintln!("SKIPPING: no session contexts built");
        return;
    }

    // Build Tantivy index
    let index = TantivySearchIndex::in_memory().expect("Failed to create Tantivy index");
    index
        .index_learnings(&learnings)
        .expect("Failed to index learnings");
    eprintln!("Built Tantivy index with {} documents", index.num_docs());

    // Build lookup maps
    let learning_map: BTreeMap<String, &CompoundLearning> =
        learnings.iter().map(|l| (l.id.clone(), l)).collect();
    let context_map: BTreeMap<String, &SessionContext> = contexts
        .iter()
        .map(|ctx| (ctx.session_file.clone(), ctx))
        .collect();

    // Load judge cache (shared with keyword-overlap judge)
    let mut cache = load_judge_cache_from(&cache_path);
    let initial_cache_size = cache.len();
    eprintln!("Loaded judge cache with {} entries", initial_cache_size);

    let judge = JudgeContext::from_config(&config.judge);

    // Accumulators
    let mut all_scores: Vec<f64> = Vec::new();
    let mut judge_calls = 0usize;
    let mut judge_failures = 0usize;
    let mut cache_hits = 0usize;
    let top_n = 5;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!(
        "BM25 LLM JUDGE EVALUATION -- {} backend ({})",
        backend, model
    );
    eprintln!("{}", "=".repeat(80));

    for ctx in &contexts {
        let session_ctx = match context_map.get(&ctx.session_file) {
            Some(c) => *c,
            None => continue,
        };

        let first_tc = match ctx.all_tool_calls.first() {
            Some(tc) => tc,
            None => continue,
        };

        // Build query from v2 keywords
        let v2_keywords = extract_tool_input_keywords_v2(&first_tc.tool_name, &first_tc.tool_input);
        let query_string = v2_keywords.join(" ");
        if query_string.trim().is_empty() {
            continue;
        }

        // BM25 match
        let bm25_results = match index.search(&query_string, top_n) {
            Ok(r) => r,
            Err(_) => continue,
        };

        if bm25_results.is_empty() {
            continue;
        }

        let session_short = &ctx.session_file[..8.min(ctx.session_file.len())];
        eprintln!(
            "\n  Session {} | {} tool calls | {} BM25 matches",
            session_short,
            ctx.all_tool_calls.len(),
            bm25_results.len(),
        );

        for bm25_result in &bm25_results {
            let learning = match learning_map.get(&bm25_result.id) {
                Some(l) => *l,
                None => continue,
            };

            let cache_key = judge_cache_key(&ctx.session_file, &bm25_result.id);
            let was_cached = cache.contains_key(&cache_key);

            match llm_judge_relevance(&ctx.session_file, learning, session_ctx, &mut cache, &judge)
            {
                Some(score) => {
                    all_scores.push(score);
                    if was_cached {
                        cache_hits += 1;
                    }
                    judge_calls += 1;

                    eprintln!(
                        "    score={} bm25={:.2} {} | {}",
                        score,
                        bm25_result.score,
                        bm25_result.id,
                        truncate_str(&learning.summary, 60)
                    );
                }
                None => {
                    judge_failures += 1;
                    judge_calls += 1;
                }
            }
        }

        // Persist cache after each session
        save_judge_cache_to(&cache, &cache_path);
    }

    // Aggregate report
    let avg = if all_scores.is_empty() {
        0.0
    } else {
        all_scores.iter().sum::<f64>() / all_scores.len() as f64
    };

    let mut sorted_scores = all_scores.clone();
    sorted_scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_val = if sorted_scores.is_empty() {
        0.0
    } else {
        let mid = sorted_scores.len() / 2;
        if sorted_scores.len().is_multiple_of(2) {
            (sorted_scores[mid - 1] + sorted_scores[mid]) / 2.0
        } else {
            sorted_scores[mid]
        }
    };

    // Score distribution
    let mut dist = [0usize; 5];
    for &s in &all_scores {
        let idx = (s as usize).saturating_sub(1).min(4);
        dist[idx] += 1;
    }

    let noise_count = all_scores.iter().filter(|&&s| s <= 2.0).count();
    let noise_pct = if all_scores.is_empty() {
        0.0
    } else {
        noise_count as f64 / all_scores.len() as f64 * 100.0
    };

    let new_cache_entries = cache.len() - initial_cache_size;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("BM25 LLM JUDGE AGGREGATE RESULTS");
    eprintln!("{}", "=".repeat(80));

    eprintln!("\n  Execution:");
    eprintln!("    Model:                       {}", model);
    eprintln!("    Total judge calls:           {}", judge_calls);
    eprintln!("    Cache hits:                  {}", cache_hits);
    eprintln!("    New judgments:                {}", new_cache_entries);
    eprintln!("    Failures:                    {}", judge_failures);
    eprintln!("    Total cache entries:          {}", cache.len());

    eprintln!("\n  BM25-matched learnings:");
    eprintln!("    Count:                       {}", all_scores.len());
    eprintln!("    Avg relevance:               {:.2}", avg);
    eprintln!("    Median relevance:            {:.1}", median_val);
    eprintln!(
        "    Distribution:                1={} 2={} 3={} 4={} 5={}",
        dist[0], dist[1], dist[2], dist[3], dist[4]
    );
    eprintln!(
        "    Noise (<=2):                 {} / {} ({:.0}%)",
        noise_count,
        all_scores.len(),
        noise_pct
    );

    eprintln!("\n  COMPARISON WITH BASELINE:");
    eprintln!("    Keyword-overlap baseline:    2.32 avg (256 pairs)");
    eprintln!(
        "    BM25 result:                 {:.2} avg ({:.0}% noise)",
        avg, noise_pct
    );
    let delta = avg - 2.32;
    eprintln!("    Delta:                       {:+.2}", delta);
    eprintln!(
        "    Interpretation:              {}",
        if delta > 0.3 {
            "BM25 substantially improves relevance"
        } else if delta > 0.0 {
            "BM25 slightly improves relevance"
        } else if delta > -0.3 {
            "BM25 roughly equivalent to keyword-overlap"
        } else {
            "BM25 performs worse than keyword-overlap"
        }
    );

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("To reproduce: cargo test --features tantivy-search -- --ignored replay_tantivy_llm_judge --nocapture");
    eprintln!("{}", "=".repeat(80));
}

/// Replay adaptive threshold analysis using BM25 scoring on real sessions.
///
/// Compares baseline behavior (always inject up to 5) against adaptive threshold
/// and dynamic K filtering. Reports how many sessions get suppressed, average
/// injection count, and noise reduction.
///
/// Run with:
/// ```bash
/// cargo test --features tantivy-search -- --ignored replay_adaptive_threshold --nocapture
/// ```
#[test]
#[ignore]
#[cfg(feature = "tantivy-search")]
fn replay_adaptive_threshold() {
    use crate::config::RetrievalConfig;
    use crate::search::TantivySearchIndex;
    use crate::stats::scoring::{
        recency, recency_weight, reference_boost, CompositeScore, Strategy,
    };

    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() {
        eprintln!(
            "SKIPPING: transcript directory does not exist: {}",
            transcript_dir_str
        );
        return;
    }
    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    // Load data
    let learnings = load_learnings(learnings_path);
    eprintln!("Loaded {} learnings", learnings.len());
    if learnings.is_empty() {
        eprintln!("SKIPPING: no learnings loaded");
        return;
    }

    let contexts = build_session_contexts(transcript_dir);
    eprintln!("Built session contexts for {} sessions", contexts.len());
    if contexts.is_empty() {
        eprintln!("SKIPPING: no session contexts built");
        return;
    }

    // Build Tantivy index once for all sessions
    let index = TantivySearchIndex::in_memory().expect("Failed to create Tantivy index");
    index
        .index_learnings(&learnings)
        .expect("Failed to index learnings");
    eprintln!("Built Tantivy index with {} documents", index.num_docs());

    // Config: default adaptive threshold settings
    let config = RetrievalConfig::default();
    let top_n = config.max_injections as usize;
    let strategy = Strategy::Moderate;
    let now = chrono::Utc::now();

    // Track aggregate stats
    let mut sessions_with_results = 0usize;
    let mut baseline_total_injections = 0usize;
    let mut adaptive_total_injections = 0usize;
    let mut sessions_suppressed = 0usize;
    let mut sessions_reduced = 0usize;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("ADAPTIVE THRESHOLD + DYNAMIC K ANALYSIS");
    eprintln!("{}", "=".repeat(80));
    eprintln!(
        "  Config: min_confidence={:.3}, min_score_gap={:.3}, dynamic_k_ratio={:.3}",
        config.min_confidence_threshold, config.min_score_gap, config.dynamic_k_ratio
    );
    eprintln!("  Baseline: always inject up to {} learnings", top_n);
    eprintln!();

    for ctx in &contexts {
        let first_tc = match ctx.all_tool_calls.first() {
            Some(tc) => tc,
            None => continue,
        };

        // Extract keywords and build BM25 query
        let v2_keywords = extract_tool_input_keywords_v2(&first_tc.tool_name, &first_tc.tool_input);
        let query_string = v2_keywords.join(" ");
        if query_string.trim().is_empty() {
            continue;
        }

        let bm25_results = match index.search(&query_string, 20) {
            Ok(r) => r,
            Err(_) => continue,
        };

        if bm25_results.is_empty() {
            continue;
        }

        // Score with composite scoring (same as production pipeline)
        let learning_map: std::collections::HashMap<String, &CompoundLearning> =
            learnings.iter().map(|l| (l.id.clone(), l)).collect();

        // Normalize BM25 scores to use as relevance
        let max_bm25 = bm25_results
            .iter()
            .map(|r| r.score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut scored: Vec<CompositeScore> = bm25_results
            .iter()
            .filter_map(|r| {
                let learning = learning_map.get(&r.id)?;
                let relevance = if max_bm25 < f32::EPSILON {
                    1.0
                } else {
                    (r.score / max_bm25) as f64
                };
                let half_life = config.half_life_for_category(&learning.category);
                let lambda_cat = recency::lambda_from_half_life(half_life);
                let recency_val = recency_weight(learning.timestamp, now, lambda_cat);
                let ref_boost = reference_boost(None); // no stats cache for replay
                Some(CompositeScore::new(
                    (*learning).clone(),
                    relevance,
                    recency_val,
                    ref_boost,
                    strategy,
                ))
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if scored.is_empty() {
            continue;
        }

        sessions_with_results += 1;

        // Baseline: take top N
        let baseline_count = scored.len().min(top_n);
        baseline_total_injections += baseline_count;

        // Adaptive threshold + dynamic K
        let adaptive_result = super::apply_adaptive_threshold(
            scored.clone(),
            config.min_confidence_threshold,
            config.min_score_gap,
        );

        let adaptive_count = match adaptive_result {
            None => {
                sessions_suppressed += 1;
                0
            }
            Some(passed) => {
                let qualified = super::apply_dynamic_k(passed, config.dynamic_k_ratio, top_n);
                let count = qualified.len();
                if count < baseline_count {
                    sessions_reduced += 1;
                }
                count
            }
        };
        adaptive_total_injections += adaptive_count;

        // Per-session detail
        let session_short = &ctx.session_file[..8.min(ctx.session_file.len())];
        let top_score = scored.first().map(|s| s.score).unwrap_or(0.0);
        let median_score = if scored.len() >= 3 {
            scored[scored.len() / 2].score
        } else {
            0.0
        };
        if adaptive_count != baseline_count {
            eprintln!(
                "  {} | baseline={} adaptive={} top={:.3} median={:.3} gap={:.3}",
                session_short,
                baseline_count,
                adaptive_count,
                top_score,
                median_score,
                top_score - median_score
            );
        }
    }

    // Aggregate report
    eprintln!("\n{}", "=".repeat(80));
    eprintln!("AGGREGATE RESULTS");
    eprintln!("{}", "=".repeat(80));
    eprintln!(
        "  Sessions with BM25 results:     {}",
        sessions_with_results
    );
    eprintln!("  Sessions fully suppressed:      {}", sessions_suppressed);
    eprintln!("  Sessions with reduced injection: {}", sessions_reduced);
    eprintln!(
        "  Baseline total injections:      {}",
        baseline_total_injections
    );
    eprintln!(
        "  Adaptive total injections:      {}",
        adaptive_total_injections
    );

    if sessions_with_results > 0 {
        let baseline_avg = baseline_total_injections as f64 / sessions_with_results as f64;
        let adaptive_avg = adaptive_total_injections as f64 / sessions_with_results as f64;
        let reduction_pct = if baseline_total_injections > 0 {
            (1.0 - adaptive_total_injections as f64 / baseline_total_injections as f64) * 100.0
        } else {
            0.0
        };

        eprintln!("  Avg injections/session (baseline):  {:.2}", baseline_avg);
        eprintln!("  Avg injections/session (adaptive):  {:.2}", adaptive_avg);
        eprintln!(
            "  Injection reduction:                {:.1}%",
            reduction_pct
        );
    }

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("To reproduce: cargo test --features tantivy-search -- --ignored replay_adaptive_threshold --nocapture");
    eprintln!("{}", "=".repeat(80));
}

/// Replay BM25 + adaptive threshold with LLM judge evaluation.
///
/// Combines BM25 matching with adaptive threshold + dynamic K filtering,
/// then runs the LLM judge on the surviving (session, learning) pairs.
/// This measures whether adaptive filtering removes the *noisy* learnings.
///
/// Run with:
/// ```bash
/// GROVE_LLM_JUDGE_BACKEND=cli cargo test --features tantivy-search -- --ignored replay_tantivy_adaptive_llm_judge --nocapture
/// ```
#[test]
#[ignore]
#[cfg(feature = "tantivy-search")]
fn replay_tantivy_adaptive_llm_judge() {
    use crate::config::RetrievalConfig;
    use crate::search::TantivySearchIndex;
    use crate::stats::scoring::{
        recency, recency_weight, reference_boost, CompositeScore, Strategy,
    };

    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() {
        eprintln!(
            "SKIPPING: transcript directory does not exist: {}",
            transcript_dir_str
        );
        return;
    }
    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    // Load config
    let config = crate::config::Config::load();
    let retrieval_config = RetrievalConfig::default();
    let backend = &config.judge.backend;
    let model = &config.judge.model;
    let api_url = &config.judge.api_url;
    let cache_path = if config.judge.cache_path.is_empty() {
        llm_judge_cache_path()
    } else {
        config.judge.cache_path.clone()
    };

    if backend == "api"
        && std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .is_none()
    {
        eprintln!("SKIPPING: backend=api but ANTHROPIC_API_KEY not set");
        return;
    }

    eprintln!(
        "Judge config: backend={}, model={}, api={}",
        backend, model, api_url
    );
    eprintln!("Cache path: {}", cache_path);
    eprintln!(
        "Adaptive config: min_confidence={:.3}, min_score_gap={:.3}, dynamic_k_ratio={:.3}",
        retrieval_config.min_confidence_threshold,
        retrieval_config.min_score_gap,
        retrieval_config.dynamic_k_ratio
    );

    // Load data
    let learnings = load_learnings(learnings_path);
    eprintln!("Loaded {} learnings", learnings.len());
    if learnings.is_empty() {
        eprintln!("SKIPPING: no learnings loaded");
        return;
    }

    let contexts = build_session_contexts(transcript_dir);
    eprintln!("Built session contexts for {} sessions", contexts.len());
    if contexts.is_empty() {
        eprintln!("SKIPPING: no session contexts built");
        return;
    }

    // Build Tantivy index
    let index = TantivySearchIndex::in_memory().expect("Failed to create Tantivy index");
    index
        .index_learnings(&learnings)
        .expect("Failed to index learnings");
    eprintln!("Built Tantivy index with {} documents", index.num_docs());

    // Build lookup maps
    let learning_map: BTreeMap<String, &CompoundLearning> =
        learnings.iter().map(|l| (l.id.clone(), l)).collect();
    let context_map: BTreeMap<String, &SessionContext> = contexts
        .iter()
        .map(|ctx| (ctx.session_file.clone(), ctx))
        .collect();

    // Load judge cache (shared across all judge tests)
    let mut cache = load_judge_cache_from(&cache_path);
    let initial_cache_size = cache.len();
    eprintln!("Loaded judge cache with {} entries", initial_cache_size);

    let judge = JudgeContext::from_config(&config.judge);

    let top_n = retrieval_config.max_injections as usize;
    let strategy = Strategy::Moderate;
    let now = chrono::Utc::now();

    // Accumulators
    let mut all_scores: Vec<f64> = Vec::new();
    let mut judge_calls = 0usize;
    let mut judge_failures = 0usize;
    let mut cache_hits = 0usize;
    let mut sessions_evaluated = 0usize;
    let mut sessions_suppressed = 0usize;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!(
        "BM25 + ADAPTIVE THRESHOLD LLM JUDGE -- {} backend ({})",
        backend, model
    );
    eprintln!("{}", "=".repeat(80));

    for ctx in &contexts {
        let session_ctx = match context_map.get(&ctx.session_file) {
            Some(c) => *c,
            None => continue,
        };

        let first_tc = match ctx.all_tool_calls.first() {
            Some(tc) => tc,
            None => continue,
        };

        // Build query from v2 keywords
        let v2_keywords = extract_tool_input_keywords_v2(&first_tc.tool_name, &first_tc.tool_input);
        let query_string = v2_keywords.join(" ");
        if query_string.trim().is_empty() {
            continue;
        }

        // BM25 match (fetch more than top_n so adaptive threshold has material to work with)
        let bm25_results = match index.search(&query_string, 20) {
            Ok(r) => r,
            Err(_) => continue,
        };

        if bm25_results.is_empty() {
            continue;
        }

        // Build composite scores (same as production pipeline)
        let learning_hash: std::collections::HashMap<String, &CompoundLearning> =
            learnings.iter().map(|l| (l.id.clone(), l)).collect();

        let max_bm25 = bm25_results
            .iter()
            .map(|r| r.score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut scored: Vec<CompositeScore> = bm25_results
            .iter()
            .filter_map(|r| {
                let learning = learning_hash.get(&r.id)?;
                let relevance = if max_bm25 < f32::EPSILON {
                    1.0
                } else {
                    (r.score / max_bm25) as f64
                };
                let half_life = retrieval_config.half_life_for_category(&learning.category);
                let lambda_cat = recency::lambda_from_half_life(half_life);
                let recency_val = recency_weight(learning.timestamp, now, lambda_cat);
                let ref_boost = reference_boost(None);
                Some(CompositeScore::new(
                    (*learning).clone(),
                    relevance,
                    recency_val,
                    ref_boost,
                    strategy,
                ))
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if scored.is_empty() {
            continue;
        }

        sessions_evaluated += 1;

        // Apply adaptive threshold + dynamic K
        let filtered = match super::apply_adaptive_threshold(
            scored,
            retrieval_config.min_confidence_threshold,
            retrieval_config.min_score_gap,
        ) {
            None => {
                sessions_suppressed += 1;
                let session_short = &ctx.session_file[..8.min(ctx.session_file.len())];
                eprintln!(
                    "\n  Session {} | SUPPRESSED (scores too clustered)",
                    session_short
                );
                continue;
            }
            Some(passed) => super::apply_dynamic_k(passed, retrieval_config.dynamic_k_ratio, top_n),
        };

        let session_short = &ctx.session_file[..8.min(ctx.session_file.len())];
        eprintln!(
            "\n  Session {} | {} tool calls | {} adaptive matches",
            session_short,
            ctx.all_tool_calls.len(),
            filtered.len(),
        );

        // Judge each surviving learning
        for cs in &filtered {
            let learning = match learning_map.get(&cs.learning.id) {
                Some(l) => *l,
                None => continue,
            };

            let cache_key = judge_cache_key(&ctx.session_file, &cs.learning.id);
            let was_cached = cache.contains_key(&cache_key);

            match llm_judge_relevance(&ctx.session_file, learning, session_ctx, &mut cache, &judge)
            {
                Some(score) => {
                    all_scores.push(score);
                    if was_cached {
                        cache_hits += 1;
                    }
                    judge_calls += 1;

                    eprintln!(
                        "    score={} composite={:.3} {} | {}",
                        score,
                        cs.score,
                        cs.learning.id,
                        truncate_str(&learning.summary, 60)
                    );
                }
                None => {
                    judge_failures += 1;
                    judge_calls += 1;
                }
            }
        }

        // Persist cache after each session
        save_judge_cache_to(&cache, &cache_path);
    }

    // Aggregate report
    let avg = if all_scores.is_empty() {
        0.0
    } else {
        all_scores.iter().sum::<f64>() / all_scores.len() as f64
    };

    let mut sorted_scores = all_scores.clone();
    sorted_scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_val = if sorted_scores.is_empty() {
        0.0
    } else {
        let mid = sorted_scores.len() / 2;
        if sorted_scores.len().is_multiple_of(2) {
            (sorted_scores[mid - 1] + sorted_scores[mid]) / 2.0
        } else {
            sorted_scores[mid]
        }
    };

    // Score distribution
    let mut dist = [0usize; 5];
    for &s in &all_scores {
        let idx = (s as usize).saturating_sub(1).min(4);
        dist[idx] += 1;
    }

    let noise_count = all_scores.iter().filter(|&&s| s <= 2.0).count();
    let noise_pct = if all_scores.is_empty() {
        0.0
    } else {
        noise_count as f64 / all_scores.len() as f64 * 100.0
    };

    let new_cache_entries = cache.len() - initial_cache_size;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("BM25 + ADAPTIVE THRESHOLD LLM JUDGE AGGREGATE RESULTS");
    eprintln!("{}", "=".repeat(80));

    eprintln!("\n  Execution:");
    eprintln!("    Model:                       {}", model);
    eprintln!("    Total judge calls:           {}", judge_calls);
    eprintln!("    Cache hits:                  {}", cache_hits);
    eprintln!("    New judgments:                {}", new_cache_entries);
    eprintln!("    Failures:                    {}", judge_failures);
    eprintln!("    Total cache entries:          {}", cache.len());

    eprintln!("\n  Adaptive-filtered learnings:");
    eprintln!("    Sessions evaluated:          {}", sessions_evaluated);
    eprintln!("    Sessions suppressed:         {}", sessions_suppressed);
    eprintln!("    Pairs judged:                {}", all_scores.len());
    eprintln!("    Avg relevance:               {:.2}", avg);
    eprintln!("    Median relevance:            {:.1}", median_val);
    eprintln!(
        "    Distribution:                1={} 2={} 3={} 4={} 5={}",
        dist[0], dist[1], dist[2], dist[3], dist[4]
    );
    eprintln!(
        "    Noise (<=2):                 {} / {} ({:.0}%)",
        noise_count,
        all_scores.len(),
        noise_pct
    );

    eprintln!("\n  COMPARISON:");
    eprintln!("    Keyword-overlap baseline:    2.32 avg (256 pairs)");
    eprintln!("    BM25-only:                   2.76 avg (54% noise, 145 pairs)");
    eprintln!(
        "    BM25 + adaptive:             {:.2} avg ({:.0}% noise, {} pairs)",
        avg,
        noise_pct,
        all_scores.len()
    );

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("To reproduce: GROVE_LLM_JUDGE_BACKEND=cli cargo test --features tantivy-search -- --ignored replay_tantivy_adaptive_llm_judge --nocapture");
    eprintln!("{}", "=".repeat(80));
}

/// Audit specificity heuristics against the real learning corpus.
///
/// Runs NED, PSTF, GPD, and composite scoring against all learnings to:
/// - Show score distribution per heuristic
/// - Identify which learnings would be rejected at various thresholds
/// - Calibrate threshold values
///
/// Run with:
/// ```bash
/// cargo test -- --ignored replay_specificity_audit --nocapture
/// ```
#[test]
#[ignore]
fn replay_specificity_audit() {
    use crate::core::quality::assess_specificity;

    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    let learnings = load_learnings(learnings_path);
    eprintln!("Loaded {} learnings", learnings.len());
    if learnings.is_empty() {
        eprintln!("SKIPPING: no learnings loaded");
        return;
    }

    eprintln!("\n{}", "=".repeat(100));
    eprintln!("SPECIFICITY HEURISTICS AUDIT");
    eprintln!("{}", "=".repeat(100));

    // Score all learnings
    struct ScoredLearning {
        id: String,
        summary: String,
        ned: f64,
        pstf: f64,
        generic_count: u32,
        composite: f64,
        tags: Vec<String>,
    }

    let mut scored: Vec<ScoredLearning> = learnings
        .iter()
        .map(|l| {
            let s = assess_specificity(l);
            ScoredLearning {
                id: l.id.clone(),
                summary: l.summary.clone(),
                ned: s.ned,
                pstf: s.pstf,
                generic_count: s.generic_count,
                composite: s.composite,
                tags: l.tags.clone(),
            }
        })
        .collect();

    // Sort by composite score ascending (worst first)
    scored.sort_by(|a, b| {
        a.composite
            .partial_cmp(&b.composite)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Per-learning detail
    eprintln!(
        "\n  {:>5} {:>5} {:>4} {:>6}  {:<20} Summary",
        "NED", "PSTF", "GPD", "SCORE", "ID"
    );
    eprintln!("  {}", "-".repeat(95));

    for sl in &scored {
        let truncated = if sl.summary.len() > 60 {
            format!("{}...", &sl.summary[..57])
        } else {
            sl.summary.clone()
        };
        let marker = if sl.composite < 1.5 { " [REJECT]" } else { "" };
        eprintln!(
            "  {:>5.1} {:>5.2} {:>4} {:>6.2}  {:<20} {}{}",
            sl.ned, sl.pstf, sl.generic_count, sl.composite, sl.id, truncated, marker
        );
    }

    // Aggregate stats
    let composites: Vec<f64> = scored.iter().map(|s| s.composite).collect();
    let neds: Vec<f64> = scored.iter().map(|s| s.ned).collect();
    let pstfs: Vec<f64> = scored.iter().map(|s| s.pstf).collect();

    let avg = |v: &[f64]| -> f64 {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    let min = |v: &[f64]| -> f64 { v.iter().cloned().fold(f64::INFINITY, f64::min) };
    let max = |v: &[f64]| -> f64 { v.iter().cloned().fold(f64::NEG_INFINITY, f64::max) };

    eprintln!("\n{}", "=".repeat(100));
    eprintln!("AGGREGATE STATISTICS");
    eprintln!("{}", "=".repeat(100));

    eprintln!("\n  Composite score:");
    eprintln!(
        "    Min: {:.2}  Max: {:.2}  Avg: {:.2}",
        min(&composites),
        max(&composites),
        avg(&composites)
    );

    eprintln!("\n  NED (named entities per 100 words):");
    eprintln!(
        "    Min: {:.1}  Max: {:.1}  Avg: {:.1}",
        min(&neds),
        max(&neds),
        avg(&neds)
    );

    eprintln!("\n  PSTF (project-specific tag frequency):");
    eprintln!(
        "    Min: {:.2}  Max: {:.2}  Avg: {:.2}",
        min(&pstfs),
        max(&pstfs),
        avg(&pstfs)
    );

    let generic_counts: Vec<u32> = scored.iter().map(|s| s.generic_count).collect();
    let with_generics = generic_counts.iter().filter(|&&c| c > 0).count();
    eprintln!("\n  GPD (generic phrase count):",);
    eprintln!(
        "    Learnings with generic phrases: {} / {} ({:.0}%)",
        with_generics,
        scored.len(),
        with_generics as f64 / scored.len() as f64 * 100.0
    );

    // Threshold analysis
    eprintln!("\n{}", "=".repeat(100));
    eprintln!("THRESHOLD ANALYSIS");
    eprintln!("{}", "=".repeat(100));

    for threshold in [1.0, 1.25, 1.5, 1.75, 2.0, 2.5] {
        let rejected = scored.iter().filter(|s| s.composite < threshold).count();
        eprintln!(
            "  Threshold {:.2}: {} / {} rejected ({:.0}%)",
            threshold,
            rejected,
            scored.len(),
            rejected as f64 / scored.len() as f64 * 100.0
        );

        // List rejected learnings at default threshold
        if (threshold - 1.5).abs() < 0.01 && rejected > 0 {
            eprintln!("    Rejected learnings at default threshold (1.5):");
            for sl in scored.iter().filter(|s| s.composite < threshold) {
                let truncated = if sl.summary.len() > 55 {
                    format!("{}...", &sl.summary[..52])
                } else {
                    sl.summary.clone()
                };
                eprintln!(
                    "      {:.2} {} | {} | tags: [{}]",
                    sl.composite,
                    sl.id,
                    truncated,
                    sl.tags.join(", ")
                );
            }
        }
    }

    // Tag analysis
    eprintln!("\n{}", "=".repeat(100));
    eprintln!("TAG ANALYSIS");
    eprintln!("{}", "=".repeat(100));

    let mut tag_counts: BTreeMap<String, usize> = BTreeMap::new();
    for sl in &scored {
        for tag in &sl.tags {
            *tag_counts.entry(tag.clone()).or_default() += 1;
        }
    }

    let mut tag_vec: Vec<(String, usize)> = tag_counts.into_iter().collect();
    tag_vec.sort_by(|a, b| b.1.cmp(&a.1));

    eprintln!("\n  Most common tags:");
    for (tag, count) in tag_vec.iter().take(15) {
        eprintln!("    {:>3}x  {}", count, tag);
    }

    eprintln!("\n{}", "=".repeat(100));
    eprintln!("To reproduce: cargo test -- --ignored replay_specificity_audit --nocapture");
    eprintln!("{}", "=".repeat(100));
}

/// Replay BM25 + adaptive threshold + domain enrichment with LLM judge evaluation.
///
/// Identical to `replay_tantivy_adaptive_llm_judge` except that domain keywords
/// inferred from `SessionContext.file_paths` are merged into the v2 keywords
/// before building the BM25 query string.
///
/// Run with:
/// ```bash
/// GROVE_LLM_JUDGE_BACKEND=cli cargo test --features tantivy-search -- --ignored replay_tantivy_enriched_llm_judge --nocapture
/// ```
#[test]
#[ignore]
#[cfg(feature = "tantivy-search")]
fn replay_tantivy_enriched_llm_judge() {
    use crate::config::RetrievalConfig;
    use crate::search::TantivySearchIndex;
    use crate::stats::scoring::{
        recency, recency_weight, reference_boost, CompositeScore, Strategy,
    };

    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() {
        eprintln!(
            "SKIPPING: transcript directory does not exist: {}",
            transcript_dir_str
        );
        return;
    }
    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    // Load config
    let config = crate::config::Config::load();
    let retrieval_config = RetrievalConfig::default();
    let backend = &config.judge.backend;
    let model = &config.judge.model;
    let api_url = &config.judge.api_url;
    let cache_path = if config.judge.cache_path.is_empty() {
        llm_judge_cache_path()
    } else {
        config.judge.cache_path.clone()
    };

    if backend == "api"
        && std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .is_none()
    {
        eprintln!("SKIPPING: backend=api but ANTHROPIC_API_KEY not set");
        return;
    }

    eprintln!(
        "Judge config: backend={}, model={}, api={}",
        backend, model, api_url
    );
    eprintln!("Cache path: {}", cache_path);
    eprintln!(
        "Adaptive config: min_confidence={:.3}, min_score_gap={:.3}, dynamic_k_ratio={:.3}",
        retrieval_config.min_confidence_threshold,
        retrieval_config.min_score_gap,
        retrieval_config.dynamic_k_ratio
    );

    // Load data
    let learnings = load_learnings(learnings_path);
    eprintln!("Loaded {} learnings", learnings.len());
    if learnings.is_empty() {
        eprintln!("SKIPPING: no learnings loaded");
        return;
    }

    let contexts = build_session_contexts(transcript_dir);
    eprintln!("Built session contexts for {} sessions", contexts.len());
    if contexts.is_empty() {
        eprintln!("SKIPPING: no session contexts built");
        return;
    }

    // Build Tantivy index
    let index = TantivySearchIndex::in_memory().expect("Failed to create Tantivy index");
    index
        .index_learnings(&learnings)
        .expect("Failed to index learnings");
    eprintln!("Built Tantivy index with {} documents", index.num_docs());

    // Build lookup maps
    let learning_map: BTreeMap<String, &CompoundLearning> =
        learnings.iter().map(|l| (l.id.clone(), l)).collect();
    let context_map: BTreeMap<String, &SessionContext> = contexts
        .iter()
        .map(|ctx| (ctx.session_file.clone(), ctx))
        .collect();

    // Load judge cache (shared across all judge tests)
    let mut cache = load_judge_cache_from(&cache_path);
    let initial_cache_size = cache.len();
    eprintln!("Loaded judge cache with {} entries", initial_cache_size);

    let judge = JudgeContext::from_config(&config.judge);

    let top_n = retrieval_config.max_injections as usize;
    let strategy = Strategy::Moderate;
    let now = chrono::Utc::now();

    // Accumulators
    let mut all_scores: Vec<f64> = Vec::new();
    let mut judge_calls = 0usize;
    let mut judge_failures = 0usize;
    let mut cache_hits = 0usize;
    let mut sessions_evaluated = 0usize;
    let mut sessions_suppressed = 0usize;
    let mut enrichment_additions = 0usize;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!(
        "BM25 + ADAPTIVE + DOMAIN ENRICHMENT LLM JUDGE -- {} backend ({})",
        backend, model
    );
    eprintln!("{}", "=".repeat(80));

    for ctx in &contexts {
        let session_ctx = match context_map.get(&ctx.session_file) {
            Some(c) => *c,
            None => continue,
        };

        let first_tc = match ctx.all_tool_calls.first() {
            Some(tc) => tc,
            None => continue,
        };

        // Build query from v2 keywords + domain enrichment
        let mut v2_keywords =
            extract_tool_input_keywords_v2(&first_tc.tool_name, &first_tc.tool_input);
        let domain_keywords = super::infer_domains_from_paths(&ctx.file_paths);
        let num_domain = domain_keywords.len();
        v2_keywords.extend(domain_keywords);

        let query_string = v2_keywords.join(" ");
        if query_string.trim().is_empty() {
            continue;
        }

        if num_domain > 0 {
            enrichment_additions += 1;
        }

        // BM25 match (fetch more than top_n so adaptive threshold has material to work with)
        let bm25_results = match index.search(&query_string, 20) {
            Ok(r) => r,
            Err(_) => continue,
        };

        if bm25_results.is_empty() {
            continue;
        }

        // Build composite scores (same as production pipeline)
        let learning_hash: std::collections::HashMap<String, &CompoundLearning> =
            learnings.iter().map(|l| (l.id.clone(), l)).collect();

        let max_bm25 = bm25_results
            .iter()
            .map(|r| r.score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut scored: Vec<CompositeScore> = bm25_results
            .iter()
            .filter_map(|r| {
                let learning = learning_hash.get(&r.id)?;
                let relevance = if max_bm25 < f32::EPSILON {
                    1.0
                } else {
                    (r.score / max_bm25) as f64
                };
                let half_life = retrieval_config.half_life_for_category(&learning.category);
                let lambda_cat = recency::lambda_from_half_life(half_life);
                let recency_val = recency_weight(learning.timestamp, now, lambda_cat);
                let ref_boost = reference_boost(None);
                Some(CompositeScore::new(
                    (*learning).clone(),
                    relevance,
                    recency_val,
                    ref_boost,
                    strategy,
                ))
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if scored.is_empty() {
            continue;
        }

        sessions_evaluated += 1;

        // Apply adaptive threshold + dynamic K
        let filtered = match super::apply_adaptive_threshold(
            scored,
            retrieval_config.min_confidence_threshold,
            retrieval_config.min_score_gap,
        ) {
            None => {
                sessions_suppressed += 1;
                let session_short = &ctx.session_file[..8.min(ctx.session_file.len())];
                eprintln!(
                    "\n  Session {} | SUPPRESSED (scores too clustered)",
                    session_short
                );
                continue;
            }
            Some(passed) => super::apply_dynamic_k(passed, retrieval_config.dynamic_k_ratio, top_n),
        };

        let session_short = &ctx.session_file[..8.min(ctx.session_file.len())];
        eprintln!(
            "\n  Session {} | {} tool calls | {} adaptive matches | {} domain keywords",
            session_short,
            ctx.all_tool_calls.len(),
            filtered.len(),
            num_domain,
        );

        // Judge each surviving learning
        for cs in &filtered {
            let learning = match learning_map.get(&cs.learning.id) {
                Some(l) => *l,
                None => continue,
            };

            let cache_key = judge_cache_key(&ctx.session_file, &cs.learning.id);
            let was_cached = cache.contains_key(&cache_key);

            match llm_judge_relevance(&ctx.session_file, learning, session_ctx, &mut cache, &judge)
            {
                Some(score) => {
                    all_scores.push(score);
                    if was_cached {
                        cache_hits += 1;
                    }
                    judge_calls += 1;

                    eprintln!(
                        "    score={} composite={:.3} {} | {}",
                        score,
                        cs.score,
                        cs.learning.id,
                        truncate_str(&learning.summary, 60)
                    );
                }
                None => {
                    judge_failures += 1;
                    judge_calls += 1;
                }
            }
        }

        // Persist cache after each session
        save_judge_cache_to(&cache, &cache_path);
    }

    // Aggregate report
    let avg = if all_scores.is_empty() {
        0.0
    } else {
        all_scores.iter().sum::<f64>() / all_scores.len() as f64
    };

    let mut sorted_scores = all_scores.clone();
    sorted_scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_val = if sorted_scores.is_empty() {
        0.0
    } else {
        let mid = sorted_scores.len() / 2;
        if sorted_scores.len().is_multiple_of(2) {
            (sorted_scores[mid - 1] + sorted_scores[mid]) / 2.0
        } else {
            sorted_scores[mid]
        }
    };

    // Score distribution
    let mut dist = [0usize; 5];
    for &s in &all_scores {
        let idx = (s as usize).saturating_sub(1).min(4);
        dist[idx] += 1;
    }

    let noise_count = all_scores.iter().filter(|&&s| s <= 2.0).count();
    let noise_pct = if all_scores.is_empty() {
        0.0
    } else {
        noise_count as f64 / all_scores.len() as f64 * 100.0
    };

    let new_cache_entries = cache.len() - initial_cache_size;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("BM25 + ADAPTIVE + DOMAIN ENRICHMENT LLM JUDGE AGGREGATE RESULTS");
    eprintln!("{}", "=".repeat(80));

    eprintln!("\n  Execution:");
    eprintln!("    Model:                       {}", model);
    eprintln!("    Total judge calls:           {}", judge_calls);
    eprintln!("    Cache hits:                  {}", cache_hits);
    eprintln!("    New judgments:                {}", new_cache_entries);
    eprintln!("    Failures:                    {}", judge_failures);
    eprintln!("    Total cache entries:          {}", cache.len());

    eprintln!("\n  Domain enrichment:");
    eprintln!(
        "    Sessions with domain keywords: {} / {}",
        enrichment_additions, sessions_evaluated
    );

    eprintln!("\n  Adaptive+enriched learnings:");
    eprintln!("    Sessions evaluated:          {}", sessions_evaluated);
    eprintln!("    Sessions suppressed:         {}", sessions_suppressed);
    eprintln!("    Pairs judged:                {}", all_scores.len());
    eprintln!("    Avg relevance:               {:.2}", avg);
    eprintln!("    Median relevance:            {:.1}", median_val);
    eprintln!(
        "    Distribution:                1={} 2={} 3={} 4={} 5={}",
        dist[0], dist[1], dist[2], dist[3], dist[4]
    );
    eprintln!(
        "    Noise (<=2):                 {} / {} ({:.0}%)",
        noise_count,
        all_scores.len(),
        noise_pct
    );

    eprintln!("\n  COMPARISON:");
    eprintln!("    Keyword-overlap baseline:    2.32 avg (256 pairs)");
    eprintln!("    BM25-only:                   2.76 avg (54% noise, 145 pairs)");
    eprintln!("    BM25 + adaptive:             2.88 avg (50% noise, 122 pairs)");
    eprintln!(
        "    BM25 + adaptive + enriched:  {:.2} avg ({:.0}% noise, {} pairs)",
        avg,
        noise_pct,
        all_scores.len()
    );

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("To reproduce: GROVE_LLM_JUDGE_BACKEND=cli cargo test --features tantivy-search -- --ignored replay_tantivy_enriched_llm_judge --nocapture");
    eprintln!("{}", "=".repeat(80));
}

/// Replay BM25 + adaptive threshold + user intent keywords with LLM judge.
///
/// Identical to `replay_tantivy_adaptive_llm_judge` except that keywords
/// from the user's first real transcript message are merged into the v2
/// keywords before building the BM25 query string.
///
/// Run with:
/// ```bash
/// GROVE_LLM_JUDGE_BACKEND=cli cargo test --features tantivy-search -- --ignored replay_tantivy_user_intent_llm_judge --nocapture
/// ```
#[test]
#[ignore]
#[cfg(feature = "tantivy-search")]
fn replay_tantivy_user_intent_llm_judge() {
    use crate::config::RetrievalConfig;
    use crate::search::TantivySearchIndex;
    use crate::stats::scoring::{
        recency, recency_weight, reference_boost, CompositeScore, Strategy,
    };

    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() {
        eprintln!(
            "SKIPPING: transcript directory does not exist: {}",
            transcript_dir_str
        );
        return;
    }
    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    // Load config
    let config = crate::config::Config::load();
    let retrieval_config = RetrievalConfig::default();
    let backend = &config.judge.backend;
    let model = &config.judge.model;
    let api_url = &config.judge.api_url;
    let cache_path = if config.judge.cache_path.is_empty() {
        llm_judge_cache_path()
    } else {
        config.judge.cache_path.clone()
    };

    if backend == "api"
        && std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .is_none()
    {
        eprintln!("SKIPPING: backend=api but ANTHROPIC_API_KEY not set");
        return;
    }

    eprintln!(
        "Judge config: backend={}, model={}, api={}",
        backend, model, api_url
    );
    eprintln!("Cache path: {}", cache_path);

    // Load data
    let learnings = load_learnings(learnings_path);
    eprintln!("Loaded {} learnings", learnings.len());
    if learnings.is_empty() {
        eprintln!("SKIPPING: no learnings loaded");
        return;
    }

    let contexts = build_session_contexts(transcript_dir);
    eprintln!("Built session contexts for {} sessions", contexts.len());
    if contexts.is_empty() {
        eprintln!("SKIPPING: no session contexts built");
        return;
    }

    // Build Tantivy index
    let index = TantivySearchIndex::in_memory().expect("Failed to create Tantivy index");
    index
        .index_learnings(&learnings)
        .expect("Failed to index learnings");
    eprintln!("Built Tantivy index with {} documents", index.num_docs());

    // Build lookup maps
    let learning_map: BTreeMap<String, &CompoundLearning> =
        learnings.iter().map(|l| (l.id.clone(), l)).collect();
    let context_map: BTreeMap<String, &SessionContext> = contexts
        .iter()
        .map(|ctx| (ctx.session_file.clone(), ctx))
        .collect();

    // Load judge cache
    let mut cache = load_judge_cache_from(&cache_path);
    let initial_cache_size = cache.len();
    eprintln!("Loaded judge cache with {} entries", initial_cache_size);

    let judge = JudgeContext::from_config(&config.judge);

    let top_n = retrieval_config.max_injections as usize;
    let strategy = Strategy::Moderate;
    let now = chrono::Utc::now();

    // Max user intent keywords to inject (cap to avoid BM25 flooding)
    let max_user_keywords = 15;

    // Accumulators
    let mut all_scores: Vec<f64> = Vec::new();
    let mut judge_calls = 0usize;
    let mut judge_failures = 0usize;
    let mut cache_hits = 0usize;
    let mut sessions_evaluated = 0usize;
    let mut sessions_suppressed = 0usize;
    let mut sessions_with_user_intent = 0usize;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!(
        "BM25 + ADAPTIVE + USER INTENT LLM JUDGE -- {} backend ({})",
        backend, model
    );
    eprintln!("Max user intent keywords: {}", max_user_keywords);
    eprintln!("{}", "=".repeat(80));

    for ctx in &contexts {
        let session_ctx = match context_map.get(&ctx.session_file) {
            Some(c) => *c,
            None => continue,
        };

        let first_tc = match ctx.all_tool_calls.first() {
            Some(tc) => tc,
            None => continue,
        };

        // Build query from v2 keywords + user intent
        let mut v2_keywords =
            extract_tool_input_keywords_v2(&first_tc.tool_name, &first_tc.tool_input);

        // Extract user intent from transcript
        let transcript_path = transcript_dir.join(&ctx.session_file);
        let user_keywords =
            super::extract_user_intent_keywords(&transcript_path, max_user_keywords);
        let num_user = user_keywords.len();
        v2_keywords.extend(user_keywords);

        let query_string = v2_keywords.join(" ");
        if query_string.trim().is_empty() {
            continue;
        }

        if num_user > 0 {
            sessions_with_user_intent += 1;
        }

        // BM25 match
        let bm25_results = match index.search(&query_string, 20) {
            Ok(r) => r,
            Err(_) => continue,
        };

        if bm25_results.is_empty() {
            continue;
        }

        // Build composite scores
        let learning_hash: std::collections::HashMap<String, &CompoundLearning> =
            learnings.iter().map(|l| (l.id.clone(), l)).collect();

        let max_bm25 = bm25_results
            .iter()
            .map(|r| r.score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut scored: Vec<CompositeScore> = bm25_results
            .iter()
            .filter_map(|r| {
                let learning = learning_hash.get(&r.id)?;
                let relevance = if max_bm25 < f32::EPSILON {
                    1.0
                } else {
                    (r.score / max_bm25) as f64
                };
                let half_life = retrieval_config.half_life_for_category(&learning.category);
                let lambda_cat = recency::lambda_from_half_life(half_life);
                let recency_val = recency_weight(learning.timestamp, now, lambda_cat);
                let ref_boost = reference_boost(None);
                Some(CompositeScore::new(
                    (*learning).clone(),
                    relevance,
                    recency_val,
                    ref_boost,
                    strategy,
                ))
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if scored.is_empty() {
            continue;
        }

        sessions_evaluated += 1;

        // Apply adaptive threshold + dynamic K
        let filtered = match super::apply_adaptive_threshold(
            scored,
            retrieval_config.min_confidence_threshold,
            retrieval_config.min_score_gap,
        ) {
            None => {
                sessions_suppressed += 1;
                let session_short = &ctx.session_file[..8.min(ctx.session_file.len())];
                eprintln!(
                    "\n  Session {} | SUPPRESSED (scores too clustered)",
                    session_short
                );
                continue;
            }
            Some(passed) => super::apply_dynamic_k(passed, retrieval_config.dynamic_k_ratio, top_n),
        };

        let session_short = &ctx.session_file[..8.min(ctx.session_file.len())];
        eprintln!(
            "\n  Session {} | {} tool calls | {} adaptive matches | {} user intent kws",
            session_short,
            ctx.all_tool_calls.len(),
            filtered.len(),
            num_user,
        );

        // Judge each surviving learning
        for cs in &filtered {
            let learning = match learning_map.get(&cs.learning.id) {
                Some(l) => *l,
                None => continue,
            };

            let cache_key = judge_cache_key(&ctx.session_file, &cs.learning.id);
            let was_cached = cache.contains_key(&cache_key);

            match llm_judge_relevance(&ctx.session_file, learning, session_ctx, &mut cache, &judge)
            {
                Some(score) => {
                    all_scores.push(score);
                    if was_cached {
                        cache_hits += 1;
                    }
                    judge_calls += 1;

                    eprintln!(
                        "    score={} composite={:.3} {} | {}",
                        score,
                        cs.score,
                        cs.learning.id,
                        truncate_str(&learning.summary, 60)
                    );
                }
                None => {
                    judge_failures += 1;
                    judge_calls += 1;
                }
            }
        }

        // Persist cache after each session
        save_judge_cache_to(&cache, &cache_path);
    }

    // Aggregate report
    let avg = if all_scores.is_empty() {
        0.0
    } else {
        all_scores.iter().sum::<f64>() / all_scores.len() as f64
    };

    let mut sorted_scores = all_scores.clone();
    sorted_scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_val = if sorted_scores.is_empty() {
        0.0
    } else {
        let mid = sorted_scores.len() / 2;
        if sorted_scores.len().is_multiple_of(2) {
            (sorted_scores[mid - 1] + sorted_scores[mid]) / 2.0
        } else {
            sorted_scores[mid]
        }
    };

    // Score distribution
    let mut dist = [0usize; 5];
    for &s in &all_scores {
        let idx = (s as usize).saturating_sub(1).min(4);
        dist[idx] += 1;
    }

    let noise_count = all_scores.iter().filter(|&&s| s <= 2.0).count();
    let noise_pct = if all_scores.is_empty() {
        0.0
    } else {
        noise_count as f64 / all_scores.len() as f64 * 100.0
    };

    let new_cache_entries = cache.len() - initial_cache_size;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("BM25 + ADAPTIVE + USER INTENT LLM JUDGE AGGREGATE RESULTS");
    eprintln!("{}", "=".repeat(80));

    eprintln!("\n  Execution:");
    eprintln!("    Model:                       {}", model);
    eprintln!("    Total judge calls:           {}", judge_calls);
    eprintln!("    Cache hits:                  {}", cache_hits);
    eprintln!("    New judgments:                {}", new_cache_entries);
    eprintln!("    Failures:                    {}", judge_failures);
    eprintln!("    Total cache entries:          {}", cache.len());

    eprintln!("\n  User intent:");
    eprintln!(
        "    Sessions with user intent:   {} / {}",
        sessions_with_user_intent, sessions_evaluated
    );
    eprintln!("    Max keywords per session:    {}", max_user_keywords);

    eprintln!("\n  Adaptive+user-intent learnings:");
    eprintln!("    Sessions evaluated:          {}", sessions_evaluated);
    eprintln!("    Sessions suppressed:         {}", sessions_suppressed);
    eprintln!("    Pairs judged:                {}", all_scores.len());
    eprintln!("    Avg relevance:               {:.2}", avg);
    eprintln!("    Median relevance:            {:.1}", median_val);
    eprintln!(
        "    Distribution:                1={} 2={} 3={} 4={} 5={}",
        dist[0], dist[1], dist[2], dist[3], dist[4]
    );
    eprintln!(
        "    Noise (<=2):                 {} / {} ({:.0}%)",
        noise_count,
        all_scores.len(),
        noise_pct
    );

    eprintln!("\n  COMPARISON:");
    eprintln!("    Keyword-overlap baseline:    2.32 avg (256 pairs)");
    eprintln!("    BM25-only:                   2.76 avg (54% noise, 145 pairs)");
    eprintln!("    BM25 + adaptive:             2.88 avg (50% noise, 122 pairs)");
    eprintln!(
        "    BM25 + adaptive + intent:    {:.2} avg ({:.0}% noise, {} pairs)",
        avg,
        noise_pct,
        all_scores.len()
    );

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("To reproduce: GROVE_LLM_JUDGE_BACKEND=cli cargo test --features tantivy-search -- --ignored replay_tantivy_user_intent_llm_judge --nocapture");
    eprintln!("{}", "=".repeat(80));
}

/// Replay BM25 + adaptive threshold with intent-based POST-RETRIEVAL FILTERING.
///
/// Unlike `replay_tantivy_user_intent_llm_judge` which expands the BM25 query
/// with user intent keywords (causing regression), this benchmark keeps the
/// BM25 query unchanged (tool_input keywords only) and applies user intent
/// as a filter AFTER adaptive threshold + dynamic K.
///
/// The hypothesis: intent-as-filter should reduce pair count (improving precision)
/// while preserving high-relevance matches, since those already overlap with
/// the user's stated intent.
///
/// Run with:
/// ```bash
/// GROVE_LLM_JUDGE_BACKEND=cli cargo test --features tantivy-search -- --ignored replay_tantivy_intent_filter_llm_judge --nocapture
/// ```
#[test]
#[ignore]
#[cfg(feature = "tantivy-search")]
fn replay_tantivy_intent_filter_llm_judge() {
    use crate::config::RetrievalConfig;
    use crate::search::TantivySearchIndex;
    use crate::stats::scoring::{
        recency, recency_weight, reference_boost, CompositeScore, Strategy,
    };

    let transcript_dir_str = transcript_dir();
    let transcript_dir = Path::new(&transcript_dir_str);
    let learnings_path_str = learnings_path();
    let learnings_path = Path::new(&learnings_path_str);

    if !transcript_dir.exists() {
        eprintln!(
            "SKIPPING: transcript directory does not exist: {}",
            transcript_dir_str
        );
        return;
    }
    if !learnings_path.exists() {
        eprintln!(
            "SKIPPING: learnings file does not exist: {}",
            learnings_path_str
        );
        return;
    }

    // Load config
    let config = crate::config::Config::load();
    let retrieval_config = RetrievalConfig::default();
    let backend = &config.judge.backend;
    let model = &config.judge.model;
    let api_url = &config.judge.api_url;
    let cache_path = if config.judge.cache_path.is_empty() {
        llm_judge_cache_path()
    } else {
        config.judge.cache_path.clone()
    };

    if backend == "api"
        && std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .is_none()
    {
        eprintln!("SKIPPING: backend=api but ANTHROPIC_API_KEY not set");
        return;
    }

    eprintln!(
        "Judge config: backend={}, model={}, api={}",
        backend, model, api_url
    );
    eprintln!("Cache path: {}", cache_path);

    // Load data
    let learnings = load_learnings(learnings_path);
    eprintln!("Loaded {} learnings", learnings.len());
    if learnings.is_empty() {
        eprintln!("SKIPPING: no learnings loaded");
        return;
    }

    let contexts = build_session_contexts(transcript_dir);
    eprintln!("Built session contexts for {} sessions", contexts.len());
    if contexts.is_empty() {
        eprintln!("SKIPPING: no session contexts built");
        return;
    }

    // Build Tantivy index
    let index = TantivySearchIndex::in_memory().expect("Failed to create Tantivy index");
    index
        .index_learnings(&learnings)
        .expect("Failed to index learnings");
    eprintln!("Built Tantivy index with {} documents", index.num_docs());

    // Build lookup maps
    let learning_map: BTreeMap<String, &CompoundLearning> =
        learnings.iter().map(|l| (l.id.clone(), l)).collect();
    let context_map: BTreeMap<String, &SessionContext> = contexts
        .iter()
        .map(|ctx| (ctx.session_file.clone(), ctx))
        .collect();

    // Load judge cache
    let mut cache = load_judge_cache_from(&cache_path);
    let initial_cache_size = cache.len();
    eprintln!("Loaded judge cache with {} entries", initial_cache_size);

    let judge = JudgeContext::from_config(&config.judge);

    let top_n = retrieval_config.max_injections as usize;
    let strategy = Strategy::Moderate;
    let now = chrono::Utc::now();

    // Intent filter config
    let max_user_keywords = 15;
    let min_overlap = 1; // Require at least 1 intent keyword in learning text

    // Accumulators
    let mut all_scores: Vec<f64> = Vec::new();
    let mut judge_calls = 0usize;
    let mut judge_failures = 0usize;
    let mut cache_hits = 0usize;
    let mut sessions_evaluated = 0usize;
    let mut sessions_suppressed = 0usize;
    let mut sessions_with_intent = 0usize;
    let mut total_pre_filter = 0usize;
    let mut total_post_filter = 0usize;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!(
        "BM25 + ADAPTIVE + INTENT FILTER LLM JUDGE -- {} backend ({})",
        backend, model
    );
    eprintln!(
        "Intent filter: max_keywords={}, min_overlap={}",
        max_user_keywords, min_overlap
    );
    eprintln!("{}", "=".repeat(80));

    for ctx in &contexts {
        let session_ctx = match context_map.get(&ctx.session_file) {
            Some(c) => *c,
            None => continue,
        };

        let first_tc = match ctx.all_tool_calls.first() {
            Some(tc) => tc,
            None => continue,
        };

        // Build query from v2 keywords ONLY (no expansion)
        let v2_keywords = extract_tool_input_keywords_v2(&first_tc.tool_name, &first_tc.tool_input);
        let query_string = v2_keywords.join(" ");
        if query_string.trim().is_empty() {
            continue;
        }

        // Extract user intent for post-retrieval filtering
        let transcript_path = transcript_dir.join(&ctx.session_file);
        let intent_keywords =
            super::extract_user_intent_keywords(&transcript_path, max_user_keywords);
        let has_intent = !intent_keywords.is_empty();

        // BM25 match
        let bm25_results = match index.search(&query_string, 20) {
            Ok(r) => r,
            Err(_) => continue,
        };

        if bm25_results.is_empty() {
            continue;
        }

        // Build composite scores
        let learning_hash: std::collections::HashMap<String, &CompoundLearning> =
            learnings.iter().map(|l| (l.id.clone(), l)).collect();

        let max_bm25 = bm25_results
            .iter()
            .map(|r| r.score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut scored: Vec<CompositeScore> = bm25_results
            .iter()
            .filter_map(|r| {
                let learning = learning_hash.get(&r.id)?;
                let relevance = if max_bm25 < f32::EPSILON {
                    1.0
                } else {
                    (r.score / max_bm25) as f64
                };
                let half_life = retrieval_config.half_life_for_category(&learning.category);
                let lambda_cat = recency::lambda_from_half_life(half_life);
                let recency_val = recency_weight(learning.timestamp, now, lambda_cat);
                let ref_boost = reference_boost(None);
                Some(CompositeScore::new(
                    (*learning).clone(),
                    relevance,
                    recency_val,
                    ref_boost,
                    strategy,
                ))
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if scored.is_empty() {
            continue;
        }

        sessions_evaluated += 1;

        // Apply adaptive threshold + dynamic K (same as production)
        let filtered = match super::apply_adaptive_threshold(
            scored,
            retrieval_config.min_confidence_threshold,
            retrieval_config.min_score_gap,
        ) {
            None => {
                sessions_suppressed += 1;
                let session_short = &ctx.session_file[..8.min(ctx.session_file.len())];
                eprintln!(
                    "\n  Session {} | SUPPRESSED (scores too clustered)",
                    session_short
                );
                continue;
            }
            Some(passed) => super::apply_dynamic_k(passed, retrieval_config.dynamic_k_ratio, top_n),
        };

        let pre_filter_count = filtered.len();
        total_pre_filter += pre_filter_count;

        // Apply intent-based post-retrieval filter
        let final_results: Vec<CompositeScore> = if has_intent {
            sessions_with_intent += 1;
            filtered
                .into_iter()
                .filter(|cs| {
                    super::learning_matches_intent(
                        &cs.learning.summary,
                        &cs.learning.detail,
                        &intent_keywords,
                        min_overlap,
                    )
                })
                .collect()
        } else {
            filtered
        };

        let post_filter_count = final_results.len();
        total_post_filter += post_filter_count;

        let session_short = &ctx.session_file[..8.min(ctx.session_file.len())];
        let filter_note = if has_intent && pre_filter_count != post_filter_count {
            format!(" | filtered {}/{}", post_filter_count, pre_filter_count)
        } else {
            String::new()
        };
        eprintln!(
            "\n  Session {} | {} tool calls | {} adaptive{} | {} intent kws",
            session_short,
            ctx.all_tool_calls.len(),
            pre_filter_count,
            filter_note,
            intent_keywords.len(),
        );

        if final_results.is_empty() {
            eprintln!("    (all learnings filtered by intent)");
            continue;
        }

        // Judge each surviving learning
        for cs in &final_results {
            let learning = match learning_map.get(&cs.learning.id) {
                Some(l) => *l,
                None => continue,
            };

            let cache_key = judge_cache_key(&ctx.session_file, &cs.learning.id);
            let was_cached = cache.contains_key(&cache_key);

            match llm_judge_relevance(&ctx.session_file, learning, session_ctx, &mut cache, &judge)
            {
                Some(score) => {
                    all_scores.push(score);
                    if was_cached {
                        cache_hits += 1;
                    }
                    judge_calls += 1;

                    eprintln!(
                        "    score={} composite={:.3} {} | {}",
                        score,
                        cs.score,
                        cs.learning.id,
                        truncate_str(&learning.summary, 60)
                    );
                }
                None => {
                    judge_failures += 1;
                    judge_calls += 1;
                }
            }
        }

        // Persist cache after each session
        save_judge_cache_to(&cache, &cache_path);
    }

    // Aggregate report
    let avg = if all_scores.is_empty() {
        0.0
    } else {
        all_scores.iter().sum::<f64>() / all_scores.len() as f64
    };

    let mut sorted_scores = all_scores.clone();
    sorted_scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_val = if sorted_scores.is_empty() {
        0.0
    } else {
        let mid = sorted_scores.len() / 2;
        if sorted_scores.len().is_multiple_of(2) {
            (sorted_scores[mid - 1] + sorted_scores[mid]) / 2.0
        } else {
            sorted_scores[mid]
        }
    };

    // Score distribution
    let mut dist = [0usize; 5];
    for &s in &all_scores {
        let idx = (s as usize).saturating_sub(1).min(4);
        dist[idx] += 1;
    }

    let noise_count = all_scores.iter().filter(|&&s| s <= 2.0).count();
    let noise_pct = if all_scores.is_empty() {
        0.0
    } else {
        noise_count as f64 / all_scores.len() as f64 * 100.0
    };

    let new_cache_entries = cache.len() - initial_cache_size;
    let filter_reduction = if total_pre_filter > 0 {
        (1.0 - total_post_filter as f64 / total_pre_filter as f64) * 100.0
    } else {
        0.0
    };

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("BM25 + ADAPTIVE + INTENT FILTER LLM JUDGE AGGREGATE RESULTS");
    eprintln!("{}", "=".repeat(80));

    eprintln!("\n  Execution:");
    eprintln!("    Model:                       {}", model);
    eprintln!("    Total judge calls:           {}", judge_calls);
    eprintln!("    Cache hits:                  {}", cache_hits);
    eprintln!("    New judgments:                {}", new_cache_entries);
    eprintln!("    Failures:                    {}", judge_failures);
    eprintln!("    Total cache entries:          {}", cache.len());

    eprintln!("\n  Intent filter:");
    eprintln!(
        "    Sessions with intent:        {} / {}",
        sessions_with_intent, sessions_evaluated
    );
    eprintln!("    Max keywords per session:    {}", max_user_keywords);
    eprintln!("    Min overlap required:        {}", min_overlap);
    eprintln!("    Pairs before filter:         {}", total_pre_filter);
    eprintln!("    Pairs after filter:          {}", total_post_filter);
    eprintln!("    Filter reduction:            {:.1}%", filter_reduction);

    eprintln!("\n  Intent-filtered learnings:");
    eprintln!("    Sessions evaluated:          {}", sessions_evaluated);
    eprintln!("    Sessions suppressed:         {}", sessions_suppressed);
    eprintln!("    Pairs judged:                {}", all_scores.len());
    eprintln!("    Avg relevance:               {:.2}", avg);
    eprintln!("    Median relevance:            {:.1}", median_val);
    eprintln!(
        "    Distribution:                1={} 2={} 3={} 4={} 5={}",
        dist[0], dist[1], dist[2], dist[3], dist[4]
    );
    eprintln!(
        "    Noise (<=2):                 {} / {} ({:.0}%)",
        noise_count,
        all_scores.len(),
        noise_pct
    );

    eprintln!("\n  COMPARISON:");
    eprintln!("    Keyword-overlap baseline:    2.32 avg (256 pairs)");
    eprintln!("    BM25-only:                   2.76 avg (54% noise, 145 pairs)");
    eprintln!("    BM25 + adaptive:             2.88 avg (50% noise, 122 pairs)");
    eprintln!("    BM25 + adaptive + expand:    2.69 avg (57% noise, 150 pairs)");
    eprintln!(
        "    BM25 + adaptive + filter:    {:.2} avg ({:.0}% noise, {} pairs)",
        avg,
        noise_pct,
        all_scores.len()
    );

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("To reproduce: GROVE_LLM_JUDGE_BACKEND=cli cargo test --features tantivy-search -- --ignored replay_tantivy_intent_filter_llm_judge --nocapture");
    eprintln!("{}", "=".repeat(80));
}
