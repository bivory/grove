//! Reflection schema validation for Grove.
//!
//! This module implements Layer 1 (schema) and Layer 2 (write gate) validation
//! for candidate learnings. Candidates must pass both layers to be written.

use serde::{Deserialize, Serialize};

use crate::core::learning::{
    CompoundLearning, Confidence, LearningCategory, LearningScope, LearningStatus,
    WriteGateCriterion,
};

// =============================================================================
// Constants
// =============================================================================

/// Minimum length for learning summary.
pub const SUMMARY_MIN_LENGTH: usize = 10;
/// Maximum length for learning summary.
pub const SUMMARY_MAX_LENGTH: usize = 200;
/// Minimum length for learning detail.
pub const DETAIL_MIN_LENGTH: usize = 20;
/// Maximum length for learning detail.
pub const DETAIL_MAX_LENGTH: usize = 2000;
/// Minimum number of tags.
pub const TAGS_MIN_COUNT: usize = 1;
/// Maximum number of tags.
pub const TAGS_MAX_COUNT: usize = 10;

// =============================================================================
// Types
// =============================================================================

/// Stage at which a candidate was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStage {
    /// Layer 1: Schema validation.
    Schema,
    /// Layer 2: Write gate filter.
    WriteGate,
    /// Near-duplicate detection.
    Duplicate,
}

impl std::fmt::Display for ValidationStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationStage::Schema => write!(f, "schema"),
            ValidationStage::WriteGate => write!(f, "write_gate"),
            ValidationStage::Duplicate => write!(f, "duplicate"),
        }
    }
}

/// A candidate learning that was rejected during validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RejectedCandidate {
    /// Summary of the rejected learning (for tracking).
    pub summary: String,
    /// Why the candidate was rejected.
    pub rejection_reason: String,
    /// At which stage the candidate was rejected.
    pub stage: ValidationStage,
}

impl RejectedCandidate {
    /// Create a new rejected candidate.
    pub fn new(
        summary: impl Into<String>,
        rejection_reason: impl Into<String>,
        stage: ValidationStage,
    ) -> Self {
        Self {
            summary: summary.into(),
            rejection_reason: rejection_reason.into(),
            stage,
        }
    }

    /// Create a schema validation rejection.
    pub fn schema_error(summary: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(summary, reason, ValidationStage::Schema)
    }

    /// Create a write gate rejection.
    pub fn write_gate_error(summary: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(summary, reason, ValidationStage::WriteGate)
    }

    /// Create a duplicate detection rejection.
    pub fn duplicate_error(summary: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(summary, reason, ValidationStage::Duplicate)
    }
}

/// Schema validation error for a single field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaValidationError {
    /// Category is not a valid enum value.
    InvalidCategory(String),
    /// Summary is too short.
    SummaryTooShort { length: usize, min: usize },
    /// Summary is too long.
    SummaryTooLong { length: usize, max: usize },
    /// Detail is too short.
    DetailTooShort { length: usize, min: usize },
    /// Detail is too long.
    DetailTooLong { length: usize, max: usize },
    /// Summary and detail are the same.
    SummaryEqualsDetail,
    /// Too few tags.
    TooFewTags { count: usize, min: usize },
    /// Too many tags.
    TooManyTags { count: usize, max: usize },
    /// Tag is empty.
    EmptyTag { index: usize },
    /// No criteria claimed.
    NoCriteriaClaimed,
    /// Invalid criterion value.
    InvalidCriterion(String),
}

impl std::fmt::Display for SchemaValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaValidationError::InvalidCategory(c) => {
                write!(f, "invalid category: '{}'", c)
            }
            SchemaValidationError::SummaryTooShort { length, min } => {
                write!(f, "summary too short: {} chars (min: {})", length, min)
            }
            SchemaValidationError::SummaryTooLong { length, max } => {
                write!(f, "summary too long: {} chars (max: {})", length, max)
            }
            SchemaValidationError::DetailTooShort { length, min } => {
                write!(f, "detail too short: {} chars (min: {})", length, min)
            }
            SchemaValidationError::DetailTooLong { length, max } => {
                write!(f, "detail too long: {} chars (max: {})", length, max)
            }
            SchemaValidationError::SummaryEqualsDetail => {
                write!(f, "summary and detail must differ")
            }
            SchemaValidationError::TooFewTags { count, min } => {
                write!(f, "too few tags: {} (min: {})", count, min)
            }
            SchemaValidationError::TooManyTags { count, max } => {
                write!(f, "too many tags: {} (max: {})", count, max)
            }
            SchemaValidationError::EmptyTag { index } => {
                write!(f, "empty tag at index {}", index)
            }
            SchemaValidationError::NoCriteriaClaimed => {
                write!(f, "no write gate criteria claimed")
            }
            SchemaValidationError::InvalidCriterion(c) => {
                write!(f, "invalid criterion: '{}'", c)
            }
        }
    }
}

impl std::error::Error for SchemaValidationError {}

// =============================================================================
// Candidate Input
// =============================================================================

/// Raw candidate learning input from Claude's reflection output.
///
/// This is the unvalidated input format. Fields are optional/strings
/// to allow validation to produce specific error messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateLearning {
    /// Category (should be one of 7 enum values).
    pub category: String,
    /// Brief summary.
    pub summary: String,
    /// Detailed explanation.
    pub detail: String,
    /// Scope (default to "project" if missing/invalid).
    #[serde(default = "default_scope")]
    pub scope: String,
    /// Confidence level (optional, default to "medium").
    #[serde(default = "default_confidence")]
    pub confidence: String,
    /// Write gate criteria claimed.
    #[serde(default)]
    pub criteria_met: Vec<String>,
    /// Tags for categorization.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional context files.
    #[serde(default)]
    pub context_files: Option<Vec<String>>,
}

fn default_scope() -> String {
    "project".to_string()
}

fn default_confidence() -> String {
    "medium".to_string()
}

// =============================================================================
// Validation
// =============================================================================

