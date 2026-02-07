//! Memory backends for Grove.
//!
//! This module provides the trait interface and implementations for
//! memory backends that store and retrieve compound learnings.
//!
//! Available backends:
//! - **Markdown**: Built-in append-only markdown file backend (default)
//! - **Total Recall**: Adapter for Total Recall memory system
//! - **MCP**: Adapter for MCP memory servers

pub mod markdown;
pub mod traits;

pub use markdown::MarkdownBackend;
pub use traits::{MemoryBackend, SearchFilters, SearchQuery, SearchResult, WriteResult};
