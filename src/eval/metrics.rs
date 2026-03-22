//! Metrics aggregation and formatting for benchmark results.

use serde::Serialize;

/// A 95% confidence interval computed via bootstrap resampling.
#[derive(Debug, Clone, Serialize)]
pub struct ConfidenceInterval {
    pub lower: f64,
    pub upper: f64,
}

/// Aggregated benchmark metrics.
#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkMetrics {
    /// Number of sessions that produced scored pairs.
    pub sessions_evaluated: usize,
    /// Number of sessions suppressed by adaptive threshold.
    pub sessions_suppressed: usize,
    /// Total (session, learning) pairs judged.
    pub pairs_judged: usize,
    /// Average relevance score across all pairs.
    pub avg_relevance: f64,
    /// Median relevance score.
    pub median_relevance: f64,
    /// Score distribution: counts for scores 1-5.
    pub score_distribution: [usize; 5],
    /// Number of pairs with score <= 2 (noise).
    pub noise_count: usize,
    /// Noise percentage.
    pub noise_pct: f64,
    /// Precision at threshold 3: fraction of surfaced pairs scoring >= 3.
    pub precision_at_3: f64,
    /// Recall at threshold 4: fraction of all relevant (>=4) pairs that were surfaced.
    pub recall_at_4: f64,
    /// Recall at threshold 5: fraction of all highly relevant (>=5) pairs surfaced.
    pub recall_at_5: f64,
    /// F1 score combining precision_at_3 and recall_at_4.
    pub f1_at_4: f64,
    /// Per-session precision at 3: mean of per-session top-K precision (position-sensitive).
    pub precision_at_3_per_session: f64,
    /// Total sessions in the corpus.
    pub sessions_total: usize,
    /// Coverage: fraction of corpus sessions that received at least one surfaced result.
    pub coverage: f64,
    /// Minimum pairs surfaced per session (among sessions with results).
    pub pairs_per_session_min: usize,
    /// Mean pairs surfaced per session.
    pub pairs_per_session_mean: f64,
    /// Maximum pairs surfaced per session (among sessions with results).
    pub pairs_per_session_max: usize,
    /// Mean Reciprocal Rank: average of 1/rank of first relevant (>=4) pair per session.
    pub mrr_at_4: f64,
    /// Bootstrap 95% CI for avg_relevance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_avg_relevance: Option<ConfidenceInterval>,
    /// Bootstrap 95% CI for precision_at_3.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_precision_at_3: Option<ConfidenceInterval>,
    /// Bootstrap 95% CI for recall_at_4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_recall_at_4: Option<ConfidenceInterval>,
    /// Bootstrap 95% CI for f1_at_4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_f1_at_4: Option<ConfidenceInterval>,
}

/// Judge execution statistics.
#[derive(Debug, Clone, Serialize)]
pub struct JudgeStats {
    /// Total judge invocations.
    pub total_calls: usize,
    /// Results served from cache.
    pub cache_hits: usize,
    /// Fresh LLM calls made.
    pub new_judgments: usize,
    /// Failed judge calls.
    pub failures: usize,
}

/// Complete evaluation output with metadata.
#[derive(Debug, Clone, Serialize)]
pub struct EvalOutput {
    /// Name of the benchmark configuration used.
    pub config_name: String,
    /// Name of the corpus evaluated.
    pub corpus_name: String,
    /// Aggregated metrics.
    pub metrics: BenchmarkMetrics,
    /// Judge execution statistics.
    pub judge_stats: JudgeStats,
    /// ISO 8601 timestamp.
    pub timestamp: String,
}

/// Compute benchmark metrics from a list of scores.
///
/// `ground_truth_relevant` is the total count of relevant pairs (score >= 4) across all
/// sessions in the ground truth cache (typically from a broad `bm25` run). When provided,
/// recall metrics are computed. Pass `None` if ground truth is unavailable.
pub fn compute_metrics(
    scores: &[f64],
    sessions_evaluated: usize,
    sessions_suppressed: usize,
) -> BenchmarkMetrics {
    compute_metrics_with_recall(scores, sessions_evaluated, sessions_suppressed, None, &[])
}

/// Compute benchmark metrics with optional recall against ground truth.
///
/// `ground_truth` maps `"session_file:learning_id"` → score for the broad (bm25) run.
/// `surfaced_keys` is the set of `"session_file:learning_id"` keys this config surfaced.
pub fn compute_metrics_with_recall(
    scores: &[f64],
    sessions_evaluated: usize,
    sessions_suppressed: usize,
    recall_data: Option<&RecallData>,
    session_scores: &[Vec<f64>],
) -> BenchmarkMetrics {
    if scores.is_empty() {
        return BenchmarkMetrics {
            sessions_evaluated,
            sessions_suppressed,
            pairs_judged: 0,
            avg_relevance: 0.0,
            median_relevance: 0.0,
            score_distribution: [0; 5],
            noise_count: 0,
            noise_pct: 0.0,
            precision_at_3: 0.0,
            recall_at_4: 0.0,
            recall_at_5: 0.0,
            f1_at_4: 0.0,
            precision_at_3_per_session: 0.0,
            sessions_total: 0,
            coverage: 0.0,
            pairs_per_session_min: 0,
            pairs_per_session_mean: 0.0,
            pairs_per_session_max: 0,
            mrr_at_4: 0.0,
            ci_avg_relevance: None,
            ci_precision_at_3: None,
            ci_recall_at_4: None,
            ci_f1_at_4: None,
        };
    }

    let avg = scores.iter().sum::<f64>() / scores.len() as f64;

    let mut sorted = scores.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = {
        let mid = sorted.len() / 2;
        if sorted.len().is_multiple_of(2) {
            (sorted[mid - 1] + sorted[mid]) / 2.0
        } else {
            sorted[mid]
        }
    };

    let mut dist = [0usize; 5];
    for &s in scores {
        let idx = (s as usize).saturating_sub(1).min(4);
        dist[idx] += 1;
    }

    let noise_count = scores.iter().filter(|&&s| s <= 2.0).count();
    let noise_pct = noise_count as f64 / scores.len() as f64 * 100.0;

    // Precision: fraction of surfaced pairs scoring >= 3
    let relevant_surfaced = scores.iter().filter(|&&s| s >= 3.0).count();
    let precision_at_3 = relevant_surfaced as f64 / scores.len() as f64;

    // Recall: computed against ground truth if available
    let (recall_at_4, recall_at_5) = if let Some(rd) = recall_data {
        let r4 = if rd.ground_truth_at_4 > 0 {
            rd.surfaced_at_4 as f64 / rd.ground_truth_at_4 as f64
        } else {
            0.0
        };
        let r5 = if rd.ground_truth_at_5 > 0 {
            rd.surfaced_at_5 as f64 / rd.ground_truth_at_5 as f64
        } else {
            0.0
        };
        (r4, r5)
    } else {
        (0.0, 0.0)
    };

    // F1: harmonic mean of precision_at_3 and recall_at_4
    let f1_at_4 = if precision_at_3 + recall_at_4 > 0.0 {
        2.0 * precision_at_3 * recall_at_4 / (precision_at_3 + recall_at_4)
    } else {
        0.0
    };

    let precision_at_3_per_session = compute_per_session_precision(session_scores, 3, 3.0);

    BenchmarkMetrics {
        sessions_evaluated,
        sessions_suppressed,
        pairs_judged: scores.len(),
        avg_relevance: avg,
        median_relevance: median,
        score_distribution: dist,
        noise_count,
        noise_pct,
        precision_at_3,
        recall_at_4,
        recall_at_5,
        f1_at_4,
        precision_at_3_per_session,
        sessions_total: 0,
        coverage: 0.0,
        pairs_per_session_min: 0,
        pairs_per_session_mean: 0.0,
        pairs_per_session_max: 0,
        mrr_at_4: 0.0,
        ci_avg_relevance: None,
        ci_precision_at_3: None,
        ci_recall_at_4: None,
        ci_f1_at_4: None,
    }
}

/// Compute per-session precision: for each session, take the top-K scores (position-ordered)
/// and compute the fraction that meet the relevance threshold. Returns the mean across sessions.
///
/// This is position-sensitive — reranking changes the order within each session's results,
/// which directly affects which items land in the top-K window.
pub fn compute_per_session_precision(session_scores: &[Vec<f64>], k: usize, threshold: f64) -> f64 {
    if session_scores.is_empty() || k == 0 {
        return 0.0;
    }

    let mut total_precision = 0.0;
    let mut session_count = 0;

    for session in session_scores {
        if session.is_empty() {
            continue;
        }
        let window = session.len().min(k);
        let relevant = session[..window]
            .iter()
            .filter(|&&s| s >= threshold)
            .count();
        total_precision += relevant as f64 / window as f64;
        session_count += 1;
    }

    if session_count == 0 {
        0.0
    } else {
        total_precision / session_count as f64
    }
}

/// Compute Mean Reciprocal Rank at a given threshold.
///
/// For each session, finds the position of the first score >= `threshold`
/// and computes 1/(position+1). Returns the mean across all sessions.
/// Sessions with no relevant result contribute 0 to the mean.
pub fn compute_mrr(session_scores: &[Vec<f64>], threshold: f64) -> f64 {
    if session_scores.is_empty() {
        return 0.0;
    }

    let mut sum_rr = 0.0;
    let mut count = 0;

    for scores in session_scores {
        if scores.is_empty() {
            continue;
        }
        count += 1;
        if let Some(pos) = scores.iter().position(|&s| s >= threshold) {
            sum_rr += 1.0 / (pos as f64 + 1.0);
        }
    }

    if count == 0 {
        0.0
    } else {
        sum_rr / count as f64
    }
}

/// Compute pairs-per-session statistics (min, mean, max).
///
/// Returns `(min, mean, max)` for sessions that have at least one scored pair.
/// Returns `(0, 0.0, 0)` if no sessions have results.
pub fn compute_pairs_per_session(session_scores: &[Vec<f64>]) -> (usize, f64, usize) {
    let counts: Vec<usize> = session_scores
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.len())
        .collect();

    if counts.is_empty() {
        return (0, 0.0, 0);
    }

    let min = *counts.iter().min().unwrap_or(&0);
    let max = *counts.iter().max().unwrap_or(&0);
    let mean = counts.iter().sum::<usize>() as f64 / counts.len() as f64;

    (min, mean, max)
}

/// Data needed to compute recall metrics against a ground truth cache.
pub struct RecallData {
    /// Count of ground truth pairs scoring >= 4 (relevant).
    pub ground_truth_at_4: usize,
    /// Count of ground truth pairs scoring >= 5 (highly relevant).
    pub ground_truth_at_5: usize,
    /// Count of surfaced pairs that scored >= 4 in the ground truth.
    pub surfaced_at_4: usize,
    /// Count of surfaced pairs that scored >= 5 in the ground truth.
    pub surfaced_at_5: usize,
}

