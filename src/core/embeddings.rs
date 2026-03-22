//! Semantic deduplication via embedding similarity.
//!
//! Provides cosine similarity computation (always available) and ONNX-based
//! embedding generation (behind the `semantic-dedup` feature flag) for
//! detecting paraphrase-level duplicates that string matching misses.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(feature = "semantic-dedup")]
use crate::config::SemanticDedupConfig;
#[cfg(feature = "semantic-dedup")]
use crate::core::reflect::DuplicateCheckResult;
#[cfg(feature = "semantic-dedup")]
use crate::core::CompoundLearning;

/// Compute cosine similarity between two vectors.
///
/// Returns a value in [-1.0, 1.0]. Returns 0.0 if either vector is zero-length
/// or has zero magnitude.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0_f64;
    let mut norm_a = 0.0_f64;
    let mut norm_b = 0.0_f64;

    for (ai, bi) in a.iter().zip(b.iter()) {
        let ai = *ai as f64;
        let bi = *bi as f64;
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < f64::EPSILON {
        return 0.0;
    }

    dot / denom
}

/// Trait for embedding text into vectors.
#[cfg(feature = "semantic-dedup")]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed one or more texts into vectors.
    fn embed(&self, texts: &[&str]) -> crate::error::Result<Vec<Vec<f32>>>;
}

/// Provider backed by fastembed's AllMiniLML6V2 model.
///
/// Uses a `Mutex` for interior mutability because `TextEmbedding::embed`
/// requires `&mut self`.
#[cfg(feature = "semantic-dedup")]
pub struct FastEmbedProvider {
    model: std::sync::Mutex<fastembed::TextEmbedding>,
}

#[cfg(feature = "semantic-dedup")]
impl FastEmbedProvider {
    /// Create a new provider, downloading the model if needed.
    pub fn new() -> crate::error::Result<Self> {
        use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(false),
        )
        .map_err(|e| crate::error::GroveError::Config {
            message: format!("fastembed init failed: {e}"),
        })?;

        Ok(Self {
            model: std::sync::Mutex::new(model),
        })
    }
}

#[cfg(feature = "semantic-dedup")]
impl EmbeddingProvider for FastEmbedProvider {
    fn embed(&self, texts: &[&str]) -> crate::error::Result<Vec<Vec<f32>>> {
        let owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        self.model
            .lock()
            .map_err(|e| crate::error::GroveError::Config {
                message: format!("embedding lock poisoned: {e}"),
            })?
            .embed(owned, None)
            .map_err(|e| crate::error::GroveError::Config {
                message: format!("embedding failed: {e}"),
            })
    }
}

/// Sidecar cache mapping learning IDs to their embedding vectors.
///
/// Stored as JSON at `.grove/embeddings.json`. Fail-open on all I/O errors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbeddingCache {
    /// Map of learning_id → embedding vector.
    pub entries: HashMap<String, Vec<f32>>,

    /// Path to the cache file (not serialized).
    #[serde(skip)]
    path: PathBuf,
}

impl EmbeddingCache {
    /// Load cache from disk. Returns empty cache on any error (fail-open).
    pub fn load(path: &Path) -> Self {
        let cache_path = path.join("embeddings.json");
        match std::fs::read_to_string(&cache_path) {
            Ok(content) => {
                let mut cache: EmbeddingCache = serde_json::from_str(&content).unwrap_or_default();
                cache.path = cache_path;
                cache
            }
            Err(_) => EmbeddingCache {
                entries: HashMap::new(),
                path: cache_path,
            },
        }
    }

    /// Save cache to disk atomically (temp-file + rename).
    /// Fail-open: errors are logged but not propagated.
    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string(self) {
            Ok(json) => {
                let tmp_path = self.path.with_extension("json.tmp");
                if let Err(e) = std::fs::write(&tmp_path, json) {
                    tracing::warn!("Failed to write embedding cache temp file: {e}");
                    return;
                }
                if let Err(e) = std::fs::rename(&tmp_path, &self.path) {
                    tracing::warn!("Failed to rename embedding cache: {e}");
                    let _ = std::fs::remove_file(&tmp_path);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to serialize embedding cache: {e}");
            }
        }
    }

