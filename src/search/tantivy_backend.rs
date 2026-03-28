//! Tantivy-based search backend for Grove.
//!
//! This module provides full-text search with BM25 relevance scoring,
//! stemming, and fuzzy matching using Tantivy.

use std::collections::HashSet;
use std::path::Path;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, FuzzyTermQuery, Occur, Query, QueryParser};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED, STRING,
};
use tantivy::tokenizer::TextAnalyzer;
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::core::CompoundLearning;
use crate::error::{GroveError, Result};

/// Field weights for relevance scoring.
const SUMMARY_BOOST: f32 = 2.0;
const TAGS_BOOST: f32 = 1.5;
const RELEVANCE_CONTEXT_BOOST: f32 = 1.5;
const DETAIL_BOOST: f32 = 1.0;

/// Heap size for IndexWriter (15MB).
/// Reduced from Tantivy's default 50MB to work better on constrained systems.
/// This is sufficient for Grove's typical workload of hundreds of learnings.
const INDEX_WRITER_HEAP_SIZE: usize = 15_000_000;

/// Escape special characters in query strings to prevent query injection.
///
/// Tantivy's query parser supports special syntax (AND, OR, field:value, wildcards, etc.)
/// which could cause unexpected behavior if user input is passed directly.
/// This function escapes all special characters so they're treated as literals.
fn escape_query(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len() * 2);
    for c in query.chars() {
        match c {
            '+' | '-' | '!' | '(' | ')' | '{' | '}' | '[' | ']' | '^' | '"' | '~' | '*' | '?'
            | ':' | '\\' | '/' => {
                escaped.push('\\');
                escaped.push(c);
            }
            _ => escaped.push(c),
        }
    }
    escaped
}

/// Escape special characters in a single query term.
///
/// Like [`escape_query`] but public, for use by callers that build
/// boosted query strings (e.g., `term^2.0`) where the boost syntax
/// must be appended after escaping.
pub fn escape_query_term(term: &str) -> String {
    escape_query(term)
}

/// A search result from Tantivy.
#[derive(Debug, Clone)]
pub struct TantivySearchResult {
    /// Learning ID.
    pub id: String,
    /// Relevance score (higher is better).
    pub score: f32,
}

/// Tantivy search index for learnings.
///
/// Provides full-text search with:
/// - **Stemming**: "tracking" matches "track", "writes" matches "write"
/// - **Fuzzy matching**: Typo tolerance via edit distance
/// - **BM25 scoring**: Relevance ranking with field boosts
///
/// # Example
///
/// ```ignore
/// let index = TantivySearchIndex::in_memory()?;
/// index.index_learnings(&learnings)?;
/// let results = index.search("rust file handling", 10)?;
/// ```
pub struct TantivySearchIndex {
    index: Index,
    reader: IndexReader,
    // Field handles for document construction and querying
    id_field: Field,
    summary_field: Field,
    detail_field: Field,
    tags_field: Field,
    category_field: Field,
    relevance_context_field: Field,
}

impl TantivySearchIndex {
    /// Create a new in-memory index.
    pub fn in_memory() -> Result<Self> {
        let schema = Self::build_schema();
        let index = Index::create_in_ram(schema.clone());
        Self::from_index_and_schema(index, schema)
    }

    /// Create a persistent index at the given path.
    pub fn persistent(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path)
            .map_err(|e| GroveError::backend(format!("Failed to create index dir: {}", e)))?;

        let schema = Self::build_schema();
        let index = Index::create_in_dir(path, schema.clone())
            .or_else(|_| Index::open_in_dir(path))
            .map_err(|e| GroveError::backend(format!("Failed to open index: {}", e)))?;