/// Compute a bootstrap confidence interval for a statistic.
///
/// Resamples `data` with replacement `n_resamples` times, computes `statistic`
/// on each resample, and returns the percentile-based CI at the given `alpha`
/// (e.g., 0.05 for 95% CI). Returns `None` if data has fewer than 2 elements.
pub fn bootstrap_ci<F>(
    data: &[f64],
    statistic: F,
    n_resamples: usize,
    alpha: f64,
) -> Option<ConfidenceInterval>
where
    F: Fn(&[f64]) -> f64,
{
    if data.len() < 2 || n_resamples == 0 {
        return None;
    }

    use rand::Rng;
    let mut rng = rand::rng();
    let n = data.len();
    let mut estimates: Vec<f64> = Vec::with_capacity(n_resamples);

    let mut sample = vec![0.0; n];
    for _ in 0..n_resamples {
        for s in sample.iter_mut() {
            *s = data[rng.random_range(0..n)];
        }
        estimates.push(statistic(&sample));
    }

    estimates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let lo_idx = ((alpha / 2.0) * estimates.len() as f64).floor() as usize;
    let hi_idx = ((1.0 - alpha / 2.0) * estimates.len() as f64).ceil() as usize;
    let hi_idx = hi_idx.min(estimates.len() - 1);

    Some(ConfidenceInterval {
        lower: estimates[lo_idx],
        upper: estimates[hi_idx],
    })
}

/// Compute bootstrap CIs for key metrics by resampling at the session level.
///
/// Resamples sessions (not individual scores) to account for within-session correlation.
/// Returns CIs for (avg_relevance, precision_at_3, recall_at_4, f1_at_4).
pub fn bootstrap_session_cis(
    session_scores: &[Vec<f64>],
    recall_data: Option<&RecallData>,
    n_resamples: usize,
    alpha: f64,
) -> (
    Option<ConfidenceInterval>,
    Option<ConfidenceInterval>,
    Option<ConfidenceInterval>,
    Option<ConfidenceInterval>,
) {
    if session_scores.len() < 2 || n_resamples == 0 {
        return (None, None, None, None);
    }

    use rand::Rng;
    let mut rng = rand::rng();
    let n = session_scores.len();

    let mut avg_estimates = Vec::with_capacity(n_resamples);
    let mut prec_estimates = Vec::with_capacity(n_resamples);
    let mut recall_estimates = Vec::with_capacity(n_resamples);
    let mut f1_estimates = Vec::with_capacity(n_resamples);

    for _ in 0..n_resamples {
        let mut all_scores: Vec<f64> = Vec::new();
        for _ in 0..n {
            let idx = rng.random_range(0..n);
            all_scores.extend_from_slice(&session_scores[idx]);
        }

        if all_scores.is_empty() {
            continue;
        }

        let avg = all_scores.iter().sum::<f64>() / all_scores.len() as f64;
        avg_estimates.push(avg);

        let prec =
            all_scores.iter().filter(|&&s| s >= 3.0).count() as f64 / all_scores.len() as f64;
        prec_estimates.push(prec);

        if let Some(rd) = recall_data {
            if rd.ground_truth_at_4 > 0 {
                let surfaced_ge4 = all_scores.iter().filter(|&&s| s >= 4.0).count();
                let recall = surfaced_ge4 as f64 / rd.ground_truth_at_4 as f64;
                recall_estimates.push(recall);

                let f1 = if prec + recall > 0.0 {
                    2.0 * prec * recall / (prec + recall)
                } else {
                    0.0
                };
                f1_estimates.push(f1);
            }
        }
    }

    let sort_and_ci = |mut estimates: Vec<f64>| -> Option<ConfidenceInterval> {
        if estimates.len() < 2 {
            return None;
        }
        estimates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let lo_idx = ((alpha / 2.0) * estimates.len() as f64).floor() as usize;
        let hi_idx = ((1.0 - alpha / 2.0) * estimates.len() as f64).ceil() as usize;
        let hi_idx = hi_idx.min(estimates.len() - 1);
        Some(ConfidenceInterval {
            lower: estimates[lo_idx],
            upper: estimates[hi_idx],
        })
    };

    (
        sort_and_ci(avg_estimates),
        sort_and_ci(prec_estimates),
        sort_and_ci(recall_estimates),
        sort_and_ci(f1_estimates),
    )
}

/// Compute benchmark metrics with optional recall and optional bootstrap CIs.
///
/// When `n_bootstrap > 0`, computes 95% confidence intervals by resampling
/// sessions. This preserves within-session score correlation.
pub fn compute_metrics_with_ci(
    scores: &[f64],
    sessions_evaluated: usize,
    sessions_suppressed: usize,
    recall_data: Option<RecallData>,
    session_scores: &[Vec<f64>],
    n_bootstrap: usize,
) -> BenchmarkMetrics {
    let mut metrics = compute_metrics_with_recall(
        scores,
        sessions_evaluated,
        sessions_suppressed,
        recall_data.as_ref(),
        session_scores,
    );

    if n_bootstrap > 0 && !session_scores.is_empty() {
        let (ci_avg, ci_prec, ci_recall, ci_f1) =
            bootstrap_session_cis(session_scores, recall_data.as_ref(), n_bootstrap, 0.05);
        metrics.ci_avg_relevance = ci_avg;
        metrics.ci_precision_at_3 = ci_prec;
        metrics.ci_recall_at_4 = ci_recall;
        metrics.ci_f1_at_4 = ci_f1;
    }

    metrics
}

/// Format a scorecard for human-readable terminal output.
pub fn format_scorecard(output: &EvalOutput) -> String {
    let m = &output.metrics;
    let j = &output.judge_stats;

    let mut lines = Vec::new();
    lines.push("=".repeat(70));
    lines.push(format!(
        "EVAL SCORECARD: {} (corpus: {})",
        output.config_name, output.corpus_name
    ));
    lines.push("=".repeat(70));

    lines.push(String::new());
    lines.push("  Execution:".to_string());
    lines.push(format!(
        "    Total judge calls:           {}",
        j.total_calls
    ));
    lines.push(format!("    Cache hits:                  {}", j.cache_hits));
    lines.push(format!(
        "    New judgments:                {}",
        j.new_judgments
    ));
    lines.push(format!("    Failures:                    {}", j.failures));

    lines.push(String::new());
    lines.push("  Results:".to_string());
    lines.push(format!(
        "    Sessions evaluated:          {}",
        m.sessions_evaluated
    ));
    lines.push(format!(
        "    Sessions suppressed:         {}",
        m.sessions_suppressed
    ));
    lines.push(format!(
        "    Pairs judged:                {}",
        m.pairs_judged
    ));
    if m.sessions_total > 0 {
        let sessions_with_results = (m.coverage * m.sessions_total as f64).round() as usize;
        lines.push(format!(
            "    Coverage:                    {:.0}% ({}/{})",
            m.coverage * 100.0,
            sessions_with_results,
            m.sessions_total
        ));
    }
    if m.pairs_per_session_max > 0 {
        lines.push(format!(
            "    Pairs per session:           {} / {:.1} / {} (min/mean/max)",
            m.pairs_per_session_min, m.pairs_per_session_mean, m.pairs_per_session_max
        ));
    }
    lines.push(format!(
        "    Avg relevance:               {:.2}",
        m.avg_relevance
    ));
    if let Some(ref ci) = m.ci_avg_relevance {
        lines.push(format!(
            "      95% CI:                    [{:.2}, {:.2}]",
            ci.lower, ci.upper
        ));
    }
    lines.push(format!(
        "    Median relevance:            {:.1}",
        m.median_relevance
    ));
    lines.push(format!(
        "    Distribution:                1={} 2={} 3={} 4={} 5={}",
        m.score_distribution[0],
        m.score_distribution[1],
        m.score_distribution[2],
        m.score_distribution[3],
        m.score_distribution[4]
    ));
    lines.push(format!(
        "    Noise (<=2):                 {} / {} ({:.0}%)",
        m.noise_count, m.pairs_judged, m.noise_pct
    ));
    lines.push(format!(
        "    Precision (>=3):             {:.1}%",
        m.precision_at_3 * 100.0
    ));
    if let Some(ref ci) = m.ci_precision_at_3 {
        lines.push(format!(
            "      95% CI:                    [{:.1}%, {:.1}%]",
            ci.lower * 100.0,
            ci.upper * 100.0
        ));
    }
    lines.push(format!(
        "    Precision (>=3, per-sess):   {:.1}%",
        m.precision_at_3_per_session * 100.0
    ));
    if m.recall_at_4 > 0.0 {
        lines.push(format!(
            "    Recall (>=4):                {:.1}%",
            m.recall_at_4 * 100.0
        ));
        if let Some(ref ci) = m.ci_recall_at_4 {
            lines.push(format!(
                "      95% CI:                    [{:.1}%, {:.1}%]",
                ci.lower * 100.0,
                ci.upper * 100.0
            ));
        }
        lines.push(format!(
            "    Recall (>=5):                {:.1}%",
            m.recall_at_5 * 100.0
        ));
        lines.push(format!("    F1 (P@3 x R@4):             {:.3}", m.f1_at_4));
        if let Some(ref ci) = m.ci_f1_at_4 {
            lines.push(format!(
                "      95% CI:                    [{:.3}, {:.3}]",
                ci.lower, ci.upper
            ));
        }
    }
    if m.mrr_at_4 > 0.0 {
        lines.push(format!(
            "    MRR@4:                       {:.3}",
            m.mrr_at_4
        ));
    }

    lines.push(format!("\n{}", "=".repeat(70)));
    lines.join("\n")
}

/// Append the metric abbreviation key to a set of output lines.
fn append_metric_key(lines: &mut Vec<String>, has_recall: bool) {
    lines.push(String::new());
    lines.push("  Key:".to_string());
    lines.push("    Pairs   = (session, learning) pairs judged".to_string());
    lines.push("    Avg/Med = mean / median relevance score (1-5)".to_string());
    lines.push("    Noise%  = % of pairs scoring <= 2 (irrelevant)".to_string());
    lines.push("    P@3g    = precision: % of all surfaced pairs scoring >= 3".to_string());
    lines.push(
        "    P@3     = precision per-session: mean of per-session top-3 precision (>= 3)"
            .to_string(),
    );
    if has_recall {
        lines.push(
            "    R@4     = recall: % of ground-truth relevant pairs (>= 4) that were surfaced"
                .to_string(),
        );
        lines.push("    F1      = harmonic mean of P@3g and R@4".to_string());
    }
    lines.push(
        "    Cov%    = coverage: % of corpus sessions receiving at least one result".to_string(),
    );
    lines.push(
        "    MRR     = mean reciprocal rank: avg of 1/rank of first relevant (>= 4) pair"
            .to_string(),
    );
}

/// Format CI parts for a single metrics instance (compact one-line summary).
fn format_ci_parts(m: &BenchmarkMetrics) -> String {
    let mut parts = vec![];
    if let Some(ref ci) = m.ci_avg_relevance {
        parts.push(format!("Avg [{:.2}, {:.2}]", ci.lower, ci.upper));
    }
    if let Some(ref ci) = m.ci_precision_at_3 {
        parts.push(format!(
            "P@3g [{:.1}%, {:.1}%]",
            ci.lower * 100.0,
            ci.upper * 100.0
        ));
    }
    if let Some(ref ci) = m.ci_recall_at_4 {
        parts.push(format!(
            "R@4 [{:.1}%, {:.1}%]",
            ci.lower * 100.0,
            ci.upper * 100.0
        ));
    }
    if let Some(ref ci) = m.ci_f1_at_4 {
        parts.push(format!("F1 [{:.3}, {:.3}]", ci.lower, ci.upper));
    }
    parts.join("  ")
}

