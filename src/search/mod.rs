//! Search module with optional Tantivy full-text search support.
//!
//! This module provides advanced search capabilities beyond simple substring
//! matching, including stemming, fuzzy matching, and BM25 relevance scoring.
//!
//! # Feature Flag
//!
//! This module requires the `tantivy-search` feature:
//!
//! ```toml
//! [dependencies]
//! grove = { version = "0.5", features = ["tantivy-search"] }
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use grove::search::TantivySearchIndex;
//!
//! let index = TantivySearchIndex::in_memory()?;
//! index.index_learnings(&learnings)?;
//!
//! // Standard search with stemming
//! let results = index.search("rust file handling", 10)?;
//!
//! // Handles typos automatically:
//! let results = index.search("contaner", 10)?; // finds "container"
//! ```
//!
//! # Search Behavior
//!
//! The `search()` method uses stemming with automatic fuzzy fallback:
//! - **Stemming**: "tracking" matches "track", "writes" matches "write"
//! - **Fuzzy fallback**: When few results found, retries with typo tolerance
//! - **BM25 scoring**: Results ranked by relevance with field boosts

#[cfg(feature = "tantivy-search")]
pub mod tantivy_backend;

#[cfg(feature = "tantivy-search")]
pub use tantivy_backend::{TantivySearchIndex, TantivySearchResult};
