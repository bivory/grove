//! CLI command for offline retrieval quality evaluation.

use crate::eval::corpus::{
    build_negative_corpus, entry_to_config, load_corpus, load_corpus_manifest,
    resolve_corpus_config, Corpus, CorpusConfig,
};
use crate::eval::judge::{self, JudgeContext};
use crate::eval::metrics::{
    self, NegativePairResult, NegativeSweepOutput, SweepCorpusResult, SweepOutput,
};
use crate::eval::runner::BenchmarkConfig;

/// Options for the eval run command.
pub struct EvalRunOptions {
    pub config: String,
    pub transcript_dir: Option<String>,
    pub learnings_path: Option<String>,
    pub cache_path: Option<String>,
    pub json: bool,
    pub batch: bool,
    /// Bootstrap resamples for confidence intervals (0 = disabled).
    pub bootstrap: usize,
}

/// Options for the eval compare command.
pub struct EvalCompareOptions {
    pub configs: String,
    pub transcript_dir: Option<String>,
    pub learnings_path: Option<String>,
    pub cache_path: Option<String>,
    pub json: bool,
    pub batch: bool,
    /// Bootstrap resamples for confidence intervals (0 = disabled).
    pub bootstrap: usize,
}

/// Run a single benchmark configuration.
pub fn run_eval(options: EvalRunOptions) -> Result<bool, Box<dyn std::error::Error>> {
    let config = BenchmarkConfig::from_name(&options.config)?;

    let corpus_config = resolve_corpus_config(
        options.transcript_dir.as_deref(),
        options.learnings_path.as_deref(),
    )?;

    let grove_config = crate::config::Config::load();
    let judge_ctx = JudgeContext::from_config(&grove_config.judge);

    let cache_path = judge::resolve_cache_path(
        options.cache_path.as_deref(),
        &grove_config.judge.cache_path,
    );

    let mode = if options.batch { "batch" } else { "sequential" };
    eprintln!("Loading corpus: {} ({})", corpus_config.name, mode);
    eprintln!("  Transcripts: {}", corpus_config.transcript_dir.display());
    eprintln!("  Learnings:   {}", corpus_config.learnings_path.display());
    eprintln!("  Judge:       {} ({})", judge_ctx.backend, judge_ctx.model);
    eprintln!("  Cache:       {}", cache_path.display());

    let corpus = crate::eval::corpus::load_corpus(&corpus_config)?;
    eprintln!(
        "  Loaded {} learnings, {} sessions",
        corpus.learnings.len(),
        corpus.contexts.len()
    );

    let mut cache = judge::load_judge_cache(&cache_path);
    eprintln!("  Judge cache: {} entries", cache.len());

    let output = if options.batch {
        crate::eval::runner::run_benchmark_batch(
            &config,
            &corpus,
            &judge_ctx,
            &mut cache,
            &cache_path,
            &corpus_config.transcript_dir,
            grove_config.judge.batch_timeout(),
            options.bootstrap,
        )?
    } else {
        crate::eval::runner::run_benchmark(
            &config,
            &corpus,
            &judge_ctx,
            &mut cache,
            &cache_path,
            &corpus_config.transcript_dir,
            options.bootstrap,
        )?
    };

    if options.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", metrics::format_scorecard(&output));
    }

    Ok(true)
}

/// Run multiple benchmark configurations and compare results.
pub fn run_compare(options: EvalCompareOptions) -> Result<bool, Box<dyn std::error::Error>> {
    let config_names: Vec<&str> = options.configs.split(',').map(|s| s.trim()).collect();
    let configs: Vec<BenchmarkConfig> = config_names
        .iter()
        .map(|name| BenchmarkConfig::from_name(name))
        .collect::<crate::Result<Vec<_>>>()?;

    let corpus_config = resolve_corpus_config(
        options.transcript_dir.as_deref(),
        options.learnings_path.as_deref(),
    )?;

    let grove_config = crate::config::Config::load();
    let judge_ctx = JudgeContext::from_config(&grove_config.judge);

    let cache_path = judge::resolve_cache_path(
        options.cache_path.as_deref(),
        &grove_config.judge.cache_path,
    );

    eprintln!("Loading corpus: {}", corpus_config.name);
    let corpus = crate::eval::corpus::load_corpus(&corpus_config)?;
    eprintln!(
        "  Loaded {} learnings, {} sessions",
        corpus.learnings.len(),
        corpus.contexts.len()
    );

    let mut cache = judge::load_judge_cache(&cache_path);
    let mut outputs = Vec::new();

    for config in &configs {
        eprintln!("\nRunning benchmark: {}", config.name());
        let output = if options.batch {
            crate::eval::runner::run_benchmark_batch(
                config,
                &corpus,
                &judge_ctx,
                &mut cache,
                &cache_path,
                &corpus_config.transcript_dir,
                grove_config.judge.batch_timeout(),
                options.bootstrap,
            )?
        } else {
            crate::eval::runner::run_benchmark(
                config,
                &corpus,
                &judge_ctx,
                &mut cache,
                &cache_path,
                &corpus_config.transcript_dir,
                options.bootstrap,
            )?
        };
        outputs.push(output);
    }

    if options.json {
        println!("{}", serde_json::to_string_pretty(&outputs)?);
    } else {
        println!("{}", metrics::format_comparison(&outputs));
    }

    Ok(true)
}

