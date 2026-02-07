//! Passive decay evaluation for Grove learnings.
//!
//! This module handles automatic archival of stale learnings based on
//! time since last verification (surfacing or reference).
//!
//! Decay logic:
//! 1. Compute last_verified = max(last_referenced, last_surfaced, created_at)
//! 2. If now - last_verified > passive_duration_days → archive
//! 3. Learnings with hit_rate > immunity_hit_rate are immune
//! 4. Decay checks are throttled to once per day

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};

use crate::config::DecayConfig;
use crate::stats::{LearningStats, StatsCache, StatsLogger};

/// Result of evaluating decay for a single learning.
#[derive(Debug, Clone, PartialEq)]
pub enum DecayResult {
    /// Learning should remain active.
    Active,
    /// Learning should be archived due to passive decay.
    Decayed,
    /// Learning is immune to decay due to high hit rate.
    Immune,
    /// Learning is already archived.
    AlreadyArchived,
}

impl DecayResult {
    /// Whether this result means the learning should be archived.
    pub fn should_archive(&self) -> bool {
        matches!(self, Self::Decayed)
    }
}

/// Evaluate decay for a single learning.
///
/// Returns the decay result indicating whether the learning should be archived.
///
/// # Arguments
/// * `learning_id` - ID of the learning (for lookup in cache if stats not provided)
/// * `stats` - The learning's stats from the cache
/// * `created_at` - When the learning was originally created
/// * `config` - Decay configuration with thresholds
/// * `now` - Current timestamp (for testability)
pub fn evaluate(
    stats: &LearningStats,
    created_at: DateTime<Utc>,
    config: &DecayConfig,
    now: DateTime<Utc>,
) -> DecayResult {
    // Already archived
    if stats.archived {
        return DecayResult::AlreadyArchived;
    }

    // Check immunity based on hit rate
    if stats.hit_rate >= config.immunity_hit_rate {
        return DecayResult::Immune;
    }

    // Compute last verified timestamp
    let last_verified = compute_last_verified(stats, created_at);

    // Check if past decay threshold
    let decay_threshold = Duration::days(config.passive_duration_days as i64);
    if now - last_verified > decay_threshold {
        return DecayResult::Decayed;
    }

    DecayResult::Active
}

/// Compute the last verified timestamp for a learning.
///
/// Returns the maximum of:
/// - last_referenced
/// - last_surfaced
/// - created_at
fn compute_last_verified(stats: &LearningStats, created_at: DateTime<Utc>) -> DateTime<Utc> {
    let mut last = created_at;

    if let Some(ts) = stats.last_referenced {
        if ts > last {
            last = ts;
        }
    }

    if let Some(ts) = stats.last_surfaced {
        if ts > last {
            last = ts;
        }
    }

    last
}

/// Check if a decay check should be run (throttled to once per day).
///
/// Returns true if:
/// - No previous decay check was recorded
/// - The last decay check was more than 24 hours ago
pub fn should_run_decay_check(last_decay_check: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    match last_decay_check {
        None => true,
        Some(last) => now - last > Duration::hours(24),
    }
}

/// Run decay evaluation on all learnings in the cache.
///
/// Returns a list of learning IDs that were decayed.
///
/// # Arguments
/// * `cache` - The stats cache with learning stats
/// * `learning_timestamps` - Map of learning_id to created_at timestamp
/// * `config` - Decay configuration
/// * `now` - Current timestamp
pub fn run_decay_evaluation(
    cache: &StatsCache,
    learning_timestamps: &std::collections::HashMap<String, DateTime<Utc>>,
    config: &DecayConfig,
    now: DateTime<Utc>,
) -> Vec<String> {
    let mut decayed = Vec::new();

    for (learning_id, stats) in &cache.learnings {
        // Get created_at from the provided map, or use a very old default
        let created_at = learning_timestamps
            .get(learning_id)
            .copied()
            .unwrap_or(DateTime::UNIX_EPOCH);

        let result = evaluate(stats, created_at, config, now);

        if result.should_archive() {
            decayed.push(learning_id.clone());
        }
    }

    decayed
}