        Self::from_index_and_schema(index, schema)
    }

    /// Build a TantivySearchIndex from an index and schema, extracting field handles.
    fn from_index_and_schema(index: Index, schema: Schema) -> Result<Self> {
        Self::register_tokenizers(&index);

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(|e| GroveError::backend(format!("Failed to create reader: {}", e)))?;

        let field = |name: &str| -> Result<Field> {
            schema.get_field(name).map_err(|_| {
                GroveError::backend(format!("Schema missing required field: {}", name))
            })
        };

        Ok(Self {
            index,
            reader,
            id_field: field("id")?,
            summary_field: field("summary")?,
            detail_field: field("detail")?,
            tags_field: field("tags")?,
            category_field: field("category")?,
            relevance_context_field: field("relevance_context")?,
        })
    }

    /// Build the schema for learnings with stemming support.
    fn build_schema() -> Schema {
        let mut schema_builder = Schema::builder();

        // ID is stored and indexed (STRING = indexed without tokenization)
        // This allows delete-by-ID for upsert behavior
        schema_builder.add_text_field("id", STRING | STORED);

        // Text field options with stemming tokenizer
        let text_options_stored = TextOptions::default().set_stored().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("en_stem")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );

        let text_options = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("en_stem")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );

        // Text fields for full-text search with stemming
        schema_builder.add_text_field("summary", text_options_stored.clone());
        schema_builder.add_text_field("detail", text_options.clone());
        schema_builder.add_text_field("tags", text_options_stored.clone());
        schema_builder.add_text_field("category", text_options_stored);
        schema_builder.add_text_field("relevance_context", text_options);

        schema_builder.build()
    }

    /// Register the stemming tokenizer with the index.
    fn register_tokenizers(index: &Index) {
        let tokenizer = TextAnalyzer::builder(tantivy::tokenizer::SimpleTokenizer::default())
            .filter(tantivy::tokenizer::LowerCaser)
            .filter(tantivy::tokenizer::Stemmer::new(
                tantivy::tokenizer::Language::English,
            ))
            .build();
        index.tokenizers().register("en_stem", tokenizer);
    }

    /// Index a batch of learnings (upsert behavior).
    ///
    /// If a learning with the same ID already exists, it will be replaced.
    /// This prevents duplicate documents when re-indexing.
    pub fn index_learnings(&self, learnings: &[CompoundLearning]) -> Result<()> {
        let mut writer: IndexWriter = self.index.writer(INDEX_WRITER_HEAP_SIZE).map_err(|e| {
            GroveError::backend(format!(
                "Failed to allocate {}MB for index writer: {}. \
                     System may be memory constrained.",
                INDEX_WRITER_HEAP_SIZE / 1_000_000,
                e
            ))
        })?;

        for learning in learnings {
            // Delete any existing document with this ID (upsert behavior)
            let id_term = Term::from_field_text(self.id_field, &learning.id);
            writer.delete_term(id_term);

            let tags_text = learning.tags.join(" ");
            let category_text = format!("{:?}", learning.category).to_lowercase();

            let relevance_text = learning.relevance_context.clone().unwrap_or_default();

            writer
                .add_document(doc!(
                    self.id_field => learning.id.clone(),
                    self.summary_field => learning.summary.clone(),
                    self.detail_field => learning.detail.clone(),
                    self.tags_field => tags_text,
                    self.category_field => category_text,
                    self.relevance_context_field => relevance_text,
                ))
                .map_err(|e| GroveError::backend(format!("Failed to add document: {}", e)))?;
        }

        writer
            .commit()
            .map_err(|e| GroveError::backend(format!("Failed to commit: {}", e)))?;

        // Reload the reader to see the new documents
        self.reader
            .reload()
            .map_err(|e| GroveError::backend(format!("Failed to reload reader: {}", e)))?;

        Ok(())
    }

    /// Stemmed search without fuzzy fallback.
    ///
    /// Use `search()` for the recommended search behavior with fuzzy fallback.
    /// This method is exposed for testing and benchmarking purposes.
    #[doc(hidden)]
    pub fn search_stemmed(
        &self,
        query_str: &str,
        limit: usize,
    ) -> Result<Vec<TantivySearchResult>> {
        if limit == 0 || query_str.trim().is_empty() {
            return Ok(Vec::new());
        }
        let searcher = self.reader.searcher();

        // Create query parser for multiple fields with boosting
        let mut query_parser = QueryParser::for_index(
            &self.index,
            vec![
                self.summary_field,
                self.detail_field,
                self.tags_field,
                self.category_field,
                self.relevance_context_field,
            ],
        );

        // Set field boosts
        query_parser.set_field_boost(self.summary_field, SUMMARY_BOOST);
        query_parser.set_field_boost(self.tags_field, TAGS_BOOST);
        query_parser.set_field_boost(self.relevance_context_field, RELEVANCE_CONTEXT_BOOST);
        query_parser.set_field_boost(self.detail_field, DETAIL_BOOST);

        // Escape special characters to prevent query injection
        let escaped_query = escape_query(query_str);
        let query = query_parser
            .parse_query(&escaped_query)
            .map_err(|e| GroveError::backend(format!("Failed to parse query: {}", e)))?;

        self.execute_search(&searcher, &*query, limit)
    }

    /// Stemmed search with pre-escaped query containing boost syntax.
    ///
    /// Unlike [`search_stemmed`], this method does NOT escape the query string.
    /// The caller is responsible for escaping individual terms before appending
    /// boost suffixes (e.g., `escaped_term^2.0`). Use [`escape_query_term`]
    /// for per-term escaping.
    ///
    /// This is used for benchmark evaluation of per-term BM25 boosting.
    pub fn search_boosted(
        &self,
        pre_escaped_query: &str,
        limit: usize,
    ) -> Result<Vec<TantivySearchResult>> {
        if limit == 0 || pre_escaped_query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let searcher = self.reader.searcher();

        let mut query_parser = QueryParser::for_index(
            &self.index,
            vec![
                self.summary_field,
                self.detail_field,
                self.tags_field,
                self.category_field,
                self.relevance_context_field,
            ],
        );

        query_parser.set_field_boost(self.summary_field, SUMMARY_BOOST);
        query_parser.set_field_boost(self.tags_field, TAGS_BOOST);
        query_parser.set_field_boost(self.relevance_context_field, RELEVANCE_CONTEXT_BOOST);
        query_parser.set_field_boost(self.detail_field, DETAIL_BOOST);

        // No escaping — query is pre-escaped with boost syntax intact
        let query = query_parser
            .parse_query(pre_escaped_query)
            .map_err(|e| GroveError::backend(format!("Failed to parse boosted query: {}", e)))?;

        self.execute_search(&searcher, &*query, limit)
    }

    /// Fuzzy search with typo tolerance (edit distance).
    ///
    /// Use `search()` for the recommended search behavior with automatic fuzzy fallback.
    /// This method is exposed for testing and benchmarking purposes.
    #[doc(hidden)]
    pub fn search_fuzzy(
        &self,
        query_str: &str,
        limit: usize,
        edit_distance: u8,
    ) -> Result<Vec<TantivySearchResult>> {
        if limit == 0 || query_str.trim().is_empty() {
            return Ok(Vec::new());
        }
        let searcher = self.reader.searcher();

        // Build fuzzy queries for each term across all searchable fields
        let terms: Vec<&str> = query_str.split_whitespace().collect();
        let fields = [
            self.summary_field,
            self.detail_field,
            self.tags_field,
            self.category_field,
            self.relevance_context_field,
        ];

        let mut subqueries: Vec<(Occur, Box<dyn Query>)> = Vec::new();

        for term_str in &terms {
            let term_lower = term_str.to_lowercase();
            let mut field_queries: Vec<(Occur, Box<dyn Query>)> = Vec::new();

            // Use smaller edit distance for short terms to reduce false positives
            // Short terms (<=4 chars) with edit distance 2 match too many unrelated words
            let term_edit_distance = if term_lower.len() <= 4 {
                edit_distance.min(1)
            } else {
                edit_distance
            };

            for field in &fields {
                let term = Term::from_field_text(*field, &term_lower);
                let fuzzy_query = FuzzyTermQuery::new(term, term_edit_distance, true);
                field_queries.push((Occur::Should, Box::new(fuzzy_query)));
            }

            // OR across fields for this term
            let term_query = BooleanQuery::new(field_queries);
            // AND between terms (all terms must match somewhere)
            subqueries.push((Occur::Must, Box::new(term_query)));
        }

        let query = BooleanQuery::new(subqueries);
        self.execute_search(&searcher, &query, limit)
    }

    /// Search for learnings matching the query.
    ///
    /// Uses stemming for better term matching (e.g., "tracking" matches "track"),
    /// with automatic fuzzy fallback for typo tolerance when few results are found.
    ///
    /// # Arguments
    /// * `query_str` - Search query (multiple terms supported)
    /// * `limit` - Maximum results to return
    ///
    /// # Returns
    /// Results ordered by relevance score (highest first).
    ///
    /// # Example
    /// ```ignore
    /// let results = index.search("rust file handling", 10)?;
    /// // Also handles typos:
    /// let results = index.search("contaner", 10)?; // finds "container"
    /// ```
    pub fn search(&self, query_str: &str, limit: usize) -> Result<Vec<TantivySearchResult>> {
        if limit == 0 || query_str.trim().is_empty() {
            return Ok(Vec::new());
        }

        // First try stemmed search
        let mut results = self.search_stemmed(query_str, limit)?;

        // If we got few results, supplement with fuzzy search (edit distance 2)
        if results.len() < limit {
            let fuzzy_results = self.search_fuzzy(query_str, limit, 2)?;

            // Merge results, avoiding duplicates
            let seen: HashSet<String> = results.iter().map(|r| r.id.clone()).collect();
            for result in fuzzy_results {
                if !seen.contains(&result.id) && results.len() < limit {
                    results.push(result);
                }
            }
        }

        Ok(results)
    }

    /// Execute a search query and collect results.
    fn execute_search(
        &self,
        searcher: &tantivy::Searcher,
        query: &dyn Query,
        limit: usize,
    ) -> Result<Vec<TantivySearchResult>> {
        let top_docs = searcher
            .search(query, &TopDocs::with_limit(limit))
            .map_err(|e| GroveError::backend(format!("Search failed: {}", e)))?;

        let mut results = Vec::new();
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher
                .doc(doc_address)
                .map_err(|e| GroveError::backend(format!("Failed to retrieve doc: {}", e)))?;

            if let Some(id_value) = doc.get_first(self.id_field) {
                if let Some(id_str) = id_value.as_str() {
                    results.push(TantivySearchResult {
                        id: String::from(id_str),
                        score,
                    });
                }
            }
        }

        Ok(results)
    }

    /// Get the number of documents in the index.
    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// Get approximate index size in bytes (for in-memory, this is heap usage).
    pub fn index_size_bytes(&self) -> Result<u64> {
        let searcher = self.reader.searcher();
        let space = searcher
            .space_usage()
            .map_err(|e| GroveError::backend(format!("Failed to get space usage: {}", e)))?;
        Ok(space.total().get_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Confidence, LearningCategory, LearningScope, WriteGateCriterion};
    use tempfile;

    fn sample_learnings() -> Vec<CompoundLearning> {
        vec![
            CompoundLearning::new(
                LearningCategory::Convention,
                "Use tissue CLI for issue tracking",
                "This project uses tissue instead of GitHub Issues for tracking work.",
                LearningScope::Project,
                Confidence::High,
                vec![WriteGateCriterion::StableFact],
                vec![
                    "tooling".to_string(),
                    "issues".to_string(),
                    "tissue".to_string(),
                ],
                "test-session",
            )
            .with_id("cl_20260101_001"),
            CompoundLearning::new(
                LearningCategory::Pattern,
                "Use atomic writes for file operations",
                "Write to temp file then rename to ensure crash safety.",
                LearningScope::Project,
                Confidence::High,
                vec![WriteGateCriterion::BehaviorChanging],
                vec!["rust".to_string(), "file-io".to_string()],
                "test-session",
            )
            .with_id("cl_20260101_002"),
            CompoundLearning::new(
                LearningCategory::Pitfall,
                "Validate float values before display",
                "NaN and Infinity can cause issues in JSON serialization.",
                LearningScope::Project,
                Confidence::High,
                vec![WriteGateCriterion::BehaviorChanging],
                vec!["rust".to_string(), "float".to_string()],
                "test-session",
            )
            .with_id("cl_20260101_003"),
        ]
    }

    #[test]
    fn test_in_memory_index() {
        let index = TantivySearchIndex::in_memory().unwrap();
        assert_eq!(index.num_docs(), 0);
    }

    #[test]
    fn test_index_and_search() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();

        index.index_learnings(&learnings).unwrap();
        assert_eq!(index.num_docs(), 3);

        // Search for "tissue" should find the first learning
        let results = index.search("tissue", 10).unwrap();
        assert!(!results.is_empty(), "Search should return results");
        assert!(
            results[0].id.contains("cl_"),
            "Result ID '{}' should contain 'cl_'",
            results[0].id
        );
    }

    #[test]
    fn test_search_by_tag() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        // Search for "rust" should find multiple learnings
        let results = index.search("rust", 10).unwrap();
        assert!(results.len() >= 2);
    }

    #[test]
    fn test_search_relevance_ordering() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        // Search for "issue" - should prefer the one with "issue" in summary
        let results = index.search("issue", 10).unwrap();
        assert!(!results.is_empty());
        // Results should be ordered by score (descending)
        for i in 1..results.len() {
            assert!(results[i - 1].score >= results[i].score);
        }
    }

    #[test]
    fn test_empty_search() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        // Search for something that doesn't exist
        let results = index.search("kubernetes", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_index_size() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        let size = index.index_size_bytes().unwrap();
        // Should be small for 3 documents
        assert!(size < 100_000, "Index size {} bytes seems too large", size);
    }

    #[test]
    fn test_stemming_search() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        // "tracking" should match "tracking" in detail via stemming (track -> track)
        let results = index.search("tracking", 10).unwrap();
        assert!(
            !results.is_empty(),
            "Stemming should match 'tracking' to 'tracking'"
        );

        // "writes" should match "writes" via stemming (write -> write)
        let results = index.search("writes", 10).unwrap();
        assert!(
            !results.is_empty(),
            "Stemming should match 'writes' to 'write'"
        );
    }

    #[test]
    fn test_fuzzy_search_typo() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        // "tisue" (typo for tissue) needs edit distance 2 because stemming
        // changes "tissue" to "tissu" in the index
        let results = index.search_fuzzy("tisue", 10, 2).unwrap();
        assert!(
            !results.is_empty(),
            "Fuzzy search should find 'tissue' with typo 'tisue'"
        );
        assert!(results[0].id.contains("001"), "Should find tissue learning");
    }

    #[test]
    fn test_fuzzy_search_multiple_typos() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        // "atomc writs" (two typos) should find atomic writes with edit distance 2
        let results = index.search_fuzzy("atomc writs", 10, 2).unwrap();
        assert!(
            !results.is_empty(),
            "Fuzzy search should handle multiple typos"
        );
    }

    #[test]
    fn test_generous_search() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        // Generous search should find both exact and fuzzy matches
        let results = index.search("tissue", 10).unwrap();
        assert!(!results.is_empty(), "Should find exact match");

        // Should also work with typos
        let results = index.search("tisue", 10).unwrap();
        assert!(
            !results.is_empty(),
            "Generous search should fall back to fuzzy for typos"
        );
    }

    #[test]
    fn test_persistent_index() {
        let dir = tempfile::tempdir().unwrap();
        let index = TantivySearchIndex::persistent(dir.path()).unwrap();
        let learnings = sample_learnings();

        index.index_learnings(&learnings).unwrap();
        assert_eq!(index.num_docs(), 3);

        // Search should work
        let results = index.search("tissue", 10).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn test_persistent_index_reopen() {
        let dir = tempfile::tempdir().unwrap();

        // Create and populate index
        {
            let index = TantivySearchIndex::persistent(dir.path()).unwrap();
            index.index_learnings(&sample_learnings()).unwrap();
            assert_eq!(index.num_docs(), 3);
        }

        // Reopen and verify documents persisted
        {
            let index = TantivySearchIndex::persistent(dir.path()).unwrap();
            assert_eq!(
                index.num_docs(),
                3,
                "Documents should persist across reopens"
            );

            let results = index.search("tissue", 10).unwrap();
            assert!(!results.is_empty(), "Search should work after reopen");
        }
    }

    #[test]
    fn test_search_by_category() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        // Search for category should find matching learnings
        let results = index.search("convention", 10).unwrap();
        assert!(!results.is_empty(), "Should find by category");
        assert!(
            results[0].id.contains("001"),
            "Convention category should match tissue learning"
        );

        let results = index.search("pitfall", 10).unwrap();
        assert!(!results.is_empty(), "Should find pitfall category");
    }

    #[test]
    fn test_num_docs_explicit() {
        let index = TantivySearchIndex::in_memory().unwrap();
        assert_eq!(index.num_docs(), 0, "Empty index should have 0 docs");

        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();
        assert_eq!(index.num_docs(), 3, "Should have 3 docs after indexing");

        // Re-indexing same learnings should NOT create duplicates (upsert behavior)
        index.index_learnings(&learnings).unwrap();
        assert_eq!(
            index.num_docs(),
            3,
            "Should still have 3 docs after re-indexing (upsert, not duplicate)"
        );
    }

    #[test]
    fn test_upsert_updates_content() {
        let index = TantivySearchIndex::in_memory().unwrap();

        // Index original learning
        let mut learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        // Verify original content is searchable
        let results = index.search("tissue", 10).unwrap();
        assert!(!results.is_empty(), "Should find original content");

        // Update the learning's summary (same ID, different content)
        learnings[0] = CompoundLearning::new(
            LearningCategory::Convention,
            "Use GitHub Issues for tracking", // Changed from "tissue"
            "This project uses GitHub Issues.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::StableFact],
            vec!["tooling".to_string(), "github".to_string()],
            "test-session",
        )
        .with_id("cl_20260101_001"); // Same ID

        index.index_learnings(&learnings).unwrap();

        // Should still have 3 docs (not 4)
        assert_eq!(index.num_docs(), 3, "Should not create duplicate");

        // Old content should NOT be found (use stemmed search for precision)
        let results = index.search_stemmed("tissue", 10).unwrap();
        assert!(
            results.is_empty(),
            "Old content should be replaced, not found. Found: {:?}",
            results
        );

        // New content SHOULD be found
        let results = index.search_stemmed("github", 10).unwrap();
        assert!(!results.is_empty(), "New content should be searchable");
    }

    #[test]
    fn test_search_limit_zero() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        // Limit of 0 should return empty results (gracefully handled)
        let results = index.search("tissue", 0).unwrap();
        assert!(results.is_empty(), "Limit 0 should return no results");

        let results = index.search_fuzzy("tissue", 0, 1).unwrap();
        assert!(
            results.is_empty(),
            "Fuzzy with limit 0 should return no results"
        );

        let results = index.search("tissue", 0).unwrap();
        assert!(
            results.is_empty(),
            "Generous with limit 0 should return no results"
        );
    }

    #[test]
    fn test_search_empty_index() {
        let index = TantivySearchIndex::in_memory().unwrap();

        // Searching empty index should return empty results, not error
        let results = index.search("anything", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_empty_query() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        // Empty query should return empty results, not error
        let results = index.search("", 10).unwrap();
        assert!(results.is_empty(), "Empty query should return no results");

        // Whitespace-only query should also return empty
        let results = index.search("   ", 10).unwrap();
        assert!(
            results.is_empty(),
            "Whitespace query should return no results"
        );

        // Same for stemmed search
        let results = index.search_stemmed("", 10).unwrap();
        assert!(results.is_empty());

        // Same for fuzzy search
        let results = index.search_fuzzy("", 10, 2).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_special_characters() {
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = sample_learnings();
        index.index_learnings(&learnings).unwrap();

        // Query with special characters should not cause parse errors
        // These would normally be interpreted as query syntax
        let special_queries = [
            "foo AND bar", // Boolean operator
            "field:value", // Field targeting
            "foo*",        // Wildcard
            "\"unclosed",  // Unclosed quote
            "(unbalanced", // Unbalanced parens
            "foo OR bar",  // Boolean operator
            "+required",   // Required term
            "-excluded",   // Excluded term
            "foo~2",       // Fuzzy modifier
            "/regex/",     // Regex
        ];

        for query in &special_queries {
            // Should not panic or return error, just return results (possibly empty)
            let result = index.search(query, 10);
            assert!(
                result.is_ok(),
                "Query '{}' should not cause error: {:?}",
                query,
                result
            );
        }
    }

    #[test]
    fn test_search_by_relevance_context() {
        let index = TantivySearchIndex::in_memory().unwrap();

        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            "Phoenix LiveView lifecycle management",
            "LiveView mount/3 must initialize all assigns used by render/1.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["phoenix".to_string(), "elixir".to_string()],
            "test-session",
        )
        .with_id("cl_20260314_001")
        .with_relevance_context(
            "Surface when working on LiveView components, mount/handle_params callbacks, \
             or modifying files in lib/app_web/live/. Relevant for Phoenix real-time UI.",
        );

        // A second learning without relevance_context
        let learning2 = CompoundLearning::new(
            LearningCategory::Convention,
            "Use snake_case for module names",
            "Elixir convention is snake_case for module file names.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::StableFact],
            vec!["elixir".to_string(), "naming".to_string()],
            "test-session",
        )
        .with_id("cl_20260314_002");

        index.index_learnings(&[learning, learning2]).unwrap();

        // Search for terms only in relevance_context
        let results = index.search("handle_params", 10).unwrap();
        assert!(
            !results.is_empty(),
            "Should find learning via relevance_context terms"
        );
        assert_eq!(results[0].id, "cl_20260314_001");

        // Search for "real-time" only in relevance_context
        let results = index.search("real-time", 10).unwrap();
        assert!(
            !results.is_empty(),
            "Should find learning via relevance_context"
        );
    }

    #[test]
    fn test_escape_query_term_special_chars() {
        assert_eq!(escape_query_term("hello"), "hello");
        assert_eq!(escape_query_term("test:value"), "test\\:value");
        assert_eq!(escape_query_term("a+b"), "a\\+b");
        assert_eq!(escape_query_term("foo^bar"), "foo\\^bar");
    }

    #[test]
    fn test_escape_query_term_used_with_boost() {
        // Demonstrate the pattern: escape term, then append boost
        let term = "test:value";
        let boosted = format!("{}^2.0", escape_query_term(term));
        // The colon is escaped but the boost ^ is not
        assert_eq!(boosted, "test\\:value^2.0");
    }

    #[test]
    fn test_boosted_query_executes_in_tantivy() {
        // Verify that a boosted query string actually parses and executes
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = vec![CompoundLearning::new(
            LearningCategory::Pattern,
            "Use builder pattern for config",
            "Builder pattern provides fluent API for complex configuration.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["pattern".to_string(), "builder".to_string()],
            "test-session",
        )
        .with_id("cl_boost_001")];
        index.index_learnings(&learnings).unwrap();

        // Boosted query: "builder" gets 2.0x, "pattern" gets 1.5x
        // Uses search_boosted which doesn't re-escape the boost syntax
        let boosted_query = "builder^2.0 pattern^1.5";
        let results = index.search_boosted(boosted_query, 10).unwrap();
        assert!(!results.is_empty(), "Boosted query should find results");
    }

    #[test]
    fn test_boosted_vs_unboosted_same_results() {
        // Both should find the same learning, just potentially with different scores
        let index = TantivySearchIndex::in_memory().unwrap();
        let learnings = vec![CompoundLearning::new(
            LearningCategory::Pitfall,
            "Avoid using unwrap in production",
            "Always use proper error handling instead of unwrap.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["error-handling".to_string(), "unwrap".to_string()],
            "test-session",
        )
        .with_id("cl_boost_002")];
        index.index_learnings(&learnings).unwrap();

        let unboosted = index.search("unwrap error", 10).unwrap();
        let boosted = index.search_boosted("unwrap^2.0 error^1.0", 10).unwrap();

        assert_eq!(unboosted.len(), boosted.len());
        assert_eq!(unboosted[0].id, boosted[0].id);
    }
}
