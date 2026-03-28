//! Consolidate command for Grove.
//!
//! LLM-powered corpus maintenance: groups related learnings by tag similarity,
//! merges overlapping learnings via LLM, and detects stale file references.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::backends::MemoryBackend;
use crate::cli::maintain::FailedUpdate;
use crate::config::{Config, JudgeConfig};
use crate::core::{
    CompoundLearning, Confidence, LearningCategory, LearningScope, LearningStatus,
    WriteGateCriterion,
};
use crate::llm;

// ---------------------------------------------------------------------------
// Thresholds
// ---------------------------------------------------------------------------

/// Minimum Jaccard similarity for same-category grouping.
const MIN_JACCARD_SAME_CATEGORY: f64 = 0.5;
/// Minimum Jaccard similarity for cross-category grouping.
const MIN_JACCARD_ANY_CATEGORY: f64 = 0.7;
/// Learnings with fewer than this many tags are excluded from grouping.
const MIN_TAGS_FOR_GROUPING: usize = 2;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Options for the consolidate subcommand.
#[derive(Debug, Clone, Default)]
pub struct ConsolidateOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Apply changes (default: dry-run).
    pub apply: bool,
    /// Only detect stale references, skip LLM merge.
    pub stale_only: bool,
}

/// A group of related learnings identified for potential merge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningGroup {
    /// IDs of learnings in this group.
    pub learning_ids: Vec<String>,
    /// Summaries of learnings in this group (for display).
    pub summaries: Vec<String>,
    /// Shared tags across the group.
    pub shared_tags: Vec<String>,
    /// Shared category name (if all learnings share one).
    pub category: Option<String>,
    /// Average pairwise Jaccard similarity.
    pub similarity: f64,
}

/// A stale reference detected in a learning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleReference {
    /// Learning ID containing the stale reference.
    pub learning_id: String,
    /// Summary of the learning (for display).
    pub learning_summary: String,
    /// The file path that is stale.
    pub reference: String,
    /// Why it's considered stale.
    pub reason: String,
}

/// A proposed merge from the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeProposal {
    /// The group that was merged.
    pub group: LearningGroup,
    /// The merged summary.
    pub merged_summary: String,
    /// The merged detail.
    pub merged_detail: String,
    /// Tags for the merged learning.
    pub merged_tags: Vec<String>,
    /// Category for the merged learning.
    pub category: String,
}

/// Output of the consolidate command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidateOutput {
    /// Whether the operation was successful.
    pub success: bool,
    /// Groups of related learnings found.
    pub groups: Vec<LearningGroup>,
    /// Stale references detected.
    pub stale_references: Vec<StaleReference>,
    /// Merge proposals (empty in stale-only mode).
    pub merge_proposals: Vec<MergeProposal>,
    /// IDs of learnings that were archived (only in apply mode).
    pub archived: Vec<String>,
    /// IDs of new merged learnings written (only in apply mode).
    pub written: Vec<String>,
    /// Failures encountered.
    pub failed: Vec<FailedUpdate>,
    /// Error message if operation failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ConsolidateOutput {
    fn success(
        groups: Vec<LearningGroup>,
        stale_references: Vec<StaleReference>,
        merge_proposals: Vec<MergeProposal>,
    ) -> Self {
        Self {
            success: true,
            groups,
            stale_references,
            merge_proposals,
            archived: Vec::new(),
            written: Vec::new(),
            failed: Vec::new(),
            error: None,
        }
    }

    fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            groups: Vec::new(),
            stale_references: Vec::new(),
            merge_proposals: Vec::new(),
            archived: Vec::new(),
            written: Vec::new(),
            failed: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// LLM response for a merge operation.
