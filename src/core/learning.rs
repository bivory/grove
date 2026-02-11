//! Learning entity types for Grove.
//!
//! These types represent compound learnings captured through structured
//! reflection. Each learning has a category, scope, confidence level,
//! and lifecycle status.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};

/// Placeholder ID used before the backend assigns a real ID.
///
/// Learnings created with `CompoundLearning::new()` use this placeholder.
/// The backend's `next_id()` method must be called to assign a unique ID
/// before persisting the learning.
pub const PENDING_LEARNING_ID: &str = "pending";

/// Counter for generating unique learning IDs within the same day.
/// Used only for backward compatibility and testing.
static LEARNING_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Schema version for learning serialization.
///
/// Increment when the schema changes in a breaking way.
pub const LEARNING_SCHEMA_VERSION: u8 = 1;

/// Compound learning entity.
///
/// Represents a learning captured through structured reflection. Learnings
/// have categories, scopes, confidence levels, and lifecycle status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompoundLearning {
    /// Unique identifier (format: cl_YYYYMMDD_NNN).
    pub id: String,
    /// Schema version for forward compatibility.
    pub schema_version: u8,
    /// Category of the learning.
    pub category: LearningCategory,
    /// Brief summary (10-200 characters).
    pub summary: String,
    /// Detailed explanation (20-2000 characters).
    pub detail: String,
    /// Scope of the learning.
    pub scope: LearningScope,
    /// Confidence level.
    pub confidence: Confidence,
    /// Write gate criteria that were met.
    pub criteria_met: Vec<WriteGateCriterion>,
    /// Tags for categorization and search.
    pub tags: Vec<String>,
    /// Session ID where this learning was captured.
    pub session_id: String,
    /// Ticket ID associated with this learning (if any).
    pub ticket_id: Option<String>,
    /// When the learning was created.
    pub timestamp: DateTime<Utc>,
    /// Files that provide context for this learning.
    pub context_files: Option<Vec<String>>,
    /// Current status of the learning.
    pub status: LearningStatus,
}

impl CompoundLearning {
    /// Create a new learning with the given category and content.
    ///
    /// The learning is created with a placeholder ID (`PENDING_LEARNING_ID`).
    /// Before persisting, use `with_id()` or set `.id` directly using the
    /// backend's `next_id()` method to assign a unique identifier.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        category: LearningCategory,
        summary: impl Into<String>,
        detail: impl Into<String>,
        scope: LearningScope,
        confidence: Confidence,
        criteria_met: Vec<WriteGateCriterion>,
        tags: Vec<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            id: PENDING_LEARNING_ID.to_string(),
            schema_version: LEARNING_SCHEMA_VERSION,
            category,
            summary: summary.into(),
            detail: detail.into(),
            scope,
            confidence,
            criteria_met,
            tags,
            session_id: session_id.into(),
            ticket_id: None,
            timestamp: Utc::now(),
            context_files: None,
            status: LearningStatus::Active,
        }
    }

    /// Set the learning ID.
    ///
    /// Use this with the backend's `next_id()` method to assign a unique ID
    /// before persisting the learning.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Set the ticket ID.
    pub fn with_ticket_id(mut self, ticket_id: impl Into<String>) -> Self {
        self.ticket_id = Some(ticket_id.into());
        self
    }

    /// Set the context files.
    pub fn with_context_files(mut self, files: Vec<String>) -> Self {
        self.context_files = Some(files);
        self
    }

    /// Archive this learning.
    pub fn archive(&mut self) {
        self.status = LearningStatus::Archived;
    }

    /// Mark this learning as superseded.
    pub fn supersede(&mut self) {
        self.status = LearningStatus::Superseded;
    }

    /// Reactivate an archived learning.
    pub fn reactivate(&mut self) {
        self.status = LearningStatus::Active;
    }

    /// Check if this learning is active.
    pub fn is_active(&self) -> bool {
        self.status == LearningStatus::Active
    }
}

/// Generate a learning ID using a process-local counter.
///
/// Format: cl_YYYYMMDD_NNN where NNN is a counter that resets daily.
///
/// **Warning**: This function uses a process-local counter that is NOT
/// safe across multiple processes. Use the backend's `next_id()` method
/// instead for production code to avoid ID collisions.
///
/// This function is primarily retained for backward compatibility and testing.
pub fn generate_learning_id() -> String {
    let now = Utc::now();
    let counter = LEARNING_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("cl_{}_{:03}", now.format("%Y%m%d"), counter % 1000)
}

