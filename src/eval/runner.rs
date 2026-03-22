//! Benchmark runner: orchestrates corpus loading, search, and judge evaluation.

use super::corpus::Corpus;
use super::judge::JudgeContext;
use super::metrics::EvalOutput;
use std::collections::BTreeMap;
use std::path::Path;

#[cfg(feature = "tantivy-search")]
use {
    super::judge::{self, JudgeResult},
    super::metrics::{compute_metrics_with_ci, JudgeStats, RecallData},
    crate::backends::SearchQuery,
    crate::hooks::{
        apply_adaptive_threshold, apply_dynamic_k, build_tantivy_query_string_boosted,
        build_tantivy_query_string_boosted_with_params, extract_tool_input_keywords_v2,
        extract_user_intent_keywords, learning_matches_intent, rerank_with_llm,
    },
    crate::search::TantivySearchIndex,
    crate::stats::scoring::{recency, recency_weight, reference_boost, CompositeScore, Strategy},
    std::collections::HashSet,
};

/// Configurable boost parameters for eval benchmarking.
///
/// Controls the BM25 per-term boost factors and dynamic-K ratio so the eval
/// runner can sweep alternatives without production code changes.
#[derive(Debug, Clone)]
pub struct BoostParams {
    /// Short name for this parameter set (e.g. "boosted-v2").
    pub name: String,
    /// Boost factor for tool-input keywords (production default: 2.0).
    pub keyword_boost: f64,
    /// Boost factor for tags (production default: 1.5).
    pub tag_boost: f64,
    /// Ratio of top score below which learnings are excluded (production default: 0.3).
    pub dynamic_k_ratio: f64,
}

impl Default for BoostParams {
    /// Production defaults matching the hardcoded values.
    fn default() -> Self {
        Self {
            name: "boosted-adaptive".to_string(),
            keyword_boost: 2.0,
            tag_boost: 1.5,
            dynamic_k_ratio: 0.3,
        }
    }
}

impl BoostParams {
    /// Parse inline parameters from a `key=value,...` string.
    ///
    /// Valid keys: `kw` (keyword_boost), `tag` (tag_boost), `dk` (dynamic_k_ratio).
    /// Unspecified keys default to production values.
    pub fn parse(params_str: &str) -> crate::Result<Self> {
        let mut result = Self::default();

        for part in params_str.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (key, val) = part.split_once('=').ok_or_else(|| {
                crate::GroveError::config(format!(
                    "Invalid boost param '{}': expected key=value (keys: kw, tag, dk)",
                    part
                ))
            })?;
            let val: f64 = val.parse().map_err(|_| {
                crate::GroveError::config(format!(
                    "Invalid boost param value '{}': expected a number",
                    val
                ))
            })?;
            match key.trim() {
                "kw" => result.keyword_boost = val,
                "tag" => result.tag_boost = val,
                "dk" => result.dynamic_k_ratio = val,
                _ => {
                    return Err(crate::GroveError::config(format!(
                        "Unknown boost param key '{}': valid keys are kw, tag, dk",
                        key
                    )));
                }
            }
        }

        // Build a descriptive name from actual values
        result.name = format!(
            "boosted(kw={:.1},tag={:.1},dk={:.2})",
            result.keyword_boost, result.tag_boost, result.dynamic_k_ratio
        );

        Ok(result)
    }
}

/// Benchmark configuration variant.
#[derive(Debug, Clone)]
pub enum BenchmarkConfig {
    /// BM25 search only (no adaptive threshold).
    Bm25Only,
    /// BM25 + adaptive threshold + dynamic K.
    Bm25Adaptive,
    /// BM25 + adaptive + intent-as-filter.
    Bm25AdaptiveIntentFilter,
    /// BM25 with per-term boost + adaptive threshold.
    Bm25BoostedAdaptive,
    /// BM25 + adaptive + LLM reranking.
    Bm25AdaptiveRerank,
    /// BM25 with per-term boost + adaptive + LLM reranking.
    Bm25BoostedAdaptiveRerank,
    /// BM25 + adaptive but flat 90-day half-life (ablation control).
    Bm25FlatRecency,
    /// BM25 with custom boost params + adaptive threshold.
    Bm25BoostedCustom(BoostParams),
    /// BM25 with corpus-size heuristic: boosted below threshold, plain above.
    Bm25Heuristic(usize),
    /// BM25 with corpus-derived vocabulary enrichment + adaptive threshold.
    Bm25CorpusEnriched,
    /// BM25 + adaptive threshold + per-query adaptive dynamic K.
    Bm25AdaptiveDk,
}

