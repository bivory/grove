//! Stats event types and JSONL log writer for Grove.
//!
//! This module provides the event log model for tracking quality metrics.
//! Events are stored in an append-only JSONL file (`.grove/stats.log`).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::{LearningCategory, SkipDecider};
use crate::error::{GroveError, Result};
use crate::util::read_to_string_limited;

/// Schema version for stats events.
///
/// Increment when the event schema changes in a breaking way.
pub const STATS_SCHEMA_VERSION: u8 = 1;

/// A stats event that is written to the JSONL log.
///
/// All events include version, timestamp, and event type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatsEvent {
    /// Schema version for forward compatibility.
    pub v: u8,
    /// Timestamp of the event.
    pub ts: DateTime<Utc>,
    /// The event type and its data.
    #[serde(flatten)]
    pub data: StatsEventType,
}

impl StatsEvent {
    /// Create a new stats event with the current timestamp.
    pub fn new(data: StatsEventType) -> Self {
        Self {
            v: STATS_SCHEMA_VERSION,
            ts: Utc::now(),
            data,
        }
    }

    /// Create a stats event with a specific timestamp (for testing).
    pub fn with_timestamp(data: StatsEventType, ts: DateTime<Utc>) -> Self {
        Self {
            v: STATS_SCHEMA_VERSION,
            ts,
            data,
        }
    }
}

/// The type of stats event and its associated data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StatsEventType {
    /// A learning was surfaced (injected) into a session.
    Surfaced {
        /// The learning that was surfaced.
        learning_id: String,
        /// The session where it was surfaced.
        session_id: String,
        /// The category of the learning (for category-aware decay).
        #[serde(skip_serializing_if = "Option::is_none")]
        category: Option<LearningCategory>,
    },

    /// A learning was referenced (used) in a session.
    Referenced {
        /// The learning that was referenced.
        learning_id: String,
        /// The session where it was referenced.
        session_id: String,
        /// The ticket associated with the reference (if any).
        #[serde(skip_serializing_if = "Option::is_none")]
        ticket_id: Option<String>,
    },

    /// A learning was dismissed (surfaced but not referenced).
    Dismissed {
        /// The learning that was dismissed.
        learning_id: String,
        /// The session where it was dismissed.
        session_id: String,
    },

    /// A learning was corrected (superseded by a newer learning).
    Corrected {
        /// The learning that was corrected.
        learning_id: String,
        /// The session where the correction occurred.
        session_id: String,
        /// The ID of the learning that supersedes this one (if any).
        #[serde(skip_serializing_if = "Option::is_none")]
        superseded_by: Option<String>,
    },

    /// A reflection was completed.
    Reflection {
        /// The session where reflection occurred.
        session_id: String,
        /// Number of candidate learnings produced.
        candidates: u32,
        /// Number of learnings accepted (passed write gate).
        accepted: u32,
        /// Categories of the accepted learnings.
        categories: Vec<LearningCategory>,
        /// The ticket associated with the reflection (if any).
        #[serde(skip_serializing_if = "Option::is_none")]
        ticket_id: Option<String>,
        /// The backend that received the learnings.
        backend: String,
    },

    /// A reflection was skipped.
    Skip {
        /// The session where skip occurred.
        session_id: String,
        /// The reason for skipping.
        reason: String,
        /// Who decided to skip.
        decider: SkipDecider,
        /// Number of lines changed in the session.
        lines_changed: u32,
        /// The ticket associated with the skip (if any).
        #[serde(skip_serializing_if = "Option::is_none")]
        ticket_id: Option<String>,
        /// Files that were modified during the skipped session.
        #[serde(default)]
        context_files: Vec<String>,
    },

    /// A learning was archived due to passive decay.
    Archived {
        /// The learning that was archived.
        learning_id: String,
        /// The reason for archiving.
        reason: String,
    },

    /// A learning was restored from archived status.
    Restored {
        /// The learning that was restored.
        learning_id: String,
    },

    /// A candidate was rejected during reflection.
    Rejected {
        /// The session where rejection occurred.
        session_id: String,
        /// Summary of the rejected candidate.
        summary: String,
        /// Tags from the rejected candidate (if available).
        #[serde(default)]
        tags: Vec<String>,
        /// Why the candidate was rejected.
        reason: String,
        /// At which stage the candidate was rejected.
        stage: String,
    },
}

