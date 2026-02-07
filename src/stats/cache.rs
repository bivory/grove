//! Materialized stats cache for Grove.
//!
//! This module provides the cache that aggregates stats from the event log.
//! The cache is rebuilt when stale (log has more entries than cached count).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::LearningCategory;
use crate::error::{GroveError, Result};
use crate::stats::{StatsEvent, StatsEventType, StatsLogger};

/// Materialized stats cache.
///
/// Aggregates event log data for fast dashboard reads. Rebuilt from the
/// event log when stale.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatsCache {
    /// When the cache was generated.
    pub generated_at: DateTime<Utc>,
    /// Number of log entries processed in this rebuild.
    pub log_entries_processed: usize,
    /// Last time passive decay was checked (for throttling).
    #[serde(default)]
    pub last_decay_check: Option<DateTime<Utc>>,
    /// Per-learning statistics.
    pub learnings: HashMap<String, LearningStats>,
    /// Reflection statistics.
    pub reflections: ReflectionStats,
    /// Write gate statistics.
    pub write_gate: WriteGateStats,
    /// Cross-pollination edges.
    pub cross_pollination: Vec<CrossPollinationEdge>,
    /// Aggregate statistics.
    pub aggregates: AggregateStats,
}

impl Default for StatsCache {
    fn default() -> Self {
        Self {
            generated_at: Utc::now(),
            log_entries_processed: 0,
            last_decay_check: None,
            learnings: HashMap::new(),
            reflections: ReflectionStats::default(),
            write_gate: WriteGateStats::default(),
            cross_pollination: Vec::new(),
            aggregates: AggregateStats::default(),
        }
    }
}

impl StatsCache {
    /// Create a new empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build cache from a list of events.
    pub fn from_events(events: &[StatsEvent]) -> Self {
        let mut cache = Self::new();
        cache.process_events(events);
        cache.compute_aggregates();
        cache
    }

    /// Process events to populate the cache.
    fn process_events(&mut self, events: &[StatsEvent]) {
        for event in events {
            self.process_event(event);
        }
        self.log_entries_processed = events.len();
        self.generated_at = Utc::now();
    }

    /// Process a single event.
    fn process_event(&mut self, event: &StatsEvent) {
        match &event.data {
            StatsEventType::Surfaced {
                learning_id,
                session_id: _,
            } => {
                let stats = self.learnings.entry(learning_id.clone()).or_default();
                stats.surfaced += 1;
                stats.last_surfaced = Some(event.ts);
            }

            StatsEventType::Referenced {
                learning_id,
                session_id: _,
                ticket_id,
            } => {
                // First, extract the origin ticket if it exists (for cross-pollination check)
                let origin_for_cross_poll = {
                    let stats = self.learnings.entry(learning_id.clone()).or_default();
                    stats.referenced += 1;
                    stats.last_referenced = Some(event.ts);

                    if let Some(tid) = ticket_id {
                        if !stats.referencing_tickets.contains(tid) {
                            stats.referencing_tickets.push(tid.clone());
                        }
                    }

                    // Clone origin ticket for cross-pollination check
                    stats.origin_ticket.clone()
                };

                // Now handle cross-pollination outside the borrow
                if let (Some(tid), Some(origin)) = (ticket_id, origin_for_cross_poll) {
                    if &origin != tid {
                        self.add_cross_pollination(learning_id, &origin, tid);
                    }
                }
            }

            StatsEventType::Dismissed {
                learning_id,
                session_id: _,
            } => {
                let stats = self.learnings.entry(learning_id.clone()).or_default();
                stats.dismissed += 1;
            }

            StatsEventType::Corrected {
                learning_id,
                session_id: _,
                superseded_by: _,
            } => {
                let stats = self.learnings.entry(learning_id.clone()).or_default();
                stats.corrected += 1;
            }

            StatsEventType::Reflection {
                session_id: _,
                candidates,
                accepted,
                categories: _,
                ticket_id,
                backend,
            } => {
                self.reflections.completed += 1;
                *self
                    .reflections
                    .by_backend
                    .entry(backend.clone())
                    .or_insert(0) += 1;

                // Track write gate stats from reflection event
                self.write_gate.total_evaluated += *candidates;
                self.write_gate.total_accepted += *accepted;
                self.write_gate.total_rejected += candidates.saturating_sub(*accepted);

                // Update origin tickets for learnings
                if let Some(tid) = ticket_id {
                    // Note: We can't know which learnings were created here without more context
                    // This is tracked when learnings are first referenced
                    let _ = tid; // Suppress unused warning
                }
            }

            StatsEventType::Skip {
                session_id: _,
                reason: _,
                decider: _,
                lines_changed: _,
                ticket_id: _,
            } => {
                self.reflections.skipped += 1;
            }

            StatsEventType::Archived {
                learning_id,
                reason: _,
            } => {
                // Mark learning as archived in stats
                let stats = self.learnings.entry(learning_id.clone()).or_default();
                stats.archived = true;
            }

            StatsEventType::Restored { learning_id } => {
                // Mark learning as not archived
                let stats = self.learnings.entry(learning_id.clone()).or_default();
                stats.archived = false;
            }
        }
    }