impl BenchmarkConfig {
    /// Parse a config name string into a BenchmarkConfig.
    ///
    /// Supports named presets and inline parameterized configs:
    /// - `"boosted-adaptive"` — production defaults (kw=2.0, tag=1.5, dk=0.3)
    /// - `"boosted(dk=0.35)"` — override dynamic_k_ratio only
    /// - `"boosted(kw=1.5,tag=1.0,dk=0.35)"` — override all boost params
    ///
    /// Unspecified params in `boosted(...)` default to production values.
    pub fn from_name(name: &str) -> crate::Result<Self> {
        // Check for parameterized syntax: boosted(key=val,...)
        if let Some(params_str) = name
            .strip_prefix("boosted(")
            .and_then(|s| s.strip_suffix(')'))
        {
            let params = BoostParams::parse(params_str)?;
            return Ok(Self::Bm25BoostedCustom(params));
        }

        // Check for heuristic(N) syntax
        if let Some(threshold_str) = name
            .strip_prefix("heuristic(")
            .and_then(|s| s.strip_suffix(')'))
        {
            let threshold: usize = threshold_str.trim().parse().map_err(|_| {
                crate::GroveError::config(format!(
                    "Invalid heuristic threshold '{}': expected a number",
                    threshold_str
                ))
            })?;
            return Ok(Self::Bm25Heuristic(threshold));
        }

        match name {
            "bm25" => Ok(Self::Bm25Only),
            "adaptive" => Ok(Self::Bm25Adaptive),
            "intent-filter" => Ok(Self::Bm25AdaptiveIntentFilter),
            "boosted-adaptive" => Ok(Self::Bm25BoostedAdaptive),
            "adaptive-rerank" => Ok(Self::Bm25AdaptiveRerank),
            "boosted-adaptive-rerank" => Ok(Self::Bm25BoostedAdaptiveRerank),
            "flat-recency" => Ok(Self::Bm25FlatRecency),
            "heuristic" => Ok(Self::Bm25Heuristic(50)),
            "corpus-enriched" => Ok(Self::Bm25CorpusEnriched),
            "adaptive-dk" => Ok(Self::Bm25AdaptiveDk),
            _ => Err(crate::GroveError::config(format!(
                "Unknown benchmark config: '{}'. Valid: bm25, adaptive, intent-filter, \
                 boosted-adaptive, adaptive-rerank, boosted-adaptive-rerank, flat-recency, \
                 heuristic, heuristic(N), corpus-enriched, adaptive-dk, boosted(kw=F,tag=F,dk=F)",
                name
            ))),
        }
    }

    /// Human-readable name for this config.
    pub fn name(&self) -> String {
        match self {
            Self::Bm25Only => "bm25".to_string(),
            Self::Bm25Adaptive => "adaptive".to_string(),
            Self::Bm25AdaptiveIntentFilter => "intent-filter".to_string(),
            Self::Bm25BoostedAdaptive => "boosted-adaptive".to_string(),
            Self::Bm25AdaptiveRerank => "adaptive-rerank".to_string(),
            Self::Bm25BoostedAdaptiveRerank => "boosted-adaptive-rerank".to_string(),
            Self::Bm25FlatRecency => "flat-recency".to_string(),
            Self::Bm25BoostedCustom(params) => params.name.clone(),
            Self::Bm25Heuristic(threshold) => format!("heuristic({})", threshold),
            Self::Bm25CorpusEnriched => "corpus-enriched".to_string(),
            Self::Bm25AdaptiveDk => "adaptive-dk".to_string(),
        }
    }

    #[cfg(feature = "tantivy-search")]
    fn uses_adaptive(&self) -> bool {
        !matches!(self, Self::Bm25Only)
    }

    #[cfg(feature = "tantivy-search")]
    fn uses_intent_filter(&self) -> bool {
        matches!(self, Self::Bm25AdaptiveIntentFilter)
    }

    #[cfg(feature = "tantivy-search")]
    fn uses_boosted_query(&self) -> bool {
        matches!(
            self,
            Self::Bm25BoostedAdaptive
                | Self::Bm25BoostedAdaptiveRerank
                | Self::Bm25BoostedCustom(_)
        )
    }

    #[cfg(feature = "tantivy-search")]
    fn uses_rerank(&self) -> bool {
        matches!(
            self,
            Self::Bm25AdaptiveRerank | Self::Bm25BoostedAdaptiveRerank
        )
    }

    #[cfg(feature = "tantivy-search")]
    fn uses_flat_recency(&self) -> bool {
        matches!(self, Self::Bm25FlatRecency)
    }