impl StatsEventType {
    /// Create a surfaced event.
    pub fn surfaced(
        learning_id: impl Into<String>,
        session_id: impl Into<String>,
        category: Option<LearningCategory>,
    ) -> Self {
        Self::Surfaced {
            learning_id: learning_id.into(),
            session_id: session_id.into(),
            category,
        }
    }

    /// Create a referenced event.
    pub fn referenced(
        learning_id: impl Into<String>,
        session_id: impl Into<String>,
        ticket_id: Option<String>,
    ) -> Self {
        Self::Referenced {
            learning_id: learning_id.into(),
            session_id: session_id.into(),
            ticket_id,
        }
    }

    /// Create a dismissed event.
    pub fn dismissed(learning_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self::Dismissed {
            learning_id: learning_id.into(),
            session_id: session_id.into(),
        }
    }

    /// Create a corrected event.
    pub fn corrected(
        learning_id: impl Into<String>,
        session_id: impl Into<String>,
        superseded_by: Option<String>,
    ) -> Self {
        Self::Corrected {
            learning_id: learning_id.into(),
            session_id: session_id.into(),
            superseded_by,
        }
    }

    /// Create a reflection event.
    pub fn reflection(
        session_id: impl Into<String>,
        candidates: u32,
        accepted: u32,
        categories: Vec<LearningCategory>,
        ticket_id: Option<String>,
        backend: impl Into<String>,
    ) -> Self {
        Self::Reflection {
            session_id: session_id.into(),
            candidates,
            accepted,
            categories,
            ticket_id,
            backend: backend.into(),
        }
    }

    /// Create a skip event.
    pub fn skip(
        session_id: impl Into<String>,
        reason: impl Into<String>,
        decider: SkipDecider,
        lines_changed: u32,
        ticket_id: Option<String>,
    ) -> Self {
        Self::Skip {
            session_id: session_id.into(),
            reason: reason.into(),
            decider,
            lines_changed,
            ticket_id,
            context_files: Vec::new(),
        }
    }

    /// Create a skip event with context files.
    pub fn skip_with_files(
        session_id: impl Into<String>,
        reason: impl Into<String>,
        decider: SkipDecider,
        lines_changed: u32,
        ticket_id: Option<String>,
        context_files: Vec<String>,
    ) -> Self {
        Self::Skip {
            session_id: session_id.into(),
            reason: reason.into(),
            decider,
            lines_changed,
            ticket_id,
            context_files,
        }
    }

    /// Create an archived event.
    pub fn archived(learning_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Archived {
            learning_id: learning_id.into(),
            reason: reason.into(),
        }
    }

    /// Create a restored event.
    pub fn restored(learning_id: impl Into<String>) -> Self {
        Self::Restored {
            learning_id: learning_id.into(),
        }
    }

    /// Create a rejected event.
    pub fn rejected(
        session_id: impl Into<String>,
        summary: impl Into<String>,
        tags: Vec<String>,
        reason: impl Into<String>,
        stage: impl Into<String>,
    ) -> Self {
        Self::Rejected {
            session_id: session_id.into(),
            summary: summary.into(),
            tags,
            reason: reason.into(),
            stage: stage.into(),
        }
    }

    /// Get the event name as a string.
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::Surfaced { .. } => "surfaced",
            Self::Referenced { .. } => "referenced",
            Self::Dismissed { .. } => "dismissed",
            Self::Corrected { .. } => "corrected",
            Self::Reflection { .. } => "reflection",
            Self::Skip { .. } => "skip",
            Self::Archived { .. } => "archived",
            Self::Restored { .. } => "restored",
            Self::Rejected { .. } => "rejected",
        }
    }
}

/// JSONL log writer for stats events.
///
/// Appends events to `.grove/stats.log` in JSONL format.
#[derive(Debug, Clone)]
pub struct StatsLogger {
    /// Path to the stats log file.
    path: PathBuf,
}