#[derive(Debug, Deserialize)]
struct MergeResponse {
    summary: String,
    detail: String,
    tags: Vec<String>,
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

/// Compute Jaccard similarity between two tag sets.
fn jaccard_similarity(a: &[String], b: &[String]) -> f64 {
    let set_a: HashSet<&str> = a.iter().map(|s| s.as_str()).collect();
    let set_b: HashSet<&str> = b.iter().map(|s| s.as_str()).collect();
    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

/// Simple union-find (disjoint set) for clustering.
struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

/// Group related active learnings by tag Jaccard similarity.
pub fn group_related_learnings(learnings: &[CompoundLearning]) -> Vec<LearningGroup> {
    // Filter to active learnings with enough tags
    let eligible: Vec<&CompoundLearning> = learnings
        .iter()
        .filter(|l| l.status == LearningStatus::Active && l.tags.len() >= MIN_TAGS_FOR_GROUPING)
        .collect();

    if eligible.len() < 2 {
        return Vec::new();
    }

    let mut uf = UnionFind::new(eligible.len());

    // Pairwise comparison
    for i in 0..eligible.len() {
        for j in (i + 1)..eligible.len() {
            let sim = jaccard_similarity(&eligible[i].tags, &eligible[j].tags);
            let same_cat = eligible[i].category == eligible[j].category;
            let threshold = if same_cat {
                MIN_JACCARD_SAME_CATEGORY
            } else {
                MIN_JACCARD_ANY_CATEGORY
            };
            if sim >= threshold {
                uf.union(i, j);
            }
        }
    }

    // Collect clusters
    let mut clusters: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..eligible.len() {
        let root = uf.find(i);
        clusters.entry(root).or_default().push(i);
    }

    // Build LearningGroups, filter singletons
    let mut groups: Vec<LearningGroup> = clusters
        .into_values()
        .filter(|members| members.len() >= 2)
        .map(|members| {
            let learning_ids: Vec<String> =
                members.iter().map(|&i| eligible[i].id.clone()).collect();
            let summaries: Vec<String> = members
                .iter()
                .map(|&i| eligible[i].summary.clone())
                .collect();

            // Shared tags = intersection of all members' tags
            let tag_sets: Vec<BTreeSet<&str>> = members
                .iter()
                .map(|&i| eligible[i].tags.iter().map(|t| t.as_str()).collect())
                .collect();
            let shared: BTreeSet<&str> = if let Some(first) = tag_sets.first().cloned() {
                tag_sets
                    .iter()
                    .skip(1)
                    .fold(first, |acc, s| acc.intersection(s).copied().collect())
            } else {
                BTreeSet::new()
            };
            let shared_tags: Vec<String> = shared.into_iter().map(|s| s.to_string()).collect();

            // Category if all same
            let first_cat = &eligible[members[0]].category;
            let category = if members.iter().all(|&i| &eligible[i].category == first_cat) {
                Some(format!("{:?}", first_cat).to_lowercase())
            } else {
                None
            };

            // Average pairwise similarity
            let mut total_sim = 0.0;
            let mut pair_count = 0;
            for (idx_a, &a) in members.iter().enumerate() {
                for &b in members.iter().skip(idx_a + 1) {
                    total_sim += jaccard_similarity(&eligible[a].tags, &eligible[b].tags);
                    pair_count += 1;
                }
            }
            let similarity = if pair_count > 0 {
                total_sim / pair_count as f64
            } else {
                0.0
            };

            LearningGroup {
                learning_ids,
                summaries,
                shared_tags,
                category,
                similarity,
            }
        })
        .collect();

    // Sort by group size descending
    groups.sort_by(|a, b| b.learning_ids.len().cmp(&a.learning_ids.len()));
    groups
}

// ---------------------------------------------------------------------------
// Staleness detection
// ---------------------------------------------------------------------------

/// Detect stale file references in active learnings.
pub fn detect_stale_references(
    learnings: &[CompoundLearning],
    project_root: &Path,
) -> Vec<StaleReference> {
    let mut stale = Vec::new();
    for learning in learnings {
        if learning.status != LearningStatus::Active {
            continue;
        }
        if let Some(ref files) = learning.context_files {
            for file_path in files {
                let full_path = project_root.join(file_path);
                if !full_path.exists() {
                    stale.push(StaleReference {
                        learning_id: learning.id.clone(),
                        learning_summary: learning.summary.clone(),
                        reference: file_path.clone(),
                        reason: "file not found".to_string(),
                    });
                }
            }
        }
    }
    stale
}

// ---------------------------------------------------------------------------
// LLM merge
// ---------------------------------------------------------------------------

/// Type alias for the LLM merge function, enabling test injection.
pub type MergeFn = dyn Fn(&str, &str) -> Option<String>;

/// Boxed merge function for owned contexts (closures, default caller).
pub type MergeFnBox = Box<dyn Fn(&str, &str) -> Option<String>>;

const MERGE_SYSTEM_PROMPT: &str = r#"You are a technical writing editor for a developer learning corpus. You merge overlapping learnings into a single canonical learning that preserves all unique insights.

Given a group of related learnings, produce ONE merged learning in JSON:
{
  "summary": "...",
  "detail": "...",
  "tags": ["..."]
}

Rules:
- Preserve every distinct insight from the source learnings
- Do not invent information not present in the sources
- Use the most specific and precise language from the sources
- summary: 10-200 characters, covers the combined insight
- detail: 20-2000 characters, merges all unique details
- tags: union of all relevant tags, deduplicated
- If learnings conflict, note the conflict in the detail

Respond with ONLY the JSON object. No explanation, no markdown fences."#;

/// Build the user prompt for a merge request.
fn build_merge_user_prompt(learnings: &[CompoundLearning], group: &LearningGroup) -> String {
    let learning_map: HashMap<&str, &CompoundLearning> =
        learnings.iter().map(|l| (l.id.as_str(), l)).collect();

    let mut parts = Vec::new();
    parts.push("Merge the following learnings into one:\n".to_string());

    for id in &group.learning_ids {
        if let Some(learning) = learning_map.get(id.as_str()) {
            parts.push(format!(
                "---\nID: {}\nCategory: {:?}\nSummary: {}\nDetail: {}\nTags: {}\n",
                learning.id,
                learning.category,
                learning.summary,
                learning.detail,
                learning.tags.join(", ")
            ));
        }
    }

    parts.join("\n")
}

/// Create a default LLM merge caller from judge config.
pub fn default_merge_caller(config: &JudgeConfig) -> MergeFnBox {
    let backend = config.backend.clone();
    let model = config.model.clone();
    let api_url = config.api_url.clone();

    Box::new(
        move |system_prompt: &str, user_prompt: &str| -> Option<String> {
            match backend.as_str() {
                "api" => llm::call_llm_api(&model, &api_url, system_prompt, user_prompt, 1024),
                _ => llm::call_llm_cli(&model, system_prompt, user_prompt),
            }
        },
    )
}

/// Strip markdown fences and leading/trailing whitespace from LLM responses.
fn clean_llm_response(raw: &str) -> &str {
    let trimmed = raw.trim();

    // Strip ```json ... ``` or ``` ... ```
    if let Some(rest) = trimmed.strip_prefix("```") {
        // Skip optional language tag on the first line
        let rest = if let Some(newline_pos) = rest.find('\n') {
            &rest[newline_pos + 1..]
        } else {
            rest
        };
        if let Some(body) = rest.strip_suffix("```") {
            return body.trim();
        }
    }

    trimmed
}

/// Attempt to merge a group of learnings via LLM.
fn merge_group(
    learnings: &[CompoundLearning],
    group: &LearningGroup,
    merge_fn: &MergeFn,
) -> Option<MergeProposal> {
    let user_prompt = build_merge_user_prompt(learnings, group);
    let response = merge_fn(MERGE_SYSTEM_PROMPT, &user_prompt)?;
    let cleaned = clean_llm_response(&response);

    let parsed: MergeResponse = match serde_json::from_str(cleaned) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "Warning: failed to parse LLM merge response for group {:?}: {}",
                group.learning_ids, e
            );
            return None;
        }
    };

    Some(MergeProposal {
        group: group.clone(),
        merged_summary: parsed.summary,
        merged_detail: parsed.detail,
        merged_tags: parsed.tags,
        category: group
            .category
            .clone()
            .unwrap_or_else(|| "pattern".to_string()),
    })
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Run the consolidate command.
pub fn run_consolidate<B: MemoryBackend>(
    backend: &B,
    _config: &Config,
    options: &ConsolidateOptions,
    project_root: &Path,
    merge_fn: &MergeFn,
) -> ConsolidateOutput {
    // 1. Load all learnings
    let all_learnings = match backend.list_all() {
        Ok(l) => l,
        Err(e) => return ConsolidateOutput::failure(format!("Failed to list learnings: {}", e)),
    };

    let active: Vec<&CompoundLearning> = all_learnings
        .iter()
        .filter(|l| l.status == LearningStatus::Active)
        .collect();

    if !options.quiet {
        eprintln!("Scanning {} active learning(s)...", active.len());
    }

    // 2. Staleness detection (always runs)
    let stale_references = detect_stale_references(&all_learnings, project_root);

    if !options.quiet && !stale_references.is_empty() {
        eprintln!("Found {} stale reference(s)", stale_references.len());
    }

    // 3. Early return for stale-only mode
    if options.stale_only {
        return ConsolidateOutput::success(Vec::new(), stale_references, Vec::new());
    }

    // 4. Group related learnings
    let active_owned: Vec<CompoundLearning> = active.into_iter().cloned().collect();
    let groups = group_related_learnings(&active_owned);

    if !options.quiet {
        if groups.is_empty() {
            eprintln!("No related groups found");
        } else {
            eprintln!("Found {} group(s), merging via LLM...", groups.len());
        }
    }

    // 5. Merge each group via LLM
    let mut merge_proposals = Vec::new();
    for (i, group) in groups.iter().enumerate() {
        if !options.quiet {
            eprintln!(
                "  Merging group {}/{} ({} learnings)...",
                i + 1,
                groups.len(),
                group.learning_ids.len()
            );
        }
        match merge_group(&all_learnings, group, merge_fn) {
            Some(proposal) => merge_proposals.push(proposal),
            None => {
                eprintln!(
                    "Warning: skipping group {:?} (LLM merge failed)",
                    group.learning_ids
                );
            }
        }
    }

    // 6. Dry-run: return proposals without applying
    if !options.apply {
        return ConsolidateOutput::success(groups, stale_references, merge_proposals);
    }

    // 7. Apply: write merged learnings, archive sources
    let mut archived = Vec::new();
    let mut written = Vec::new();
    let mut failed = Vec::new();

    for proposal in &merge_proposals {
        // Parse category from the proposal
        let category = parse_category(&proposal.category);

        // Create merged learning
        let mut merged = CompoundLearning::new(
            category,
            &proposal.merged_summary,
            &proposal.merged_detail,
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            proposal.merged_tags.clone(),
            "consolidate",
        );
        merged.id = backend.next_id();

        // Write merged learning
        match backend.write(&merged) {
            Ok(result) if result.success => {
                written.push(result.learning_id);
            }
            Ok(result) => {
                failed.push(FailedUpdate {
                    id: merged.id.clone(),
                    error: result.message.unwrap_or_else(|| "write failed".to_string()),
                });
                continue; // Don't archive sources if write failed
            }
            Err(e) => {
                failed.push(FailedUpdate {
                    id: merged.id.clone(),
                    error: e.to_string(),
                });
                continue;
            }
        }

        // Archive source learnings
        for source_id in &proposal.group.learning_ids {
            match backend.archive(source_id) {
                Ok(()) => archived.push(source_id.clone()),
                Err(e) => failed.push(FailedUpdate {
                    id: source_id.clone(),
                    error: e.to_string(),
                }),
            }
        }
    }

    let mut output = ConsolidateOutput::success(groups, stale_references, merge_proposals);
    output.archived = archived;
    output.written = written;
    output.failed = failed;
    output
}