/// Validate a candidate learning against Layer 1 schema rules.
///
/// Returns either a valid CompoundLearning or a RejectedCandidate with the reason.
pub fn validate_schema(
    candidate: &CandidateLearning,
    session_id: &str,
) -> Result<CompoundLearning, Vec<SchemaValidationError>> {
    let mut errors = Vec::new();

    // Validate category
    let category = match parse_category(&candidate.category) {
        Some(c) => c,
        None => {
            errors.push(SchemaValidationError::InvalidCategory(
                candidate.category.clone(),
            ));
            LearningCategory::Pattern // placeholder, won't be used
        }
    };

    // Validate summary length
    let summary_len = candidate.summary.chars().count();
    if summary_len < SUMMARY_MIN_LENGTH {
        errors.push(SchemaValidationError::SummaryTooShort {
            length: summary_len,
            min: SUMMARY_MIN_LENGTH,
        });
    }
    if summary_len > SUMMARY_MAX_LENGTH {
        errors.push(SchemaValidationError::SummaryTooLong {
            length: summary_len,
            max: SUMMARY_MAX_LENGTH,
        });
    }

    // Validate detail length
    let detail_len = candidate.detail.chars().count();
    if detail_len < DETAIL_MIN_LENGTH {
        errors.push(SchemaValidationError::DetailTooShort {
            length: detail_len,
            min: DETAIL_MIN_LENGTH,
        });
    }
    if detail_len > DETAIL_MAX_LENGTH {
        errors.push(SchemaValidationError::DetailTooLong {
            length: detail_len,
            max: DETAIL_MAX_LENGTH,
        });
    }

    // Validate summary ≠ detail
    if candidate.summary == candidate.detail {
        errors.push(SchemaValidationError::SummaryEqualsDetail);
    }

    // Validate tags count
    let tag_count = candidate.tags.len();
    if tag_count < TAGS_MIN_COUNT {
        errors.push(SchemaValidationError::TooFewTags {
            count: tag_count,
            min: TAGS_MIN_COUNT,
        });
    }
    if tag_count > TAGS_MAX_COUNT {
        errors.push(SchemaValidationError::TooManyTags {
            count: tag_count,
            max: TAGS_MAX_COUNT,
        });
    }

    // Validate tags are non-empty
    for (i, tag) in candidate.tags.iter().enumerate() {
        if tag.trim().is_empty() {
            errors.push(SchemaValidationError::EmptyTag { index: i });
        }
    }

    // Parse scope (default to Project if invalid)
    let scope = parse_scope(&candidate.scope).unwrap_or(LearningScope::Project);

    // Parse confidence (default to Medium if invalid)
    let confidence = parse_confidence(&candidate.confidence).unwrap_or(Confidence::Medium);

    // Validate criteria_met
    let mut criteria = Vec::new();
    for criterion_str in &candidate.criteria_met {
        match parse_criterion(criterion_str) {
            Some(c) => criteria.push(c),
            None => {
                errors.push(SchemaValidationError::InvalidCriterion(
                    criterion_str.clone(),
                ));
            }
        }
    }

    // Must have at least one valid criterion
    if criteria.is_empty() {
        errors.push(SchemaValidationError::NoCriteriaClaimed);
    }

    // If there are any errors, return them
    if !errors.is_empty() {
        return Err(errors);
    }

    // Build valid learning
    let mut learning = CompoundLearning::new(
        category,
        &candidate.summary,
        &candidate.detail,
        scope,
        confidence,
        criteria,
        candidate.tags.clone(),
        session_id,
    );

    if let Some(ref files) = candidate.context_files {
        learning = learning.with_context_files(files.clone());
    }

    Ok(learning)
}

/// Validate a batch of candidates, returning valid learnings and rejected candidates.
pub fn validate_batch(
    candidates: Vec<CandidateLearning>,
    session_id: &str,
) -> (Vec<CompoundLearning>, Vec<RejectedCandidate>) {
    let mut valid = Vec::new();
    let mut rejected = Vec::new();

    for candidate in candidates {
        match validate_schema(&candidate, session_id) {
            Ok(learning) => valid.push(learning),
            Err(errors) => {
                let reasons: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
                rejected.push(RejectedCandidate::schema_error(
                    &candidate.summary,
                    reasons.join("; "),
                ));
            }
        }
    }

    (valid, rejected)
}

// =============================================================================
// Write Gate Filter (Layer 2)
// =============================================================================

/// Result of write gate validation for a single learning.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteGateResult {
    /// Whether the learning passed the write gate.
    pub passed: bool,
    /// The criteria that were claimed.
    pub criteria_claimed: Vec<WriteGateCriterion>,
    /// Assessment of plausibility for each claimed criterion.
    pub plausibility: Vec<CriterionPlausibility>,
    /// Overall confidence in the criteria claims.
    pub confidence: WriteGateConfidence,
}

/// Plausibility assessment for a single criterion claim.
#[derive(Debug, Clone, PartialEq)]
pub struct CriterionPlausibility {
    /// The criterion being assessed.
    pub criterion: WriteGateCriterion,
    /// Whether the claim seems plausible based on content.
    pub plausible: bool,
    /// Evidence found (or lack thereof).
    pub evidence: String,
}

/// Overall confidence in the write gate assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteGateConfidence {
    /// Strong evidence supports the criteria claims.
    High,
    /// Some evidence supports the criteria claims.
    Medium,
    /// Weak evidence, but passing anyway (fail-open).
    Low,
}

impl WriteGateResult {
    /// Create a passing result with the given criteria.
    pub fn pass(
        criteria_claimed: Vec<WriteGateCriterion>,
        plausibility: Vec<CriterionPlausibility>,
        confidence: WriteGateConfidence,
    ) -> Self {
        Self {
            passed: true,
            criteria_claimed,
            plausibility,
            confidence,
        }
    }

    /// Create a failing result (no valid criteria claimed).
    pub fn fail() -> Self {
        Self {
            passed: false,
            criteria_claimed: Vec::new(),
            plausibility: Vec::new(),
            confidence: WriteGateConfidence::Low,
        }
    }
}

/// Validate a learning against the write gate criteria.
///
/// The write gate checks that claimed criteria are plausible given the content.
/// This is a heuristic check - the primary defense is stats tracking, not
/// strict rejection. Therefore, this function is lenient and rarely rejects.
pub fn validate_write_gate(learning: &CompoundLearning) -> WriteGateResult {
    if learning.criteria_met.is_empty() {
        return WriteGateResult::fail();
    }

    let mut plausibility = Vec::new();

    for criterion in &learning.criteria_met {
        let assessment =
            assess_criterion_plausibility(criterion, &learning.summary, &learning.detail);
        plausibility.push(assessment);
    }

    // Calculate confidence based on plausibility assessments
    let plausible_count = plausibility.iter().filter(|p| p.plausible).count();
    let confidence = if plausible_count == learning.criteria_met.len() {
        WriteGateConfidence::High
    } else if plausible_count > 0 {
        WriteGateConfidence::Medium
    } else {
        // Even with no plausible evidence, we pass (fail-open)
        // The insights engine will track this for pattern detection
        WriteGateConfidence::Low
    };

    // Fail-open: only fail if no criteria were claimed at all
    // (which shouldn't happen if schema validation passed)
    WriteGateResult::pass(learning.criteria_met.clone(), plausibility, confidence)
}

