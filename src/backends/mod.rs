//! Memory backends for Grove.
//!
//! This module provides the trait interface and implementations for
//! memory backends that store and retrieve compound learnings.
//!
//! Available backends:
//! - **Markdown**: Built-in append-only markdown file backend (default)
//! - **Total Recall**: Adapter for Total Recall memory system
//! - **MCP**: Adapter for MCP memory servers
//! - **Fallback**: Wrapper that tries primary, falls back to secondary on failure

pub mod fallback;
pub mod markdown;
pub mod total_recall;
pub mod total_recall_format;
pub mod traits;

pub use fallback::FallbackBackend;
pub use markdown::MarkdownBackend;
pub use total_recall::TotalRecallBackend;
pub use total_recall_format as tr_format;
pub use traits::{MemoryBackend, SearchFilters, SearchQuery, SearchResult, WriteResult};