/// Run decay and log archived events.
///
/// This is the main entry point for decay processing. It:
/// 1. Checks if a decay check should run (throttling)
/// 2. Evaluates all learnings for decay
/// 3. Logs archived events for decayed learnings
/// 4. Returns the list of decayed learning IDs
///
/// # Arguments
/// * `cache` - The stats cache (mutable for updating last_decay_check)
/// * `learning_timestamps` - Map of learning_id to created_at
/// * `config` - Decay configuration
/// * `logger` - Stats logger for appending events
/// * `now` - Current timestamp
/// * `force` - If true, skip throttle check (for maintenance)
pub fn run_decay_and_log(
    cache: &mut StatsCache,
    learning_timestamps: &std::collections::HashMap<String, DateTime<Utc>>,
    config: &DecayConfig,
    logger: &StatsLogger,
    now: DateTime<Utc>,
    force: bool,
) -> crate::error::Result<Vec<String>> {
    // Check throttle unless forced
    if !force && !should_run_decay_check(cache.last_decay_check, now) {
        return Ok(vec![]);
    }

    // Run evaluation
    let decayed = run_decay_evaluation(cache, learning_timestamps, config, now);

    // Log archived events for each decayed learning
    for learning_id in &decayed {
        logger.append_archived(learning_id, "passive_decay")?;

        // Mark as archived in the cache
        if let Some(stats) = cache.learnings.get_mut(learning_id) {
            stats.archived = true;
        }
    }

    // Update last decay check
    cache.set_last_decay_check(now);

    Ok(decayed)
}

/// Get learnings approaching decay threshold.
///
/// Returns learning IDs that are within `warning_days` of the decay threshold.
/// Useful for generating decay warnings in the insights engine.
pub fn get_decay_warnings(
    cache: &StatsCache,
    learning_timestamps: &std::collections::HashMap<String, DateTime<Utc>>,
    config: &DecayConfig,
    warning_days: u32,
    now: DateTime<Utc>,
) -> Vec<String> {
    let mut warnings = Vec::new();

    let warning_threshold = Duration::days(
        (config.passive_duration_days - warning_days.min(config.passive_duration_days)) as i64,
    );
    let decay_threshold = Duration::days(config.passive_duration_days as i64);

    for (learning_id, stats) in &cache.learnings {
        // Skip archived and immune learnings
        if stats.archived {
            continue;
        }
        if stats.hit_rate >= config.immunity_hit_rate {
            continue;
        }

        let created_at = learning_timestamps
            .get(learning_id)
            .copied()
            .unwrap_or(DateTime::UNIX_EPOCH);

        let last_verified = compute_last_verified(stats, created_at);
        let age = now - last_verified;

        // In warning window: past warning threshold but not yet decayed
        if age > warning_threshold && age <= decay_threshold {
            warnings.push(learning_id.clone());
        }
    }

    warnings
}

