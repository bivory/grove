//! Grove - Compound Learning Gate for Claude Code
//!
//! Grove enforces structured reflection at ticket boundaries with pluggable
//! memory backends. It captures learnings when developers complete tickets
//! and injects relevant context at session start.

pub mod backends;
pub mod cli;
pub mod config;
pub mod core;
pub mod discovery;
pub mod error;
pub mod hooks;
pub mod stats;
pub mod storage;

pub use backends::{
    MarkdownBackend, MemoryBackend, SearchFilters, SearchQuery, SearchResult, WriteResult,
};
pub use config::Config;
pub use core::{GateState, GateStatus, SessionState};
pub use discovery::{
    create_primary_backend, detect_backends, detect_ticketing_system, match_close_command,
    probe_beads, probe_markdown, probe_tissue, BackendInfo, BackendType, ClosePattern,
    TicketingInfo, TicketingSystem,
};
pub use error::{GroveError, Result};
pub use stats::{
    rank, rank_learnings, score, weights, AggregateStats, LearningStats, ReflectionStats,
    ScoredLearning, StatsCache, StatsCacheManager, StatsEvent, StatsEventType, StatsLogger,
    WriteGateStats, STATS_SCHEMA_VERSION,
};
pub use storage::{FileSessionStore, SessionStore};

// CLI commands
pub use cli::{
    BackendsCommand, CleanCommand, DebugCommand, InitCommand, ListCommand, MaintainCommand,
    ObserveCommand, ReflectCommand, SearchCommand, SkipCommand, StatsCommand, TicketsCommand,
    TraceCommand,
};