/// Assess plausibility of a single criterion claim.
fn assess_criterion_plausibility(
    criterion: &WriteGateCriterion,
    summary: &str,
    detail: &str,
) -> CriterionPlausibility {
    let combined = format!("{} {}", summary, detail).to_lowercase();

    let (plausible, evidence) = match criterion {
        WriteGateCriterion::BehaviorChanging => {
            // Look for action/behavior change indicators
            let indicators = [
                "should",
                "will",
                "avoid",
                "use",
                "don't",
                "always",
                "never",
                "instead",
                "rather",
                "prefer",
                "better",
                "next time",
                "from now on",
                "going forward",
                "remember to",
                "make sure",
            ];
            let found: Vec<&str> = indicators
                .iter()
                .filter(|&&i| combined.contains(i))
                .copied()
                .collect();
            if found.is_empty() {
                (false, "no behavior change indicators found".to_string())
            } else {
                (true, format!("found: {}", found.join(", ")))
            }
        }
        WriteGateCriterion::DecisionRationale => {
            // Look for decision/comparison language
            let indicators = [
                "because",
                "since",
                "chose",
                "decided",
                "over",
                "instead of",
                "rather than",
                "vs",
                "versus",
                "compared to",
                "trade-off",
                "reason",
                "why",
                "due to",
            ];
            let found: Vec<&str> = indicators
                .iter()
                .filter(|&&i| combined.contains(i))
                .copied()
                .collect();
            if found.is_empty() {
                (false, "no decision rationale indicators found".to_string())
            } else {
                (true, format!("found: {}", found.join(", ")))
            }
        }
        WriteGateCriterion::StableFact => {
            // Stable facts are hard to validate heuristically
            // Look for factual/definitional language
            let indicators = [
                "is",
                "are",
                "was",
                "has",
                "works",
                "requires",
                "means",
                "defined as",
                "refers to",
                "represents",
                "consists of",
                "located in",
                "stored in",
                "configured",
            ];
            let found: Vec<&str> = indicators
                .iter()
                .filter(|&&i| combined.contains(i))
                .copied()
                .collect();
            // Be lenient for stable facts
            if !found.is_empty() {
                (true, format!("found: {}", found.join(", ")))
            } else {
                (true, "assuming stable fact (lenient check)".to_string())
            }
        }
        WriteGateCriterion::ExplicitRequest => {
            // Look for user request indicators
            let indicators = [
                "remember",
                "note",
                "important",
                "user asked",
                "user said",
                "user requested",
                "user wants",
                "told me to",
                "asked me to",
                "keep in mind",
                "for the record",
            ];
            let found: Vec<&str> = indicators
                .iter()
                .filter(|&&i| combined.contains(i))
                .copied()
                .collect();
            if found.is_empty() {
                (
                    false,
                    "no explicit user request indicators found".to_string(),
                )
            } else {
                (true, format!("found: {}", found.join(", ")))
            }
        }
    };

    CriterionPlausibility {
        criterion: *criterion,
        plausible,
        evidence,
    }
}

/// Apply both schema validation and write gate to a batch of candidates.
///
/// Returns learnings that passed both layers, plus all rejected candidates.
pub fn validate_full(
    candidates: Vec<CandidateLearning>,
    session_id: &str,
) -> (Vec<CompoundLearning>, Vec<RejectedCandidate>) {
    // Layer 1: Schema validation
    let (schema_valid, mut rejected) = validate_batch(candidates, session_id);

    // Layer 2: Write gate filter
    let mut fully_valid = Vec::new();
    for learning in schema_valid {
        let gate_result = validate_write_gate(&learning);
        if gate_result.passed {
            fully_valid.push(learning);
        } else {
            rejected.push(RejectedCandidate::write_gate_error(
                &learning.summary,
                "no valid criteria claimed",
            ));
        }
    }

    (fully_valid, rejected)
}

// =============================================================================
// Near-Duplicate Detection
// =============================================================================

/// Result of checking for near-duplicate learnings.
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateCheckResult {
    /// Whether the candidate is a duplicate of an existing learning.
    pub is_duplicate: bool,
    /// ID of the existing learning this is a duplicate of (if any).
    pub duplicate_of: Option<String>,
    /// The matched summary (for debugging/logging).
    pub matched_summary: Option<String>,
}

impl DuplicateCheckResult {
    /// Create a result indicating no duplicate was found.
    pub fn no_duplicate() -> Self {
        Self {
            is_duplicate: false,
            duplicate_of: None,
            matched_summary: None,
        }
    }

    /// Create a result indicating a duplicate was found.
    pub fn duplicate(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            is_duplicate: true,
            duplicate_of: Some(id.into()),
            matched_summary: Some(summary.into()),
        }
    }
}

/// Check if a summary matches an existing active learning.
///
/// Stage 1 behavior: Exact match only (case-insensitive summary comparison).
/// Only checks against active learnings (archived/superseded are ignored).
fn check_duplicate_by_summary(
    summary: &str,
    existing: &[CompoundLearning],
) -> DuplicateCheckResult {
    let normalized = summary.to_lowercase();

    for learning in existing {
        // Only check against active learnings
        if learning.status != LearningStatus::Active {
            continue;
        }

        // Case-insensitive exact match
        if learning.summary.to_lowercase() == normalized {
            return DuplicateCheckResult::duplicate(&learning.id, &learning.summary);
        }
    }

    DuplicateCheckResult::no_duplicate()
}

/// Check if a candidate learning is a near-duplicate of existing learnings.
///
/// Stage 1 behavior: Exact match only (case-insensitive summary comparison).
/// Only checks against active learnings (archived/superseded are ignored).
pub fn check_near_duplicate(
    candidate: &CandidateLearning,
    existing: &[CompoundLearning],
) -> DuplicateCheckResult {
    check_duplicate_by_summary(&candidate.summary, existing)
}

/// Apply all validation layers including duplicate detection.
///
/// Returns learnings that passed all layers, plus all rejected candidates.
/// Checks for duplicates both against existing learnings and within the batch.
pub fn validate_with_duplicates(
    candidates: Vec<CandidateLearning>,
    session_id: &str,
    existing_learnings: &[CompoundLearning],
) -> (Vec<CompoundLearning>, Vec<RejectedCandidate>) {
    // Layer 1 + 2: Schema + Write gate
    let (valid, mut rejected) = validate_full(candidates, session_id);

    // Layer 3: Duplicate detection
    // Track validated learnings to detect duplicates within the same batch
    let mut final_valid: Vec<CompoundLearning> = Vec::new();

    for learning in valid {
        // Check against existing learnings in storage
        let existing_dup = check_duplicate_by_summary(&learning.summary, existing_learnings);
        if existing_dup.is_duplicate {
            rejected.push(RejectedCandidate::duplicate_error(
                &learning.summary,
                format!(
                    "duplicate of existing learning: {}",
                    existing_dup.duplicate_of.unwrap_or_default()
                ),
            ));
            continue;
        }

        // Check against learnings already validated in this batch
        let batch_dup = check_duplicate_by_summary(&learning.summary, &final_valid);
        if batch_dup.is_duplicate {
            rejected.push(RejectedCandidate::duplicate_error(
                &learning.summary,
                format!(
                    "duplicate within batch: {}",
                    batch_dup.duplicate_of.unwrap_or_default()
                ),
            ));
            continue;
        }

        final_valid.push(learning);
    }

    (final_valid, rejected)
}

