//! Offline evaluation harness for retrieval quality benchmarks.
//!
//! Provides corpus loading, LLM judge integration, metrics aggregation,
//! and benchmark orchestration. Extracted from the replay harness test
//! infrastructure to power the `grove eval` CLI command.

pub mod corpus;
pub mod judge;
pub mod metrics;
pub mod runner;

pub use corpus::{
    build_negative_corpus, build_session_contexts, condense_transcript, load_learnings,
    parse_all_tool_calls, parse_session_transcript, CorpusEntry, CorpusManifest, SessionContext,
    SessionSummary, ToolCall,
};
pub use judge::{JudgeContext, JudgeResult};
pub use metrics::{
    BenchmarkMetrics, ConfidenceInterval, EvalOutput, JudgeStats, NegativePairResult,
    NegativeSweepOutput, RecallData, SweepCorpusResult, SweepOutput,
};
pub use runner::{run_benchmark, run_benchmark_batch, BenchmarkConfig, BoostParams};