impl StatsLogger {
    /// Create a new stats logger with the given path.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Append an event to the log.
    pub fn append(&self, event: &StatsEvent) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                GroveError::backend(format!(
                    "Failed to create directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        // Serialize event to JSON with newline for atomic append
        let mut line = serde_json::to_string(event)
            .map_err(|e| GroveError::serde(format!("Failed to serialize stats event: {}", e)))?;
        line.push('\n');

        // Append to file with a single write_all call for atomicity
        // Using O_APPEND + single write_all ensures the entire line is written atomically
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| {
                GroveError::backend(format!(
                    "Failed to open stats log {}: {}",
                    self.path.display(),
                    e
                ))
            })?;

        file.write_all(line.as_bytes()).map_err(|e| {
            GroveError::backend(format!(
                "Failed to write to stats log {}: {}",
                self.path.display(),
                e
            ))
        })?;

        Ok(())
    }

    /// Append a surfaced event.
    pub fn append_surfaced(
        &self,
        learning_id: impl Into<String>,
        session_id: impl Into<String>,
        category: Option<LearningCategory>,
    ) -> Result<()> {
        let event = StatsEvent::new(StatsEventType::surfaced(learning_id, session_id, category));
        self.append(&event)
    }

    /// Append a referenced event.
    pub fn append_referenced(
        &self,
        learning_id: impl Into<String>,
        session_id: impl Into<String>,
        ticket_id: Option<String>,
    ) -> Result<()> {
        let event = StatsEvent::new(StatsEventType::referenced(
            learning_id,
            session_id,
            ticket_id,
        ));
        self.append(&event)
    }

    /// Append a dismissed event.
    pub fn append_dismissed(
        &self,
        learning_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<()> {
        let event = StatsEvent::new(StatsEventType::dismissed(learning_id, session_id));
        self.append(&event)
    }

    /// Append a corrected event.
    pub fn append_corrected(
        &self,
        learning_id: impl Into<String>,
        session_id: impl Into<String>,
        superseded_by: Option<String>,
    ) -> Result<()> {
        let event = StatsEvent::new(StatsEventType::corrected(
            learning_id,
            session_id,
            superseded_by,
        ));
        self.append(&event)
    }

    /// Append a reflection event.
    pub fn append_reflection(
        &self,
        session_id: impl Into<String>,
        candidates: u32,
        accepted: u32,
        categories: Vec<LearningCategory>,
        ticket_id: Option<String>,
        backend: impl Into<String>,
    ) -> Result<()> {
        let event = StatsEvent::new(StatsEventType::reflection(
            session_id, candidates, accepted, categories, ticket_id, backend,
        ));
        self.append(&event)
    }

    /// Append a skip event.
    pub fn append_skip(
        &self,
        session_id: impl Into<String>,
        reason: impl Into<String>,
        decider: SkipDecider,
        lines_changed: u32,
        ticket_id: Option<String>,
    ) -> Result<()> {
        let event = StatsEvent::new(StatsEventType::skip(
            session_id,
            reason,
            decider,
            lines_changed,
            ticket_id,
        ));
        self.append(&event)
    }

    /// Append an archived event.
    pub fn append_archived(
        &self,
        learning_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<()> {
        let event = StatsEvent::new(StatsEventType::archived(learning_id, reason));
        self.append(&event)
    }

    /// Append a restored event.
    pub fn append_restored(&self, learning_id: impl Into<String>) -> Result<()> {
        let event = StatsEvent::new(StatsEventType::restored(learning_id));
        self.append(&event)
    }

    /// Append a rejected event.
    pub fn append_rejected(
        &self,
        session_id: impl Into<String>,
        summary: impl Into<String>,
        tags: Vec<String>,
        reason: impl Into<String>,
        stage: impl Into<String>,
    ) -> Result<()> {
        let event = StatsEvent::new(StatsEventType::rejected(
            session_id, summary, tags, reason, stage,
        ));
        self.append(&event)
    }

    /// Read all events from the log.
    pub fn read_all(&self) -> Result<Vec<StatsEvent>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let content = read_to_string_limited(&self.path)?;

        let mut events = Vec::new();
        for (line_num, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }

            let event: StatsEvent = serde_json::from_str(line).map_err(|e| {
                GroveError::serde(format!(
                    "Failed to parse stats event on line {}: {}",
                    line_num + 1,
                    e
                ))
            })?;
            events.push(event);
        }

        Ok(events)
    }