/// Collect learning IDs that are immune to decay.
pub fn get_immune_learnings(cache: &StatsCache, config: &DecayConfig) -> HashSet<String> {
    cache
        .learnings
        .iter()
        .filter(|(_, stats)| !stats.archived && stats.hit_rate >= config.immunity_hit_rate)
        .map(|(id, _)| id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn default_config() -> DecayConfig {
        DecayConfig::default()
    }

    fn make_stats(
        surfaced: Option<DateTime<Utc>>,
        referenced: Option<DateTime<Utc>>,
        hit_rate: f64,
        archived: bool,
    ) -> LearningStats {
        LearningStats {
            surfaced: 0,
            referenced: 0,
            dismissed: 0,
            corrected: 0,
            hit_rate,
            last_surfaced: surfaced,
            last_referenced: referenced,
            origin_ticket: None,
            referencing_tickets: vec![],
            archived,
        }
    }

    // Basic decay evaluation tests

    #[test]
    fn test_evaluate_active_recent() {
        let config = default_config();
        let now = Utc::now();
        let created_at = now - Duration::days(30);

        let stats = make_stats(Some(now - Duration::days(10)), None, 0.0, false);

        let result = evaluate(&stats, created_at, &config, now);
        assert_eq!(result, DecayResult::Active);
    }

    #[test]
    fn test_evaluate_decayed_old() {
        let config = default_config(); // 90 days threshold
        let now = Utc::now();
        let created_at = now - Duration::days(100);

        let stats = make_stats(None, None, 0.0, false);

        let result = evaluate(&stats, created_at, &config, now);
        assert_eq!(result, DecayResult::Decayed);
    }

    #[test]
    fn test_evaluate_immune_high_hit_rate() {
        let config = default_config(); // 0.8 immunity threshold
        let now = Utc::now();
        let created_at = now - Duration::days(100);

        // Old but high hit rate
        let stats = make_stats(None, None, 0.85, false);

        let result = evaluate(&stats, created_at, &config, now);
        assert_eq!(result, DecayResult::Immune);
    }

    #[test]
    fn test_evaluate_already_archived() {
        let config = default_config();
        let now = Utc::now();
        let created_at = now - Duration::days(100);

        let stats = make_stats(None, None, 0.0, true);

        let result = evaluate(&stats, created_at, &config, now);
        assert_eq!(result, DecayResult::AlreadyArchived);
    }

    #[test]
    fn test_evaluate_uses_last_referenced() {
        let config = default_config();
        let now = Utc::now();
        let created_at = now - Duration::days(100);

        // Old creation but recent reference
        let stats = make_stats(None, Some(now - Duration::days(10)), 0.0, false);

        let result = evaluate(&stats, created_at, &config, now);
        assert_eq!(result, DecayResult::Active);
    }

    #[test]
    fn test_evaluate_uses_last_surfaced() {
        let config = default_config();
        let now = Utc::now();
        let created_at = now - Duration::days(100);

        // Old creation but recent surfacing
        let stats = make_stats(Some(now - Duration::days(10)), None, 0.0, false);

        let result = evaluate(&stats, created_at, &config, now);
        assert_eq!(result, DecayResult::Active);
    }

    #[test]
    fn test_evaluate_at_exact_threshold() {
        let config = default_config(); // 90 days
        let now = Utc::now();
        let created_at = now - Duration::days(90);

        let stats = make_stats(None, None, 0.0, false);

        let result = evaluate(&stats, created_at, &config, now);
        // At exactly 90 days, should still be active (not strictly greater)
        assert_eq!(result, DecayResult::Active);
    }

    #[test]
    fn test_evaluate_just_past_threshold() {
        let config = default_config(); // 90 days
        let now = Utc::now();
        let created_at = now - Duration::days(91);

        let stats = make_stats(None, None, 0.0, false);

        let result = evaluate(&stats, created_at, &config, now);
        assert_eq!(result, DecayResult::Decayed);
    }

    // Throttling tests

    #[test]
    fn test_should_run_decay_check_no_previous() {
        let now = Utc::now();
        assert!(should_run_decay_check(None, now));
    }

    #[test]
    fn test_should_run_decay_check_recent() {
        let now = Utc::now();
        let last_check = now - Duration::hours(12);
        assert!(!should_run_decay_check(Some(last_check), now));
    }

    #[test]
    fn test_should_run_decay_check_old() {
        let now = Utc::now();
        let last_check = now - Duration::hours(25);
        assert!(should_run_decay_check(Some(last_check), now));
    }

    // Batch evaluation tests

    #[test]
    fn test_run_decay_evaluation_mixed() {
        let now = Utc::now();
        let config = default_config();

        let mut cache = StatsCache::new();

        // Active - recent
        cache.learnings.insert(
            "L001".to_string(),
            make_stats(Some(now - Duration::days(10)), None, 0.0, false),
        );

        // Should decay - old
        cache
            .learnings
            .insert("L002".to_string(), make_stats(None, None, 0.0, false));

        // Immune - high hit rate
        cache
            .learnings
            .insert("L003".to_string(), make_stats(None, None, 0.9, false));

        let mut timestamps = HashMap::new();
        timestamps.insert("L001".to_string(), now - Duration::days(10));
        timestamps.insert("L002".to_string(), now - Duration::days(100));
        timestamps.insert("L003".to_string(), now - Duration::days(100));

        let decayed = run_decay_evaluation(&cache, &timestamps, &config, now);

        assert_eq!(decayed.len(), 1);
        assert!(decayed.contains(&"L002".to_string()));
    }

    #[test]
    fn test_run_decay_and_log() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join("stats.log");
        let logger = StatsLogger::new(&log_path);

        let now = Utc::now();
        let config = default_config();

        let mut cache = StatsCache::new();
        cache
            .learnings
            .insert("L001".to_string(), make_stats(None, None, 0.0, false));

        let mut timestamps = HashMap::new();
        timestamps.insert("L001".to_string(), now - Duration::days(100));

        let decayed =
            run_decay_and_log(&mut cache, &timestamps, &config, &logger, now, true).unwrap();

        assert_eq!(decayed.len(), 1);
        assert!(decayed.contains(&"L001".to_string()));

        // Verify cache was updated
        assert!(cache.learnings.get("L001").unwrap().archived);
        assert_eq!(cache.last_decay_check, Some(now));

        // Verify event was logged
        let events = logger.read_all().unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_run_decay_and_log_throttled() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join("stats.log");
        let logger = StatsLogger::new(&log_path);

        let now = Utc::now();
        let config = default_config();

        let mut cache = StatsCache::new();
        cache
            .learnings
            .insert("L001".to_string(), make_stats(None, None, 0.0, false));
        cache.set_last_decay_check(now - Duration::hours(12)); // Recent check

        let mut timestamps = HashMap::new();
        timestamps.insert("L001".to_string(), now - Duration::days(100));

        // Without force, should be throttled
        let decayed =
            run_decay_and_log(&mut cache, &timestamps, &config, &logger, now, false).unwrap();

        assert!(decayed.is_empty());

        // With force, should run
        let decayed =
            run_decay_and_log(&mut cache, &timestamps, &config, &logger, now, true).unwrap();

        assert_eq!(decayed.len(), 1);
    }

    // Warning tests

    #[test]
    fn test_get_decay_warnings() {
        let now = Utc::now();
        let config = default_config(); // 90 days

        let mut cache = StatsCache::new();

        // Active - fresh (no warning)
        cache.learnings.insert(
            "L001".to_string(),
            make_stats(Some(now - Duration::days(10)), None, 0.0, false),
        );

        // In warning window (85 days, 7-day warning = 83-90 day window)
        cache
            .learnings
            .insert("L002".to_string(), make_stats(None, None, 0.0, false));

        // Past decay (100 days - already decayed)
        cache
            .learnings
            .insert("L003".to_string(), make_stats(None, None, 0.0, false));

        let mut timestamps = HashMap::new();
        timestamps.insert("L001".to_string(), now - Duration::days(10));
        timestamps.insert("L002".to_string(), now - Duration::days(85));
        timestamps.insert("L003".to_string(), now - Duration::days(100));

        let warnings = get_decay_warnings(&cache, &timestamps, &config, 7, now);

        assert_eq!(warnings.len(), 1);
        assert!(warnings.contains(&"L002".to_string()));
    }

    #[test]
    fn test_get_immune_learnings() {
        let config = default_config();

        let mut cache = StatsCache::new();

        cache
            .learnings
            .insert("L001".to_string(), make_stats(None, None, 0.5, false));

        cache
            .learnings
            .insert("L002".to_string(), make_stats(None, None, 0.85, false));

        cache.learnings.insert(
            "L003".to_string(),
            make_stats(None, None, 0.9, true), // Archived
        );

        let immune = get_immune_learnings(&cache, &config);

        assert_eq!(immune.len(), 1);
        assert!(immune.contains("L002"));
    }

    // compute_last_verified tests

    #[test]
    fn test_compute_last_verified_created_only() {
        let created_at = Utc::now() - Duration::days(50);
        let stats = make_stats(None, None, 0.0, false);

        let result = compute_last_verified(&stats, created_at);
        assert_eq!(result, created_at);
    }

    #[test]
    fn test_compute_last_verified_surfaced_wins() {
        let now = Utc::now();
        let created_at = now - Duration::days(50);
        let surfaced_at = now - Duration::days(10);

        let stats = make_stats(Some(surfaced_at), None, 0.0, false);

        let result = compute_last_verified(&stats, created_at);
        assert_eq!(result, surfaced_at);
    }

    #[test]
    fn test_compute_last_verified_referenced_wins() {
        let now = Utc::now();
        let created_at = now - Duration::days(50);
        let surfaced_at = now - Duration::days(20);
        let referenced_at = now - Duration::days(5);

        let stats = make_stats(Some(surfaced_at), Some(referenced_at), 0.0, false);

        let result = compute_last_verified(&stats, created_at);
        assert_eq!(result, referenced_at);
    }
}