    /// Add a cross-pollination edge.
    fn add_cross_pollination(&mut self, learning_id: &str, origin: &str, referenced_in: &str) {
        // Find existing edge or create new one
        let edge = self
            .cross_pollination
            .iter_mut()
            .find(|e| e.learning_id == learning_id);

        if let Some(edge) = edge {
            if !edge.referenced_in.contains(&referenced_in.to_string()) {
                edge.referenced_in.push(referenced_in.to_string());
            }
        } else {
            self.cross_pollination.push(CrossPollinationEdge {
                learning_id: learning_id.to_string(),
                origin_ticket: origin.to_string(),
                referenced_in: vec![referenced_in.to_string()],
            });
        }
    }

    /// Compute aggregate statistics from per-learning stats.
    fn compute_aggregates(&mut self) {
        let mut total_learnings = 0;
        let mut total_archived = 0;
        let mut total_hit_rate = 0.0;
        let mut hit_rate_count = 0;
        let by_category: HashMap<LearningCategory, CategoryStats> = HashMap::new();

        for stats in self.learnings.values_mut() {
            // Compute hit rate for this learning
            if stats.surfaced > 0 {
                stats.hit_rate = stats.referenced as f64 / stats.surfaced as f64;
                total_hit_rate += stats.hit_rate;
                hit_rate_count += 1;
            }

            total_learnings += 1;
            if stats.archived {
                total_archived += 1;
            }
        }

        // Compute write gate pass rate
        if self.write_gate.total_evaluated > 0 {
            self.write_gate.pass_rate =
                self.write_gate.total_accepted as f64 / self.write_gate.total_evaluated as f64;
        }

        self.aggregates.total_learnings = total_learnings;
        self.aggregates.total_archived = total_archived;
        self.aggregates.average_hit_rate = if hit_rate_count > 0 {
            total_hit_rate / hit_rate_count as f64
        } else {
            0.0
        };
        self.aggregates.cross_pollination_count = self.cross_pollination.len();
        self.aggregates.by_category = by_category;
    }

    /// Check if the cache is stale compared to the log.
    pub fn is_stale(&self, log_line_count: usize) -> bool {
        self.log_entries_processed != log_line_count
    }

    /// Set the origin ticket for a learning.
    pub fn set_origin_ticket(&mut self, learning_id: &str, ticket_id: &str) {
        let stats = self.learnings.entry(learning_id.to_string()).or_default();
        if stats.origin_ticket.is_none() {
            stats.origin_ticket = Some(ticket_id.to_string());
        }
    }

    /// Update the last decay check timestamp.
    pub fn set_last_decay_check(&mut self, ts: DateTime<Utc>) {
        self.last_decay_check = Some(ts);
    }

    /// Record a write gate rejection reason.
    pub fn record_rejection_reason(&mut self, reason: &str) {
        *self
            .write_gate
            .rejection_reasons
            .entry(reason.to_string())
            .or_insert(0) += 1;
    }
}

/// Per-learning statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LearningStats {
    /// Number of times surfaced.
    pub surfaced: u32,
    /// Number of times referenced.
    pub referenced: u32,
    /// Number of times dismissed.
    pub dismissed: u32,
    /// Number of times corrected.
    pub corrected: u32,
    /// Hit rate (referenced / surfaced).
    pub hit_rate: f64,
    /// Last time surfaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_surfaced: Option<DateTime<Utc>>,
    /// Last time referenced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_referenced: Option<DateTime<Utc>>,
    /// Ticket where learning originated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_ticket: Option<String>,
    /// Tickets that referenced this learning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub referencing_tickets: Vec<String>,
    /// Whether the learning is archived.
    #[serde(default)]
    pub archived: bool,
}