/// Reset the learning ID counter.
///
/// Primarily for testing purposes.
#[cfg(test)]
pub fn reset_learning_counter() {
    LEARNING_COUNTER.store(0, Ordering::SeqCst);
}

/// Category of a learning.
///
/// Each category represents a different type of insight captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningCategory {
    /// A reusable code pattern or architectural approach.
    Pattern,
    /// A mistake made or gotcha encountered (with fix).
    Pitfall,
    /// A project convention learned or established.
    Convention,
    /// Something learned about a library, API, or external system.
    Dependency,
    /// A workflow improvement or development process insight.
    Process,
    /// Business logic or domain knowledge captured.
    Domain,
    /// A debugging technique or diagnostic approach that worked.
    Debugging,
}

impl LearningCategory {
    /// Get all category variants.
    pub fn all() -> &'static [LearningCategory] {
        &[
            LearningCategory::Pattern,
            LearningCategory::Pitfall,
            LearningCategory::Convention,
            LearningCategory::Dependency,
            LearningCategory::Process,
            LearningCategory::Domain,
            LearningCategory::Debugging,
        ]
    }

    /// Get the display name for this category.
    pub fn display_name(&self) -> &'static str {
        match self {
            LearningCategory::Pattern => "Pattern",
            LearningCategory::Pitfall => "Pitfall",
            LearningCategory::Convention => "Convention",
            LearningCategory::Dependency => "Dependency",
            LearningCategory::Process => "Process",
            LearningCategory::Domain => "Domain",
            LearningCategory::Debugging => "Debugging",
        }
    }
}

/// Scope of a learning.
///
/// Determines where the learning is stored and who can see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LearningScope {
    /// Stored in the primary backend, visible to whole team.
    #[default]
    Project,
    /// Stored in ~/.grove/personal-learnings.md, visible only to individual.
    Personal,
    /// Stored in the primary backend, visible to whole team.
    Team,
    /// Daily log only (if available), transient.
    Ephemeral,
}

impl LearningScope {
    /// Check if this scope is committed to the repository.
    pub fn is_committed(&self) -> bool {
        matches!(self, LearningScope::Project | LearningScope::Team)
    }

    /// Check if this scope is personal (not shared).
    pub fn is_personal(&self) -> bool {
        matches!(self, LearningScope::Personal | LearningScope::Ephemeral)
    }
}

/// Status of a learning in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LearningStatus {
    /// Learning is active and can be surfaced.
    #[default]
    Active,
    /// Learning has been archived due to decay or manual action.
    Archived,
    /// Learning has been superseded by a newer learning.
    Superseded,
}

/// Confidence level for a learning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// High confidence - well tested or established.
    High,
    /// Medium confidence - reasonable certainty.
    #[default]
    Medium,
    /// Low confidence - experimental or uncertain.
    Low,
}

/// Write gate criterion that a learning can meet.
///
/// Each learning must claim at least one criterion to pass the write gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteGateCriterion {
    /// Would you do something differently next time?
    BehaviorChanging,
    /// Why was X chosen over Y?
    DecisionRationale,
    /// Will this matter in future sessions?
    StableFact,
    /// Did the user say "remember this"?
    ExplicitRequest,
}