    /// Count the number of events in the log.
    pub fn count(&self) -> Result<usize> {
        if !self.path.exists() {
            return Ok(0);
        }

        let content = read_to_string_limited(&self.path)?;

        Ok(content.lines().filter(|l| !l.trim().is_empty()).count())
    }

    /// Get the path to the log file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // StatsEventType factory tests

    #[test]
    fn test_surfaced_event() {
        let event =
            StatsEventType::surfaced("L001", "session-123", Some(LearningCategory::Pattern));
        assert_eq!(event.event_name(), "surfaced");

        if let StatsEventType::Surfaced {
            learning_id,
            session_id,
            category,
        } = event
        {
            assert_eq!(learning_id, "L001");
            assert_eq!(session_id, "session-123");
            assert_eq!(category, Some(LearningCategory::Pattern));
        } else {
            panic!("Expected Surfaced event");
        }
    }

    #[test]
    fn test_surfaced_event_without_category() {
        let event = StatsEventType::surfaced("L001", "session-123", None);
        assert_eq!(event.event_name(), "surfaced");

        if let StatsEventType::Surfaced { category, .. } = event {
            assert_eq!(category, None);
        } else {
            panic!("Expected Surfaced event");
        }
    }

    #[test]
    fn test_referenced_event() {
        let event = StatsEventType::referenced("L001", "session-123", Some("T042".to_string()));
        assert_eq!(event.event_name(), "referenced");

        if let StatsEventType::Referenced {
            learning_id,
            session_id,
            ticket_id,
        } = event
        {
            assert_eq!(learning_id, "L001");
            assert_eq!(session_id, "session-123");
            assert_eq!(ticket_id, Some("T042".to_string()));
        } else {
            panic!("Expected Referenced event");
        }
    }

    #[test]
    fn test_referenced_event_without_ticket() {
        let event = StatsEventType::referenced("L001", "session-123", None);

        if let StatsEventType::Referenced { ticket_id, .. } = event {
            assert!(ticket_id.is_none());
        } else {
            panic!("Expected Referenced event");
        }
    }

    #[test]
    fn test_dismissed_event() {
        let event = StatsEventType::dismissed("L003", "session-123");
        assert_eq!(event.event_name(), "dismissed");

        if let StatsEventType::Dismissed {
            learning_id,
            session_id,
        } = event
        {
            assert_eq!(learning_id, "L003");
            assert_eq!(session_id, "session-123");
        } else {
            panic!("Expected Dismissed event");
        }
    }

    #[test]
    fn test_corrected_event() {
        let event = StatsEventType::corrected("L005", "session-123", Some("L012".to_string()));
        assert_eq!(event.event_name(), "corrected");

        if let StatsEventType::Corrected {
            learning_id,
            session_id,
            superseded_by,
        } = event
        {
            assert_eq!(learning_id, "L005");
            assert_eq!(session_id, "session-123");
            assert_eq!(superseded_by, Some("L012".to_string()));
        } else {
            panic!("Expected Corrected event");
        }
    }

    #[test]
    fn test_reflection_event() {
        let categories = vec![LearningCategory::Pitfall, LearningCategory::Pattern];
        let event = StatsEventType::reflection(
            "session-123",
            5,
            3,
            categories.clone(),
            Some("T042".to_string()),
            "markdown",
        );
        assert_eq!(event.event_name(), "reflection");

        if let StatsEventType::Reflection {
            session_id,
            candidates,
            accepted,
            categories: cats,
            ticket_id,
            backend,
        } = event
        {
            assert_eq!(session_id, "session-123");
            assert_eq!(candidates, 5);
            assert_eq!(accepted, 3);
            assert_eq!(cats, categories);
            assert_eq!(ticket_id, Some("T042".to_string()));
            assert_eq!(backend, "markdown");
        } else {
            panic!("Expected Reflection event");
        }
    }