/// Reflection statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReflectionStats {
    /// Number of completed reflections.
    pub completed: u32,
    /// Number of skipped reflections.
    pub skipped: u32,
    /// Reflections by backend.
    pub by_backend: HashMap<String, u32>,
}

/// Write gate statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WriteGateStats {
    /// Total candidates evaluated.
    pub total_evaluated: u32,
    /// Total accepted (passed write gate).
    pub total_accepted: u32,
    /// Total rejected (failed write gate).
    pub total_rejected: u32,
    /// Pass rate (accepted / evaluated).
    pub pass_rate: f64,
    /// Rejection reasons with counts.
    pub rejection_reasons: HashMap<String, u32>,
    /// Retrospective misses (learnings that should have been captured).
    #[serde(default)]
    pub retrospective_misses: u32,
}

/// Cross-pollination edge (learning referenced outside origin ticket).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrossPollinationEdge {
    /// The learning that was cross-pollinated.
    pub learning_id: String,
    /// The ticket where the learning originated.
    pub origin_ticket: String,
    /// Tickets that referenced this learning.
    pub referenced_in: Vec<String>,
}

/// Category-level statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CategoryStats {
    /// Number of learnings in this category.
    pub count: u32,
    /// Average hit rate for learnings in this category.
    pub avg_hit_rate: f64,
}

/// Aggregate statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AggregateStats {
    /// Total number of learnings.
    pub total_learnings: u32,
    /// Total archived learnings.
    pub total_archived: u32,
    /// Average hit rate across all learnings.
    pub average_hit_rate: f64,
    /// Number of cross-pollination events.
    pub cross_pollination_count: usize,
    /// Stats by category.
    pub by_category: HashMap<LearningCategory, CategoryStats>,
    /// Stats by scope.
    #[serde(default)]
    pub by_scope: HashMap<String, CategoryStats>,
}

/// Stats cache manager that handles loading, saving, and rebuilding.
#[derive(Debug, Clone)]
pub struct StatsCacheManager {
    /// Path to the cache file.
    cache_path: PathBuf,
    /// Path to the stats log.
    log_path: PathBuf,
}

impl StatsCacheManager {
    /// Create a new cache manager.
    pub fn new(cache_path: impl AsRef<Path>, log_path: impl AsRef<Path>) -> Self {
        Self {
            cache_path: cache_path.as_ref().to_path_buf(),
            log_path: log_path.as_ref().to_path_buf(),
        }
    }

    /// Load the cache from disk.
    pub fn load(&self) -> Result<Option<StatsCache>> {
        if !self.cache_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&self.cache_path).map_err(|e| {
            GroveError::backend(format!(
                "Failed to read cache {}: {}",
                self.cache_path.display(),
                e
            ))
        })?;

        let cache: StatsCache = serde_json::from_str(&content)
            .map_err(|e| GroveError::serde(format!("Failed to parse cache: {}", e)))?;

