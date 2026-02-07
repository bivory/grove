//! Stats tracking for Grove.
//!
//! This module provides quality tracking via an append-only JSONL event log.
//! Events track learning surfacing, references, dismissals, and reflections.
//!
//! The stats log (`.grove/stats.log`) is the source of truth. A materialized
//! cache (`~/.grove/stats-cache.json`) is rebuilt from the log for fast reads.

pub mod cache;
pub mod decay;
pub mod insights;
pub mod scoring;
pub mod tracker;

pub use cache::{
    AggregateStats, CategoryStats, CrossPollinationEdge, LearningStats, ReflectionStats,
    StatsCache, StatsCacheManager, WriteGateStats,
};
pub use decay::{
    evaluate as evaluate_decay, get_decay_warnings, get_immune_learnings, run_decay_and_log,
    run_decay_evaluation, should_run_decay_check, DecayResult,
};
pub use insights::{
    generate_all as generate_insights, generate_cross_pollination_insight, generate_decay_warning,
    has_insights, Insight, InsightConfig, InsightKind,
};
pub use scoring::{rank, rank_learnings, score, weights, ScoredLearning};
pub use tracker::{StatsEvent, StatsEventType, StatsLogger, STATS_SCHEMA_VERSION};