    /// Return custom boost params if this config uses them, None otherwise.
    #[cfg(feature = "tantivy-search")]
    fn boost_params(&self) -> Option<&BoostParams> {
        match self {
            Self::Bm25BoostedCustom(params) => Some(params),
            _ => None,
        }
    }

    /// Return the corpus-size heuristic threshold, if this is a heuristic config.
    #[cfg(feature = "tantivy-search")]
    fn heuristic_threshold(&self) -> Option<usize> {
        match self {
            Self::Bm25Heuristic(t) => Some(*t),
            _ => None,
        }
    }

    /// Whether this config uses corpus vocabulary enrichment.
    #[cfg(feature = "tantivy-search")]
    fn uses_corpus_enrichment(&self) -> bool {
        matches!(self, Self::Bm25CorpusEnriched)
    }

    /// Whether this config uses per-query adaptive dynamic K.
    #[cfg(feature = "tantivy-search")]
    fn uses_adaptive_dk(&self) -> bool {
        matches!(self, Self::Bm25AdaptiveDk)
    }
}

/// A surfaced (session, learning) pair with its composite score.
#[cfg(feature = "tantivy-search")]
struct SurfacedPair {
    session_file: String,
    learning_id: String,
    composite: CompositeScore,
}

/// Result of the retrieval pipeline across all sessions.
#[cfg(feature = "tantivy-search")]
struct SurfaceResult {
    pairs: Vec<SurfacedPair>,
    sessions_evaluated: usize,
    sessions_suppressed: usize,
}

