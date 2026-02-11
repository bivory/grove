//! Core types and logic for Grove.
//!
//! This module contains the fundamental types for Grove's gate state machine,
//! session management, learning entities, and related types.

pub mod gate;
pub mod learning;
pub mod reflect;
pub mod state;

pub use gate::Gate;
pub use learning::{
    generate_learning_id, CompoundLearning, Confidence, LearningCategory, LearningScope,
    LearningStatus, WriteGateCriterion, LEARNING_SCHEMA_VERSION, PENDING_LEARNING_ID,
};
pub use reflect::{
    validate_with_duplicates, CandidateLearning, CriterionPlausibility, DuplicateCheckResult,
    RejectedCandidate, SchemaValidationError, ValidationStage, WriteGateConfidence,
    WriteGateResult,
};
pub use state::{
    CircuitBreakerState, EventType, GateState, GateStatus, InjectedLearning, InjectionOutcome,
    ReflectionResult, SessionState, SkipDecider, SkipDecision, SubagentObservation,
    TicketCloseIntent, TicketContext, TraceEvent,
};