    /// Get a cached embedding by learning ID.
    pub fn get(&self, id: &str) -> Option<&Vec<f32>> {
        self.entries.get(id)
    }

    /// Insert an embedding for a learning ID.
    pub fn insert(&mut self, id: String, embedding: Vec<f32>) {
        self.entries.insert(id, embedding);
    }
}

/// Check if a candidate learning is a semantic duplicate of any existing learning.
///
/// Embeds the candidate summary and compares against cached embeddings of existing
/// learnings. Returns a `DuplicateCheckResult` if cosine similarity >= threshold.
///
/// Only checks against active learnings (defense-in-depth; callers typically
/// pre-filter, but this ensures archived/superseded learnings never ghost-block).
///
/// Fail-open: any embedding error results in `no_duplicate()`.
#[cfg(feature = "semantic-dedup")]
pub fn check_semantic_duplicate(
    candidate_summary: &str,
    existing_learnings: &[CompoundLearning],
    provider: &dyn EmbeddingProvider,
    cache: &mut EmbeddingCache,
    config: &SemanticDedupConfig,
) -> DuplicateCheckResult {
    use crate::core::learning::LearningStatus;

    // Filter to active learnings only (defense-in-depth)
    let active: Vec<&CompoundLearning> = existing_learnings
        .iter()
        .filter(|l| l.status == LearningStatus::Active)
        .collect();

    // Embed the candidate
    let candidate_embedding = match provider.embed(&[candidate_summary]) {
        Ok(mut vecs) if !vecs.is_empty() => vecs.remove(0),
        Ok(_) => return DuplicateCheckResult::no_duplicate(),
        Err(e) => {
            tracing::warn!("Semantic dedup: embedding candidate failed (fail-open): {e}");
            return DuplicateCheckResult::no_duplicate();
        }
    };

    // Collect IDs of active learnings that need embedding
    let uncached: Vec<usize> = active
        .iter()
        .enumerate()
        .filter(|(_, l)| cache.get(&l.id).is_none())
        .map(|(i, _)| i)
        .collect();

    // Batch-embed uncached learnings
    if !uncached.is_empty() {
        let texts: Vec<&str> = uncached
            .iter()
            .map(|&i| active[i].summary.as_str())
            .collect();
        match provider.embed(&texts) {
            Ok(embeddings) => {
                for (idx, embedding) in uncached.iter().zip(embeddings) {
                    cache.insert(active[*idx].id.clone(), embedding);
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Semantic dedup: embedding existing learnings failed (fail-open): {e}"
                );
                return DuplicateCheckResult::no_duplicate();
            }
        }
    }

    // Compare candidate against all active learnings
    for learning in &active {
        if let Some(existing_embedding) = cache.get(&learning.id) {
            let similarity = cosine_similarity(&candidate_embedding, existing_embedding);
            if similarity >= config.similarity_threshold {
                return DuplicateCheckResult {
                    is_duplicate: true,
                    duplicate_of: Some(learning.id.clone()),
                    matched_summary: Some(learning.summary.clone()),
                };
            }
        }
    }

    DuplicateCheckResult::no_duplicate()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_identical_vectors() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "identical vectors should have similarity 1.0, got {sim}"
        );
    }

    #[test]
    fn test_cosine_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            sim.abs() < 1e-6,
            "orthogonal vectors should have similarity ~0.0, got {sim}"
        );
    }

    #[test]
    fn test_cosine_antiparallel_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim - (-1.0)).abs() < 1e-6,
            "anti-parallel vectors should have similarity -1.0, got {sim}"
        );
    }

    #[test]
    fn test_cosine_known_angle() {
        // 45-degree angle: cos(45°) ≈ 0.7071
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        let expected = std::f64::consts::FRAC_1_SQRT_2; // 0.7071...
        assert!(
            (sim - expected).abs() < 1e-4,
            "expected ~{expected}, got {sim}"
        );
    }

    #[test]
    fn test_cosine_empty_vectors() {
        let sim = cosine_similarity(&[], &[]);
        assert!((sim - 0.0).abs() < 1e-6, "empty vectors should return 0.0");
    }

    #[test]
    fn test_cosine_mismatched_length() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim - 0.0).abs() < 1e-6,
            "mismatched lengths should return 0.0"
        );
    }

    #[test]
    fn test_cosine_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 1e-6, "zero vector should return 0.0");
    }

    #[test]
    fn test_embedding_cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let grove_dir = dir.path();

        let mut cache = EmbeddingCache::load(grove_dir);
        cache.insert("L001".to_string(), vec![1.0, 2.0, 3.0]);
        cache.insert("L002".to_string(), vec![4.0, 5.0, 6.0]);
        cache.save();

        let loaded = EmbeddingCache::load(grove_dir);
        assert_eq!(loaded.get("L001").unwrap(), &vec![1.0, 2.0, 3.0]);
        assert_eq!(loaded.get("L002").unwrap(), &vec![4.0, 5.0, 6.0]);
        assert!(loaded.get("L999").is_none());
    }

    #[test]
    fn test_embedding_cache_missing_file_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cache = EmbeddingCache::load(dir.path());
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn test_embedding_cache_corrupt_file_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("embeddings.json"), "not valid json").unwrap();
        let cache = EmbeddingCache::load(dir.path());
        assert!(cache.entries.is_empty());
    }
}