    #[test]
    fn test_skip_event() {
        let event = StatsEventType::skip(
            "session-123",
            "auto: 2 lines, version bump",
            SkipDecider::Agent,
            2,
            Some("T042".to_string()),
        );
        assert_eq!(event.event_name(), "skip");

        if let StatsEventType::Skip {
            session_id,
            reason,
            decider,
            lines_changed,
            ticket_id,
            context_files,
        } = event
        {
            assert_eq!(session_id, "session-123");
            assert_eq!(reason, "auto: 2 lines, version bump");
            assert_eq!(decider, SkipDecider::Agent);
            assert_eq!(lines_changed, 2);
            assert_eq!(ticket_id, Some("T042".to_string()));
            assert!(context_files.is_empty());
        } else {
            panic!("Expected Skip event");
        }
    }

    #[test]
    fn test_skip_event_with_files() {
        let event = StatsEventType::skip_with_files(
            "session-456",
            "no learnings",
            SkipDecider::User,
            10,
            None,
            vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
        );
        assert_eq!(event.event_name(), "skip");

        if let StatsEventType::Skip { context_files, .. } = event {
            assert_eq!(context_files.len(), 2);
            assert!(context_files.contains(&"src/main.rs".to_string()));
            assert!(context_files.contains(&"src/lib.rs".to_string()));
        } else {
            panic!("Expected Skip event");
        }
    }

    #[test]
    fn test_archived_event() {
        let event = StatsEventType::archived("L002", "passive_decay");
        assert_eq!(event.event_name(), "archived");

        if let StatsEventType::Archived {
            learning_id,
            reason,
        } = event
        {
            assert_eq!(learning_id, "L002");
            assert_eq!(reason, "passive_decay");
        } else {
            panic!("Expected Archived event");
        }
    }

    #[test]
    fn test_restored_event() {
        let event = StatsEventType::restored("L002");
        assert_eq!(event.event_name(), "restored");

        if let StatsEventType::Restored { learning_id } = event {
            assert_eq!(learning_id, "L002");
        } else {
            panic!("Expected Restored event");
        }
    }

    #[test]
    fn test_rejected_event() {
        let event = StatsEventType::rejected(
            "session-1",
            "test summary",
            vec!["tag1".to_string(), "tag2".to_string()],
            "schema_validation",
            "Schema",
        );
        assert_eq!(event.event_name(), "rejected");

        if let StatsEventType::Rejected {
            session_id,
            summary,
            tags,
            reason,
            stage,
        } = event
        {
            assert_eq!(session_id, "session-1");
            assert_eq!(summary, "test summary");
            assert_eq!(tags, vec!["tag1".to_string(), "tag2".to_string()]);
            assert_eq!(reason, "schema_validation");
            assert_eq!(stage, "Schema");
        } else {
            panic!("Expected Rejected event");
        }
    }

    // StatsEvent tests

    #[test]
    fn test_stats_event_new() {
        let event = StatsEvent::new(StatsEventType::surfaced("L001", "s1", None));
        assert_eq!(event.v, STATS_SCHEMA_VERSION);
        assert!(event.ts <= Utc::now());
    }

    #[test]
    fn test_stats_event_with_timestamp() {
        let ts = Utc::now();
        let event = StatsEvent::with_timestamp(StatsEventType::surfaced("L001", "s1", None), ts);
        assert_eq!(event.v, STATS_SCHEMA_VERSION);
        assert_eq!(event.ts, ts);
    }

    // Serialization tests