        Ok(Some(cache))
    }

    /// Save the cache to disk.
    pub fn save(&self, cache: &StatsCache) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = self.cache_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                GroveError::backend(format!(
                    "Failed to create directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let content = serde_json::to_string_pretty(cache)
            .map_err(|e| GroveError::serde(format!("Failed to serialize cache: {}", e)))?;

        fs::write(&self.cache_path, content).map_err(|e| {
            GroveError::backend(format!(
                "Failed to write cache {}: {}",
                self.cache_path.display(),
                e
            ))
        })?;

        Ok(())
    }

    /// Rebuild the cache from the log.
    pub fn rebuild(&self) -> Result<StatsCache> {
        let logger = StatsLogger::new(&self.log_path);
        let events = logger.read_all()?;

        let mut cache = StatsCache::from_events(&events);

        // Preserve last_decay_check from existing cache if available
        if let Ok(Some(existing)) = self.load() {
            cache.last_decay_check = existing.last_decay_check;
        }

        self.save(&cache)?;
        Ok(cache)
    }

    /// Load or rebuild the cache as needed.
    pub fn load_or_rebuild(&self) -> Result<StatsCache> {
        let logger = StatsLogger::new(&self.log_path);
        let log_count = logger.count()?;

        if let Ok(Some(cache)) = self.load() {
            if !cache.is_stale(log_count) {
                return Ok(cache);
            }
        }

        self.rebuild()
    }

    /// Force a cache rebuild.
    pub fn force_rebuild(&self) -> Result<StatsCache> {
        self.rebuild()
    }

    /// Get the cache path.
    pub fn cache_path(&self) -> &Path {
        &self.cache_path
    }

    /// Get the log path.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SkipDecider;
    use tempfile::TempDir;

    // Helper to create test events
    fn surfaced_event(learning_id: &str, session_id: &str) -> StatsEvent {
        StatsEvent::new(StatsEventType::surfaced(learning_id, session_id))
    }

    fn referenced_event(
        learning_id: &str,
        session_id: &str,
        ticket_id: Option<&str>,
    ) -> StatsEvent {
        StatsEvent::new(StatsEventType::referenced(
            learning_id,
            session_id,
            ticket_id.map(|s| s.to_string()),
        ))
    }

    fn dismissed_event(learning_id: &str, session_id: &str) -> StatsEvent {
        StatsEvent::new(StatsEventType::dismissed(learning_id, session_id))
    }

    fn reflection_event(
        session_id: &str,
        candidates: u32,
        accepted: u32,
        backend: &str,
    ) -> StatsEvent {
        StatsEvent::new(StatsEventType::reflection(
            session_id,
            candidates,
            accepted,
            vec![LearningCategory::Pattern],
            None,
            backend,
        ))
    }

    fn skip_event(session_id: &str) -> StatsEvent {
        StatsEvent::new(StatsEventType::skip(
            session_id,
            "test skip",
            SkipDecider::Agent,
            5,
            None,
        ))
    }

    // StatsCache tests

    #[test]
    fn test_cache_default() {
        let cache = StatsCache::default();
        assert_eq!(cache.log_entries_processed, 0);
        assert!(cache.learnings.is_empty());
        assert_eq!(cache.reflections.completed, 0);
    }

    #[test]
    fn test_cache_from_empty_events() {
        let cache = StatsCache::from_events(&[]);
        assert_eq!(cache.log_entries_processed, 0);
        assert!(cache.learnings.is_empty());
    }

    #[test]
    fn test_surfaced_tracking() {
        let events = vec![
            surfaced_event("L001", "s1"),
            surfaced_event("L001", "s2"),
            surfaced_event("L002", "s1"),
        ];

        let cache = StatsCache::from_events(&events);

        assert_eq!(cache.learnings.get("L001").unwrap().surfaced, 2);
        assert_eq!(cache.learnings.get("L002").unwrap().surfaced, 1);
    }

    #[test]
    fn test_referenced_tracking() {
        let events = vec![
            surfaced_event("L001", "s1"),
            referenced_event("L001", "s1", Some("T001")),
            referenced_event("L001", "s2", Some("T001")),
        ];

        let cache = StatsCache::from_events(&events);

        let stats = cache.learnings.get("L001").unwrap();
        assert_eq!(stats.referenced, 2);
        assert!(stats.last_referenced.is_some());
        assert!(stats.referencing_tickets.contains(&"T001".to_string()));
    }

    #[test]
    fn test_dismissed_tracking() {
        let events = vec![surfaced_event("L001", "s1"), dismissed_event("L001", "s1")];

        let cache = StatsCache::from_events(&events);

        assert_eq!(cache.learnings.get("L001").unwrap().dismissed, 1);
    }

    #[test]
    fn test_hit_rate_calculation() {
        let events = vec![
            surfaced_event("L001", "s1"),
            surfaced_event("L001", "s2"),
            referenced_event("L001", "s1", None),
        ];

        let cache = StatsCache::from_events(&events);

        let stats = cache.learnings.get("L001").unwrap();
        assert!((stats.hit_rate - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_hit_rate_zero_surfaced() {
        let events = vec![referenced_event("L001", "s1", None)];

        let cache = StatsCache::from_events(&events);

        let stats = cache.learnings.get("L001").unwrap();
        assert!((stats.hit_rate - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_reflection_tracking() {
        let events = vec![
            reflection_event("s1", 5, 3, "markdown"),
            reflection_event("s2", 3, 2, "markdown"),
            reflection_event("s3", 4, 4, "total_recall"),
        ];

        let cache = StatsCache::from_events(&events);

        assert_eq!(cache.reflections.completed, 3);
        assert_eq!(cache.reflections.by_backend.get("markdown"), Some(&2));
        assert_eq!(cache.reflections.by_backend.get("total_recall"), Some(&1));
    }

    #[test]
    fn test_skip_tracking() {
        let events = vec![skip_event("s1"), skip_event("s2")];

        let cache = StatsCache::from_events(&events);

        assert_eq!(cache.reflections.skipped, 2);
    }

    #[test]
    fn test_write_gate_stats() {
        let events = vec![
            reflection_event("s1", 10, 7, "markdown"),
            reflection_event("s2", 5, 3, "markdown"),
        ];

        let cache = StatsCache::from_events(&events);

        assert_eq!(cache.write_gate.total_evaluated, 15);
        assert_eq!(cache.write_gate.total_accepted, 10);
        assert_eq!(cache.write_gate.total_rejected, 5);
        assert!((cache.write_gate.pass_rate - 0.6667).abs() < 0.01);
    }

    #[test]
    fn test_cross_pollination() {
        let mut cache = StatsCache::new();

        // Set origin ticket for L001
        cache.set_origin_ticket("L001", "T001");

        // Process reference from different ticket
        let events = vec![referenced_event("L001", "s1", Some("T002"))];

        for event in &events {
            cache.process_event(event);
        }

        assert_eq!(cache.cross_pollination.len(), 1);
        assert_eq!(cache.cross_pollination[0].learning_id, "L001");
        assert_eq!(cache.cross_pollination[0].origin_ticket, "T001");
        assert!(cache.cross_pollination[0]
            .referenced_in
            .contains(&"T002".to_string()));
    }

    #[test]
    fn test_archived_tracking() {
        let events = vec![
            surfaced_event("L001", "s1"),
            StatsEvent::new(StatsEventType::archived("L001", "passive_decay")),
        ];

        let cache = StatsCache::from_events(&events);

        assert!(cache.learnings.get("L001").unwrap().archived);
    }

    #[test]
    fn test_restored_tracking() {
        let events = vec![
            surfaced_event("L001", "s1"),
            StatsEvent::new(StatsEventType::archived("L001", "passive_decay")),
            StatsEvent::new(StatsEventType::restored("L001")),
        ];

        let cache = StatsCache::from_events(&events);

        assert!(!cache.learnings.get("L001").unwrap().archived);
    }

    #[test]
    fn test_staleness_check() {
        let cache = StatsCache::from_events(&[surfaced_event("L001", "s1")]);

        assert!(!cache.is_stale(1));
        assert!(cache.is_stale(2));
        assert!(cache.is_stale(0));
    }

    #[test]
    fn test_aggregates() {
        let events = vec![
            surfaced_event("L001", "s1"),
            surfaced_event("L002", "s1"),
            referenced_event("L001", "s1", None),
            StatsEvent::new(StatsEventType::archived("L002", "decay")),
        ];

        let cache = StatsCache::from_events(&events);

        assert_eq!(cache.aggregates.total_learnings, 2);
        assert_eq!(cache.aggregates.total_archived, 1);
        assert!(cache.aggregates.average_hit_rate > 0.0);
    }

    #[test]
    fn test_record_rejection_reason() {
        let mut cache = StatsCache::new();

        cache.record_rejection_reason("schema_validation");
        cache.record_rejection_reason("schema_validation");
        cache.record_rejection_reason("near_duplicate");

        assert_eq!(
            cache.write_gate.rejection_reasons.get("schema_validation"),
            Some(&2)
        );
        assert_eq!(
            cache.write_gate.rejection_reasons.get("near_duplicate"),
            Some(&1)
        );
    }

    // StatsCacheManager tests

    #[test]
    fn test_manager_save_and_load() {
        let temp = TempDir::new().unwrap();
        let cache_path = temp.path().join("cache.json");
        let log_path = temp.path().join("stats.log");

        let manager = StatsCacheManager::new(&cache_path, &log_path);

        let mut cache = StatsCache::new();
        cache.log_entries_processed = 42;

        manager.save(&cache).unwrap();
        let loaded = manager.load().unwrap().unwrap();

        assert_eq!(loaded.log_entries_processed, 42);
    }

    #[test]
    fn test_manager_load_missing() {
        let temp = TempDir::new().unwrap();
        let cache_path = temp.path().join("cache.json");
        let log_path = temp.path().join("stats.log");

        let manager = StatsCacheManager::new(&cache_path, &log_path);

        assert!(manager.load().unwrap().is_none());
    }

    #[test]
    fn test_manager_rebuild() {
        let temp = TempDir::new().unwrap();
        let cache_path = temp.path().join("cache.json");
        let log_path = temp.path().join(".grove").join("stats.log");

        let logger = StatsLogger::new(&log_path);
        logger.append_surfaced("L001", "s1").unwrap();
        logger.append_referenced("L001", "s1", None).unwrap();

        let manager = StatsCacheManager::new(&cache_path, &log_path);
        let cache = manager.rebuild().unwrap();

        assert_eq!(cache.log_entries_processed, 2);
        assert_eq!(cache.learnings.get("L001").unwrap().surfaced, 1);
        assert_eq!(cache.learnings.get("L001").unwrap().referenced, 1);
    }

    #[test]
    fn test_manager_load_or_rebuild_fresh() {
        let temp = TempDir::new().unwrap();
        let cache_path = temp.path().join("cache.json");
        let log_path = temp.path().join("stats.log");

        let logger = StatsLogger::new(&log_path);
        logger.append_surfaced("L001", "s1").unwrap();

        let manager = StatsCacheManager::new(&cache_path, &log_path);

        // First call rebuilds
        let cache1 = manager.load_or_rebuild().unwrap();
        assert_eq!(cache1.log_entries_processed, 1);

        // Second call loads from cache (no rebuild)
        let cache2 = manager.load_or_rebuild().unwrap();
        assert_eq!(cache2.log_entries_processed, 1);
    }

    #[test]
    fn test_manager_load_or_rebuild_stale() {
        let temp = TempDir::new().unwrap();
        let cache_path = temp.path().join("cache.json");
        let log_path = temp.path().join("stats.log");

        let logger = StatsLogger::new(&log_path);
        logger.append_surfaced("L001", "s1").unwrap();

        let manager = StatsCacheManager::new(&cache_path, &log_path);

        // Initial build
        let cache1 = manager.load_or_rebuild().unwrap();
        assert_eq!(cache1.log_entries_processed, 1);

        // Add more events
        logger.append_surfaced("L002", "s1").unwrap();

        // Should detect staleness and rebuild
        let cache2 = manager.load_or_rebuild().unwrap();
        assert_eq!(cache2.log_entries_processed, 2);
    }

    #[test]
    fn test_manager_preserves_decay_check() {
        let temp = TempDir::new().unwrap();
        let cache_path = temp.path().join("cache.json");
        let log_path = temp.path().join("stats.log");

        let logger = StatsLogger::new(&log_path);
        logger.append_surfaced("L001", "s1").unwrap();

        let manager = StatsCacheManager::new(&cache_path, &log_path);

        // Set decay check time
        let decay_time = Utc::now();
        let mut cache = manager.rebuild().unwrap();
        cache.set_last_decay_check(decay_time);
        manager.save(&cache).unwrap();

        // Add event and rebuild
        logger.append_surfaced("L002", "s1").unwrap();
        let rebuilt = manager.rebuild().unwrap();

        assert_eq!(rebuilt.last_decay_check, Some(decay_time));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let events = vec![
            surfaced_event("L001", "s1"),
            referenced_event("L001", "s1", Some("T001")),
            reflection_event("s1", 5, 3, "markdown"),
        ];

        let cache = StatsCache::from_events(&events);
        let json = serde_json::to_string(&cache).unwrap();
        let parsed: StatsCache = serde_json::from_str(&json).unwrap();

        assert_eq!(cache.log_entries_processed, parsed.log_entries_processed);
        assert_eq!(cache.learnings.len(), parsed.learnings.len());
    }
}