impl WriteGateCriterion {
    /// Get all criterion variants.
    pub fn all() -> &'static [WriteGateCriterion] {
        &[
            WriteGateCriterion::BehaviorChanging,
            WriteGateCriterion::DecisionRationale,
            WriteGateCriterion::StableFact,
            WriteGateCriterion::ExplicitRequest,
        ]
    }

    /// Get the display name for this criterion.
    pub fn display_name(&self) -> &'static str {
        match self {
            WriteGateCriterion::BehaviorChanging => "Behavior Changing",
            WriteGateCriterion::DecisionRationale => "Decision Rationale",
            WriteGateCriterion::StableFact => "Stable Fact",
            WriteGateCriterion::ExplicitRequest => "Explicit Request",
        }
    }

    /// Get the question for this criterion.
    pub fn question(&self) -> &'static str {
        match self {
            WriteGateCriterion::BehaviorChanging => "Would you do something differently next time?",
            WriteGateCriterion::DecisionRationale => "Why was X chosen over Y?",
            WriteGateCriterion::StableFact => "Will this matter in future sessions?",
            WriteGateCriterion::ExplicitRequest => "Did the user say \"remember this\"?",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_learning_id() {
        reset_learning_counter();

        let id1 = generate_learning_id();
        let id2 = generate_learning_id();

        assert!(id1.starts_with("cl_"));
        assert!(id1.ends_with("_000"));
        assert!(id2.ends_with("_001"));
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_learning_id_format() {
        reset_learning_counter();

        let id = generate_learning_id();

        // Format: cl_YYYYMMDD_NNN
        // cl_ = 3, YYYYMMDD = 8, _ = 1, NNN = 3, total = 15
        assert!(id.starts_with("cl_"));
        assert_eq!(id.len(), 15);
    }

    #[test]
    fn test_compound_learning_new() {
        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            "Use builder pattern for complex objects",
            "The builder pattern provides a flexible way to construct complex objects step by step.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["pattern".to_string(), "design".to_string()],
            "session-123",
        );

        // New learnings get a placeholder ID that must be set before persisting
        assert_eq!(learning.id, PENDING_LEARNING_ID);
        assert_eq!(learning.schema_version, LEARNING_SCHEMA_VERSION);
        assert_eq!(learning.category, LearningCategory::Pattern);
        assert_eq!(learning.summary, "Use builder pattern for complex objects");
        assert_eq!(learning.scope, LearningScope::Project);
        assert_eq!(learning.confidence, Confidence::High);
        assert_eq!(learning.criteria_met.len(), 1);
        assert_eq!(learning.tags.len(), 2);
        assert_eq!(learning.session_id, "session-123");
        assert!(learning.ticket_id.is_none());
        assert!(learning.context_files.is_none());
        assert_eq!(learning.status, LearningStatus::Active);
    }

    #[test]
    fn test_compound_learning_with_id() {
        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            "Test learning",
            "Test detail that is long enough to pass validation requirements.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["test".to_string()],
            "session-123",
        )
        .with_id("cl_20260101_042");

        assert_eq!(learning.id, "cl_20260101_042");
    }

    #[test]
    fn test_compound_learning_with_ticket() {
        reset_learning_counter();

        let learning = CompoundLearning::new(
            LearningCategory::Pitfall,
            "Avoid N+1 queries",
            "Always use eager loading when fetching related entities.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["database".to_string()],
            "session-123",
        )
        .with_ticket_id("TICKET-456");

        assert_eq!(learning.ticket_id, Some("TICKET-456".to_string()));
    }

    #[test]
    fn test_compound_learning_with_context_files() {
        reset_learning_counter();

        let learning = CompoundLearning::new(
            LearningCategory::Convention,
            "Use snake_case for file names",
            "All Rust source files should use snake_case naming.",
            LearningScope::Project,
            Confidence::Medium,
            vec![WriteGateCriterion::StableFact],
            vec!["naming".to_string()],
            "session-123",
        )
        .with_context_files(vec!["src/main.rs".to_string(), "src/lib.rs".to_string()]);

        assert_eq!(learning.context_files.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_learning_lifecycle() {
        reset_learning_counter();

        let mut learning = CompoundLearning::new(
            LearningCategory::Domain,
            "Users can have multiple roles",
            "The authorization system supports multi-role users.",
            LearningScope::Team,
            Confidence::High,
            vec![WriteGateCriterion::StableFact],
            vec!["auth".to_string()],
            "session-123",
        );

        assert!(learning.is_active());
        assert_eq!(learning.status, LearningStatus::Active);

        learning.archive();
        assert!(!learning.is_active());
        assert_eq!(learning.status, LearningStatus::Archived);

        learning.reactivate();
        assert!(learning.is_active());
        assert_eq!(learning.status, LearningStatus::Active);

        learning.supersede();
        assert!(!learning.is_active());
        assert_eq!(learning.status, LearningStatus::Superseded);
    }

    #[test]
    fn test_learning_category_all() {
        let all = LearningCategory::all();
        assert_eq!(all.len(), 7);
    }

    #[test]
    fn test_learning_category_display_name() {
        assert_eq!(LearningCategory::Pattern.display_name(), "Pattern");
        assert_eq!(LearningCategory::Pitfall.display_name(), "Pitfall");
        assert_eq!(LearningCategory::Debugging.display_name(), "Debugging");
    }

    #[test]
    fn test_learning_scope_is_committed() {
        assert!(LearningScope::Project.is_committed());
        assert!(LearningScope::Team.is_committed());
        assert!(!LearningScope::Personal.is_committed());
        assert!(!LearningScope::Ephemeral.is_committed());
    }

    #[test]
    fn test_learning_scope_is_personal() {
        assert!(!LearningScope::Project.is_personal());
        assert!(!LearningScope::Team.is_personal());
        assert!(LearningScope::Personal.is_personal());
        assert!(LearningScope::Ephemeral.is_personal());
    }

    #[test]
    fn test_write_gate_criterion_all() {
        let all = WriteGateCriterion::all();
        assert_eq!(all.len(), 4);
    }

    #[test]
    fn test_write_gate_criterion_display_name() {
        assert_eq!(
            WriteGateCriterion::BehaviorChanging.display_name(),
            "Behavior Changing"
        );
        assert_eq!(
            WriteGateCriterion::DecisionRationale.display_name(),
            "Decision Rationale"
        );
    }

    #[test]
    fn test_write_gate_criterion_question() {
        assert_eq!(
            WriteGateCriterion::BehaviorChanging.question(),
            "Would you do something differently next time?"
        );
    }

    #[test]
    fn test_learning_serialization() {
        reset_learning_counter();

        let learning = CompoundLearning::new(
            LearningCategory::Process,
            "Run tests before committing",
            "Always run the test suite before pushing to ensure CI passes.",
            LearningScope::Project,
            Confidence::High,
            vec![
                WriteGateCriterion::BehaviorChanging,
                WriteGateCriterion::StableFact,
            ],
            vec!["testing".to_string(), "ci".to_string()],
            "session-789",
        )
        .with_ticket_id("TICKET-100")
        .with_context_files(vec!["src/tests.rs".to_string()]);

        let json = serde_json::to_string(&learning).unwrap();
        let deserialized: CompoundLearning = serde_json::from_str(&json).unwrap();

        assert_eq!(learning.id, deserialized.id);
        assert_eq!(learning.category, deserialized.category);
        assert_eq!(learning.summary, deserialized.summary);
        assert_eq!(learning.detail, deserialized.detail);
        assert_eq!(learning.scope, deserialized.scope);
        assert_eq!(learning.confidence, deserialized.confidence);
        assert_eq!(learning.criteria_met.len(), deserialized.criteria_met.len());
        assert_eq!(learning.tags, deserialized.tags);
        assert_eq!(learning.ticket_id, deserialized.ticket_id);
        assert_eq!(learning.context_files, deserialized.context_files);
        assert_eq!(learning.status, deserialized.status);
    }

    #[test]
    fn test_learning_category_serialization() {
        let categories = LearningCategory::all();

        for &category in categories {
            let json = serde_json::to_string(&category).unwrap();
            let deserialized: LearningCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(category, deserialized);
        }
    }

    #[test]
    fn test_learning_scope_serialization() {
        let scopes = [
            LearningScope::Project,
            LearningScope::Personal,
            LearningScope::Team,
            LearningScope::Ephemeral,
        ];

        for scope in scopes {
            let json = serde_json::to_string(&scope).unwrap();
            let deserialized: LearningScope = serde_json::from_str(&json).unwrap();
            assert_eq!(scope, deserialized);
        }
    }

    #[test]
    fn test_learning_status_serialization() {
        let statuses = [
            LearningStatus::Active,
            LearningStatus::Archived,
            LearningStatus::Superseded,
        ];

        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let deserialized: LearningStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, deserialized);
        }
    }

    #[test]
    fn test_confidence_serialization() {
        let confidences = [Confidence::High, Confidence::Medium, Confidence::Low];

        for confidence in confidences {
            let json = serde_json::to_string(&confidence).unwrap();
            let deserialized: Confidence = serde_json::from_str(&json).unwrap();
            assert_eq!(confidence, deserialized);
        }
    }

    #[test]
    fn test_write_gate_criterion_serialization() {
        let criteria = WriteGateCriterion::all();

        for &criterion in criteria {
            let json = serde_json::to_string(&criterion).unwrap();
            let deserialized: WriteGateCriterion = serde_json::from_str(&json).unwrap();
            assert_eq!(criterion, deserialized);
        }
    }

    #[test]
    fn test_default_values() {
        assert_eq!(LearningScope::default(), LearningScope::Project);
        assert_eq!(LearningStatus::default(), LearningStatus::Active);
        assert_eq!(Confidence::default(), Confidence::Medium);
    }

    // =========================================================================
    // Property-based tests
    // =========================================================================

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        fn arb_category() -> impl Strategy<Value = LearningCategory> {
            prop_oneof![
                Just(LearningCategory::Pattern),
                Just(LearningCategory::Pitfall),
                Just(LearningCategory::Convention),
                Just(LearningCategory::Dependency),
                Just(LearningCategory::Process),
                Just(LearningCategory::Domain),
                Just(LearningCategory::Debugging),
            ]
        }

        fn arb_scope() -> impl Strategy<Value = LearningScope> {
            prop_oneof![
                Just(LearningScope::Project),
                Just(LearningScope::Personal),
                Just(LearningScope::Team),
                Just(LearningScope::Ephemeral),
            ]
        }

        fn arb_confidence() -> impl Strategy<Value = Confidence> {
            prop_oneof![
                Just(Confidence::High),
                Just(Confidence::Medium),
                Just(Confidence::Low),
            ]
        }

        fn arb_status() -> impl Strategy<Value = LearningStatus> {
            prop_oneof![
                Just(LearningStatus::Active),
                Just(LearningStatus::Archived),
                Just(LearningStatus::Superseded),
            ]
        }

        fn arb_criterion() -> impl Strategy<Value = WriteGateCriterion> {
            prop_oneof![
                Just(WriteGateCriterion::BehaviorChanging),
                Just(WriteGateCriterion::DecisionRationale),
                Just(WriteGateCriterion::StableFact),
                Just(WriteGateCriterion::ExplicitRequest),
            ]
        }

        fn arb_criteria() -> impl Strategy<Value = Vec<WriteGateCriterion>> {
            prop::collection::vec(arb_criterion(), 1..4)
        }

        fn arb_tags() -> impl Strategy<Value = Vec<String>> {
            prop::collection::vec("[a-z]{3,10}", 0..5)
        }

        proptest! {
            // Property: Category round-trips through JSON
            #[test]
            fn prop_category_json_roundtrip(category in arb_category()) {
                let json = serde_json::to_string(&category).unwrap();
                let deserialized: LearningCategory = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(category, deserialized);
            }

            // Property: Scope round-trips through JSON
            #[test]
            fn prop_scope_json_roundtrip(scope in arb_scope()) {
                let json = serde_json::to_string(&scope).unwrap();
                let deserialized: LearningScope = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(scope, deserialized);
            }

            // Property: Confidence round-trips through JSON
            #[test]
            fn prop_confidence_json_roundtrip(confidence in arb_confidence()) {
                let json = serde_json::to_string(&confidence).unwrap();
                let deserialized: Confidence = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(confidence, deserialized);
            }

            // Property: Status round-trips through JSON
            #[test]
            fn prop_status_json_roundtrip(status in arb_status()) {
                let json = serde_json::to_string(&status).unwrap();
                let deserialized: LearningStatus = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(status, deserialized);
            }

            // Property: Criterion round-trips through JSON
            #[test]
            fn prop_criterion_json_roundtrip(criterion in arb_criterion()) {
                let json = serde_json::to_string(&criterion).unwrap();
                let deserialized: WriteGateCriterion = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(criterion, deserialized);
            }

            // Property: CompoundLearning round-trips through JSON
            #[test]
            fn prop_learning_json_roundtrip(
                category in arb_category(),
                scope in arb_scope(),
                confidence in arb_confidence(),
                criteria in arb_criteria(),
                tags in arb_tags(),
                summary in "[a-zA-Z0-9 ]{5,50}",
                detail in "[a-zA-Z0-9 .,]{10,200}",
            ) {
                let learning = CompoundLearning::new(
                    category,
                    &summary,
                    &detail,
                    scope,
                    confidence,
                    criteria.clone(),
                    tags.clone(),
                    "session-test",
                );

                let json = serde_json::to_string(&learning).unwrap();
                let deserialized: CompoundLearning = serde_json::from_str(&json).unwrap();

                prop_assert_eq!(learning.category, deserialized.category);
                prop_assert_eq!(learning.summary, deserialized.summary);
                prop_assert_eq!(learning.detail, deserialized.detail);
                prop_assert_eq!(learning.scope, deserialized.scope);
                prop_assert_eq!(learning.confidence, deserialized.confidence);
                prop_assert_eq!(learning.tags, deserialized.tags);
            }

            // Property: Personal/Team scope consistency
            #[test]
            fn prop_scope_personal_vs_committed(scope in arb_scope()) {
                // A scope cannot be both personal and committed
                if scope.is_personal() {
                    prop_assert!(!scope.is_committed());
                }
                if scope.is_committed() {
                    prop_assert!(!scope.is_personal());
                }
            }

            // Property: Active status implies is_active()
            #[test]
            fn prop_status_active_consistency(status in arb_status()) {
                let mut learning = CompoundLearning::new(
                    LearningCategory::Pattern,
                    "Test",
                    "Detail",
                    LearningScope::Project,
                    Confidence::High,
                    vec![WriteGateCriterion::BehaviorChanging],
                    vec![],
                    "session",
                );
                learning.status = status;

                let is_active = learning.is_active();
                prop_assert_eq!(is_active, status == LearningStatus::Active);
            }
        }
    }
}