#[cfg(all(test, feature = "semantic-dedup"))]
mod semantic_tests {
    use super::*;
    use crate::core::learning::{
        CompoundLearning, Confidence, LearningCategory, LearningScope, LearningStatus,
        WriteGateCriterion,
    };
    use chrono::Utc;

    /// Mock embedding provider that returns embeddings from a queue.
    /// Each call to `embed` consumes the next N embeddings from the queue.
    struct MockProvider {
        embeddings: std::sync::Mutex<Vec<Vec<f32>>>,
    }

    impl MockProvider {
        fn new(embeddings: Vec<Vec<f32>>) -> Self {
            Self {
                embeddings: std::sync::Mutex::new(embeddings),
            }
        }
    }

    impl EmbeddingProvider for MockProvider {
        fn embed(&self, texts: &[&str]) -> crate::error::Result<Vec<Vec<f32>>> {
            let mut queue = self.embeddings.lock().unwrap();
            let n = texts.len().min(queue.len());
            let result: Vec<Vec<f32>> = queue.drain(..n).collect();
            Ok(result)
        }
    }

    /// Mock provider that always fails.
    struct FailingProvider;

    impl EmbeddingProvider for FailingProvider {
        fn embed(&self, _texts: &[&str]) -> crate::error::Result<Vec<Vec<f32>>> {
            Err(crate::error::GroveError::Config {
                message: "mock embedding failure".into(),
            })
        }
    }

    fn make_learning(id: &str, summary: &str) -> CompoundLearning {
        CompoundLearning {
            id: id.to_string(),
            schema_version: 1,
            category: LearningCategory::Pattern,
            summary: summary.to_string(),
            detail: "test detail".to_string(),
            scope: LearningScope::Project,
            confidence: Confidence::High,
            criteria_met: vec![WriteGateCriterion::BehaviorChanging],
            tags: vec!["test".to_string()],
            session_id: "test-session".to_string(),
            ticket_id: None,
            timestamp: Utc::now(),
            context_files: None,
            relevance_context: None,
            status: LearningStatus::Active,
        }
    }