/// Options for the eval sweep command.
pub struct EvalSweepOptions {
    pub manifest: String,
    pub configs: String,
    /// Override judge cache base path (per-corpus caches derived from name).
    pub cache_path: Option<String>,
    pub json: bool,
    /// Bootstrap resamples for confidence intervals (0 = disabled).
    pub bootstrap: usize,
    /// Evaluate cross-corpus negative pairs to measure false positive rate.
    pub cross_negatives: bool,
    /// Benchmark config to use for negative evaluation.
    pub negative_config: String,
}

/// Run benchmarks across all corpora in a manifest file.
///
/// Iterates each corpus in the manifest, runs each benchmark config,
/// and produces an aggregate report. Each corpus uses an isolated
/// judge cache (derived from corpus name) to avoid key collisions.
///
/// When `cross_negatives` is enabled, also generates all N*(N-1) cross-corpus
/// pairs and evaluates them to measure false positive rate.
pub fn run_sweep(options: EvalSweepOptions) -> Result<bool, Box<dyn std::error::Error>> {
    let manifest_path = std::path::PathBuf::from(&options.manifest);
    let manifest = load_corpus_manifest(&manifest_path)?;

    let config_names: Vec<&str> = options.configs.split(',').map(|s| s.trim()).collect();
    let configs: Vec<BenchmarkConfig> = config_names
        .iter()
        .map(|name| BenchmarkConfig::from_name(name))
        .collect::<crate::Result<Vec<_>>>()?;

    let grove_config = crate::config::Config::load();
    let judge_ctx = JudgeContext::from_config(&grove_config.judge);

    eprintln!(
        "Sweep: {} corpora x {} configs",
        manifest.corpus.len(),
        configs.len()
    );

    // Phase 1: Load all corpora + caches
    let mut loaded: Vec<(CorpusConfig, Corpus, std::path::PathBuf)> = Vec::new();
    let mut corpora_results = Vec::new();

    for entry in &manifest.corpus {
        let corpus_config = entry_to_config(entry);

        // Per-corpus cache priority: entry cache_path > CLI --cache-path > config > env > default
        let cache_path = match &entry.cache_path {
            Some(p) => std::path::PathBuf::from(p),
            None => {
                let base = judge::resolve_cache_path(
                    options.cache_path.as_deref(),
                    &grove_config.judge.cache_path,
                );
                let parent = base.parent().unwrap_or(std::path::Path::new("."));
                parent.join(format!("judge_cache_{}.json", entry.name))
            }
        };

        eprintln!("\n--- Corpus: {} ---", corpus_config.name);
        eprintln!("  Transcripts: {}", corpus_config.transcript_dir.display());
        eprintln!("  Learnings:   {}", corpus_config.learnings_path.display());
        eprintln!("  Cache:       {}", cache_path.display());

        // Validate corpus paths exist before loading
        if !corpus_config.transcript_dir.exists() {
            eprintln!(
                "  SKIP: transcript dir not found: {}",
                corpus_config.transcript_dir.display()
            );
            continue;
        }
        if !corpus_config.learnings_path.exists() {
            eprintln!(
                "  SKIP: learnings file not found: {}",
                corpus_config.learnings_path.display()
            );
            continue;
        }

        let corpus = match load_corpus(&corpus_config) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("  SKIP: failed to load corpus: {}", e);
                continue;
            }
        };

        eprintln!(
            "  Loaded {} learnings, {} sessions",
            corpus.learnings.len(),
            corpus.contexts.len()
        );

        loaded.push((corpus_config, corpus, cache_path));
    }

    // Phase 2: Positive evaluation — run each benchmark config on each corpus
    for (corpus_config, corpus, cache_path) in &loaded {
        let learning_count = corpus.learnings.len();
        let session_count = corpus.contexts.len();

        let mut cache = judge::load_judge_cache(cache_path);
        let mut results = Vec::new();

        for config in &configs {
            eprintln!("  Running: {} on {}", config.name(), corpus_config.name);
            match crate::eval::runner::run_benchmark(
                config,
                corpus,
                &judge_ctx,
                &mut cache,
                cache_path,
                &corpus_config.transcript_dir,
                options.bootstrap,
            ) {
                Ok(output) => results.push(output),
                Err(e) => {
                    eprintln!("  ERROR running {}: {}", config.name(), e);
                }
            }
        }

        corpora_results.push(SweepCorpusResult {
            corpus_name: corpus_config.name.clone(),
            learning_count,
            session_count,
            results,
        });
    }

    // Phase 3: Negative evaluation (if --cross-negatives)
    let negative_results = if options.cross_negatives && loaded.len() >= 2 {
        let neg_config = BenchmarkConfig::from_name(&options.negative_config)?;
        eprintln!(
            "\n--- Cross-corpus negatives ({}) ---",
            options.negative_config
        );

        let mut pairs = Vec::new();
        let mut all_scores: Vec<f64> = Vec::new();

        for i in 0..loaded.len() {
            for j in 0..loaded.len() {
                if i == j {
                    continue;
                }

                let (ref cfg_i, ref corpus_i, ref cache_path_i) = loaded[i];
                let (ref cfg_j, ref corpus_j, _) = loaded[j];

                let neg_corpus = build_negative_corpus(corpus_i, corpus_j);
                let neg_name = &neg_corpus.name;
                eprintln!("  Evaluating: {}", neg_name);

                // Derive a cache path for the negative corpus
                let parent = cache_path_i.parent().unwrap_or(std::path::Path::new("."));
                let neg_cache_path = parent.join(format!("judge_cache_{}.json", neg_name));

                let mut neg_cache = judge::load_judge_cache(&neg_cache_path);

                match crate::eval::runner::run_benchmark(
                    &neg_config,
                    &neg_corpus,
                    &judge_ctx,
                    &mut neg_cache,
                    &neg_cache_path,
                    &cfg_j.transcript_dir, // sessions come from corpus j
                    0,                     // no bootstrap for negatives
                ) {
                    Ok(output) => {
                        // Collect all individual scores for overall aggregation
                        for k in 0..5 {
                            let count = output.metrics.score_distribution[k];
                            for _ in 0..count {
                                all_scores.push((k + 1) as f64);
                            }
                        }

                        pairs.push(NegativePairResult {
                            learnings_from: cfg_i.name.clone(),
                            sessions_from: cfg_j.name.clone(),
                            eval_output: output,
                        });
                    }
                    Err(e) => {
                        eprintln!("  ERROR: {}", e);
                    }
                }
            }
        }

        let total = all_scores.len();
        let overall_mean = if total > 0 {
            all_scores.iter().sum::<f64>() / total as f64
        } else {
            0.0
        };
        let fpr_3 = if total > 0 {
            all_scores.iter().filter(|&&s| s >= 3.0).count() as f64 / total as f64
        } else {
            0.0
        };
        let fpr_4 = if total > 0 {
            all_scores.iter().filter(|&&s| s >= 4.0).count() as f64 / total as f64
        } else {
            0.0
        };

        Some(NegativeSweepOutput {
            config_name: options.negative_config.clone(),
            pairs,
            overall_mean_score: overall_mean,
            overall_fpr_at_3: fpr_3,
            overall_fpr_at_4: fpr_4,
        })
    } else {
        None
    };

    let sweep_output = SweepOutput {
        corpora: corpora_results,
        configs: config_names.iter().map(|s| s.to_string()).collect(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        negative_results,
    };

    if options.json {
        println!("{}", serde_json::to_string_pretty(&sweep_output)?);
    } else {
        println!("{}", metrics::format_sweep(&sweep_output));
    }

    Ok(true)
}