// =============================================================================
// Parsing Helpers
// =============================================================================

fn parse_category(s: &str) -> Option<LearningCategory> {
    match s.to_lowercase().as_str() {
        "pattern" => Some(LearningCategory::Pattern),
        "pitfall" => Some(LearningCategory::Pitfall),
        "convention" => Some(LearningCategory::Convention),
        "dependency" => Some(LearningCategory::Dependency),
        "process" => Some(LearningCategory::Process),
        "domain" => Some(LearningCategory::Domain),
        "debugging" => Some(LearningCategory::Debugging),
        _ => None,
    }
}

fn parse_scope(s: &str) -> Option<LearningScope> {
    match s.to_lowercase().as_str() {
        "project" => Some(LearningScope::Project),
        "team" => Some(LearningScope::Team),
        "personal" => Some(LearningScope::Personal),
        "ephemeral" => Some(LearningScope::Ephemeral),
        _ => None,
    }
}

fn parse_confidence(s: &str) -> Option<Confidence> {
    match s.to_lowercase().as_str() {
        "high" => Some(Confidence::High),
        "medium" => Some(Confidence::Medium),
        "low" => Some(Confidence::Low),
        _ => None,
    }
}

fn parse_criterion(s: &str) -> Option<WriteGateCriterion> {
    match s.to_lowercase().replace('-', "_").as_str() {
        "behavior_changing" => Some(WriteGateCriterion::BehaviorChanging),
        "decision_rationale" => Some(WriteGateCriterion::DecisionRationale),
        "stable_fact" => Some(WriteGateCriterion::StableFact),
        "explicit_request" => Some(WriteGateCriterion::ExplicitRequest),
        _ => None,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_candidate() -> CandidateLearning {
        CandidateLearning {
            category: "pattern".to_string(),
            summary: "Use async/await for I/O operations".to_string(),
            detail: "When performing I/O operations, always use async/await to avoid blocking the main thread. This improves application responsiveness.".to_string(),
            scope: "project".to_string(),
            confidence: "high".to_string(),
            criteria_met: vec!["behavior_changing".to_string()],
            tags: vec!["async".to_string(), "io".to_string()],
            context_files: None,
        }
    }

    // =========================================================================
    // RejectedCandidate tests
    // =========================================================================

    #[test]
    fn test_rejected_candidate_new() {
        let rejected = RejectedCandidate::new("summary", "reason", ValidationStage::Schema);
        assert_eq!(rejected.summary, "summary");
        assert_eq!(rejected.rejection_reason, "reason");
        assert_eq!(rejected.stage, ValidationStage::Schema);
    }

    #[test]
    fn test_rejected_candidate_factories() {
        let schema = RejectedCandidate::schema_error("s", "r");
        assert_eq!(schema.stage, ValidationStage::Schema);

        let write_gate = RejectedCandidate::write_gate_error("s", "r");
        assert_eq!(write_gate.stage, ValidationStage::WriteGate);

        let duplicate = RejectedCandidate::duplicate_error("s", "r");
        assert_eq!(duplicate.stage, ValidationStage::Duplicate);
    }

    #[test]
    fn test_validation_stage_display() {
        assert_eq!(format!("{}", ValidationStage::Schema), "schema");
        assert_eq!(format!("{}", ValidationStage::WriteGate), "write_gate");
        assert_eq!(format!("{}", ValidationStage::Duplicate), "duplicate");
    }

    // =========================================================================
    // SchemaValidationError tests
    // =========================================================================

    #[test]
    fn test_schema_validation_error_display() {
        assert!(SchemaValidationError::InvalidCategory("foo".into())
            .to_string()
            .contains("invalid category"));
        assert!(
            SchemaValidationError::SummaryTooShort { length: 5, min: 10 }
                .to_string()
                .contains("too short")
        );
        assert!(SchemaValidationError::SummaryTooLong {
            length: 300,
            max: 200
        }
        .to_string()
        .contains("too long"));
        assert!(SchemaValidationError::DetailTooShort {
            length: 10,
            min: 20
        }
        .to_string()
        .contains("too short"));
        assert!(SchemaValidationError::DetailTooLong {
            length: 3000,
            max: 2000
        }
        .to_string()
        .contains("too long"));
        assert!(SchemaValidationError::SummaryEqualsDetail
            .to_string()
            .contains("must differ"));
        assert!(SchemaValidationError::TooFewTags { count: 0, min: 1 }
            .to_string()
            .contains("too few"));
        assert!(SchemaValidationError::TooManyTags { count: 15, max: 10 }
            .to_string()
            .contains("too many"));
        assert!(SchemaValidationError::EmptyTag { index: 0 }
            .to_string()
            .contains("empty"));
        assert!(SchemaValidationError::NoCriteriaClaimed
            .to_string()
            .contains("no write gate"));
        assert!(SchemaValidationError::InvalidCriterion("foo".into())
            .to_string()
            .contains("invalid criterion"));
    }

    // =========================================================================
    // Parsing tests
    // =========================================================================

    #[test]
    fn test_parse_category() {
        assert_eq!(parse_category("pattern"), Some(LearningCategory::Pattern));
        assert_eq!(parse_category("Pattern"), Some(LearningCategory::Pattern));
        assert_eq!(parse_category("PITFALL"), Some(LearningCategory::Pitfall));
        assert_eq!(
            parse_category("convention"),
            Some(LearningCategory::Convention)
        );
        assert_eq!(
            parse_category("dependency"),
            Some(LearningCategory::Dependency)
        );
        assert_eq!(parse_category("process"), Some(LearningCategory::Process));
        assert_eq!(parse_category("domain"), Some(LearningCategory::Domain));
        assert_eq!(
            parse_category("debugging"),
            Some(LearningCategory::Debugging)
        );
        assert_eq!(parse_category("invalid"), None);
    }

    #[test]
    fn test_parse_scope() {
        assert_eq!(parse_scope("project"), Some(LearningScope::Project));
        assert_eq!(parse_scope("team"), Some(LearningScope::Team));
        assert_eq!(parse_scope("personal"), Some(LearningScope::Personal));
        assert_eq!(parse_scope("ephemeral"), Some(LearningScope::Ephemeral));
        assert_eq!(parse_scope("invalid"), None);
    }

    #[test]
    fn test_parse_confidence() {
        assert_eq!(parse_confidence("high"), Some(Confidence::High));
        assert_eq!(parse_confidence("medium"), Some(Confidence::Medium));
        assert_eq!(parse_confidence("low"), Some(Confidence::Low));
        assert_eq!(parse_confidence("invalid"), None);
    }

    #[test]
    fn test_parse_criterion() {
        assert_eq!(
            parse_criterion("behavior_changing"),
            Some(WriteGateCriterion::BehaviorChanging)
        );
        assert_eq!(
            parse_criterion("behavior-changing"),
            Some(WriteGateCriterion::BehaviorChanging)
        );
        assert_eq!(
            parse_criterion("decision_rationale"),
            Some(WriteGateCriterion::DecisionRationale)
        );
        assert_eq!(
            parse_criterion("stable_fact"),
            Some(WriteGateCriterion::StableFact)
        );
        assert_eq!(
            parse_criterion("explicit_request"),
            Some(WriteGateCriterion::ExplicitRequest)
        );
        assert_eq!(parse_criterion("invalid"), None);
    }

    // =========================================================================
    // Schema validation tests
    // =========================================================================

    #[test]
    fn test_validate_valid_candidate() {
        let candidate = valid_candidate();
        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_ok());

        let learning = result.unwrap();
        assert_eq!(learning.category, LearningCategory::Pattern);
        assert_eq!(learning.summary, candidate.summary);
        assert_eq!(learning.detail, candidate.detail);
        assert_eq!(learning.scope, LearningScope::Project);
        assert_eq!(learning.confidence, Confidence::High);
        assert_eq!(learning.criteria_met.len(), 1);
        assert_eq!(learning.tags.len(), 2);
    }

    #[test]
    fn test_validate_invalid_category() {
        let mut candidate = valid_candidate();
        candidate.category = "not_a_category".to_string();

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::InvalidCategory(_))));
    }

    #[test]
    fn test_validate_summary_too_short() {
        let mut candidate = valid_candidate();
        candidate.summary = "short".to_string(); // 5 chars, min is 10

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::SummaryTooShort { .. })));
    }

    #[test]
    fn test_validate_summary_too_long() {
        let mut candidate = valid_candidate();
        candidate.summary = "x".repeat(250); // 250 chars, max is 200

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::SummaryTooLong { .. })));
    }

    #[test]
    fn test_validate_detail_too_short() {
        let mut candidate = valid_candidate();
        candidate.detail = "short detail".to_string(); // 12 chars, min is 20

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::DetailTooShort { .. })));
    }

    #[test]
    fn test_validate_detail_too_long() {
        let mut candidate = valid_candidate();
        candidate.detail = "x".repeat(2500); // 2500 chars, max is 2000

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::DetailTooLong { .. })));
    }

    #[test]
    fn test_validate_summary_equals_detail() {
        let mut candidate = valid_candidate();
        let text = "This is exactly the same text that is long enough to pass length checks.";
        candidate.summary = text.to_string();
        candidate.detail = text.to_string();

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::SummaryEqualsDetail)));
    }

    #[test]
    fn test_validate_too_few_tags() {
        let mut candidate = valid_candidate();
        candidate.tags = vec![];

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::TooFewTags { .. })));
    }

    #[test]
    fn test_validate_too_many_tags() {
        let mut candidate = valid_candidate();
        candidate.tags = (0..15).map(|i| format!("tag{}", i)).collect();

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::TooManyTags { .. })));
    }

    #[test]
    fn test_validate_empty_tag() {
        let mut candidate = valid_candidate();
        candidate.tags = vec!["valid".to_string(), "".to_string()];

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::EmptyTag { index: 1 })));
    }

    #[test]
    fn test_validate_no_criteria() {
        let mut candidate = valid_candidate();
        candidate.criteria_met = vec![];

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::NoCriteriaClaimed)));
    }

    #[test]
    fn test_validate_invalid_criterion() {
        let mut candidate = valid_candidate();
        candidate.criteria_met = vec!["not_a_criterion".to_string()];

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::InvalidCriterion(_))));
    }

    #[test]
    fn test_validate_all_invalid_criteria() {
        let mut candidate = valid_candidate();
        candidate.criteria_met = vec!["invalid1".to_string(), "invalid2".to_string()];

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        // Should have InvalidCriterion for each invalid criterion
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::InvalidCriterion(_))));
        // Should also have NoCriteriaClaimed since no valid criteria remained
        assert!(errors
            .iter()
            .any(|e| matches!(e, SchemaValidationError::NoCriteriaClaimed)));
    }

    #[test]
    fn test_validate_multiple_errors() {
        let mut candidate = valid_candidate();
        candidate.category = "invalid".to_string();
        candidate.summary = "short".to_string();
        candidate.tags = vec![];
        candidate.criteria_met = vec![];

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors.len() >= 4); // At least 4 errors
    }

    #[test]
    fn test_validate_default_scope() {
        let mut candidate = valid_candidate();
        candidate.scope = "invalid_scope".to_string();

        // Should succeed with default scope
        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_ok());

        let learning = result.unwrap();
        assert_eq!(learning.scope, LearningScope::Project);
    }

    #[test]
    fn test_validate_default_confidence() {
        let mut candidate = valid_candidate();
        candidate.confidence = "invalid_confidence".to_string();

        // Should succeed with default confidence
        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_ok());

        let learning = result.unwrap();
        assert_eq!(learning.confidence, Confidence::Medium);
    }

    #[test]
    fn test_validate_with_context_files() {
        let mut candidate = valid_candidate();
        candidate.context_files = Some(vec!["src/main.rs".to_string()]);

        let result = validate_schema(&candidate, "session-1");
        assert!(result.is_ok());

        let learning = result.unwrap();
        assert_eq!(
            learning.context_files,
            Some(vec!["src/main.rs".to_string()])
        );
    }

    // =========================================================================
    // Batch validation tests
    // =========================================================================

    #[test]
    fn test_validate_batch_all_valid() {
        let candidates = vec![valid_candidate(), valid_candidate()];
        let (valid, rejected) = validate_batch(candidates, "session-1");

        assert_eq!(valid.len(), 2);
        assert!(rejected.is_empty());
    }

    #[test]
    fn test_validate_batch_all_invalid() {
        let mut bad1 = valid_candidate();
        bad1.category = "invalid".to_string();
        let mut bad2 = valid_candidate();
        bad2.summary = "short".to_string();

        let candidates = vec![bad1, bad2];
        let (valid, rejected) = validate_batch(candidates, "session-1");

        assert!(valid.is_empty());
        assert_eq!(rejected.len(), 2);
    }

    #[test]
    fn test_validate_batch_mixed() {
        let good = valid_candidate();
        let mut bad = valid_candidate();
        bad.category = "invalid".to_string();

        let candidates = vec![good.clone(), bad, good];
        let (valid, rejected) = validate_batch(candidates, "session-1");

        assert_eq!(valid.len(), 2);
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].stage, ValidationStage::Schema);
    }

    #[test]
    fn test_validate_batch_empty() {
        let (valid, rejected) = validate_batch(vec![], "session-1");
        assert!(valid.is_empty());
        assert!(rejected.is_empty());
    }

    // =========================================================================
    // Write Gate Filter tests (Section 3.2 of test plan)
    // =========================================================================

    fn valid_learning() -> CompoundLearning {
        CompoundLearning::new(
            LearningCategory::Pattern,
            "Use async/await for I/O operations",
            "When performing I/O operations, always use async/await to avoid blocking the main thread. This improves responsiveness.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["async".to_string(), "io".to_string()],
            "session-1",
        )
    }

    #[test]
    fn test_write_gate_accepts_behavior_changing_criterion() {
        let learning = CompoundLearning::new(
            LearningCategory::Pitfall,
            "Always check for null before accessing properties",
            "Found a null pointer exception when accessing user.name without null check. Should always validate first.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["null-safety".to_string()],
            "session-1",
        );

        let result = validate_write_gate(&learning);
        assert!(result.passed);
        assert_eq!(
            result.criteria_claimed,
            vec![WriteGateCriterion::BehaviorChanging]
        );
    }

    #[test]
    fn test_write_gate_accepts_decision_rationale_criterion() {
        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            "Use repository pattern for data access",
            "Chose repository pattern over active record because it provides better testability and separation of concerns.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::DecisionRationale],
            vec!["architecture".to_string()],
            "session-1",
        );

        let result = validate_write_gate(&learning);
        assert!(result.passed);
        assert_eq!(
            result.criteria_claimed,
            vec![WriteGateCriterion::DecisionRationale]
        );
    }

    #[test]
    fn test_write_gate_accepts_stable_fact_criterion() {
        let learning = CompoundLearning::new(
            LearningCategory::Dependency,
            "Redis requires SCAN for large keyspace iteration",
            "The KEYS command blocks the server for large keyspaces. Redis SCAN is the safe alternative that works incrementally.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::StableFact],
            vec!["redis".to_string()],
            "session-1",
        );

        let result = validate_write_gate(&learning);
        assert!(result.passed);
        assert_eq!(
            result.criteria_claimed,
            vec![WriteGateCriterion::StableFact]
        );
    }

    #[test]
    fn test_write_gate_accepts_explicit_request_criterion() {
        let learning = CompoundLearning::new(
            LearningCategory::Convention,
            "Use kebab-case for API endpoints",
            "User asked to remember that this team uses kebab-case for all REST API endpoint naming.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::ExplicitRequest],
            vec!["api".to_string()],
            "session-1",
        );

        let result = validate_write_gate(&learning);
        assert!(result.passed);
        assert_eq!(
            result.criteria_claimed,
            vec![WriteGateCriterion::ExplicitRequest]
        );
    }

    #[test]
    fn test_write_gate_rejects_no_criteria() {
        let mut learning = valid_learning();
        learning.criteria_met = vec![];

        let result = validate_write_gate(&learning);
        assert!(!result.passed);
        assert!(result.criteria_claimed.is_empty());
    }

    #[test]
    fn test_write_gate_accepts_multiple_criteria() {
        let learning = CompoundLearning::new(
            LearningCategory::Pitfall,
            "Avoid N+1 queries in GraphQL resolvers",
            "Use dataloader pattern to batch queries because it prevents performance issues. The team decided on this approach.",
            LearningScope::Project,
            Confidence::High,
            vec![
                WriteGateCriterion::BehaviorChanging,
                WriteGateCriterion::DecisionRationale,
            ],
            vec!["graphql".to_string(), "performance".to_string()],
            "session-1",
        );

        let result = validate_write_gate(&learning);
        assert!(result.passed);
        assert_eq!(result.criteria_claimed.len(), 2);
    }

    // =========================================================================
    // Plausibility assessment tests
    // =========================================================================

    #[test]
    fn test_behavior_changing_plausibility_with_indicators() {
        let assessment = assess_criterion_plausibility(
            &WriteGateCriterion::BehaviorChanging,
            "Always use async/await",
            "You should avoid blocking calls and instead use async patterns.",
        );
        assert!(assessment.plausible);
        assert!(assessment.evidence.contains("found:"));
    }

    #[test]
    fn test_behavior_changing_plausibility_without_indicators() {
        let assessment = assess_criterion_plausibility(
            &WriteGateCriterion::BehaviorChanging,
            "The code has functions",
            "Functions exist in the codebase for various purposes.",
        );
        assert!(!assessment.plausible);
        assert!(assessment.evidence.contains("no behavior change"));
    }

    #[test]
    fn test_decision_rationale_plausibility_with_indicators() {
        let assessment = assess_criterion_plausibility(
            &WriteGateCriterion::DecisionRationale,
            "Chose TypeScript over JavaScript",
            "Decided on TypeScript because of better type safety versus plain JS.",
        );
        assert!(assessment.plausible);
        assert!(assessment.evidence.contains("found:"));
    }

    #[test]
    fn test_decision_rationale_plausibility_without_indicators() {
        let assessment = assess_criterion_plausibility(
            &WriteGateCriterion::DecisionRationale,
            "TypeScript is used here",
            "The project uses TypeScript for development.",
        );
        assert!(!assessment.plausible);
        assert!(assessment.evidence.contains("no decision rationale"));
    }

    #[test]
    fn test_stable_fact_plausibility_lenient() {
        // Stable facts are lenient - even without clear indicators they pass
        let assessment = assess_criterion_plausibility(
            &WriteGateCriterion::StableFact,
            "The API endpoint exists",
            "There is an endpoint at /api/users for user data.",
        );
        assert!(assessment.plausible);
    }

    #[test]
    fn test_explicit_request_plausibility_with_indicators() {
        let assessment = assess_criterion_plausibility(
            &WriteGateCriterion::ExplicitRequest,
            "Use snake_case for variables",
            "User asked to remember this convention for the project.",
        );
        assert!(assessment.plausible);
        assert!(assessment.evidence.contains("found:"));
    }

    #[test]
    fn test_explicit_request_plausibility_without_indicators() {
        let assessment = assess_criterion_plausibility(
            &WriteGateCriterion::ExplicitRequest,
            "Variables use snake_case",
            "The codebase follows snake_case convention.",
        );
        assert!(!assessment.plausible);
        assert!(assessment.evidence.contains("no explicit user request"));
    }

    // =========================================================================
    // WriteGateResult tests
    // =========================================================================

    #[test]
    fn test_write_gate_result_pass_constructor() {
        let result = WriteGateResult::pass(
            vec![WriteGateCriterion::BehaviorChanging],
            vec![],
            WriteGateConfidence::High,
        );
        assert!(result.passed);
        assert_eq!(result.criteria_claimed.len(), 1);
        assert_eq!(result.confidence, WriteGateConfidence::High);
    }

    #[test]
    fn test_write_gate_result_fail_constructor() {
        let result = WriteGateResult::fail();
        assert!(!result.passed);
        assert!(result.criteria_claimed.is_empty());
        assert_eq!(result.confidence, WriteGateConfidence::Low);
    }

    // =========================================================================
    // Confidence level tests
    // =========================================================================

    #[test]
    fn test_confidence_high_when_all_plausible() {
        let learning = CompoundLearning::new(
            LearningCategory::Pitfall,
            "Always validate input before processing",
            "You should never trust user input directly because it can be malicious.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["security".to_string()],
            "session-1",
        );

        let result = validate_write_gate(&learning);
        assert_eq!(result.confidence, WriteGateConfidence::High);
    }

    #[test]
    fn test_confidence_medium_when_some_plausible() {
        let learning = CompoundLearning::new(
            LearningCategory::Pitfall,
            "The team picked this pattern",
            "Picked it since performance and consistency matter here.",
            LearningScope::Project,
            Confidence::High,
            vec![
                WriteGateCriterion::BehaviorChanging, // Not plausible - no behavior indicators
                WriteGateCriterion::DecisionRationale, // Plausible - has "since"
            ],
            vec!["test".to_string()],
            "session-1",
        );

        let result = validate_write_gate(&learning);
        assert_eq!(result.confidence, WriteGateConfidence::Medium);
    }

    #[test]
    fn test_confidence_low_when_none_plausible_but_still_passes() {
        // Fail-open: even with no plausible evidence, we still pass
        let learning = CompoundLearning::new(
            LearningCategory::Domain,
            "The code exists",
            "There is code in the repository that does things.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::ExplicitRequest], // Not plausible without indicators
            vec!["general".to_string()],
            "session-1",
        );

        let result = validate_write_gate(&learning);
        assert!(result.passed); // Still passes (fail-open)
        assert_eq!(result.confidence, WriteGateConfidence::Low);
    }

    // =========================================================================
    // validate_full tests (Layer 1 + Layer 2)
    // =========================================================================

    #[test]
    fn test_validate_full_all_pass() {
        let candidates = vec![valid_candidate(), valid_candidate()];
        let (valid, rejected) = validate_full(candidates, "session-1");

        assert_eq!(valid.len(), 2);
        assert!(rejected.is_empty());
    }

    #[test]
    fn test_validate_full_schema_rejects() {
        let mut bad = valid_candidate();
        bad.category = "invalid".to_string();

        let candidates = vec![valid_candidate(), bad];
        let (valid, rejected) = validate_full(candidates, "session-1");

        assert_eq!(valid.len(), 1);
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].stage, ValidationStage::Schema);
    }

    #[test]
    fn test_validate_full_write_gate_rejects() {
        // Create a candidate that passes schema but has no criteria
        // (This is tricky because schema validation already requires criteria)
        // In practice, write gate rejection would happen if criteria_met is emptied
        // after schema validation, but that's an edge case.
        // For now, we test that validate_full chains correctly.

        let candidates = vec![valid_candidate()];
        let (valid, rejected) = validate_full(candidates, "session-1");

        // Should pass both layers
        assert_eq!(valid.len(), 1);
        assert!(rejected.is_empty());
    }

    #[test]
    fn test_validate_full_mixed_rejections() {
        let good = valid_candidate();

        let mut schema_bad = valid_candidate();
        schema_bad.summary = "short".to_string(); // Fails schema

        let candidates = vec![good, schema_bad];
        let (valid, rejected) = validate_full(candidates, "session-1");

        assert_eq!(valid.len(), 1);
        assert_eq!(rejected.len(), 1);
    }

    #[test]
    fn test_validate_full_empty_input() {
        let (valid, rejected) = validate_full(vec![], "session-1");
        assert!(valid.is_empty());
        assert!(rejected.is_empty());
    }

    // =========================================================================
    // Near-Duplicate Detection tests (Section 3.4 of test plan)
    // =========================================================================

    fn existing_learning(id: &str, summary: &str, status: LearningStatus) -> CompoundLearning {
        let mut learning = CompoundLearning::new(
            LearningCategory::Pattern,
            summary,
            "This is a detailed explanation that is long enough to pass validation checks.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["test".to_string()],
            "session-1",
        );
        learning.id = id.to_string();
        learning.status = status;
        learning
    }

    #[test]
    fn test_duplicate_check_result_constructors() {
        let no_dup = DuplicateCheckResult::no_duplicate();
        assert!(!no_dup.is_duplicate);
        assert!(no_dup.duplicate_of.is_none());
        assert!(no_dup.matched_summary.is_none());

        let dup = DuplicateCheckResult::duplicate("L001", "Test summary");
        assert!(dup.is_duplicate);
        assert_eq!(dup.duplicate_of, Some("L001".to_string()));
        assert_eq!(dup.matched_summary, Some("Test summary".to_string()));
    }

    #[test]
    fn test_detects_exact_duplicate() {
        let existing = vec![existing_learning(
            "L001",
            "Always validate user input",
            LearningStatus::Active,
        )];

        let candidate = CandidateLearning {
            category: "pattern".to_string(),
            summary: "Always validate user input".to_string(),
            detail: "Some different detail that is long enough to be valid.".to_string(),
            scope: "project".to_string(),
            confidence: "high".to_string(),
            criteria_met: vec!["behavior_changing".to_string()],
            tags: vec!["test".to_string()],
            context_files: None,
        };

        let result = check_near_duplicate(&candidate, &existing);
        assert!(result.is_duplicate);
        assert_eq!(result.duplicate_of, Some("L001".to_string()));
    }

    #[test]
    fn test_detects_case_insensitive_duplicate() {
        let existing = vec![existing_learning(
            "L001",
            "Always Validate User Input",
            LearningStatus::Active,
        )];

        let candidate = CandidateLearning {
            category: "pattern".to_string(),
            summary: "always validate user input".to_string(), // Different case
            detail: "Some different detail that is long enough to be valid.".to_string(),
            scope: "project".to_string(),
            confidence: "high".to_string(),
            criteria_met: vec!["behavior_changing".to_string()],
            tags: vec!["test".to_string()],
            context_files: None,
        };

        let result = check_near_duplicate(&candidate, &existing);
        assert!(result.is_duplicate);
    }

    #[test]
    fn test_ignores_archived_learnings() {
        let existing = vec![existing_learning(
            "L001",
            "Always validate user input",
            LearningStatus::Archived,
        )];

        let candidate = CandidateLearning {
            category: "pattern".to_string(),
            summary: "Always validate user input".to_string(),
            detail: "Some different detail that is long enough to be valid.".to_string(),
            scope: "project".to_string(),
            confidence: "high".to_string(),
            criteria_met: vec!["behavior_changing".to_string()],
            tags: vec!["test".to_string()],
            context_files: None,
        };

        let result = check_near_duplicate(&candidate, &existing);
        assert!(!result.is_duplicate); // Archived should be ignored
    }

    #[test]
    fn test_ignores_superseded_learnings() {
        let existing = vec![existing_learning(
            "L001",
            "Always validate user input",
            LearningStatus::Superseded,
        )];

        let candidate = CandidateLearning {
            category: "pattern".to_string(),
            summary: "Always validate user input".to_string(),
            detail: "Some different detail that is long enough to be valid.".to_string(),
            scope: "project".to_string(),
            confidence: "high".to_string(),
            criteria_met: vec!["behavior_changing".to_string()],
            tags: vec!["test".to_string()],
            context_files: None,
        };

        let result = check_near_duplicate(&candidate, &existing);
        assert!(!result.is_duplicate); // Superseded should be ignored
    }

    #[test]
    fn test_allows_different_summaries() {
        let existing = vec![existing_learning(
            "L001",
            "Always validate user input",
            LearningStatus::Active,
        )];

        let candidate = CandidateLearning {
            category: "pattern".to_string(),
            summary: "Use parameterized queries for SQL".to_string(),
            detail: "Some different detail that is long enough to be valid.".to_string(),
            scope: "project".to_string(),
            confidence: "high".to_string(),
            criteria_met: vec!["behavior_changing".to_string()],
            tags: vec!["test".to_string()],
            context_files: None,
        };

        let result = check_near_duplicate(&candidate, &existing);
        assert!(!result.is_duplicate);
    }

    #[test]
    fn test_duplicate_check_with_multiple_existing() {
        let existing = vec![
            existing_learning("L001", "First learning", LearningStatus::Active),
            existing_learning("L002", "Second learning", LearningStatus::Active),
            existing_learning("L003", "Third learning", LearningStatus::Active),
        ];

        let candidate = CandidateLearning {
            category: "pattern".to_string(),
            summary: "Second learning".to_string(),
            detail: "Some different detail that is long enough to be valid.".to_string(),
            scope: "project".to_string(),
            confidence: "high".to_string(),
            criteria_met: vec!["behavior_changing".to_string()],
            tags: vec!["test".to_string()],
            context_files: None,
        };

        let result = check_near_duplicate(&candidate, &existing);
        assert!(result.is_duplicate);
        assert_eq!(result.duplicate_of, Some("L002".to_string()));
    }

    #[test]
    fn test_duplicate_check_empty_existing() {
        let existing: Vec<CompoundLearning> = vec![];

        let candidate = CandidateLearning {
            category: "pattern".to_string(),
            summary: "Some learning".to_string(),
            detail: "Some different detail that is long enough to be valid.".to_string(),
            scope: "project".to_string(),
            confidence: "high".to_string(),
            criteria_met: vec!["behavior_changing".to_string()],
            tags: vec!["test".to_string()],
            context_files: None,
        };

        let result = check_near_duplicate(&candidate, &existing);
        assert!(!result.is_duplicate);
    }

    // =========================================================================
    // validate_with_duplicates tests
    // =========================================================================

    #[test]
    fn test_validate_with_duplicates_no_duplicates() {
        let existing = vec![existing_learning(
            "L001",
            "Existing learning summary",
            LearningStatus::Active,
        )];

        let candidates = vec![valid_candidate()]; // Different summary
        let (valid, rejected) = validate_with_duplicates(candidates, "session-1", &existing);

        assert_eq!(valid.len(), 1);
        assert!(rejected.is_empty());
    }

    #[test]
    fn test_validate_with_duplicates_finds_duplicate() {
        let existing = vec![existing_learning(
            "L001",
            "Use async/await for I/O operations", // Same as valid_candidate
            LearningStatus::Active,
        )];

        let candidates = vec![valid_candidate()];
        let (valid, rejected) = validate_with_duplicates(candidates, "session-1", &existing);

        assert!(valid.is_empty());
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].stage, ValidationStage::Duplicate);
        assert!(rejected[0].rejection_reason.contains("L001"));
    }

    #[test]
    fn test_validate_with_duplicates_mixed() {
        let existing = vec![existing_learning(
            "L001",
            "Use async/await for I/O operations", // Same as valid_candidate
            LearningStatus::Active,
        )];

        let mut different = valid_candidate();
        different.summary = "A completely different learning summary".to_string();

        let candidates = vec![valid_candidate(), different];
        let (valid, rejected) = validate_with_duplicates(candidates, "session-1", &existing);

        assert_eq!(valid.len(), 1);
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].stage, ValidationStage::Duplicate);
    }

    #[test]
    fn test_validate_with_duplicates_schema_before_duplicate() {
        let existing = vec![existing_learning(
            "L001",
            "short", // This would match a short summary
            LearningStatus::Active,
        )];

        // This will fail schema validation (summary too short)
        // before duplicate check
        let mut bad = valid_candidate();
        bad.summary = "short".to_string();

        let candidates = vec![bad];
        let (valid, rejected) = validate_with_duplicates(candidates, "session-1", &existing);

        assert!(valid.is_empty());
        assert_eq!(rejected.len(), 1);
        // Should be rejected by schema, not duplicate
        assert_eq!(rejected[0].stage, ValidationStage::Schema);
    }

    #[test]
    fn test_validate_with_duplicates_rejects_batch_duplicates() {
        let existing: Vec<CompoundLearning> = vec![];

        // Two identical candidates in the same batch
        let candidates = vec![valid_candidate(), valid_candidate()];
        let (valid, rejected) = validate_with_duplicates(candidates, "session-1", &existing);

        // First one passes, second one is rejected as duplicate
        assert_eq!(valid.len(), 1);
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].stage, ValidationStage::Duplicate);
        assert!(rejected[0].rejection_reason.contains("within batch"));
    }

    #[test]
    fn test_validate_with_duplicates_batch_case_insensitive() {
        let existing: Vec<CompoundLearning> = vec![];

        let mut second = valid_candidate();
        // Same summary but different case
        second.summary = "USE ASYNC/AWAIT FOR I/O OPERATIONS".to_string();

        let candidates = vec![valid_candidate(), second];
        let (valid, rejected) = validate_with_duplicates(candidates, "session-1", &existing);

        // First one passes, second (different case) is rejected as duplicate
        assert_eq!(valid.len(), 1);
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].stage, ValidationStage::Duplicate);
    }
}