/// Format a comparison table for multiple eval outputs.
pub fn format_comparison(outputs: &[EvalOutput]) -> String {
    if outputs.is_empty() {
        return "No results to compare.".to_string();
    }

    let mut lines = vec![
        "=".repeat(80),
        "EVAL COMPARISON".to_string(),
        "=".repeat(80),
        String::new(),
    ];

    // Check if any output has recall data
    let has_recall = outputs.iter().any(|o| o.metrics.recall_at_4 > 0.0);

    // Header
    if has_recall {
        lines.push(format!(
            "  {:<25} {:>6} {:>6} {:>6} {:>7} {:>5} {:>6} {:>6} {:>6} {:>6} {:>5} {:>6}",
            "Config",
            "Pairs",
            "Avg",
            "Med",
            "Noise%",
            "Supp",
            "P@3g",
            "P@3",
            "R@4",
            "F1",
            "Cov%",
            "MRR"
        ));
        lines.push(format!("  {}", "-".repeat(103)));
    } else {
        lines.push(format!(
            "  {:<25} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6} {:>6}",
            "Config", "Pairs", "Avg", "Median", "Noise%", "Supp", "P@3g", "P@3", "Cov%", "MRR"
        ));
        lines.push(format!("  {}", "-".repeat(101)));
    }

    for output in outputs {
        let m = &output.metrics;
        if has_recall {
            lines.push(format!(
                "  {:<25} {:>6} {:>6.2} {:>6.1} {:>6.0}% {:>5} {:>5.0}% {:>5.0}% {:>5.0}% {:>6.3} {:>4.0}% {:>6.3}",
                output.config_name,
                m.pairs_judged,
                m.avg_relevance,
                m.median_relevance,
                m.noise_pct,
                m.sessions_suppressed,
                m.precision_at_3 * 100.0,
                m.precision_at_3_per_session * 100.0,
                m.recall_at_4 * 100.0,
                m.f1_at_4,
                m.coverage * 100.0,
                m.mrr_at_4,
            ));
        } else {
            lines.push(format!(
                "  {:<25} {:>8} {:>8.2} {:>8.1} {:>7.0}% {:>8} {:>7.0}% {:>7.0}% {:>5.0}% {:>6.3}",
                output.config_name,
                m.pairs_judged,
                m.avg_relevance,
                m.median_relevance,
                m.noise_pct,
                m.sessions_suppressed,
                m.precision_at_3 * 100.0,
                m.precision_at_3_per_session * 100.0,
                m.coverage * 100.0,
                m.mrr_at_4,
            ));
        }
    }

    // Add bootstrap CI summary if any output has CIs
    let has_cis = outputs.iter().any(|o| o.metrics.ci_avg_relevance.is_some());
    if has_cis {
        lines.push(String::new());
        lines.push("  Bootstrap 95% CIs:".to_string());
        for output in outputs {
            let ci_parts = format_ci_parts(&output.metrics);
            if !ci_parts.is_empty() {
                lines.push(format!("    {:<23} {}", output.config_name, ci_parts));
            }
        }
    }

    append_metric_key(&mut lines, has_recall);

    lines.push(format!("\n{}", "=".repeat(80)));
    lines.join("\n")
}

/// Sweep result for one corpus across multiple configs.
#[derive(Debug, Clone, Serialize)]
pub struct SweepCorpusResult {
    /// Corpus name.
    pub corpus_name: String,
    /// Number of learnings in the corpus.
    pub learning_count: usize,
    /// Number of sessions in the corpus.
    pub session_count: usize,
    /// Per-config results.
    pub results: Vec<EvalOutput>,
}

/// Aggregate sweep output across all corpora.
#[derive(Debug, Clone, Serialize)]
pub struct SweepOutput {
    /// Results per corpus.
    pub corpora: Vec<SweepCorpusResult>,
    /// Configs that were evaluated.
    pub configs: Vec<String>,
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// Cross-corpus negative evaluation results (when --cross-negatives is used).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negative_results: Option<NegativeSweepOutput>,
}

/// Result for a single cross-corpus negative pair evaluation.
#[derive(Debug, Clone, Serialize)]
pub struct NegativePairResult {
    /// Corpus name the learnings came from.
    pub learnings_from: String,
    /// Corpus name the sessions came from.
    pub sessions_from: String,
    /// Full evaluation output (reuses existing metrics).
    pub eval_output: EvalOutput,
}

/// Aggregate output for all cross-corpus negative evaluations.
#[derive(Debug, Clone, Serialize)]
pub struct NegativeSweepOutput {
    /// Config name used for negative evaluation.
    pub config_name: String,
    /// Per-pair results.
    pub pairs: Vec<NegativePairResult>,
    /// Mean relevance score across all negative pairs.
    pub overall_mean_score: f64,
    /// Fraction of all negative pairs scoring >= 3 (false positive rate).
    pub overall_fpr_at_3: f64,
    /// Fraction of all negative pairs scoring >= 4 (false positive rate).
    pub overall_fpr_at_4: f64,
}

/// Format a sweep report for human-readable terminal output.
pub fn format_sweep(output: &SweepOutput) -> String {
    let mut lines = vec![
        "=".repeat(80),
        "EVAL SWEEP — MULTI-CORPUS BENCHMARK".to_string(),
        "=".repeat(80),
        String::new(),
    ];

    for corpus in &output.corpora {
        let has_recall = corpus.results.iter().any(|r| r.metrics.recall_at_4 > 0.0);

        lines.push(format!(
            "  Corpus: {} ({} learnings, {} sessions)",
            corpus.corpus_name, corpus.learning_count, corpus.session_count
        ));

        if has_recall {
            lines.push(format!(
                "  {:<25} {:>6} {:>6} {:>6} {:>7} {:>6} {:>6} {:>6} {:>6} {:>5} {:>6}",
                "Config",
                "Pairs",
                "Avg",
                "Med",
                "Noise%",
                "P@3g",
                "P@3",
                "R@4",
                "F1",
                "Cov%",
                "MRR"
            ));
            lines.push(format!("  {}", "-".repeat(97)));
        } else {
            lines.push(format!(
                "  {:<25} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6} {:>6}",
                "Config", "Pairs", "Avg", "Median", "Noise%", "P@3g", "P@3", "Cov%", "MRR"
            ));
            lines.push(format!("  {}", "-".repeat(89)));
        }

        for result in &corpus.results {
            let m = &result.metrics;
            if has_recall {
                lines.push(format!(
                    "  {:<25} {:>6} {:>6.2} {:>6.1} {:>6.0}% {:>5.0}% {:>5.0}% {:>5.0}% {:>6.3} {:>4.0}% {:>6.3}",
                    result.config_name,
                    m.pairs_judged,
                    m.avg_relevance,
                    m.median_relevance,
                    m.noise_pct,
                    m.precision_at_3 * 100.0,
                    m.precision_at_3_per_session * 100.0,
                    m.recall_at_4 * 100.0,
                    m.f1_at_4,
                    m.coverage * 100.0,
                    m.mrr_at_4,
                ));
            } else {
                lines.push(format!(
                    "  {:<25} {:>8} {:>8.2} {:>8.1} {:>7.0}% {:>7.0}% {:>7.0}% {:>5.0}% {:>6.3}",
                    result.config_name,
                    m.pairs_judged,
                    m.avg_relevance,
                    m.median_relevance,
                    m.noise_pct,
                    m.precision_at_3 * 100.0,
                    m.precision_at_3_per_session * 100.0,
                    m.coverage * 100.0,
                    m.mrr_at_4,
                ));
            }
        }

        // Add bootstrap CI summary if any result in this corpus has CIs
        let has_cis = corpus
            .results
            .iter()
            .any(|r| r.metrics.ci_avg_relevance.is_some());
        if has_cis {
            lines.push("  Bootstrap 95% CIs:".to_string());
            for result in &corpus.results {
                let ci_parts = format_ci_parts(&result.metrics);
                if !ci_parts.is_empty() {
                    lines.push(format!("    {:<23} {}", result.config_name, ci_parts));
                }
            }
        }

        lines.push(String::new());
    }

    // Show key if any corpus had recall
    let any_recall = output
        .corpora
        .iter()
        .any(|c| c.results.iter().any(|r| r.metrics.recall_at_4 > 0.0));
    append_metric_key(&mut lines, any_recall);

    // Append negative results if present
    if let Some(ref neg) = output.negative_results {
        lines.push(String::new());
        lines.push(format_negative_sweep(neg));
    }

    lines.push(format!("\n{}", "=".repeat(80)));
    lines.join("\n")
}