/// Options for the eval dedup-audit command.
pub struct EvalDedupAuditOptions {
    pub manifest: Option<String>,
    pub learnings_path: Option<String>,
    pub threshold: f64,
    pub json: bool,
}

/// Run semantic dedup audit across corpora.
///
/// Embeds all learnings using AllMiniLML6V2, computes pairwise cosine similarity,
/// and flags pairs above the threshold. Requires the `semantic-dedup` feature.
pub fn run_dedup_audit(options: EvalDedupAuditOptions) -> Result<bool, Box<dyn std::error::Error>> {
    #[cfg(not(feature = "semantic-dedup"))]
    {
        let _ = options;
        eprintln!("Error: dedup-audit requires the 'semantic-dedup' feature flag.");
        eprintln!("Rebuild with: cargo build --features semantic-dedup");
        Ok(false)
    }

    #[cfg(feature = "semantic-dedup")]
    {
        use crate::core::embeddings::{cosine_similarity, EmbeddingProvider, FastEmbedProvider};
        use crate::eval::corpus::{entry_to_config, load_corpus_manifest, load_learnings};

        /// A flagged pair of semantically similar learnings.
        #[derive(serde::Serialize)]
        struct DedupPair {
            corpus: String,
            id_a: String,
            summary_a: String,
            id_b: String,
            summary_b: String,
            similarity: f64,
        }

        /// Per-corpus audit result.
        #[derive(serde::Serialize)]
        struct CorpusDedupResult {
            corpus: String,
            learning_count: usize,
            pairs_checked: usize,
            pairs_flagged: usize,
            flagged: Vec<DedupPair>,
        }

        // Load learnings from manifest or single file
        let corpora: Vec<(String, Vec<crate::core::learning::CompoundLearning>)> =
            if let Some(manifest_path) = &options.manifest {
                let manifest = load_corpus_manifest(&std::path::PathBuf::from(manifest_path))?;
                manifest
                    .corpus
                    .iter()
                    .map(|entry| {
                        let config = entry_to_config(entry);
                        let learnings = load_learnings(&config.learnings_path);
                        (entry.name.clone(), learnings)
                    })
                    .collect()
            } else if let Some(path) = &options.learnings_path {
                let learnings = load_learnings(std::path::Path::new(path));
                vec![("default".to_string(), learnings)]
            } else {
                eprintln!("Error: provide --manifest or --learnings-path");
                return Ok(false);
            };

        eprintln!("Initializing embedding model (AllMiniLML6V2)...");
        let provider = FastEmbedProvider::new()?;

        let mut all_results = Vec::new();

        for (name, learnings) in &corpora {
            if learnings.is_empty() {
                eprintln!("  {}: 0 learnings, skipping", name);
                continue;
            }

            eprintln!("  {}: embedding {} learnings...", name, learnings.len());

            // Embed all summaries
            let texts: Vec<&str> = learnings.iter().map(|l| l.summary.as_str()).collect();
            let embeddings = provider.embed(&texts)?;

            // Pairwise comparison
            let n = learnings.len();
            let pairs_checked = n * (n - 1) / 2;
            let mut flagged = Vec::new();

            for i in 0..n {
                for j in (i + 1)..n {
                    let sim = cosine_similarity(&embeddings[i], &embeddings[j]);
                    if sim >= options.threshold {
                        flagged.push(DedupPair {
                            corpus: name.clone(),
                            id_a: learnings[i].id.clone(),
                            summary_a: learnings[i].summary.clone(),
                            id_b: learnings[j].id.clone(),
                            summary_b: learnings[j].summary.clone(),
                            similarity: sim,
                        });
                    }
                }
            }

            flagged.sort_by(|a, b| {
                b.similarity
                    .partial_cmp(&a.similarity)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            all_results.push(CorpusDedupResult {
                corpus: name.clone(),
                learning_count: n,
                pairs_checked,
                pairs_flagged: flagged.len(),
                flagged,
            });
        }

        if options.json {
            println!("{}", serde_json::to_string_pretty(&all_results)?);
        } else {
            for result in &all_results {
                println!(
                    "\n=== {} ({} learnings, {} pairs checked) ===",
                    result.corpus, result.learning_count, result.pairs_checked
                );
                if result.flagged.is_empty() {
                    println!("  No pairs above threshold ({:.2})", options.threshold);
                } else {
                    println!(
                        "  {} pairs flagged (>= {:.2}):\n",
                        result.pairs_flagged, options.threshold
                    );
                    for pair in &result.flagged {
                        println!(
                            "  sim={:.4}  {} vs {}",
                            pair.similarity, pair.id_a, pair.id_b
                        );
                        println!("    A: {}", pair.summary_a);
                        println!("    B: {}", pair.summary_b);
                        println!();
                    }
                }
            }
        }

        Ok(true)
    }
}