/// Parse a category string back to LearningCategory.
fn parse_category(s: &str) -> LearningCategory {
    match s.to_lowercase().as_str() {
        "pattern" => LearningCategory::Pattern,
        "pitfall" => LearningCategory::Pitfall,
        "convention" => LearningCategory::Convention,
        "dependency" => LearningCategory::Dependency,
        "process" => LearningCategory::Process,
        "domain" => LearningCategory::Domain,
        "debugging" => LearningCategory::Debugging,
        _ => LearningCategory::Pattern,
    }
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

/// Format consolidate output for display.
pub fn format_output(output: &ConsolidateOutput, options: &ConsolidateOptions) -> String {
    if options.quiet {
        return String::new();
    }

    if options.json {
        return serde_json::to_string_pretty(output).unwrap_or_else(|_| "{}".to_string());
    }

    if !output.success {
        return format!(
            "Consolidation failed: {}\n",
            output.error.as_deref().unwrap_or("unknown error")
        );
    }

    let mut lines = Vec::new();

    let mode = if output.archived.is_empty() && output.written.is_empty() {
        "dry run"
    } else {
        "applied"
    };
    lines.push(format!("Consolidation analysis ({}):\n", mode));

    // Groups and merge proposals
    if output.merge_proposals.is_empty() && output.groups.is_empty() {
        lines.push("No related learning groups found.\n".to_string());
    } else if !output.merge_proposals.is_empty() {
        lines.push(format!(
            "Found {} merge proposal(s):\n",
            output.merge_proposals.len()
        ));
        for (i, proposal) in output.merge_proposals.iter().enumerate() {
            let shared = if proposal.group.shared_tags.is_empty() {
                String::new()
            } else {
                format!(
                    ", shared: {}",
                    proposal
                        .group
                        .shared_tags
                        .iter()
                        .map(|t| format!("#{}", t))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            lines.push(format!(
                "  Group {} ({} learnings{}):",
                i + 1,
                proposal.group.learning_ids.len(),
                shared
            ));
            for (j, summary) in proposal.group.summaries.iter().enumerate() {
                lines.push(format!(
                    "    - {}: {}",
                    proposal.group.learning_ids[j], summary
                ));
            }
            lines.push(format!(
                "    Proposed merge: \"{}\"",
                proposal.merged_summary
            ));
            lines.push(String::new());
        }
    } else {
        lines.push(format!(
            "Found {} group(s) but no merge proposals (LLM unavailable).\n",
            output.groups.len()
        ));
    }

    // Stale references
    if !output.stale_references.is_empty() {
        lines.push(format!(
            "Found {} stale reference(s):",
            output.stale_references.len()
        ));
        for stale in &output.stale_references {
            lines.push(format!(
                "  - {}: {} ({})",
                stale.learning_id, stale.reference, stale.reason
            ));
        }
        lines.push(String::new());
    }

    // Applied results
    if !output.written.is_empty() {
        lines.push(format!(
            "Wrote {} merged learning(s):",
            output.written.len()
        ));
        for id in &output.written {
            lines.push(format!("  + {}", id));
        }
        lines.push(String::new());
    }
    if !output.archived.is_empty() {
        lines.push(format!(
            "Archived {} source learning(s):",
            output.archived.len()
        ));
        for id in &output.archived {
            lines.push(format!("  - {}", id));
        }
        lines.push(String::new());
    }

    // Failures
    if !output.failed.is_empty() {
        lines.push(format!("Failed operations ({}):", output.failed.len()));
        for fail in &output.failed {
            lines.push(format!("  ! {}: {}", fail.id, fail.error));
        }
        lines.push(String::new());
    }

    // Hint
    if mode == "dry run"
        && (!output.merge_proposals.is_empty() || !output.stale_references.is_empty())
    {
        lines.push("Run 'grove maintain consolidate --apply' to apply changes.".to_string());
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::MarkdownBackend;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    fn make_learning(
        id: &str,
        category: LearningCategory,
        summary: &str,
        tags: Vec<&str>,
    ) -> CompoundLearning {
        let mut l = CompoundLearning::new(
            category,
            summary,
            format!("Detail for {}", summary),
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            tags.into_iter().map(|t| t.to_string()).collect(),
            "test-session",
        );
        l.id = id.to_string();
        l
    }

    fn make_learning_with_files(
        id: &str,
        summary: &str,
        tags: Vec<&str>,
        files: Vec<&str>,
    ) -> CompoundLearning {
        let mut l = make_learning(id, LearningCategory::Pattern, summary, tags);
        l.context_files = Some(files.into_iter().map(|f| f.to_string()).collect());
        l
    }

    // -- Jaccard similarity --

    #[test]
    fn test_jaccard_similarity_identical() {
        let a = vec!["rust".to_string(), "testing".to_string()];
        let b = vec!["rust".to_string(), "testing".to_string()];
        assert!((jaccard_similarity(&a, &b) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_jaccard_similarity_disjoint() {
        let a = vec!["rust".to_string(), "testing".to_string()];
        let b = vec!["python".to_string(), "deployment".to_string()];
        assert!((jaccard_similarity(&a, &b)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_jaccard_similarity_partial() {
        let a = vec!["rust".to_string(), "testing".to_string(), "cli".to_string()];
        let b = vec!["rust".to_string(), "testing".to_string(), "api".to_string()];
        // intersection = {rust, testing} = 2, union = {rust, testing, cli, api} = 4
        assert!((jaccard_similarity(&a, &b) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_jaccard_similarity_empty() {
        let a: Vec<String> = Vec::new();
        let b: Vec<String> = Vec::new();
        assert!((jaccard_similarity(&a, &b)).abs() < f64::EPSILON);
    }

    // -- Grouping --

    #[test]
    fn test_group_related_same_category() {
        let learnings = vec![
            make_learning(
                "cl_001",
                LearningCategory::Pattern,
                "Error handling pattern",
                vec!["rust", "error-handling", "anyhow"],
            ),
            make_learning(
                "cl_002",
                LearningCategory::Pattern,
                "Context on errors",
                vec!["rust", "error-handling", "context"],
            ),
        ];
        // Jaccard = {rust, error-handling} / {rust, error-handling, anyhow, context} = 2/4 = 0.5
        let groups = group_related_learnings(&learnings);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].learning_ids.len(), 2);
        assert_eq!(groups[0].category, Some("pattern".to_string()));
    }

    #[test]
    fn test_group_related_cross_category() {
        let learnings = vec![
            make_learning(
                "cl_001",
                LearningCategory::Pattern,
                "Pattern A",
                vec!["rust", "testing", "mock"],
            ),
            make_learning(
                "cl_002",
                LearningCategory::Pitfall,
                "Pitfall B",
                vec!["rust", "testing", "mock", "extra"],
            ),
        ];
        // Jaccard = {rust, testing, mock} / {rust, testing, mock, extra} = 3/4 = 0.75 >= 0.7
        let groups = group_related_learnings(&learnings);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].category.is_none()); // different categories
    }

    #[test]
    fn test_group_related_cross_category_below_threshold() {
        let learnings = vec![
            make_learning(
                "cl_001",
                LearningCategory::Pattern,
                "Pattern A",
                vec!["rust", "testing"],
            ),
            make_learning(
                "cl_002",
                LearningCategory::Pitfall,
                "Pitfall B",
                vec!["rust", "deployment"],
            ),
        ];
        // Jaccard = {rust} / {rust, testing, deployment} = 1/3 = 0.33 < 0.7
        let groups = group_related_learnings(&learnings);
        assert_eq!(groups.len(), 0);
    }

    #[test]
    fn test_group_transitive_closure() {
        let learnings = vec![
            make_learning(
                "cl_001",
                LearningCategory::Pattern,
                "A",
                vec!["rust", "error", "anyhow"],
            ),
            make_learning(
                "cl_002",
                LearningCategory::Pattern,
                "B",
                vec!["rust", "error", "thiserror"],
            ),
            make_learning(
                "cl_003",
                LearningCategory::Pattern,
                "C",
                vec!["rust", "thiserror", "derive"],
            ),
        ];
        // A~B: {rust, error} / {rust, error, anyhow, thiserror} = 2/4 = 0.5 (same cat, >=0.5) YES
        // B~C: {rust, thiserror} / {rust, error, thiserror, derive} = 2/4 = 0.5 (same cat, >=0.5) YES
        // A~C: {rust} / {rust, error, anyhow, thiserror, derive} = 1/5 = 0.2 NO
        // But transitively: A~B~C => one group
        let groups = group_related_learnings(&learnings);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].learning_ids.len(), 3);
    }

    #[test]
    fn test_group_skips_low_tag_learnings() {
        let learnings = vec![
            make_learning("cl_001", LearningCategory::Pattern, "A", vec!["rust"]), // only 1 tag
            make_learning(
                "cl_002",
                LearningCategory::Pattern,
                "B",
                vec!["rust", "testing"],
            ),
        ];
        let groups = group_related_learnings(&learnings);
        assert_eq!(groups.len(), 0); // cl_001 excluded, cl_002 alone = singleton
    }

    #[test]
    fn test_group_no_singletons() {
        let learnings = vec![
            make_learning(
                "cl_001",
                LearningCategory::Pattern,
                "A",
                vec!["rust", "testing"],
            ),
            make_learning(
                "cl_002",
                LearningCategory::Pitfall,
                "B",
                vec!["python", "deployment"],
            ),
        ];
        // No overlap => no groups
        let groups = group_related_learnings(&learnings);
        assert_eq!(groups.len(), 0);
    }

    #[test]
    fn test_group_empty_input() {
        let groups = group_related_learnings(&[]);
        assert_eq!(groups.len(), 0);
    }

    #[test]
    fn test_group_skips_non_active() {
        let mut l1 = make_learning(
            "cl_001",
            LearningCategory::Pattern,
            "A",
            vec!["rust", "error", "anyhow"],
        );
        let l2 = make_learning(
            "cl_002",
            LearningCategory::Pattern,
            "B",
            vec!["rust", "error", "thiserror"],
        );
        l1.status = LearningStatus::Archived;
        let groups = group_related_learnings(&[l1, l2]);
        assert_eq!(groups.len(), 0); // l1 is archived, l2 alone = singleton
    }

    // -- Staleness detection --

    #[test]
    fn test_stale_reference_missing_file() {
        let temp = TempDir::new().unwrap();
        let learnings = vec![make_learning_with_files(
            "cl_001",
            "Test learning",
            vec!["rust", "testing"],
            vec!["src/nonexistent.rs"],
        )];
        let stale = detect_stale_references(&learnings, temp.path());
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].learning_id, "cl_001");
        assert_eq!(stale[0].reference, "src/nonexistent.rs");
        assert_eq!(stale[0].reason, "file not found");
    }

    #[test]
    fn test_stale_reference_existing_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("src");
        fs::create_dir_all(&file_path).unwrap();
        fs::write(file_path.join("lib.rs"), "fn main() {}").unwrap();

        let learnings = vec![make_learning_with_files(
            "cl_001",
            "Test learning",
            vec!["rust", "testing"],
            vec!["src/lib.rs"],
        )];
        let stale = detect_stale_references(&learnings, temp.path());
        assert_eq!(stale.len(), 0);
    }

    #[test]
    fn test_stale_reference_no_context_files() {
        let temp = TempDir::new().unwrap();
        let learnings = vec![make_learning(
            "cl_001",
            LearningCategory::Pattern,
            "Test learning",
            vec!["rust", "testing"],
        )];
        let stale = detect_stale_references(&learnings, temp.path());
        assert_eq!(stale.len(), 0);
    }

    #[test]
    fn test_stale_reference_skips_non_active() {
        let temp = TempDir::new().unwrap();
        let mut l = make_learning_with_files(
            "cl_001",
            "Test learning",
            vec!["rust", "testing"],
            vec!["src/nonexistent.rs"],
        );
        l.status = LearningStatus::Archived;
        let stale = detect_stale_references(&[l], temp.path());
        assert_eq!(stale.len(), 0);
    }

    // -- Mock LLM helpers --

    fn mock_merge_fn(response: &str) -> MergeFnBox {
        let response = response.to_string();
        Box::new(move |_system: &str, _user: &str| Some(response.clone()))
    }

    fn mock_merge_fn_fail() -> MergeFnBox {
        Box::new(|_system: &str, _user: &str| None)
    }

    fn mock_merge_fn_counting(response: &str, counter: &'static AtomicUsize) -> MergeFnBox {
        let response = response.to_string();
        Box::new(move |_system: &str, _user: &str| {
            counter.fetch_add(1, Ordering::SeqCst);
            Some(response.clone())
        })
    }

    fn valid_merge_response() -> String {
        serde_json::json!({
            "summary": "Merged: error handling with context",
            "detail": "Use anyhow with .context() for all error propagation in the codebase.",
            "tags": ["rust", "error-handling", "anyhow", "context"]
        })
        .to_string()
    }

    fn setup_backend_with_learnings(learnings: &[CompoundLearning]) -> (TempDir, MarkdownBackend) {
        let temp = TempDir::new().unwrap();
        let learnings_path = temp.path().join(".grove").join("learnings.md");
        fs::create_dir_all(learnings_path.parent().unwrap()).unwrap();

        let backend = MarkdownBackend::new(&learnings_path);
        for learning in learnings {
            backend.write(learning).unwrap();
        }
        (temp, backend)
    }

    // -- Integration tests --

    #[test]
    fn test_consolidate_empty_corpus() {
        let temp = TempDir::new().unwrap();
        let learnings_path = temp.path().join(".grove").join("learnings.md");
        fs::create_dir_all(learnings_path.parent().unwrap()).unwrap();
        let backend = MarkdownBackend::new(&learnings_path);
        let config = Config::default();
        let options = ConsolidateOptions::default();
        let merge_fn = mock_merge_fn_fail();

        let output = run_consolidate(&backend, &config, &options, temp.path(), &merge_fn);

        assert!(output.success);
        assert!(output.groups.is_empty());
        assert!(output.merge_proposals.is_empty());
        assert!(output.stale_references.is_empty());
    }

    #[test]
    fn test_consolidate_dry_run() {
        let learnings = vec![
            make_learning(
                "cl_001",
                LearningCategory::Pattern,
                "Error handling pattern",
                vec!["rust", "error-handling", "anyhow"],
            ),
            make_learning(
                "cl_002",
                LearningCategory::Pattern,
                "Context on errors",
                vec!["rust", "error-handling", "context"],
            ),
        ];
        let (_temp, backend) = setup_backend_with_learnings(&learnings);
        let config = Config::default();
        let options = ConsolidateOptions::default(); // apply = false
        let merge_fn = mock_merge_fn(&valid_merge_response());

        let output = run_consolidate(&backend, &config, &options, _temp.path(), &merge_fn);

        assert!(output.success);
        assert_eq!(output.merge_proposals.len(), 1);
        assert_eq!(
            output.merge_proposals[0].merged_summary,
            "Merged: error handling with context"
        );
        // Dry run: nothing written or archived
        assert!(output.written.is_empty());
        assert!(output.archived.is_empty());

        // Verify source learnings are still active
        let all = backend.list_all().unwrap();
        assert!(all.iter().all(|l| l.status == LearningStatus::Active));
    }

    #[test]
    fn test_consolidate_apply() {
        let learnings = vec![
            make_learning(
                "cl_001",
                LearningCategory::Pattern,
                "Error handling pattern",
                vec!["rust", "error-handling", "anyhow"],
            ),
            make_learning(
                "cl_002",
                LearningCategory::Pattern,
                "Context on errors",
                vec!["rust", "error-handling", "context"],
            ),
        ];
        let (_temp, backend) = setup_backend_with_learnings(&learnings);
        let config = Config::default();
        let options = ConsolidateOptions {
            apply: true,
            ..Default::default()
        };
        let merge_fn = mock_merge_fn(&valid_merge_response());

        let output = run_consolidate(&backend, &config, &options, _temp.path(), &merge_fn);

        assert!(output.success);
        assert_eq!(output.written.len(), 1);
        // Both source learnings archived
        assert_eq!(output.archived.len(), 2);

        // Verify in backend: sources archived, new learning active
        let all = backend.list_all().unwrap();
        let active: Vec<_> = all
            .iter()
            .filter(|l| l.status == LearningStatus::Active)
            .collect();
        let archived: Vec<_> = all
            .iter()
            .filter(|l| l.status == LearningStatus::Archived)
            .collect();
        assert_eq!(active.len(), 1);
        assert_eq!(archived.len(), 2);
        assert!(active[0].summary.contains("Merged"));
    }

    #[test]
    fn test_consolidate_llm_failure_skips_group() {
        let learnings = vec![
            make_learning(
                "cl_001",
                LearningCategory::Pattern,
                "Error handling pattern",
                vec!["rust", "error-handling", "anyhow"],
            ),
            make_learning(
                "cl_002",
                LearningCategory::Pattern,
                "Context on errors",
                vec!["rust", "error-handling", "context"],
            ),
        ];
        let (_temp, backend) = setup_backend_with_learnings(&learnings);
        let config = Config::default();
        let options = ConsolidateOptions::default();
        let merge_fn = mock_merge_fn_fail();

        let output = run_consolidate(&backend, &config, &options, _temp.path(), &merge_fn);

        assert!(output.success);
        assert!(!output.groups.is_empty()); // groups found
        assert!(output.merge_proposals.is_empty()); // but no proposals (LLM failed)
    }

    #[test]
    fn test_consolidate_stale_only() {
        static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

        let l = make_learning_with_files(
            "cl_001",
            "Test learning",
            vec!["rust", "testing"],
            vec!["src/nonexistent.rs"],
        );
        // Need a second learning with matching tags for potential grouping
        let l2 = make_learning(
            "cl_002",
            LearningCategory::Pattern,
            "Related",
            vec!["rust", "testing"],
        );

        let (_temp, backend) = setup_backend_with_learnings(&[l, l2]);
        let config = Config::default();
        let options = ConsolidateOptions {
            stale_only: true,
            ..Default::default()
        };
        let merge_fn = mock_merge_fn_counting(&valid_merge_response(), &CALL_COUNT);

        CALL_COUNT.store(0, Ordering::SeqCst);
        let output = run_consolidate(&backend, &config, &options, _temp.path(), &merge_fn);

        assert!(output.success);
        assert!(!output.stale_references.is_empty());
        assert!(output.groups.is_empty()); // stale-only skips grouping
        assert!(output.merge_proposals.is_empty());
        assert_eq!(CALL_COUNT.load(Ordering::SeqCst), 0); // LLM never called
    }

    // -- Output formatting --

    #[test]
    fn test_format_output_json() {
        let output = ConsolidateOutput::success(Vec::new(), Vec::new(), Vec::new());
        let options = ConsolidateOptions {
            json: true,
            ..Default::default()
        };
        let formatted = format_output(&output, &options);
        assert!(formatted.contains("\"success\": true"));
    }

    #[test]
    fn test_format_output_quiet() {
        let output = ConsolidateOutput::success(Vec::new(), Vec::new(), Vec::new());
        let options = ConsolidateOptions {
            quiet: true,
            ..Default::default()
        };
        let formatted = format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_output_dry_run_with_proposals() {
        let proposal = MergeProposal {
            group: LearningGroup {
                learning_ids: vec!["cl_001".to_string(), "cl_002".to_string()],
                summaries: vec!["Summary A".to_string(), "Summary B".to_string()],
                shared_tags: vec!["rust".to_string()],
                category: Some("pattern".to_string()),
                similarity: 0.6,
            },
            merged_summary: "Merged summary".to_string(),
            merged_detail: "Merged detail".to_string(),
            merged_tags: vec!["rust".to_string(), "testing".to_string()],
            category: "pattern".to_string(),
        };
        let output = ConsolidateOutput::success(Vec::new(), Vec::new(), vec![proposal]);
        let options = ConsolidateOptions::default();
        let formatted = format_output(&output, &options);

        assert!(formatted.contains("dry run"));
        assert!(formatted.contains("1 merge proposal"));
        assert!(formatted.contains("Merged summary"));
        assert!(formatted.contains("cl_001"));
        assert!(formatted.contains("--apply"));
    }

    #[test]
    fn test_format_output_failure() {
        let output = ConsolidateOutput::failure("test error");
        let options = ConsolidateOptions::default();
        let formatted = format_output(&output, &options);
        assert!(formatted.contains("failed"));
        assert!(formatted.contains("test error"));
    }

    // -- parse_category --

    #[test]
    fn test_parse_category_known() {
        assert_eq!(parse_category("pattern"), LearningCategory::Pattern);
        assert_eq!(parse_category("pitfall"), LearningCategory::Pitfall);
        assert_eq!(parse_category("Convention"), LearningCategory::Convention);
        assert_eq!(parse_category("DEBUGGING"), LearningCategory::Debugging);
    }

    #[test]
    fn test_parse_category_unknown_defaults_to_pattern() {
        assert_eq!(parse_category("unknown"), LearningCategory::Pattern);
    }

    // -- clean_llm_response --

    #[test]
    fn test_clean_llm_response_plain_json() {
        let input = r#"{"summary": "test", "detail": "test", "tags": []}"#;
        assert_eq!(clean_llm_response(input), input);
    }

    #[test]
    fn test_clean_llm_response_markdown_fences() {
        let input = "```json\n{\"summary\": \"test\", \"detail\": \"test\", \"tags\": []}\n```";
        assert_eq!(
            clean_llm_response(input),
            r#"{"summary": "test", "detail": "test", "tags": []}"#
        );
    }

    #[test]
    fn test_clean_llm_response_plain_fences() {
        let input = "```\n{\"summary\": \"test\"}\n```";
        assert_eq!(clean_llm_response(input), r#"{"summary": "test"}"#);
    }

    #[test]
    fn test_clean_llm_response_with_whitespace() {
        let input = "\n  {\"summary\": \"test\"}\n  ";
        assert_eq!(clean_llm_response(input), r#"{"summary": "test"}"#);
    }

    // -- merge_group --

    #[test]
    fn test_merge_group_success() {
        let learnings = vec![
            make_learning(
                "cl_001",
                LearningCategory::Pattern,
                "A",
                vec!["rust", "error"],
            ),
            make_learning(
                "cl_002",
                LearningCategory::Pattern,
                "B",
                vec!["rust", "error"],
            ),
        ];
        let group = LearningGroup {
            learning_ids: vec!["cl_001".to_string(), "cl_002".to_string()],
            summaries: vec!["A".to_string(), "B".to_string()],
            shared_tags: vec!["rust".to_string(), "error".to_string()],
            category: Some("pattern".to_string()),
            similarity: 1.0,
        };
        let merge_fn = mock_merge_fn(&valid_merge_response());
        let proposal = merge_group(&learnings, &group, &merge_fn);

        assert!(proposal.is_some());
        let p = proposal.unwrap();
        assert_eq!(p.merged_summary, "Merged: error handling with context");
    }

    #[test]
    fn test_merge_group_invalid_json() {
        let learnings = vec![make_learning(
            "cl_001",
            LearningCategory::Pattern,
            "A",
            vec!["rust", "error"],
        )];
        let group = LearningGroup {
            learning_ids: vec!["cl_001".to_string()],
            summaries: vec!["A".to_string()],
            shared_tags: vec![],
            category: Some("pattern".to_string()),
            similarity: 1.0,
        };
        let merge_fn = mock_merge_fn("not valid json");
        let proposal = merge_group(&learnings, &group, &merge_fn);

        assert!(proposal.is_none()); // fail-open
    }

    #[test]
    fn test_merge_group_llm_failure() {
        let learnings = vec![make_learning(
            "cl_001",
            LearningCategory::Pattern,
            "A",
            vec!["rust", "error"],
        )];
        let group = LearningGroup {
            learning_ids: vec!["cl_001".to_string()],
            summaries: vec!["A".to_string()],
            shared_tags: vec![],
            category: Some("pattern".to_string()),
            similarity: 1.0,
        };
        let merge_fn = mock_merge_fn_fail();
        let proposal = merge_group(&learnings, &group, &merge_fn);

        assert!(proposal.is_none()); // fail-open
    }
}