/// Shared retrieval pipeline: BM25 search → composite scoring → adaptive threshold
/// → intent filter → LLM reranking. Returns all surfaced (session, learning, score) pairs.
///
/// Both sequential and batch paths use this helper per design doc §12.5.
#[cfg(feature = "tantivy-search")]
#[allow(clippy::too_many_arguments)]
fn surface_learnings(
    config: &BenchmarkConfig,
    corpus: &Corpus,
    judge_ctx: &JudgeContext,
    transcript_dir: &Path,
) -> crate::Result<SurfaceResult> {
    use crate::config::{RerankConfig, RetrievalConfig, RetrievalProfile};
    use crate::hooks::{enrich_query_with_corpus_vocabulary, extract_corpus_vocabulary};

    let mut retrieval_config = RetrievalConfig::default();
    // Override dynamic_k_ratio if custom boost params are provided
    if let Some(params) = config.boost_params() {
        retrieval_config.dynamic_k_ratio = params.dynamic_k_ratio;
    }
    let rerank_config = RerankConfig {
        enabled: true,
        timeout_seconds: 60,
        ..RerankConfig::default()
    };

    let index = TantivySearchIndex::in_memory()
        .map_err(|e| crate::GroveError::config(format!("Failed to create Tantivy index: {}", e)))?;
    index
        .index_learnings(&corpus.learnings)
        .map_err(|e| crate::GroveError::config(format!("Failed to index learnings: {}", e)))?;

    let top_n = retrieval_config.max_injections as usize;
    let strategy = Strategy::Moderate;
    let now = chrono::Utc::now();
    let max_user_keywords = 15;
    let min_overlap = 1;
    let flat_half_life: u32 = 90;

    let learning_hash: std::collections::HashMap<String, &crate::core::learning::CompoundLearning> =
        corpus.learnings.iter().map(|l| (l.id.clone(), l)).collect();

    // Pre-compute heuristic profile (once per corpus, not per session)
    let heuristic_profile = config
        .heuristic_threshold()
        .map(|threshold| RetrievalProfile::select(corpus.learnings.len(), threshold));

    // Pre-compute corpus vocabulary for enrichment configs
    let corpus_vocab = if config.uses_corpus_enrichment() {
        extract_corpus_vocabulary(&corpus.learnings, 2)
    } else {
        HashSet::new()
    };

    let mut pairs: Vec<SurfacedPair> = Vec::new();
    let mut sessions_evaluated = 0usize;
    let mut sessions_suppressed = 0usize;

    for ctx in &corpus.contexts {
        let first_tc = match ctx.all_tool_calls.first() {
            Some(tc) => tc,
            None => continue,
        };

        let v2_keywords = extract_tool_input_keywords_v2(&first_tc.tool_name, &first_tc.tool_input);
        if v2_keywords.is_empty() {
            continue;
        }

        // For corpus-enriched: enrich keywords with corpus vocabulary
        let effective_keywords = if config.uses_corpus_enrichment() && !corpus_vocab.is_empty() {
            let enrichment =
                enrich_query_with_corpus_vocabulary(&v2_keywords, &ctx.file_paths, &corpus_vocab);
            let mut combined = v2_keywords.clone();
            combined.extend(enrichment);
            combined
        } else {
            v2_keywords.clone()
        };

        // BM25 search — select strategy based on config
        let use_boosted = if let Some(profile) = heuristic_profile {
            profile == RetrievalProfile::SmallCorpus
        } else {
            config.uses_boosted_query() || config.uses_corpus_enrichment()
        };

        let bm25_results = if use_boosted {
            let search_query = SearchQuery {
                keywords: effective_keywords.clone(),
                files: ctx.file_paths.clone(),
                tags: Vec::new(),
                ticket_id: None,
            };
            let boosted_query = if let Some(params) = config.boost_params() {
                build_tantivy_query_string_boosted_with_params(
                    &search_query,
                    params.keyword_boost,
                    params.tag_boost,
                )
            } else {
                build_tantivy_query_string_boosted(&search_query)
            };
            if boosted_query.trim().is_empty() {
                continue;
            }
            match index.search_boosted(&boosted_query, 20) {
                Ok(r) => r,
                Err(_) => continue,
            }
        } else {
            let query_string = effective_keywords.join(" ");
            if query_string.trim().is_empty() {
                continue;
            }
            match index.search_stemmed(&query_string, 20) {
                Ok(r) => r,
                Err(_) => continue,
            }
        };

        if bm25_results.is_empty() {
            continue;
        }

        let max_bm25 = bm25_results
            .iter()
            .map(|r| r.score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut scored: Vec<CompositeScore> = bm25_results
            .iter()
            .filter_map(|r| {
                let learning = learning_hash.get(&r.id)?;
                let relevance = if max_bm25 < f32::EPSILON {
                    1.0
                } else {
                    (r.score / max_bm25) as f64
                };
                let half_life = if config.uses_flat_recency() {
                    flat_half_life
                } else {
                    retrieval_config.half_life_for_category(&learning.category)
                };
                let lambda = recency::lambda_from_half_life(half_life);
                let recency_val = recency_weight(learning.timestamp, now, lambda);
                let ref_boost = reference_boost(Some(0.5));
                Some(CompositeScore::new(
                    (*learning).clone(),
                    relevance,
                    recency_val,
                    ref_boost,
                    strategy,
                ))
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if scored.is_empty() {
            continue;
        }

        sessions_evaluated += 1;

        let filtered = if config.uses_adaptive() {
            match apply_adaptive_threshold(
                scored,
                retrieval_config.min_confidence_threshold,
                retrieval_config.min_score_gap,
            ) {
                None => {
                    sessions_suppressed += 1;
                    continue;
                }
                Some(passed) => {
                    let dk = if config.uses_adaptive_dk() {
                        let score_values: Vec<f64> = passed.iter().map(|s| s.score).collect();
                        crate::hooks::adaptive_dk_ratio(
                            &score_values,
                            retrieval_config.dynamic_k_ratio,
                            None, // No stats cache in eval
                            passed.first().map(|s| &s.learning.category),
                        )
                    } else {
                        retrieval_config.dynamic_k_ratio
                    };
                    apply_dynamic_k(passed, dk, top_n)
                }
            }
        } else {
            scored.truncate(top_n);
            scored
        };

        let after_intent = if config.uses_intent_filter() {
            let transcript_path = transcript_dir.join(&ctx.session_file);
            let intent_keywords = extract_user_intent_keywords(&transcript_path, max_user_keywords);

            if intent_keywords.is_empty() {
                filtered
            } else {
                filtered
                    .into_iter()
                    .filter(|cs| {
                        learning_matches_intent(
                            &cs.learning.summary,
                            &cs.learning.detail,
                            &intent_keywords,
                            min_overlap,
                        )
                    })
                    .collect()
            }
        } else {
            filtered
        };

        let final_results = if config.uses_rerank() && !after_intent.is_empty() {
            let tool_input_str = serde_json::to_string(&first_tc.tool_input).unwrap_or_default();
            let git_files: Vec<String> = ctx.file_paths.iter().take(10).cloned().collect();
            let mut reranked = rerank_with_llm(
                after_intent,
                &rerank_config,
                &judge_ctx.api_url,
                &first_tc.tool_name,
                &tool_input_str,
                "",
                &git_files,
            );
            reranked.truncate(top_n);
            reranked
        } else {
            after_intent
        };

        if final_results.is_empty() {
            continue;
        }

        for cs in final_results {
            if corpus.learning_map.contains_key(&cs.learning.id) {
                pairs.push(SurfacedPair {
                    session_file: ctx.session_file.clone(),
                    learning_id: cs.learning.id.clone(),
                    composite: cs,
                });
            }
        }
    }

    Ok(SurfaceResult {
        pairs,
        sessions_evaluated,
        sessions_suppressed,
    })
}

/// Compute metrics from surfaced pairs and a judge cache.
///
/// Shared between sequential and batch paths for the final metrics aggregation step.
/// When `n_bootstrap > 0`, computes 95% confidence intervals via session-level resampling.
#[cfg(feature = "tantivy-search")]
fn compute_eval_output(
    config: &BenchmarkConfig,
    corpus: &Corpus,
    cache: &BTreeMap<String, f64>,
    initial_cache_size: usize,
    surfaced: &SurfaceResult,
    surfaced_keys: &HashSet<String>,
    n_bootstrap: usize,
) -> EvalOutput {
    let gt_at_4 = cache.values().filter(|&&s| s >= 4.0).count();
    let gt_at_5 = cache.values().filter(|&&s| s >= 5.0).count();
    let new_judgments = cache.len() - initial_cache_size;

    // Group by session for per-session scoring
    let mut pairs_by_session: std::collections::HashMap<&str, Vec<&SurfacedPair>> =
        std::collections::HashMap::new();
    for pair in &surfaced.pairs {
        pairs_by_session
            .entry(&pair.session_file)
            .or_default()
            .push(pair);
    }

    let mut session_scores: Vec<Vec<f64>> = Vec::new();
    let mut judge_calls = 0usize;
    let mut judge_failures = 0usize;
    let mut cache_hits = 0usize;

    for pairs in pairs_by_session.values() {
        let mut scores_for_session: Vec<f64> = Vec::new();
        for pair in pairs {
            judge_calls += 1;
            let key = judge::judge_cache_key(&pair.session_file, &pair.learning_id);
            if let Some(&score) = cache.get(&key) {
                scores_for_session.push(score);
                cache_hits += 1;
            } else {
                judge_failures += 1;
            }
        }
        if !scores_for_session.is_empty() {
            session_scores.push(scores_for_session);
        }
    }

    let surfaced_at_4 = surfaced_keys
        .iter()
        .filter(|k| cache.get(*k).is_some_and(|&s| s >= 4.0))
        .count();
    let surfaced_at_5 = surfaced_keys
        .iter()
        .filter(|k| cache.get(*k).is_some_and(|&s| s >= 5.0))
        .count();
    let recall_data = if gt_at_4 > 0 {
        Some(RecallData {
            ground_truth_at_4: gt_at_4,
            ground_truth_at_5: gt_at_5,
            surfaced_at_4,
            surfaced_at_5,
        })
    } else {
        None
    };

    let all_scores: Vec<f64> = session_scores.iter().flatten().copied().collect();
    let mut metrics = compute_metrics_with_ci(
        &all_scores,
        surfaced.sessions_evaluated,
        surfaced.sessions_suppressed,
        recall_data,
        &session_scores,
        n_bootstrap,
    );

    // Coverage, pairs-per-session, and MRR
    let sessions_total = corpus.contexts.len();
    metrics.sessions_total = sessions_total;
    let sessions_with_results = session_scores.len();
    metrics.coverage = if sessions_total > 0 {
        sessions_with_results as f64 / sessions_total as f64
    } else {
        0.0
    };
    let (pps_min, pps_mean, pps_max) = super::metrics::compute_pairs_per_session(&session_scores);
    metrics.pairs_per_session_min = pps_min;
    metrics.pairs_per_session_mean = pps_mean;
    metrics.pairs_per_session_max = pps_max;
    metrics.mrr_at_4 = super::metrics::compute_mrr(&session_scores, 4.0);

    let judge_stats = JudgeStats {
        total_calls: judge_calls,
        cache_hits,
        new_judgments,
        failures: judge_failures,
    };

    EvalOutput {
        config_name: config.name().to_string(),
        corpus_name: corpus.name.clone(),
        metrics,
        judge_stats,
        timestamp: chrono::Utc::now().to_rfc3339(),
    }
}

/// Run a single benchmark configuration against a corpus.
#[cfg(feature = "tantivy-search")]
pub fn run_benchmark(
    config: &BenchmarkConfig,
    corpus: &Corpus,
    judge_ctx: &JudgeContext,
    cache: &mut BTreeMap<String, f64>,
    cache_path: &Path,
    transcript_dir: &Path,
    n_bootstrap: usize,
) -> crate::Result<EvalOutput> {
    let initial_cache_size = cache.len();

    eprintln!(
        "\n{}\n{} — {} backend ({})\n{}",
        "=".repeat(70),
        config.name().to_uppercase(),
        judge_ctx.backend,
        judge_ctx.model,
        "=".repeat(70),
    );

    let surfaced = surface_learnings(config, corpus, judge_ctx, transcript_dir)?;

    let mut surfaced_keys: HashSet<String> = HashSet::new();

    // Judge each surfaced pair sequentially
    for pair in &surfaced.pairs {
        let learning = match corpus.learning_map.get(&pair.learning_id) {
            Some(&idx) => &corpus.learnings[idx],
            None => continue,
        };

        let cache_key = judge::judge_cache_key(&pair.session_file, &learning.id);
        surfaced_keys.insert(cache_key);

        let ctx = corpus
            .contexts
            .iter()
            .find(|c| c.session_file == pair.session_file);
        let ctx = match ctx {
            Some(c) => c,
            None => continue,
        };

        if let Some(JudgeResult { score, cached: _ }) =
            judge::judge_relevance(&pair.session_file, learning, ctx, cache, judge_ctx)
        {
            eprintln!(
                "    score={} composite={:.3} {} | {}",
                score,
                pair.composite.score,
                pair.learning_id,
                truncate_str(&learning.summary, 60)
            );
        }

        judge::save_judge_cache(cache, cache_path);
    }

    Ok(compute_eval_output(
        config,
        corpus,
        cache,
        initial_cache_size,
        &surfaced,
        &surfaced_keys,
        n_bootstrap,
    ))
}

/// Run a single benchmark using the Batch API for judge calls.
///
/// Phase 1: Collect all (session, learning) pairs via shared retrieval pipeline.
/// Phase 2: Submit batch, poll, retrieve.
/// Phase 3: Apply results and compute metrics via shared helper.
#[cfg(feature = "tantivy-search")]
#[allow(clippy::too_many_arguments)]
pub fn run_benchmark_batch(
    config: &BenchmarkConfig,
    corpus: &Corpus,
    judge_ctx: &JudgeContext,
    cache: &mut BTreeMap<String, f64>,
    cache_path: &Path,
    transcript_dir: &Path,
    batch_timeout: u64,
    n_bootstrap: usize,
) -> crate::Result<EvalOutput> {
    use crate::llm::batch;

    let initial_cache_size = cache.len();

    eprintln!(
        "\n{}\n{} (BATCH) — {} backend ({})\n{}",
        "=".repeat(70),
        config.name().to_uppercase(),
        judge_ctx.backend,
        judge_ctx.model,
        "=".repeat(70),
    );

    // Phase 1: Surface learnings using shared retrieval pipeline
    let surfaced = surface_learnings(config, corpus, judge_ctx, transcript_dir)?;

    let mut surfaced_keys: HashSet<String> = HashSet::new();
    let mut batch_requests: Vec<batch::BatchRequest> = Vec::new();

    // Build batch requests for uncached pairs
    for pair in &surfaced.pairs {
        let key = judge::judge_cache_key(&pair.session_file, &pair.learning_id);
        surfaced_keys.insert(key.clone());

        let learning = match corpus.learning_map.get(&pair.learning_id) {
            Some(&idx) => &corpus.learnings[idx],
            None => continue,
        };

        let ctx = corpus
            .contexts
            .iter()
            .find(|c| c.session_file == pair.session_file);
        let ctx = match ctx {
            Some(c) => c,
            None => continue,
        };

        if let Some(req) =
            judge::build_judge_batch_request(&pair.session_file, learning, ctx, cache, judge_ctx)
        {
            batch_requests.push(req);
        }
    }

    let cache_hits_at_start = surfaced.pairs.len() - batch_requests.len();

    // Phase 2: Submit batch if there are uncached pairs
    if !batch_requests.is_empty() {
        eprintln!(
            "Submitting batch: {} requests ({} already cached)",
            batch_requests.len(),
            cache_hits_at_start
        );
        let api_url = &judge_ctx.api_url;

        let batch_state = match batch::create_batch(api_url, batch_requests) {
            Some(state) => state,
            None => {
                eprintln!("Warning: batch creation failed, falling back to sequential");
                return run_benchmark(
                    config,
                    corpus,
                    judge_ctx,
                    cache,
                    cache_path,
                    transcript_dir,
                    n_bootstrap,
                );
            }
        };

        eprintln!("Batch created: {}", batch_state.batch_id);

        let ended = match batch::poll_batch_until_ended(
            api_url,
            &batch_state.batch_id,
            batch_timeout,
            &|status, processing, succeeded, errored, expired| {
                eprintln!(
                    "  [{}] processing={} succeeded={} errored={} expired={}",
                    status, processing, succeeded, errored, expired
                );
            },
        ) {
            Some(ended) => ended,
            None => {
                eprintln!("Warning: batch polling failed, falling back to sequential");
                return run_benchmark(
                    config,
                    corpus,
                    judge_ctx,
                    cache,
                    cache_path,
                    transcript_dir,
                    n_bootstrap,
                );
            }
        };

        if !ended {
            eprintln!("Warning: batch timed out, retrieving partial results");
        }

        let batch_results = match batch::retrieve_batch_results(api_url, &batch_state.batch_id) {
            Some(results) => results,
            None => {
                eprintln!("Warning: failed to retrieve results, falling back to sequential");
                return run_benchmark(
                    config,
                    corpus,
                    judge_ctx,
                    cache,
                    cache_path,
                    transcript_dir,
                    n_bootstrap,
                );
            }
        };

        let mut applied = 0usize;
        let mut failures = 0usize;
        for result in &batch_results {
            match judge::apply_judge_batch_result(result, cache) {
                Some(_) => applied += 1,
                None => failures += 1,
            }
        }
        eprintln!("Batch results: {} applied, {} failed", applied, failures);

        judge::save_judge_cache(cache, cache_path);
    } else {
        eprintln!("All pairs cached, no batch needed");
    }

    // Phase 3: Compute metrics using shared helper
    Ok(compute_eval_output(
        config,
        corpus,
        cache,
        initial_cache_size,
        &surfaced,
        &surfaced_keys,
        n_bootstrap,
    ))
}

#[cfg(not(feature = "tantivy-search"))]
#[allow(clippy::too_many_arguments)]
pub fn run_benchmark_batch(
    _config: &BenchmarkConfig,
    _corpus: &Corpus,
    _judge_ctx: &JudgeContext,
    _cache: &mut BTreeMap<String, f64>,
    _cache_path: &Path,
    _transcript_dir: &Path,
    _batch_timeout: u64,
    _n_bootstrap: usize,
) -> crate::Result<EvalOutput> {
    Err(crate::GroveError::config(
        "grove eval --batch requires the tantivy-search feature. Rebuild with: cargo build --features tantivy-search".to_string(),
    ))
}

#[cfg(not(feature = "tantivy-search"))]
pub fn run_benchmark(
    _config: &BenchmarkConfig,
    _corpus: &Corpus,
    _judge_ctx: &JudgeContext,
    _cache: &mut BTreeMap<String, f64>,
    _cache_path: &Path,
    _transcript_dir: &Path,
    _n_bootstrap: usize,
) -> crate::Result<EvalOutput> {
    Err(crate::GroveError::config(
        "grove eval requires the tantivy-search feature. Rebuild with: cargo build --features tantivy-search".to_string(),
    ))
}

/// Truncate a string to at most `max_bytes` bytes without splitting UTF-8.
#[cfg(feature = "tantivy-search")]
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_name_parses_all_configs() {
        let cases = vec![
            ("bm25", "bm25"),
            ("adaptive", "adaptive"),
            ("intent-filter", "intent-filter"),
            ("boosted-adaptive", "boosted-adaptive"),
            ("adaptive-rerank", "adaptive-rerank"),
            ("boosted-adaptive-rerank", "boosted-adaptive-rerank"),
            ("flat-recency", "flat-recency"),
            ("heuristic", "heuristic(50)"),
            ("corpus-enriched", "corpus-enriched"),
            ("adaptive-dk", "adaptive-dk"),
        ];
        for (input, expected_name) in cases {
            let config = BenchmarkConfig::from_name(input).unwrap();
            assert_eq!(config.name(), expected_name, "Mismatch for input '{input}'");
        }
    }

    #[test]
    fn from_name_unknown_returns_error() {
        assert!(BenchmarkConfig::from_name("unknown").is_err());
    }

    #[test]
    fn from_name_error_lists_all_valid_names() {
        let err = BenchmarkConfig::from_name("bad").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("boosted-adaptive"),
            "Should list boosted-adaptive: {msg}"
        );
        assert!(
            msg.contains("adaptive-rerank"),
            "Should list adaptive-rerank: {msg}"
        );
        assert!(
            msg.contains("flat-recency"),
            "Should list flat-recency: {msg}"
        );
        assert!(msg.contains("heuristic"), "Should list heuristic: {msg}");
        assert!(
            msg.contains("corpus-enriched"),
            "Should list corpus-enriched: {msg}"
        );
        assert!(
            msg.contains("adaptive-dk"),
            "Should list adaptive-dk: {msg}"
        );
        assert!(
            msg.contains("boosted(kw=F,tag=F,dk=F)"),
            "Should list parameterized syntax: {msg}"
        );
    }

    #[test]
    fn boost_params_default_matches_current() {
        let params = BoostParams::default();
        assert!((params.keyword_boost - 2.0).abs() < f64::EPSILON);
        assert!((params.tag_boost - 1.5).abs() < f64::EPSILON);
        assert!((params.dynamic_k_ratio - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn boosted_inline_parses_all_params() {
        let config = BenchmarkConfig::from_name("boosted(kw=1.5,tag=1.0,dk=0.35)").unwrap();
        if let BenchmarkConfig::Bm25BoostedCustom(params) = &config {
            assert!((params.keyword_boost - 1.5).abs() < f64::EPSILON);
            assert!((params.tag_boost - 1.0).abs() < f64::EPSILON);
            assert!((params.dynamic_k_ratio - 0.35).abs() < f64::EPSILON);
        } else {
            panic!("Expected Bm25BoostedCustom variant");
        }
    }

    #[test]
    fn boosted_inline_partial_defaults_remaining() {
        let config = BenchmarkConfig::from_name("boosted(dk=0.35)").unwrap();
        if let BenchmarkConfig::Bm25BoostedCustom(params) = &config {
            // kw and tag should use production defaults
            assert!((params.keyword_boost - 2.0).abs() < f64::EPSILON);
            assert!((params.tag_boost - 1.5).abs() < f64::EPSILON);
            assert!((params.dynamic_k_ratio - 0.35).abs() < f64::EPSILON);
        } else {
            panic!("Expected Bm25BoostedCustom variant");
        }
    }

    #[test]
    fn boosted_inline_name_reflects_params() {
        let config = BenchmarkConfig::from_name("boosted(dk=0.35)").unwrap();
        assert_eq!(config.name(), "boosted(kw=2.0,tag=1.5,dk=0.35)");
    }

    #[test]
    fn boosted_inline_unknown_key_errors() {
        assert!(BenchmarkConfig::from_name("boosted(bad=1.0)").is_err());
    }

    #[test]
    fn boosted_inline_invalid_value_errors() {
        assert!(BenchmarkConfig::from_name("boosted(dk=abc)").is_err());
    }

    #[test]
    fn eval_runner_strategy_matches_config_default() {
        // The eval runner hardcodes Strategy::Moderate (line ~151 of run_benchmark).
        // This must match the Config::default() strategy so that eval benchmarks
        // measure the same scoring behavior as production retrieval.
        use crate::config::Config;
        use crate::stats::scoring::Strategy;

        let config = Config::default();
        let eval_strategy = Strategy::Moderate;
        assert_eq!(
            config.retrieval.strategy,
            eval_strategy.as_str(),
            "Eval runner strategy must match config default for benchmark validity"
        );
    }

    #[test]
    fn boosted_adaptive_is_valid_default_config() {
        // boosted-adaptive is the default eval config (wins 2/3 corpora in
        // 3-corpus benchmark, best coverage floor 93-99%).
        let config = BenchmarkConfig::from_name("boosted-adaptive").unwrap();
        assert_eq!(config.name(), "boosted-adaptive");
    }

    #[test]
    fn heuristic_default_threshold_is_50() {
        let config = BenchmarkConfig::from_name("heuristic").unwrap();
        assert_eq!(config.name(), "heuristic(50)");
        if let BenchmarkConfig::Bm25Heuristic(t) = config {
            assert_eq!(t, 50);
        } else {
            panic!("Expected Bm25Heuristic variant");
        }
    }

    #[test]
    fn heuristic_custom_threshold() {
        let config = BenchmarkConfig::from_name("heuristic(40)").unwrap();
        assert_eq!(config.name(), "heuristic(40)");
        if let BenchmarkConfig::Bm25Heuristic(t) = config {
            assert_eq!(t, 40);
        } else {
            panic!("Expected Bm25Heuristic variant");
        }
    }

    #[test]
    fn heuristic_invalid_threshold_errors() {
        assert!(BenchmarkConfig::from_name("heuristic(abc)").is_err());
    }

    #[test]
    fn corpus_enriched_parses() {
        let config = BenchmarkConfig::from_name("corpus-enriched").unwrap();
        assert_eq!(config.name(), "corpus-enriched");
    }
}