/// Format a cross-corpus negative sweep report for terminal output.
pub fn format_negative_sweep(output: &NegativeSweepOutput) -> String {
    let mut lines = vec![
        "=".repeat(80),
        format!("CROSS-CORPUS NEGATIVES (config: {})", output.config_name),
        "=".repeat(80),
        String::new(),
    ];

    if output.pairs.is_empty() {
        lines.push("  No cross-corpus pairs evaluated.".to_string());
    } else {
        lines.push(format!(
            "  {:<30} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
            "Pair", "Sessions", "Surfaced%", "Pairs", "Mean", "FPR@3", "FPR@4"
        ));
        lines.push(format!("  {}", "-".repeat(80)));

        for pair in &output.pairs {
            let m = &pair.eval_output.metrics;
            let pair_name = format!("{} -> {}", pair.learnings_from, pair.sessions_from);
            let surfaced_pct = if m.pairs_judged > 0 {
                m.precision_at_3 * 100.0
            } else {
                0.0
            };
            let fpr_3 = if m.pairs_judged > 0 {
                m.score_distribution[2..].iter().sum::<usize>() as f64 / m.pairs_judged as f64
                    * 100.0
            } else {
                0.0
            };
            let fpr_4 = if m.pairs_judged > 0 {
                m.score_distribution[3..].iter().sum::<usize>() as f64 / m.pairs_judged as f64
                    * 100.0
            } else {
                0.0
            };
            lines.push(format!(
                "  {:<30} {:>8} {:>7.0}% {:>8} {:>8.2} {:>7.0}% {:>7.0}%",
                pair_name,
                m.sessions_evaluated,
                surfaced_pct,
                m.pairs_judged,
                m.avg_relevance,
                fpr_3,
                fpr_4,
            ));
        }

        lines.push(format!("  {}", "-".repeat(80)));
        lines.push(format!(
            "  {:<30} {:>8} {:>8} {:>8} {:>8.2} {:>7.0}% {:>7.0}%",
            "OVERALL",
            "",
            "",
            output
                .pairs
                .iter()
                .map(|p| p.eval_output.metrics.pairs_judged)
                .sum::<usize>(),
            output.overall_mean_score,
            output.overall_fpr_at_3 * 100.0,
            output.overall_fpr_at_4 * 100.0,
        ));
    }

    lines.push(String::new());
    lines.push("  Key:".to_string());
    lines
        .push("    FPR@3 = false positive rate: % of cross-project pairs scoring >= 3".to_string());
    lines
        .push("    FPR@4 = false positive rate: % of cross-project pairs scoring >= 4".to_string());
    lines.push("    Expectation: FPR@3 < 20%, FPR@4 < 5%, Mean ~1.5".to_string());
    lines.push(
        "    High FPR indicates retrieval is not discriminating between projects".to_string(),
    );

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_metrics_empty_scores() {
        let m = compute_metrics(&[], 0, 0);
        assert_eq!(m.pairs_judged, 0);
        assert_eq!(m.avg_relevance, 0.0);
        assert_eq!(m.noise_count, 0);
    }

    #[test]
    fn compute_metrics_single_score() {
        let m = compute_metrics(&[4.0], 1, 0);
        assert_eq!(m.pairs_judged, 1);
        assert_eq!(m.avg_relevance, 4.0);
        assert_eq!(m.median_relevance, 4.0);
        assert_eq!(m.noise_count, 0);
        assert_eq!(m.score_distribution, [0, 0, 0, 1, 0]);
    }

    #[test]
    fn compute_metrics_multiple_scores() {
        let scores = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let m = compute_metrics(&scores, 5, 1);
        assert_eq!(m.pairs_judged, 5);
        assert_eq!(m.avg_relevance, 3.0);
        assert_eq!(m.median_relevance, 3.0);
        assert_eq!(m.noise_count, 2);
        assert!((m.noise_pct - 40.0).abs() < 0.01);
        assert_eq!(m.score_distribution, [1, 1, 1, 1, 1]);
        assert_eq!(m.sessions_evaluated, 5);
        assert_eq!(m.sessions_suppressed, 1);
    }

    #[test]
    fn compute_metrics_even_count_median() {
        let scores = vec![1.0, 2.0, 3.0, 4.0];
        let m = compute_metrics(&scores, 4, 0);
        assert_eq!(m.median_relevance, 2.5);
    }

    #[test]
    fn format_scorecard_produces_output() {
        let output = EvalOutput {
            config_name: "adaptive".to_string(),
            corpus_name: "test".to_string(),
            metrics: compute_metrics(&[3.0, 4.0, 5.0], 3, 0),
            judge_stats: JudgeStats {
                total_calls: 3,
                cache_hits: 1,
                new_judgments: 2,
                failures: 0,
            },
            timestamp: "2026-03-15T00:00:00Z".to_string(),
        };
        let card = format_scorecard(&output);
        assert!(card.contains("EVAL SCORECARD: adaptive"));
        assert!(card.contains("Avg relevance"));
    }

    #[test]
    fn format_comparison_multiple() {
        let outputs = vec![
            EvalOutput {
                config_name: "bm25".to_string(),
                corpus_name: "test".to_string(),
                metrics: compute_metrics(&[2.0, 3.0], 2, 0),
                judge_stats: JudgeStats {
                    total_calls: 2,
                    cache_hits: 0,
                    new_judgments: 2,
                    failures: 0,
                },
                timestamp: "2026-03-15T00:00:00Z".to_string(),
            },
            EvalOutput {
                config_name: "adaptive".to_string(),
                corpus_name: "test".to_string(),
                metrics: compute_metrics(&[3.0, 4.0], 2, 0),
                judge_stats: JudgeStats {
                    total_calls: 2,
                    cache_hits: 0,
                    new_judgments: 2,
                    failures: 0,
                },
                timestamp: "2026-03-15T00:00:00Z".to_string(),
            },
        ];
        let table = format_comparison(&outputs);
        assert!(table.contains("EVAL COMPARISON"));
        assert!(table.contains("bm25"));
        assert!(table.contains("adaptive"));
    }

    #[test]
    fn format_sweep_single_corpus() {
        let sweep = SweepOutput {
            corpora: vec![SweepCorpusResult {
                corpus_name: "test-corpus".to_string(),
                learning_count: 38,
                session_count: 35,
                results: vec![EvalOutput {
                    config_name: "adaptive".to_string(),
                    corpus_name: "test-corpus".to_string(),
                    metrics: compute_metrics(&[3.0, 4.0, 5.0], 3, 0),
                    judge_stats: JudgeStats {
                        total_calls: 3,
                        cache_hits: 3,
                        new_judgments: 0,
                        failures: 0,
                    },
                    timestamp: "2026-03-15T00:00:00Z".to_string(),
                }],
            }],
            configs: vec!["adaptive".to_string()],
            timestamp: "2026-03-15T00:00:00Z".to_string(),
            negative_results: None,
        };

        let report = format_sweep(&sweep);
        assert!(report.contains("MULTI-CORPUS BENCHMARK"));
        assert!(report.contains("test-corpus"));
        assert!(report.contains("38 learnings"));
        assert!(report.contains("35 sessions"));
        assert!(report.contains("adaptive"));
    }

    #[test]
    fn format_sweep_multiple_corpora() {
        let make_result = |name: &str, avg: f64| SweepCorpusResult {
            corpus_name: name.to_string(),
            learning_count: 10,
            session_count: 5,
            results: vec![EvalOutput {
                config_name: "bm25".to_string(),
                corpus_name: name.to_string(),
                metrics: compute_metrics(&[avg], 1, 0),
                judge_stats: JudgeStats {
                    total_calls: 1,
                    cache_hits: 1,
                    new_judgments: 0,
                    failures: 0,
                },
                timestamp: "2026-03-15T00:00:00Z".to_string(),
            }],
        };

        let sweep = SweepOutput {
            corpora: vec![make_result("corpus-a", 3.0), make_result("corpus-b", 4.0)],
            configs: vec!["bm25".to_string()],
            timestamp: "2026-03-15T00:00:00Z".to_string(),
            negative_results: None,
        };

        let report = format_sweep(&sweep);
        assert!(report.contains("corpus-a"));
        assert!(report.contains("corpus-b"));
    }

    #[test]
    fn format_sweep_empty_corpora() {
        let sweep = SweepOutput {
            corpora: vec![],
            configs: vec!["bm25".to_string()],
            timestamp: "2026-03-15T00:00:00Z".to_string(),
            negative_results: None,
        };

        let report = format_sweep(&sweep);
        assert!(report.contains("MULTI-CORPUS BENCHMARK"));
    }

    #[test]
    fn compute_metrics_precision_at_3() {
        // 3 of 5 scores are >= 3
        let scores = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let m = compute_metrics(&scores, 5, 0);
        assert!((m.precision_at_3 - 0.6).abs() < 0.001);
    }

    #[test]
    fn compute_metrics_precision_all_high() {
        let scores = vec![4.0, 5.0, 5.0];
        let m = compute_metrics(&scores, 3, 0);
        assert!((m.precision_at_3 - 1.0).abs() < 0.001);
    }

    #[test]
    fn compute_metrics_with_recall_data() {
        let scores = vec![3.0, 4.0, 5.0];
        let recall_data = RecallData {
            ground_truth_at_4: 10,
            ground_truth_at_5: 5,
            surfaced_at_4: 2, // scores 4.0 and 5.0
            surfaced_at_5: 1, // score 5.0
        };
        let m = compute_metrics_with_recall(&scores, 3, 0, Some(&recall_data), &[]);
        assert!((m.recall_at_4 - 0.2).abs() < 0.001); // 2/10
        assert!((m.recall_at_5 - 0.2).abs() < 0.001); // 1/5
        assert!(m.f1_at_4 > 0.0);
    }

    #[test]
    fn compute_metrics_without_recall_data() {
        let scores = vec![3.0, 4.0];
        let m = compute_metrics_with_recall(&scores, 2, 0, None, &[]);
        assert_eq!(m.recall_at_4, 0.0);
        assert_eq!(m.recall_at_5, 0.0);
        assert_eq!(m.f1_at_4, 0.0);
    }

    #[test]
    fn compute_metrics_f1_harmonic_mean() {
        // precision_at_3 = 1.0 (all scores >= 3), recall_at_4 = 0.5
        let scores = vec![4.0, 5.0];
        let recall_data = RecallData {
            ground_truth_at_4: 4,
            ground_truth_at_5: 2,
            surfaced_at_4: 2,
            surfaced_at_5: 1,
        };
        let m = compute_metrics_with_recall(&scores, 2, 0, Some(&recall_data), &[]);
        assert!((m.precision_at_3 - 1.0).abs() < 0.001);
        assert!((m.recall_at_4 - 0.5).abs() < 0.001);
        // F1 = 2 * 1.0 * 0.5 / (1.0 + 0.5) = 2/3
        assert!((m.f1_at_4 - 2.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn format_comparison_includes_precision() {
        let outputs = vec![EvalOutput {
            config_name: "adaptive".to_string(),
            corpus_name: "test".to_string(),
            metrics: compute_metrics(&[3.0, 4.0, 5.0], 3, 0),
            judge_stats: JudgeStats {
                total_calls: 3,
                cache_hits: 0,
                new_judgments: 3,
                failures: 0,
            },
            timestamp: "2026-03-16T00:00:00Z".to_string(),
        }];
        let table = format_comparison(&outputs);
        assert!(table.contains("P@3"), "Should contain P@3 column: {table}");
    }

    // =========================================================================
    // Per-session precision tests
    // =========================================================================

    #[test]
    fn per_session_precision_known_inputs() {
        // Session 1: top-3 = [5.0, 2.0, 4.0] → 2/3 relevant (>=3)
        // Session 2: top-3 = [3.0, 3.0, 1.0] → 2/3 relevant
        let sessions = vec![vec![5.0, 2.0, 4.0, 1.0], vec![3.0, 3.0, 1.0]];
        let p = compute_per_session_precision(&sessions, 3, 3.0);
        // Both sessions: 2/3 each → mean = 2/3
        assert!((p - 2.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn per_session_precision_reorder_changes_result() {
        // Original order: top-3 = [1.0, 2.0, 5.0] → 1/3 relevant
        let session_a = vec![vec![1.0, 2.0, 5.0, 4.0]];
        let pa = compute_per_session_precision(&session_a, 3, 3.0);

        // Reranked order: top-3 = [5.0, 4.0, 1.0] → 2/3 relevant
        let session_b = vec![vec![5.0, 4.0, 1.0, 2.0]];
        let pb = compute_per_session_precision(&session_b, 3, 3.0);

        assert!((pa - 1.0 / 3.0).abs() < 0.001);
        assert!((pb - 2.0 / 3.0).abs() < 0.001);
        assert!(
            (pb - pa).abs() > 0.1,
            "Reordering should change per-session P@3"
        );
    }

    #[test]
    fn per_session_precision_all_relevant() {
        let sessions = vec![vec![5.0, 4.0, 3.0]];
        let p = compute_per_session_precision(&sessions, 3, 3.0);
        assert!((p - 1.0).abs() < 0.001);
    }

    #[test]
    fn per_session_precision_empty_sessions() {
        let p = compute_per_session_precision(&[], 3, 3.0);
        assert_eq!(p, 0.0);
    }

    #[test]
    fn per_session_precision_fewer_than_k() {
        // Session has only 2 items, k=3 → denominator is 2
        // Both >= 3 → precision = 1.0
        let sessions = vec![vec![4.0, 5.0]];
        let p = compute_per_session_precision(&sessions, 3, 3.0);
        assert!((p - 1.0).abs() < 0.001);
    }

    #[test]
    fn per_session_precision_single_empty_session_skipped() {
        // Empty session is skipped; only the non-empty one counts
        let sessions = vec![vec![], vec![5.0, 4.0, 3.0]];
        let p = compute_per_session_precision(&sessions, 3, 3.0);
        assert!((p - 1.0).abs() < 0.001);
    }

    #[test]
    fn format_sweep_includes_recall_when_present() {
        let recall_scores = vec![3.0, 4.0, 5.0];
        let recall_data = RecallData {
            ground_truth_at_4: 10,
            ground_truth_at_5: 5,
            surfaced_at_4: 2,
            surfaced_at_5: 1,
        };
        let metrics_with_recall =
            compute_metrics_with_recall(&recall_scores, 3, 0, Some(&recall_data), &[]);

        let sweep = SweepOutput {
            corpora: vec![SweepCorpusResult {
                corpus_name: "test-corpus".to_string(),
                learning_count: 38,
                session_count: 35,
                results: vec![EvalOutput {
                    config_name: "adaptive".to_string(),
                    corpus_name: "test-corpus".to_string(),
                    metrics: metrics_with_recall,
                    judge_stats: JudgeStats {
                        total_calls: 3,
                        cache_hits: 3,
                        new_judgments: 0,
                        failures: 0,
                    },
                    timestamp: "2026-03-17T00:00:00Z".to_string(),
                }],
            }],
            configs: vec!["adaptive".to_string()],
            timestamp: "2026-03-17T00:00:00Z".to_string(),
            negative_results: None,
        };

        let report = format_sweep(&sweep);
        assert!(
            report.contains("R@4"),
            "Should contain R@4 column: {report}"
        );
        assert!(report.contains("F1"), "Should contain F1 column: {report}");
    }

    #[test]
    fn bootstrap_ci_known_mean() {
        let data: Vec<f64> = (1..=5).flat_map(|v| vec![v as f64; 20]).collect();
        let ci = bootstrap_ci(
            &data,
            |s| s.iter().sum::<f64>() / s.len() as f64,
            2000,
            0.05,
        );
        let ci = ci.expect("Should produce CI for 100 samples");
        assert!(
            ci.lower < 3.0 && ci.upper > 3.0,
            "95% CI [{:.2}, {:.2}] should contain true mean 3.0",
            ci.lower,
            ci.upper
        );
        assert!(
            ci.upper - ci.lower < 1.0,
            "CI width {:.2} should be reasonably narrow",
            ci.upper - ci.lower
        );
    }

    #[test]
    fn bootstrap_ci_too_few_samples() {
        assert!(bootstrap_ci(&[3.0], |s| s[0], 1000, 0.05).is_none());
        assert!(bootstrap_ci(&[], |_| 0.0, 1000, 0.05).is_none());
    }

    #[test]
    fn bootstrap_session_cis_basic() {
        let sessions = vec![
            vec![4.0, 5.0, 3.0],
            vec![3.0, 4.0],
            vec![5.0, 4.0, 4.0],
            vec![2.0, 3.0, 4.0],
            vec![4.0, 5.0],
        ];
        let (ci_avg, ci_prec, _, _) = bootstrap_session_cis(&sessions, None, 2000, 0.05);
        assert!(
            ci_avg.is_some(),
            "Should produce CI for avg with 5 sessions"
        );
        assert!(
            ci_prec.is_some(),
            "Should produce CI for precision with 5 sessions"
        );
    }

    #[test]
    fn bootstrap_session_cis_too_few() {
        let sessions = vec![vec![4.0, 5.0]];
        let (ci_avg, ci_prec, ci_recall, ci_f1) =
            bootstrap_session_cis(&sessions, None, 1000, 0.05);
        assert!(ci_avg.is_none());
        assert!(ci_prec.is_none());
        assert!(ci_recall.is_none());
        assert!(ci_f1.is_none());
    }

    #[test]
    fn metrics_ci_fields_none_by_default() {
        let m = compute_metrics(&[3.0, 4.0], 2, 0);
        assert!(m.ci_avg_relevance.is_none());
        assert!(m.ci_precision_at_3.is_none());
        assert!(m.ci_recall_at_4.is_none());
        assert!(m.ci_f1_at_4.is_none());
    }

    #[test]
    fn compute_metrics_with_ci_populates_intervals() {
        let session_scores = vec![
            vec![4.0, 5.0, 3.0],
            vec![3.0, 4.0],
            vec![5.0, 4.0, 4.0],
            vec![2.0, 3.0, 4.0],
            vec![4.0, 5.0],
        ];
        let all_scores: Vec<f64> = session_scores.iter().flatten().copied().collect();
        let m = compute_metrics_with_ci(&all_scores, 5, 0, None, &session_scores, 1000);
        assert!(
            m.ci_avg_relevance.is_some(),
            "Should have CI for avg_relevance"
        );
        assert!(
            m.ci_precision_at_3.is_some(),
            "Should have CI for precision_at_3"
        );
    }

    #[test]
    fn compute_metrics_with_ci_zero_bootstrap_no_intervals() {
        let session_scores = vec![vec![4.0, 5.0, 3.0], vec![3.0, 4.0]];
        let all_scores: Vec<f64> = session_scores.iter().flatten().copied().collect();
        let m = compute_metrics_with_ci(&all_scores, 2, 0, None, &session_scores, 0);
        assert!(m.ci_avg_relevance.is_none());
    }

    #[test]
    fn bootstrap_session_cis_with_recall_data() {
        let sessions = vec![
            vec![4.0, 5.0, 3.0],
            vec![3.0, 4.0],
            vec![5.0, 4.0, 4.0],
            vec![2.0, 3.0, 4.0],
            vec![4.0, 5.0],
        ];
        let recall_data = RecallData {
            ground_truth_at_4: 10,
            ground_truth_at_5: 5,
            surfaced_at_4: 7,
            surfaced_at_5: 3,
        };
        let (ci_avg, ci_prec, ci_recall, ci_f1) =
            bootstrap_session_cis(&sessions, Some(&recall_data), 2000, 0.05);
        assert!(ci_avg.is_some(), "Should produce CI for avg");
        assert!(ci_prec.is_some(), "Should produce CI for precision");
        assert!(
            ci_recall.is_some(),
            "Should produce CI for recall when RecallData provided"
        );
        assert!(
            ci_f1.is_some(),
            "Should produce CI for F1 when RecallData provided"
        );
        let ci_r = ci_recall.unwrap();
        assert!(ci_r.lower > 0.0, "Recall CI lower should be positive");
    }

    #[test]
    fn confidence_interval_serializes() {
        let ci = ConfidenceInterval {
            lower: 2.5,
            upper: 3.5,
        };
        let json = serde_json::to_string(&ci).unwrap();
        assert!(json.contains("2.5"));
        assert!(json.contains("3.5"));
    }

    #[test]
    fn format_sweep_omits_recall_when_absent() {
        let sweep = SweepOutput {
            corpora: vec![SweepCorpusResult {
                corpus_name: "test-corpus".to_string(),
                learning_count: 10,
                session_count: 5,
                results: vec![EvalOutput {
                    config_name: "bm25".to_string(),
                    corpus_name: "test-corpus".to_string(),
                    metrics: compute_metrics(&[3.0, 4.0], 2, 0),
                    judge_stats: JudgeStats {
                        total_calls: 2,
                        cache_hits: 2,
                        new_judgments: 0,
                        failures: 0,
                    },
                    timestamp: "2026-03-17T00:00:00Z".to_string(),
                }],
            }],
            configs: vec!["bm25".to_string()],
            timestamp: "2026-03-17T00:00:00Z".to_string(),
            negative_results: None,
        };

        let report = format_sweep(&sweep);
        assert!(
            !report.contains("R@4"),
            "Should NOT contain R@4 column when no recall data: {report}"
        );
    }

    // =========================================================================
    // Formatting value-correctness tests (verify numbers, not just headers)
    // =========================================================================

    #[test]
    fn format_sweep_recall_values_correct() {
        let recall_data = RecallData {
            ground_truth_at_4: 10,
            ground_truth_at_5: 5,
            surfaced_at_4: 4,
            surfaced_at_5: 2,
        };
        let session_scores = vec![vec![3.0, 4.0], vec![5.0, 4.0]];
        let scores = vec![3.0, 4.0, 5.0, 4.0];
        let metrics =
            compute_metrics_with_recall(&scores, 4, 0, Some(&recall_data), &session_scores);

        // Verify computed metrics before formatting
        assert!(
            (metrics.recall_at_4 - 0.4).abs() < 0.001,
            "recall_at_4 = {}",
            metrics.recall_at_4
        );
        assert!(
            (metrics.f1_at_4 - 0.571).abs() < 0.001,
            "f1_at_4 = {}",
            metrics.f1_at_4
        );
        assert!((metrics.precision_at_3 - 1.0).abs() < 0.001);
        assert!((metrics.precision_at_3_per_session - 1.0).abs() < 0.001);

        let sweep = SweepOutput {
            corpora: vec![SweepCorpusResult {
                corpus_name: "test".to_string(),
                learning_count: 10,
                session_count: 5,
                results: vec![EvalOutput {
                    config_name: "adaptive".to_string(),
                    corpus_name: "test".to_string(),
                    metrics,
                    judge_stats: JudgeStats {
                        total_calls: 4,
                        cache_hits: 4,
                        new_judgments: 0,
                        failures: 0,
                    },
                    timestamp: "2026-03-17T00:00:00Z".to_string(),
                }],
            }],
            configs: vec!["adaptive".to_string()],
            timestamp: "2026-03-17T00:00:00Z".to_string(),
            negative_results: None,
        };

        let report = format_sweep(&sweep);

        // Verify HEADER has recall columns
        assert!(report.contains("R@4"), "Missing R@4 header: {report}");
        assert!(report.contains("F1"), "Missing F1 header: {report}");

        // Verify actual VALUES in the data row
        assert!(report.contains("4.00"), "Missing avg 4.00: {report}");
        assert!(report.contains("0.571"), "Missing F1 value 0.571: {report}");
        assert!(report.contains("40%"), "Missing recall 40%: {report}");
        assert!(report.contains("100%"), "Missing precision 100%: {report}");
    }

    #[test]
    fn format_comparison_with_recall_values() {
        let recall_data = RecallData {
            ground_truth_at_4: 8,
            ground_truth_at_5: 4,
            surfaced_at_4: 3,
            surfaced_at_5: 1,
        };
        let session_scores = vec![vec![2.0, 3.0], vec![4.0, 5.0]];
        let scores = vec![2.0, 3.0, 4.0, 5.0];
        let metrics =
            compute_metrics_with_recall(&scores, 4, 1, Some(&recall_data), &session_scores);

        // Verify computed values
        assert!((metrics.precision_at_3 - 0.75).abs() < 0.001);
        assert!((metrics.recall_at_4 - 0.375).abs() < 0.001);
        assert!(
            (metrics.f1_at_4 - 0.5).abs() < 0.001,
            "f1 = {}",
            metrics.f1_at_4
        );
        assert!((metrics.precision_at_3_per_session - 0.75).abs() < 0.001);

        let outputs = vec![EvalOutput {
            config_name: "bm25".to_string(),
            corpus_name: "test".to_string(),
            metrics,
            judge_stats: JudgeStats {
                total_calls: 4,
                cache_hits: 0,
                new_judgments: 4,
                failures: 0,
            },
            timestamp: "2026-03-17T00:00:00Z".to_string(),
        }];

        let table = format_comparison(&outputs);

        // Verify recall-mode header
        assert!(table.contains("R@4"), "Missing R@4 header: {table}");
        assert!(table.contains("F1"), "Missing F1 header: {table}");
        assert!(table.contains("Supp"), "Missing Supp header: {table}");

        // Verify actual data values
        assert!(table.contains("3.50"), "Missing avg 3.50: {table}");
        assert!(table.contains("3.5"), "Missing median 3.5: {table}");
        assert!(table.contains("0.500"), "Missing F1 value 0.500: {table}");
        assert!(table.contains("75%"), "Missing precision 75%: {table}");
        assert!(table.contains("25%"), "Missing noise 25%: {table}");

        // Verify the data row contains the config name
        assert!(table.contains("bm25"), "Missing config name: {table}");
    }

    #[test]
    fn format_scorecard_with_cis() {
        let mut metrics = compute_metrics(&[3.0, 4.0, 5.0], 3, 0);
        metrics.ci_avg_relevance = Some(ConfidenceInterval {
            lower: 3.50,
            upper: 4.50,
        });
        metrics.ci_precision_at_3 = Some(ConfidenceInterval {
            lower: 0.80,
            upper: 0.95,
        });

        let output = EvalOutput {
            config_name: "test-config".to_string(),
            corpus_name: "test-corpus".to_string(),
            metrics,
            judge_stats: JudgeStats {
                total_calls: 3,
                cache_hits: 0,
                new_judgments: 3,
                failures: 0,
            },
            timestamp: "2026-03-17T00:00:00Z".to_string(),
        };

        let card = format_scorecard(&output);

        // Verify CI label appears
        assert!(card.contains("95% CI"), "Missing '95% CI' label: {card}");

        // Verify avg relevance CI values: format is [{:.2}, {:.2}]
        assert!(
            card.contains("[3.50, 4.50]"),
            "Missing avg CI [3.50, 4.50]: {card}"
        );

        // Verify precision CI values: format is [{:.1}%, {:.1}%]
        assert!(
            card.contains("[80.0%, 95.0%]"),
            "Missing precision CI [80.0%, 95.0%]: {card}"
        );
    }

    #[test]
    fn format_scorecard_with_recall_and_f1() {
        let recall_data = RecallData {
            ground_truth_at_4: 10,
            ground_truth_at_5: 5,
            surfaced_at_4: 5,
            surfaced_at_5: 2,
        };
        let metrics = compute_metrics_with_recall(&[4.0, 5.0, 3.0], 3, 0, Some(&recall_data), &[]);

        // Sanity check computed values
        assert!((metrics.recall_at_4 - 0.5).abs() < 0.001);
        assert!((metrics.recall_at_5 - 0.4).abs() < 0.001);
        assert!(
            (metrics.f1_at_4 - 0.6667).abs() < 0.001,
            "f1 = {}",
            metrics.f1_at_4
        );

        let output = EvalOutput {
            config_name: "adaptive".to_string(),
            corpus_name: "test".to_string(),
            metrics,
            judge_stats: JudgeStats {
                total_calls: 3,
                cache_hits: 0,
                new_judgments: 3,
                failures: 0,
            },
            timestamp: "2026-03-17T00:00:00Z".to_string(),
        };

        let card = format_scorecard(&output);

        // Verify recall section is rendered
        assert!(
            card.contains("Recall (>=4):"),
            "Missing recall@4 label: {card}"
        );
        assert!(
            card.contains("50.0%"),
            "Missing recall@4 value 50.0%: {card}"
        );
        assert!(
            card.contains("Recall (>=5):"),
            "Missing recall@5 label: {card}"
        );
        assert!(
            card.contains("40.0%"),
            "Missing recall@5 value 40.0%: {card}"
        );
        assert!(card.contains("F1 (P@3 x R@4):"), "Missing F1 label: {card}");
        assert!(card.contains("0.667"), "Missing F1 value 0.667: {card}");
    }

    #[test]
    fn eval_output_json_includes_all_fields() {
        let recall_data = RecallData {
            ground_truth_at_4: 10,
            ground_truth_at_5: 5,
            surfaced_at_4: 4,
            surfaced_at_5: 2,
        };
        let session_scores = vec![vec![4.0, 5.0, 3.0], vec![3.0, 4.0], vec![5.0, 4.0, 4.0]];
        let all_scores: Vec<f64> = session_scores.iter().flatten().copied().collect();
        let metrics =
            compute_metrics_with_ci(&all_scores, 3, 0, Some(recall_data), &session_scores, 500);

        // Precondition: CIs should be populated with 3 sessions and 500 resamples
        assert!(
            metrics.ci_avg_relevance.is_some(),
            "CI avg should be populated"
        );
        assert!(
            metrics.ci_precision_at_3.is_some(),
            "CI prec should be populated"
        );
        assert!(
            metrics.ci_recall_at_4.is_some(),
            "CI recall should be populated"
        );
        assert!(metrics.ci_f1_at_4.is_some(), "CI f1 should be populated");

        let output = EvalOutput {
            config_name: "test".to_string(),
            corpus_name: "test".to_string(),
            metrics,
            judge_stats: JudgeStats {
                total_calls: 8,
                cache_hits: 0,
                new_judgments: 8,
                failures: 0,
            },
            timestamp: "2026-03-17T00:00:00Z".to_string(),
        };

        let json = serde_json::to_string_pretty(&output).unwrap();

        // Core metric fields
        assert!(
            json.contains("\"avg_relevance\""),
            "Missing avg_relevance: {json}"
        );
        assert!(
            json.contains("\"median_relevance\""),
            "Missing median_relevance: {json}"
        );
        assert!(json.contains("\"noise_pct\""), "Missing noise_pct: {json}");
        assert!(
            json.contains("\"precision_at_3\""),
            "Missing precision_at_3: {json}"
        );
        assert!(
            json.contains("\"precision_at_3_per_session\""),
            "Missing precision_at_3_per_session: {json}"
        );
        assert!(
            json.contains("\"recall_at_4\""),
            "Missing recall_at_4: {json}"
        );
        assert!(
            json.contains("\"recall_at_5\""),
            "Missing recall_at_5: {json}"
        );
        assert!(json.contains("\"f1_at_4\""), "Missing f1_at_4: {json}");

        // CI fields (should NOT be skipped when Some)
        assert!(
            json.contains("\"ci_avg_relevance\""),
            "Missing ci_avg_relevance: {json}"
        );
        assert!(
            json.contains("\"ci_precision_at_3\""),
            "Missing ci_precision_at_3: {json}"
        );
        assert!(
            json.contains("\"ci_recall_at_4\""),
            "Missing ci_recall_at_4: {json}"
        );
        assert!(
            json.contains("\"ci_f1_at_4\""),
            "Missing ci_f1_at_4: {json}"
        );

        // CI inner structure
        assert!(json.contains("\"lower\""), "Missing CI lower field: {json}");
        assert!(json.contains("\"upper\""), "Missing CI upper field: {json}");

        // Judge stats
        assert!(
            json.contains("\"total_calls\""),
            "Missing total_calls: {json}"
        );
        assert!(
            json.contains("\"cache_hits\""),
            "Missing cache_hits: {json}"
        );
    }

    #[test]
    fn format_sweep_mixed_recall_across_corpora() {
        let recall_data = RecallData {
            ground_truth_at_4: 10,
            ground_truth_at_5: 5,
            surfaced_at_4: 3,
            surfaced_at_5: 1,
        };
        let with_recall =
            compute_metrics_with_recall(&[3.0, 4.0, 5.0], 3, 0, Some(&recall_data), &[]);
        let without_recall = compute_metrics(&[3.0, 4.0], 2, 0);

        let sweep = SweepOutput {
            corpora: vec![
                SweepCorpusResult {
                    corpus_name: "has-recall".to_string(),
                    learning_count: 10,
                    session_count: 5,
                    results: vec![EvalOutput {
                        config_name: "adaptive".to_string(),
                        corpus_name: "has-recall".to_string(),
                        metrics: with_recall,
                        judge_stats: JudgeStats {
                            total_calls: 3,
                            cache_hits: 3,
                            new_judgments: 0,
                            failures: 0,
                        },
                        timestamp: "2026-03-17T00:00:00Z".to_string(),
                    }],
                },
                SweepCorpusResult {
                    corpus_name: "no-recall".to_string(),
                    learning_count: 5,
                    session_count: 3,
                    results: vec![EvalOutput {
                        config_name: "adaptive".to_string(),
                        corpus_name: "no-recall".to_string(),
                        metrics: without_recall,
                        judge_stats: JudgeStats {
                            total_calls: 2,
                            cache_hits: 2,
                            new_judgments: 0,
                            failures: 0,
                        },
                        timestamp: "2026-03-17T00:00:00Z".to_string(),
                    }],
                },
            ],
            configs: vec!["adaptive".to_string()],
            timestamp: "2026-03-17T00:00:00Z".to_string(),
            negative_results: None,
        };

        let report = format_sweep(&sweep);

        // Split report at corpus boundaries
        let sections: Vec<&str> = report.split("Corpus:").collect();
        assert!(
            sections.len() >= 3,
            "Should have 2 corpus sections: {report}"
        );

        let recall_section = sections[1];
        // Trim the trailing key section from the last corpus
        let no_recall_table = sections[2].split("Key:").next().unwrap_or(sections[2]);

        // First corpus should have R@4/F1 columns
        assert!(
            recall_section.contains("R@4"),
            "Recall corpus should have R@4 header: {recall_section}"
        );
        assert!(
            recall_section.contains("F1"),
            "Recall corpus should have F1 header: {recall_section}"
        );
        assert!(
            recall_section.contains("0.462"),
            "Recall corpus should have F1 value ~0.462: {recall_section}"
        );

        // Second corpus table should NOT have R@4/F1 columns
        assert!(
            !no_recall_table.contains("R@4"),
            "No-recall corpus table should NOT have R@4: {no_recall_table}"
        );
        // Should use wider "Median" header (not abbreviated "Med")
        assert!(
            no_recall_table.contains("Median"),
            "No-recall corpus should use 'Median' header: {no_recall_table}"
        );

        // Key section should exist with recall terms since one corpus has recall
        assert!(
            report.contains("Key:"),
            "Report should contain metric key: {report}"
        );
    }

    #[test]
    fn sweep_output_json_completeness() {
        let recall_data = RecallData {
            ground_truth_at_4: 10,
            ground_truth_at_5: 5,
            surfaced_at_4: 3,
            surfaced_at_5: 1,
        };
        let metrics = compute_metrics_with_recall(&[3.0, 4.0, 5.0], 3, 0, Some(&recall_data), &[]);

        let sweep = SweepOutput {
            corpora: vec![SweepCorpusResult {
                corpus_name: "test-corpus".to_string(),
                learning_count: 10,
                session_count: 5,
                results: vec![EvalOutput {
                    config_name: "bm25".to_string(),
                    corpus_name: "test-corpus".to_string(),
                    metrics,
                    judge_stats: JudgeStats {
                        total_calls: 3,
                        cache_hits: 0,
                        new_judgments: 3,
                        failures: 0,
                    },
                    timestamp: "2026-03-17T00:00:00Z".to_string(),
                }],
            }],
            configs: vec!["bm25".to_string()],
            timestamp: "2026-03-17T00:00:00Z".to_string(),
            negative_results: None,
        };

        let json = serde_json::to_string(&sweep).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Top-level structure
        assert!(parsed["corpora"].is_array(), "Missing corpora array");
        assert!(parsed["configs"].is_array(), "Missing configs array");
        assert!(parsed["timestamp"].is_string(), "Missing timestamp");

        // Corpus-level structure
        let corpus = &parsed["corpora"][0];
        assert_eq!(corpus["corpus_name"].as_str(), Some("test-corpus"));
        assert_eq!(corpus["learning_count"].as_u64(), Some(10));
        assert_eq!(corpus["session_count"].as_u64(), Some(5));

        // Result metrics within corpus
        let result_metrics = &corpus["results"][0]["metrics"];
        assert!(
            result_metrics["avg_relevance"].is_number(),
            "Missing avg_relevance"
        );
        assert!(
            result_metrics["median_relevance"].is_number(),
            "Missing median_relevance"
        );
        assert!(result_metrics["noise_pct"].is_number(), "Missing noise_pct");
        assert!(
            result_metrics["precision_at_3"].is_number(),
            "Missing precision_at_3"
        );
        assert!(
            result_metrics["precision_at_3_per_session"].is_number(),
            "Missing precision_at_3_per_session"
        );
        assert!(
            result_metrics["recall_at_4"].is_number(),
            "Missing recall_at_4"
        );
        assert!(
            result_metrics["recall_at_5"].is_number(),
            "Missing recall_at_5"
        );
        assert!(result_metrics["f1_at_4"].is_number(), "Missing f1_at_4");
        assert!(
            result_metrics["pairs_judged"].is_number(),
            "Missing pairs_judged"
        );
        assert!(
            result_metrics["score_distribution"].is_array(),
            "Missing score_distribution"
        );

        // Verify actual recall value is correct (3/10 = 0.3)
        let recall_val = result_metrics["recall_at_4"].as_f64().unwrap();
        assert!(
            (recall_val - 0.3).abs() < 0.001,
            "recall_at_4 should be 0.3, got {recall_val}"
        );

        // Judge stats structure
        let judge_stats = &corpus["results"][0]["judge_stats"];
        assert!(
            judge_stats["total_calls"].is_number(),
            "Missing total_calls"
        );
        assert!(judge_stats["cache_hits"].is_number(), "Missing cache_hits");
        assert!(
            judge_stats["new_judgments"].is_number(),
            "Missing new_judgments"
        );
        assert!(judge_stats["failures"].is_number(), "Missing failures");

        // CIs should be absent (no bootstrap)
        assert!(
            result_metrics.get("ci_avg_relevance").is_none()
                || result_metrics["ci_avg_relevance"].is_null(),
            "CIs should be absent without bootstrap"
        );

        // New metrics fields
        assert!(
            result_metrics["sessions_total"].is_number(),
            "Missing sessions_total"
        );
        assert!(result_metrics["coverage"].is_number(), "Missing coverage");
        assert!(
            result_metrics["pairs_per_session_min"].is_number(),
            "Missing pairs_per_session_min"
        );
        assert!(
            result_metrics["pairs_per_session_mean"].is_number(),
            "Missing pairs_per_session_mean"
        );
        assert!(
            result_metrics["pairs_per_session_max"].is_number(),
            "Missing pairs_per_session_max"
        );
        assert!(result_metrics["mrr_at_4"].is_number(), "Missing mrr_at_4");
    }

    // =========================================================================
    // Coverage, pairs-per-session, and MRR tests
    // =========================================================================

    #[test]
    fn compute_mrr_known_inputs() {
        // Session 1: [5.0, 2.0, 3.0] → first >=4 at position 0, RR = 1.0
        // Session 2: [2.0, 3.0, 4.0] → first >=4 at position 2, RR = 1/3
        // Session 3: [1.0, 2.0, 3.0] → no >=4, RR = 0.0
        let sessions = vec![
            vec![5.0, 2.0, 3.0],
            vec![2.0, 3.0, 4.0],
            vec![1.0, 2.0, 3.0],
        ];
        let mrr = compute_mrr(&sessions, 4.0);
        // (1.0 + 1/3 + 0.0) / 3 = 4/9 ≈ 0.444
        assert!(
            (mrr - 4.0 / 9.0).abs() < 0.001,
            "MRR should be ~0.444, got {mrr}"
        );
    }

    #[test]
    fn compute_mrr_all_relevant_at_top() {
        let sessions = vec![vec![4.0, 2.0], vec![5.0, 1.0]];
        let mrr = compute_mrr(&sessions, 4.0);
        // Both first position → RR = 1.0 each → MRR = 1.0
        assert!((mrr - 1.0).abs() < 0.001);
    }

    #[test]
    fn compute_mrr_no_relevant() {
        let sessions = vec![vec![1.0, 2.0, 3.0], vec![2.0, 3.0]];
        let mrr = compute_mrr(&sessions, 4.0);
        assert_eq!(mrr, 0.0);
    }

    #[test]
    fn compute_mrr_empty() {
        assert_eq!(compute_mrr(&[], 4.0), 0.0);
    }

    #[test]
    fn compute_mrr_skips_empty_sessions() {
        let sessions = vec![vec![], vec![4.0, 5.0]];
        let mrr = compute_mrr(&sessions, 4.0);
        // Only 1 non-empty session, first >=4 at position 0, RR = 1.0
        assert!((mrr - 1.0).abs() < 0.001);
    }

    #[test]
    fn compute_pairs_per_session_known() {
        let sessions = vec![vec![1.0, 2.0], vec![3.0], vec![4.0, 5.0, 3.0]];
        let (min, mean, max) = compute_pairs_per_session(&sessions);
        assert_eq!(min, 1);
        assert_eq!(max, 3);
        assert!((mean - 2.0).abs() < 0.001); // (2+1+3)/3
    }

    #[test]
    fn compute_pairs_per_session_empty() {
        let (min, mean, max) = compute_pairs_per_session(&[]);
        assert_eq!(min, 0);
        assert_eq!(max, 0);
        assert_eq!(mean, 0.0);
    }

    #[test]
    fn compute_pairs_per_session_single() {
        let sessions = vec![vec![4.0, 5.0]];
        let (min, mean, max) = compute_pairs_per_session(&sessions);
        assert_eq!(min, 2);
        assert_eq!(max, 2);
        assert!((mean - 2.0).abs() < 0.001);
    }

    #[test]
    fn format_scorecard_shows_coverage_and_mrr() {
        let mut metrics = compute_metrics(&[4.0, 5.0, 3.0], 3, 0);
        metrics.sessions_total = 10;
        metrics.coverage = 0.3; // 3/10 sessions
        metrics.pairs_per_session_min = 1;
        metrics.pairs_per_session_mean = 2.0;
        metrics.pairs_per_session_max = 3;
        metrics.mrr_at_4 = 0.750;

        let output = EvalOutput {
            config_name: "adaptive".to_string(),
            corpus_name: "test".to_string(),
            metrics,
            judge_stats: JudgeStats {
                total_calls: 3,
                cache_hits: 0,
                new_judgments: 3,
                failures: 0,
            },
            timestamp: "2026-03-18T00:00:00Z".to_string(),
        };

        let card = format_scorecard(&output);
        assert!(card.contains("Coverage:"), "Missing Coverage label: {card}");
        assert!(card.contains("30%"), "Missing coverage 30%: {card}");
        assert!(card.contains("3/10"), "Missing 3/10: {card}");
        assert!(
            card.contains("Pairs per session:"),
            "Missing pairs/session label: {card}"
        );
        assert!(card.contains("1 / 2.0 / 3"), "Missing min/mean/max: {card}");
        assert!(card.contains("MRR@4:"), "Missing MRR label: {card}");
        assert!(card.contains("0.750"), "Missing MRR value: {card}");
    }

    #[test]
    fn format_comparison_shows_cov_and_mrr() {
        let mut metrics = compute_metrics(&[4.0, 5.0], 2, 0);
        metrics.coverage = 0.8;
        metrics.mrr_at_4 = 0.500;

        let outputs = vec![EvalOutput {
            config_name: "bm25".to_string(),
            corpus_name: "test".to_string(),
            metrics,
            judge_stats: JudgeStats {
                total_calls: 2,
                cache_hits: 0,
                new_judgments: 2,
                failures: 0,
            },
            timestamp: "2026-03-18T00:00:00Z".to_string(),
        }];

        let table = format_comparison(&outputs);
        assert!(table.contains("Cov%"), "Missing Cov% header: {table}");
        assert!(table.contains("MRR"), "Missing MRR header: {table}");
        assert!(table.contains("80%"), "Missing coverage value 80%: {table}");
        assert!(table.contains("0.500"), "Missing MRR value 0.500: {table}");
    }

    #[test]
    fn format_sweep_shows_cov_and_mrr() {
        let mut metrics = compute_metrics(&[3.0, 4.0, 5.0], 3, 0);
        metrics.coverage = 0.6;
        metrics.mrr_at_4 = 0.833;

        let sweep = SweepOutput {
            corpora: vec![SweepCorpusResult {
                corpus_name: "test".to_string(),
                learning_count: 10,
                session_count: 5,
                results: vec![EvalOutput {
                    config_name: "adaptive".to_string(),
                    corpus_name: "test".to_string(),
                    metrics,
                    judge_stats: JudgeStats {
                        total_calls: 3,
                        cache_hits: 3,
                        new_judgments: 0,
                        failures: 0,
                    },
                    timestamp: "2026-03-18T00:00:00Z".to_string(),
                }],
            }],
            configs: vec!["adaptive".to_string()],
            timestamp: "2026-03-18T00:00:00Z".to_string(),
            negative_results: None,
        };

        let report = format_sweep(&sweep);
        assert!(report.contains("Cov%"), "Missing Cov% header: {report}");
        assert!(report.contains("MRR"), "Missing MRR header: {report}");
        assert!(
            report.contains("60%"),
            "Missing coverage value 60%: {report}"
        );
        assert!(
            report.contains("0.833"),
            "Missing MRR value 0.833: {report}"
        );
    }

    #[test]
    fn metric_key_includes_cov_and_mrr() {
        let mut lines = Vec::new();
        append_metric_key(&mut lines, false);
        let key = lines.join("\n");
        assert!(key.contains("Cov%"), "Key missing Cov%: {key}");
        assert!(key.contains("MRR"), "Key missing MRR: {key}");
    }

    #[test]
    fn format_comparison_shows_bootstrap_cis() {
        let mut metrics = compute_metrics(&[3.0, 4.0, 5.0], 3, 0);
        metrics.ci_avg_relevance = Some(ConfidenceInterval {
            lower: 3.50,
            upper: 4.50,
        });
        metrics.ci_precision_at_3 = Some(ConfidenceInterval {
            lower: 0.90,
            upper: 0.99,
        });

        let outputs = vec![EvalOutput {
            config_name: "bm25".to_string(),
            corpus_name: "test".to_string(),
            metrics,
            judge_stats: JudgeStats {
                total_calls: 3,
                cache_hits: 0,
                new_judgments: 3,
                failures: 0,
            },
            timestamp: "2026-03-18T00:00:00Z".to_string(),
        }];

        let table = format_comparison(&outputs);
        assert!(
            table.contains("Bootstrap 95% CIs:"),
            "Missing CI header: {table}"
        );
        assert!(
            table.contains("Avg [3.50, 4.50]"),
            "Missing Avg CI: {table}"
        );
        assert!(
            table.contains("P@3g [90.0%, 99.0%]"),
            "Missing P@3g CI: {table}"
        );
    }

    #[test]
    fn format_comparison_omits_ci_section_when_none() {
        let metrics = compute_metrics(&[3.0, 4.0], 2, 0);
        let outputs = vec![EvalOutput {
            config_name: "bm25".to_string(),
            corpus_name: "test".to_string(),
            metrics,
            judge_stats: JudgeStats {
                total_calls: 2,
                cache_hits: 0,
                new_judgments: 2,
                failures: 0,
            },
            timestamp: "2026-03-18T00:00:00Z".to_string(),
        }];

        let table = format_comparison(&outputs);
        assert!(
            !table.contains("Bootstrap 95% CIs:"),
            "Should not show CI section when no CIs: {table}"
        );
    }

    #[test]
    fn format_sweep_shows_bootstrap_cis() {
        let mut metrics = compute_metrics(&[3.0, 4.0, 5.0], 3, 0);
        metrics.ci_avg_relevance = Some(ConfidenceInterval {
            lower: 3.80,
            upper: 4.20,
        });
        metrics.ci_precision_at_3 = Some(ConfidenceInterval {
            lower: 0.95,
            upper: 1.00,
        });

        let sweep = SweepOutput {
            corpora: vec![SweepCorpusResult {
                corpus_name: "test".to_string(),
                learning_count: 10,
                session_count: 5,
                results: vec![EvalOutput {
                    config_name: "adaptive".to_string(),
                    corpus_name: "test".to_string(),
                    metrics,
                    judge_stats: JudgeStats {
                        total_calls: 3,
                        cache_hits: 3,
                        new_judgments: 0,
                        failures: 0,
                    },
                    timestamp: "2026-03-18T00:00:00Z".to_string(),
                }],
            }],
            configs: vec!["adaptive".to_string()],
            timestamp: "2026-03-18T00:00:00Z".to_string(),
            negative_results: None,
        };

        let report = format_sweep(&sweep);
        assert!(
            report.contains("Bootstrap 95% CIs:"),
            "Missing CI header: {report}"
        );
        assert!(
            report.contains("Avg [3.80, 4.20]"),
            "Missing Avg CI: {report}"
        );
        assert!(
            report.contains("P@3g [95.0%, 100.0%]"),
            "Missing P@3g CI: {report}"
        );
    }

    #[test]
    fn format_sweep_shows_recall_cis() {
        let mut metrics = compute_metrics(&[3.0, 4.0, 5.0], 3, 0);
        metrics.recall_at_4 = 0.8;
        metrics.f1_at_4 = 0.85;
        metrics.ci_avg_relevance = Some(ConfidenceInterval {
            lower: 3.50,
            upper: 4.50,
        });
        metrics.ci_recall_at_4 = Some(ConfidenceInterval {
            lower: 0.70,
            upper: 0.90,
        });
        metrics.ci_f1_at_4 = Some(ConfidenceInterval {
            lower: 0.80,
            upper: 0.90,
        });

        let sweep = SweepOutput {
            corpora: vec![SweepCorpusResult {
                corpus_name: "test".to_string(),
                learning_count: 10,
                session_count: 5,
                results: vec![EvalOutput {
                    config_name: "bm25".to_string(),
                    corpus_name: "test".to_string(),
                    metrics,
                    judge_stats: JudgeStats {
                        total_calls: 3,
                        cache_hits: 0,
                        new_judgments: 3,
                        failures: 0,
                    },
                    timestamp: "2026-03-18T00:00:00Z".to_string(),
                }],
            }],
            configs: vec!["bm25".to_string()],
            timestamp: "2026-03-18T00:00:00Z".to_string(),
            negative_results: None,
        };

        let report = format_sweep(&sweep);
        assert!(
            report.contains("R@4 [70.0%, 90.0%]"),
            "Missing R@4 CI: {report}"
        );
        assert!(
            report.contains("F1 [0.800, 0.900]"),
            "Missing F1 CI: {report}"
        );
    }

    #[test]
    fn format_ci_parts_empty_when_no_cis() {
        let metrics = compute_metrics(&[3.0], 1, 0);
        let parts = format_ci_parts(&metrics);
        assert!(parts.is_empty(), "Should be empty: {parts}");
    }

    #[test]
    fn format_ci_parts_all_fields() {
        let mut metrics = compute_metrics(&[3.0], 1, 0);
        metrics.ci_avg_relevance = Some(ConfidenceInterval {
            lower: 2.5,
            upper: 3.5,
        });
        metrics.ci_precision_at_3 = Some(ConfidenceInterval {
            lower: 0.80,
            upper: 0.95,
        });
        metrics.ci_recall_at_4 = Some(ConfidenceInterval {
            lower: 0.60,
            upper: 0.75,
        });
        metrics.ci_f1_at_4 = Some(ConfidenceInterval {
            lower: 0.70,
            upper: 0.85,
        });

        let parts = format_ci_parts(&metrics);
        assert!(parts.contains("Avg [2.50, 3.50]"), "Missing Avg: {parts}");
        assert!(
            parts.contains("P@3g [80.0%, 95.0%]"),
            "Missing P@3g: {parts}"
        );
        assert!(parts.contains("R@4 [60.0%, 75.0%]"), "Missing R@4: {parts}");
        assert!(parts.contains("F1 [0.700, 0.850]"), "Missing F1: {parts}");
    }

    // ---- Negative sweep tests ----

    fn make_negative_pair(
        learnings_from: &str,
        sessions_from: &str,
        scores: &[f64],
    ) -> NegativePairResult {
        NegativePairResult {
            learnings_from: learnings_from.to_string(),
            sessions_from: sessions_from.to_string(),
            eval_output: EvalOutput {
                config_name: "boosted-adaptive".to_string(),
                corpus_name: format!("{}-x-{}", learnings_from, sessions_from),
                metrics: compute_metrics(scores, scores.len(), 0),
                judge_stats: JudgeStats {
                    total_calls: scores.len(),
                    cache_hits: 0,
                    new_judgments: scores.len(),
                    failures: 0,
                },
                timestamp: "2026-03-18T00:00:00Z".to_string(),
            },
        }
    }

    #[test]
    fn negative_sweep_output_overall_aggregation() {
        // Pair 1: 2 pairs scored [1.0, 2.0] -> mean 1.5, fpr@3=0, fpr@4=0
        // Pair 2: 3 pairs scored [1.0, 3.0, 4.0] -> fpr@3=2/3, fpr@4=1/3
        // Overall: 5 pairs, mean=(1+2+1+3+4)/5=2.2, fpr@3=2/5=0.4, fpr@4=1/5=0.2
        let all_scores: Vec<f64> = vec![1.0, 2.0, 1.0, 3.0, 4.0];
        let total = all_scores.len();
        let ge3 = all_scores.iter().filter(|&&s| s >= 3.0).count();
        let ge4 = all_scores.iter().filter(|&&s| s >= 4.0).count();

        let output = NegativeSweepOutput {
            config_name: "boosted-adaptive".to_string(),
            pairs: vec![
                make_negative_pair("alpha", "beta", &[1.0, 2.0]),
                make_negative_pair("beta", "alpha", &[1.0, 3.0, 4.0]),
            ],
            overall_mean_score: all_scores.iter().sum::<f64>() / total as f64,
            overall_fpr_at_3: ge3 as f64 / total as f64,
            overall_fpr_at_4: ge4 as f64 / total as f64,
        };

        assert!((output.overall_mean_score - 2.2).abs() < 0.01);
        assert!((output.overall_fpr_at_3 - 0.4).abs() < 0.01);
        assert!((output.overall_fpr_at_4 - 0.2).abs() < 0.01);
    }

    #[test]
    fn negative_sweep_output_serializes() {
        let output = NegativeSweepOutput {
            config_name: "boosted-adaptive".to_string(),
            pairs: vec![make_negative_pair("a", "b", &[1.0, 2.0])],
            overall_mean_score: 1.5,
            overall_fpr_at_3: 0.0,
            overall_fpr_at_4: 0.0,
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("boosted-adaptive"));
        assert!(json.contains("overall_mean_score"));
        assert!(json.contains("overall_fpr_at_3"));
        assert!(json.contains("overall_fpr_at_4"));
    }

    #[test]
    fn format_negative_sweep_table_structure() {
        let output = NegativeSweepOutput {
            config_name: "boosted-adaptive".to_string(),
            pairs: vec![make_negative_pair("alpha", "beta", &[1.0, 2.0])],
            overall_mean_score: 1.5,
            overall_fpr_at_3: 0.0,
            overall_fpr_at_4: 0.0,
        };

        let formatted = format_negative_sweep(&output);
        assert!(formatted.contains("CROSS-CORPUS NEGATIVES"));
        assert!(formatted.contains("Pair"));
        assert!(formatted.contains("FPR@3"));
        assert!(formatted.contains("FPR@4"));
        assert!(formatted.contains("OVERALL"));
        assert!(formatted.contains("Key:"));
    }

    #[test]
    fn format_negative_sweep_empty_pairs() {
        let output = NegativeSweepOutput {
            config_name: "boosted-adaptive".to_string(),
            pairs: vec![],
            overall_mean_score: 0.0,
            overall_fpr_at_3: 0.0,
            overall_fpr_at_4: 0.0,
        };

        let formatted = format_negative_sweep(&output);
        assert!(formatted.contains("No cross-corpus pairs evaluated"));
    }

    #[test]
    fn format_negative_sweep_values() {
        let output = NegativeSweepOutput {
            config_name: "boosted-adaptive".to_string(),
            pairs: vec![
                make_negative_pair("alpha", "beta", &[1.0, 1.0, 2.0]),
                make_negative_pair("beta", "alpha", &[1.0, 3.0]),
            ],
            overall_mean_score: 1.6,
            overall_fpr_at_3: 0.2,
            overall_fpr_at_4: 0.0,
        };

        let formatted = format_negative_sweep(&output);
        assert!(
            formatted.contains("alpha -> beta"),
            "Should show pair names: {formatted}"
        );
        assert!(
            formatted.contains("beta -> alpha"),
            "Should show pair names: {formatted}"
        );
        assert!(formatted.contains("1.60"), "Should show overall mean");
    }

    #[test]
    fn sweep_output_negative_none_by_default() {
        let output = SweepOutput {
            corpora: vec![],
            configs: vec!["bm25".to_string()],
            timestamp: "2026-03-18T00:00:00Z".to_string(),
            negative_results: None,
        };
        assert!(output.negative_results.is_none());
    }

    #[test]
    fn format_sweep_with_negatives() {
        let neg = NegativeSweepOutput {
            config_name: "boosted-adaptive".to_string(),
            pairs: vec![make_negative_pair("a", "b", &[1.0])],
            overall_mean_score: 1.0,
            overall_fpr_at_3: 0.0,
            overall_fpr_at_4: 0.0,
        };

        let output = SweepOutput {
            corpora: vec![],
            configs: vec!["bm25".to_string()],
            timestamp: "2026-03-18T00:00:00Z".to_string(),
            negative_results: Some(neg),
        };

        let formatted = format_sweep(&output);
        assert!(
            formatted.contains("CROSS-CORPUS NEGATIVES"),
            "Sweep output should include negative section: {formatted}"
        );
    }
}