    #[test]
    fn test_semantic_duplicate_similar_vectors() {
        // Candidate and existing have nearly identical embeddings → duplicate
        let candidate_emb = vec![1.0, 0.0, 0.0];
        let existing_emb = vec![0.99, 0.1, 0.0]; // Very similar

        let provider = MockProvider::new(vec![candidate_emb.clone(), existing_emb.clone()]);
        let existing = vec![make_learning("L001", "Use builder pattern")];

        let config = SemanticDedupConfig {
            enabled: true,
            similarity_threshold: 0.90,
        };

        let mut cache = EmbeddingCache::default();

        let result = check_semantic_duplicate(
            "Apply the builder pattern for constructing objects",
            &existing,
            &provider,
            &mut cache,
            &config,
        );

        assert!(
            result.is_duplicate,
            "similar vectors should be flagged as duplicate"
        );
        assert_eq!(result.duplicate_of, Some("L001".to_string()));
    }

    #[test]
    fn test_semantic_duplicate_different_vectors() {
        // Candidate and existing have very different embeddings → not duplicate
        let candidate_emb = vec![1.0, 0.0, 0.0];
        let existing_emb = vec![0.0, 1.0, 0.0]; // Orthogonal

        let provider = MockProvider::new(vec![candidate_emb, existing_emb]);
        let existing = vec![make_learning("L001", "Use builder pattern")];

        let config = SemanticDedupConfig {
            enabled: true,
            similarity_threshold: 0.90,
        };

        let mut cache = EmbeddingCache::default();

        let result = check_semantic_duplicate(
            "Set up CI pipeline with GitHub Actions",
            &existing,
            &provider,
            &mut cache,
            &config,
        );

        assert!(
            !result.is_duplicate,
            "orthogonal vectors should not be flagged as duplicate"
        );
    }

    #[test]
    fn test_semantic_duplicate_provider_error_failopen() {
        let provider = FailingProvider;
        let existing = vec![make_learning("L001", "Use builder pattern")];

        let config = SemanticDedupConfig {
            enabled: true,
            similarity_threshold: 0.90,
        };

        let mut cache = EmbeddingCache::default();

        let result = check_semantic_duplicate(
            "Apply the builder pattern",
            &existing,
            &provider,
            &mut cache,
            &config,
        );

        assert!(
            !result.is_duplicate,
            "provider error should fail-open (not duplicate)"
        );
    }

    #[test]
    fn test_semantic_duplicate_uses_cache() {
        // Pre-populate cache so provider is only called for candidate
        let candidate_emb = vec![1.0, 0.0, 0.0];
        let existing_emb = vec![0.0, 1.0, 0.0];

        // Provider only provides candidate embedding (index 0)
        let provider = MockProvider::new(vec![candidate_emb]);
        let existing = vec![make_learning("L001", "Use builder pattern")];

        let config = SemanticDedupConfig {
            enabled: true,
            similarity_threshold: 0.90,
        };

        let mut cache = EmbeddingCache::default();
        // Pre-cache the existing learning
        cache.insert("L001".to_string(), existing_emb);

        let result = check_semantic_duplicate(
            "Something different",
            &existing,
            &provider,
            &mut cache,
            &config,
        );

        assert!(
            !result.is_duplicate,
            "cached orthogonal vector should not match"
        );
    }

    #[test]
    #[ignore] // Requires model download (~22MB)
    fn test_fastembed_provider_live() {
        let provider = FastEmbedProvider::new().expect("FastEmbedProvider should initialize");

        let similar = provider
            .embed(&[
                "Use the builder pattern",
                "Apply builder pattern for object construction",
            ])
            .expect("embedding should succeed");

        let sim = cosine_similarity(&similar[0], &similar[1]);
        assert!(
            sim > 0.80,
            "similar texts should have cosine > 0.80, got {sim}"
        );

        let unrelated = provider
            .embed(&["Use the builder pattern", "The weather is sunny today"])
            .expect("embedding should succeed");

        let sim = cosine_similarity(&unrelated[0], &unrelated[1]);
        assert!(
            sim < 0.50,
            "unrelated texts should have cosine < 0.50, got {sim}"
        );
    }
}