    #[test]
    fn test_surfaced_serialization() {
        let event = StatsEvent::new(StatsEventType::surfaced(
            "L001",
            "abc",
            Some(LearningCategory::Pattern),
        ));
        let json = serde_json::to_string(&event).unwrap();

        assert!(json.contains(r#""event":"surfaced""#));
        assert!(json.contains(r#""learning_id":"L001""#));
        assert!(json.contains(r#""session_id":"abc""#));
        assert!(json.contains(r#""category":"pattern""#));
        assert!(json.contains(&format!(r#""v":{}"#, STATS_SCHEMA_VERSION)));

        // Deserialize back
        let parsed: StatsEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.v, event.v);
        assert_eq!(parsed.data, event.data);
    }

    #[test]
    fn test_referenced_serialization_with_ticket() {
        let event = StatsEvent::new(StatsEventType::referenced(
            "L001",
            "abc",
            Some("T042".to_string()),
        ));
        let json = serde_json::to_string(&event).unwrap();

        assert!(json.contains(r#""event":"referenced""#));
        assert!(json.contains(r#""ticket_id":"T042""#));

        let parsed: StatsEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.data, event.data);
    }

    #[test]
    fn test_referenced_serialization_without_ticket() {
        let event = StatsEvent::new(StatsEventType::referenced("L001", "abc", None));
        let json = serde_json::to_string(&event).unwrap();

        // ticket_id should not be present when None
        assert!(!json.contains("ticket_id"));

        let parsed: StatsEvent = serde_json::from_str(&json).unwrap();
        if let StatsEventType::Referenced { ticket_id, .. } = &parsed.data {
            assert!(ticket_id.is_none());
        }
    }

    #[test]
    fn test_reflection_serialization() {
        let event = StatsEvent::new(StatsEventType::reflection(
            "abc",
            5,
            3,
            vec![LearningCategory::Pitfall, LearningCategory::Pattern],
            Some("T042".to_string()),
            "markdown",
        ));
        let json = serde_json::to_string(&event).unwrap();

        assert!(json.contains(r#""event":"reflection""#));
        assert!(json.contains(r#""candidates":5"#));
        assert!(json.contains(r#""accepted":3"#));
        assert!(json.contains(r#""backend":"markdown""#));
        assert!(json.contains(r#""categories""#));

        let parsed: StatsEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.data, event.data);
    }

    #[test]
    fn test_skip_serialization() {
        let event = StatsEvent::new(StatsEventType::skip(
            "abc",
            "auto: 2 lines",
            SkipDecider::Agent,
            2,
            None,
        ));
        let json = serde_json::to_string(&event).unwrap();

        assert!(json.contains(r#""event":"skip""#));
        assert!(json.contains(r#""reason":"auto: 2 lines""#));
        assert!(json.contains(r#""decider":"agent""#));
        assert!(json.contains(r#""lines_changed":2"#));

        let parsed: StatsEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.data, event.data);
    }

    // StatsLogger tests

    #[test]
    fn test_logger_append_and_read() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("stats.log");
        let logger = StatsLogger::new(&path);

        logger.append_surfaced("L001", "s1", None).unwrap();
        logger.append_referenced("L001", "s1", None).unwrap();

        let events = logger.read_all().unwrap();
        assert_eq!(events.len(), 2);

        assert_eq!(events[0].data.event_name(), "surfaced");
        assert_eq!(events[1].data.event_name(), "referenced");
    }

    #[test]
    fn test_logger_count() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("stats.log");
        let logger = StatsLogger::new(&path);

        assert_eq!(logger.count().unwrap(), 0);

        logger.append_surfaced("L001", "s1", None).unwrap();
        assert_eq!(logger.count().unwrap(), 1);

        logger.append_dismissed("L002", "s1").unwrap();
        assert_eq!(logger.count().unwrap(), 2);
    }

    #[test]
    fn test_logger_read_empty() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("stats.log");
        let logger = StatsLogger::new(&path);

        let events = logger.read_all().unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_logger_creates_directory() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("subdir").join("stats.log");
        let logger = StatsLogger::new(&path);

        logger.append_surfaced("L001", "s1", None).unwrap();

        assert!(path.exists());
    }

    #[test]
    fn test_logger_append_all_event_types() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("stats.log");
        let logger = StatsLogger::new(&path);

        logger.append_surfaced("L001", "s1", None).unwrap();
        logger
            .append_referenced("L001", "s1", Some("T001".to_string()))
            .unwrap();
        logger.append_dismissed("L002", "s1").unwrap();
        logger
            .append_corrected("L003", "s1", Some("L004".to_string()))
            .unwrap();
        logger
            .append_reflection(
                "s1",
                5,
                3,
                vec![LearningCategory::Pitfall],
                Some("T001".to_string()),
                "markdown",
            )
            .unwrap();
        logger
            .append_skip("s1", "too small", SkipDecider::Agent, 2, None)
            .unwrap();
        logger.append_archived("L005", "passive_decay").unwrap();
        logger.append_restored("L005").unwrap();

        let events = logger.read_all().unwrap();
        assert_eq!(events.len(), 8);

        // Verify each event type
        assert_eq!(events[0].data.event_name(), "surfaced");
        assert_eq!(events[1].data.event_name(), "referenced");
        assert_eq!(events[2].data.event_name(), "dismissed");
        assert_eq!(events[3].data.event_name(), "corrected");
        assert_eq!(events[4].data.event_name(), "reflection");
        assert_eq!(events[5].data.event_name(), "skip");
        assert_eq!(events[6].data.event_name(), "archived");
        assert_eq!(events[7].data.event_name(), "restored");
    }

    #[test]
    fn test_logger_preserves_event_data() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("stats.log");
        let logger = StatsLogger::new(&path);

        logger
            .append_reflection(
                "session-xyz",
                10,
                7,
                vec![LearningCategory::Pattern, LearningCategory::Convention],
                Some("ISSUE-123".to_string()),
                "total_recall",
            )
            .unwrap();

        let events = logger.read_all().unwrap();
        assert_eq!(events.len(), 1);

        if let StatsEventType::Reflection {
            session_id,
            candidates,
            accepted,
            categories,
            ticket_id,
            backend,
        } = &events[0].data
        {
            assert_eq!(session_id, "session-xyz");
            assert_eq!(*candidates, 10);
            assert_eq!(*accepted, 7);
            assert_eq!(categories.len(), 2);
            assert_eq!(ticket_id, &Some("ISSUE-123".to_string()));
            assert_eq!(backend, "total_recall");
        } else {
            panic!("Expected Reflection event");
        }
    }

    #[test]
    fn test_logger_path() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("stats.log");
        let logger = StatsLogger::new(&path);

        assert_eq!(logger.path(), path);
    }

    #[test]
    fn test_schema_version_in_events() {
        let event = StatsEvent::new(StatsEventType::surfaced("L001", "s1", None));
        assert_eq!(event.v, 1);

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""v":1"#));
    }

    // File size limit tests

    #[test]
    fn test_read_all_rejects_oversized_file() {
        use crate::util::MAX_FILE_SIZE;
        use std::io::Write;

        let temp = TempDir::new().unwrap();
        let path = temp.path().join("large_stats.log");

        // Create a file larger than MAX_FILE_SIZE
        let mut file = std::fs::File::create(&path).unwrap();
        // Write more than MAX_FILE_SIZE bytes
        let chunk = vec![b'x'; 1024 * 1024]; // 1 MB chunk
        for _ in 0..(MAX_FILE_SIZE / (1024 * 1024) + 1) {
            file.write_all(&chunk).unwrap();
        }
        file.sync_all().unwrap();

        let logger = StatsLogger::new(&path);
        let result = logger.read_all();

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("too large"),
            "Error should mention file is too large: {}",
            err
        );
    }

    #[test]
    fn test_count_rejects_oversized_file() {
        use crate::util::MAX_FILE_SIZE;
        use std::io::Write;

        let temp = TempDir::new().unwrap();
        let path = temp.path().join("large_stats.log");

        // Create a file larger than MAX_FILE_SIZE
        let mut file = std::fs::File::create(&path).unwrap();
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..(MAX_FILE_SIZE / (1024 * 1024) + 1) {
            file.write_all(&chunk).unwrap();
        }
        file.sync_all().unwrap();

        let logger = StatsLogger::new(&path);
        let result = logger.count();

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("too large"),
            "Error should mention file is too large: {}",
            err
        );
    }

    #[test]
    fn test_read_all_accepts_normal_sized_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("stats.log");
        let logger = StatsLogger::new(&path);

        // Write several events (well under limit)
        for _ in 0..100 {
            logger.append_surfaced("L001", "s1", None).unwrap();
        }

        // Should successfully read
        let events = logger.read_all().unwrap();
        assert_eq!(events.len(), 100);
    }
}
