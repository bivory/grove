//! Core types and logic for Grove.
//!
//! This module contains the fundamental types for Grove's gate state machine,
//! session management, learning entities, and related types.

pub mod embeddings;
pub mod gate;
pub mod judge;
pub mod learning;
pub mod quality;
pub mod reflect;
pub mod state;

pub use embeddings::cosine_similarity;
pub use gate::Gate;
pub use learning::{
    generate_learning_id, CompoundLearning, Confidence, LearningCategory, LearningScope,
    LearningStatus, WriteGateCriterion, LEARNING_SCHEMA_VERSION, PENDING_LEARNING_ID,
};
pub use quality::{assess_specificity, QualityCheckMode, SpecificityScore};
pub use reflect::{
    validate_with_duplicates, validate_with_duplicates_and_mode,
    validate_with_duplicates_and_quality, validate_with_duplicates_and_quality_semantic,
    CandidateLearning, CriterionPlausibility, DuplicateCheckResult, RejectedCandidate,
    SchemaValidationError, ValidationStage, WriteGateConfidence, WriteGateMode, WriteGateResult,
};
pub use state::{
    CircuitBreakerState, EventType, GateState, GateStatus, InjectedLearning, InjectionOutcome,
    ReflectionResult, SessionState, SkipDecider, SkipDecision, SubagentObservation,
    TicketCloseIntent, TicketContext, TraceEvent,
};
