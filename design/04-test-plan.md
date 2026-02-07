# grove - Test Plan

This document describes the testing strategy for grove, covering unit tests,
integration tests, property-based tests, and simulation harnesses.

## 1. Testing Strategy

### 1.1 Phase 1: With Implementation

The initial testing phase focuses on **unit tests** for essential functionality:

- Hook I/O parsing (malformed JSON, missing fields)
- Gate state machine transitions
- Circuit breaker edge cases
- Write gate filter criteria validation
- Learning schema validation
- Stats event log parsing

**Integration tests** target the file backend and session storage:

- Atomic write / read cycle
- Markdown backend parse/write/archive
- Session listing and cleanup
- Stats log append and cache rebuild

An **in-memory backend** supports unit testing:

```rust
struct MemoryBackend {
    learnings: RefCell<HashMap<String, CompoundLearning>>,
}

struct MemorySessionStore {
    sessions: RefCell<HashMap<String, SessionState>>,
}
```

### 1.2 Phase 2: After MVP

**Property-based tests** using `proptest`:

- State machine: random event sequences never panic, always valid state
- Serialization: round-trip through JSON for all core types
- Stats log: arbitrary event sequences produce consistent cache

**Hook simulation harness**:

```bash
# Simulate Claude Code invoking hooks
echo '{"session_id":"test","cwd":"/project","source":"startup"}' \
  | grove hook session-start

echo '{"session_id":"test","tool_name":"Bash","tool_input":{"command":"tissue status 1 closed"}}' \
  | grove hook pre-tool-use

echo '{"session_id":"test"}' \
  | grove hook stop \
  | jq -e '.decision'
```

### 1.3 Out of Scope

- Mocking Claude Code itself
- Testing external backend integrations (Total Recall, MCP) beyond interface conformance
- Performance benchmarking (deferred until real usage patterns emerge)

## 2. Test Categories

### 2.1 Unit Tests

| Category | Tests | Priority |
|----------|-------|----------|
| Hook I/O parsing | Valid JSON, malformed JSON, missing required fields, extra fields | High |
| Gate transitions | Idle->Active, Active->Pending, Pending->Blocked, Blocked->Reflected, circuit breaker | High |
| Learning schema | Valid learning, missing fields, invalid category, invalid scope | High |
| Write gate filter | All 4 criteria, no criteria claimed, multiple criteria | High |
| Near-duplicate detection | Exact match, case variation, no match | High |
| Reflection parsing | Valid output, partial failure, total failure | High |
| Session serialization | Round-trip SessionState through JSON | Medium |
| Learning serialization | Round-trip CompoundLearning through JSON | Medium |
| Stats event parsing | All event types, version field handling | Medium |
| Config loading | File exists, file missing (defaults), invalid TOML | Medium |
| Markdown sanitization | Summary escaping, detail fence balancing, tag normalization | Medium |
| Ticketing detection | tissue probe, beads probe, pattern matching | Medium |
| Backend detection | markdown probe, priority ordering | Medium |
| Retrieval scoring | Tag match, file match, keyword match, combined | Medium |
| Decay evaluation | Under threshold, over threshold, immunity | Medium |

### 2.2 Integration Tests

| Category | Tests | Priority |
|----------|-------|----------|
| Session storage | Write/read cycle, atomic rename, temp file cleanup | High |
| Markdown backend | Write learning, search, archive, parse file | High |
| Stats log | Append events, rebuild cache, staleness detection | High |
| Gate flow | Full lifecycle: detect -> close -> block -> reflect -> approve | High |
| Skip flow | Block -> skip -> approve, stats recorded | High |
| Circuit breaker flow | Multiple blocks -> forced approve | High |
| Discovery flow | Ticketing + backend discovery with various project layouts | Medium |
| Session listing | Empty dir, single session, multiple sessions, sort order | Medium |
| Session cleanup | Age filter, orphan detection | Medium |
| CLI commands | `grove reflect`, `grove skip`, `grove stats`, `grove list` | Medium |

### 2.3 Property-Based Tests (Phase 2)

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn gate_state_machine_never_panics(events: Vec<GateEvent>) {
        let mut state = GateState::default();
        for event in events {
            let _ = state.handle(event);
        }
        assert!(state.is_valid());
    }

    #[test]
    fn session_state_roundtrip(state: SessionState) {
        let json = serde_json::to_string(&state).unwrap();
        let restored: SessionState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn learning_roundtrip(learning: CompoundLearning) {
        let json = serde_json::to_string(&learning).unwrap();
        let restored: CompoundLearning = serde_json::from_str(&json).unwrap();
        assert_eq!(learning, restored);
    }

    #[test]
    fn stats_cache_consistent(events: Vec<StatsEvent>) {
        let mut cache1 = StatsCache::default();
        for event in &events {
            cache1.apply(event);
        }

        let mut cache2 = StatsCache::default();
        cache2.rebuild_from_events(&events);

        assert_eq!(cache1, cache2);
    }
}
```

## 3. Core Unit Tests

### 3.1 Gate State Machine Tests

```rust
#[cfg(test)]
mod gate_tests {
    use super::*;

    #[test]
    fn idle_to_active_on_ticket_detected() {
        let mut state = GateState::new();
        assert_eq!(state.status, GateStatus::Idle);

        state.handle(GateEvent::TicketDetected {
            ticket_id: "T001".into(),
            source: TicketSource::Tissue,
        });

        assert_eq!(state.status, GateStatus::Active);
    }

    #[test]
    fn active_to_pending_on_ticket_closed() {
        let mut state = GateState::new();
        state.status = GateStatus::Active;
        state.ticket = Some(TicketContext {
            ticket_id: "T001".into(),
            source: TicketSource::Tissue,
            title: "Fix bug".into(),
            description: None,
            detected_at: Utc::now(),
        });

        state.handle(GateEvent::TicketClosed);

        assert_eq!(state.status, GateStatus::Pending);
    }

    #[test]
    fn pending_to_blocked_on_stop() {
        let mut state = GateState::new();
        state.status = GateStatus::Pending;

        let result = state.handle(GateEvent::StopHook);

        assert_eq!(state.status, GateStatus::Blocked);
        assert_eq!(state.block_count, 1);
        assert!(matches!(result, GateDecision::Block { .. }));
    }

    #[test]
    fn blocked_to_reflected_on_reflection() {
        let mut state = GateState::new();
        state.status = GateStatus::Blocked;

        state.handle(GateEvent::ReflectionComplete {
            learnings: vec![],
            rejected: vec![],
        });

        assert_eq!(state.status, GateStatus::Reflected);
    }

    #[test]
    fn blocked_to_skipped_on_skip() {
        let mut state = GateState::new();
        state.status = GateStatus::Blocked;

        state.handle(GateEvent::Skip {
            reason: "typo fix".into(),
            decider: SkipDecider::Agent,
        });

        assert_eq!(state.status, GateStatus::Skipped);
    }

    #[test]
    fn circuit_breaker_trips_after_max_blocks() {
        let mut state = GateState::new();
        state.status = GateStatus::Pending;
        state.block_count = 2; // max_blocks default is 3

        let result = state.handle(GateEvent::StopHook);
        assert!(matches!(result, GateDecision::Block { .. }));
        assert_eq!(state.block_count, 3);

        // Next stop should trip breaker
        let result = state.handle(GateEvent::StopHook);
        assert!(matches!(result, GateDecision::Approve { forced: true, .. }));
        assert!(state.circuit_breaker_tripped);
    }

    #[test]
    fn circuit_breaker_resets_on_different_session() {
        let mut state = GateState::new();
        state.block_count = 3;
        state.last_blocked_session_id = Some("session-1".into());

        state.check_circuit_breaker_reset("session-2");

        assert_eq!(state.block_count, 0);
    }

    #[test]
    fn circuit_breaker_resets_on_cooldown() {
        let mut state = GateState::new();
        state.block_count = 3;
        state.last_blocked_at = Some(Utc::now() - Duration::seconds(400));

        state.check_circuit_breaker_reset_cooldown(300);

        assert_eq!(state.block_count, 0);
    }

    #[test]
    fn reflected_state_approves_stop() {
        let mut state = GateState::new();
        state.status = GateStatus::Reflected;

        let result = state.handle(GateEvent::StopHook);

        assert!(matches!(result, GateDecision::Approve { forced: false, .. }));
    }

    #[test]
    fn skipped_state_approves_stop() {
        let mut state = GateState::new();
        state.status = GateStatus::Skipped;

        let result = state.handle(GateEvent::StopHook);

        assert!(matches!(result, GateDecision::Approve { forced: false, .. }));
    }
}
```

### 3.2 Write Gate Filter Tests

```rust
#[cfg(test)]
mod write_gate_tests {
    use super::*;

    #[test]
    fn accepts_behavior_changing_criterion() {
        let candidate = LearningCandidate {
            category: LearningCategory::Pitfall,
            summary: "Always check for null before accessing properties".into(),
            detail: "Found a null pointer exception...".into(),
            criteria_met: vec![WriteGateCriterion::BehaviorChanging],
            tags: vec!["null-safety".into()],
            scope: LearningScope::Project,
        };

        let result = write_gate_filter(&candidate);
        assert!(result.passed);
    }

    #[test]
    fn accepts_decision_rationale_criterion() {
        let candidate = LearningCandidate {
            category: LearningCategory::Pattern,
            summary: "Use repository pattern for data access".into(),
            detail: "Chose repository over active record because...".into(),
            criteria_met: vec![WriteGateCriterion::DecisionRationale],
            tags: vec!["architecture".into()],
            scope: LearningScope::Project,
        };

        let result = write_gate_filter(&candidate);
        assert!(result.passed);
    }

    #[test]
    fn accepts_stable_fact_criterion() {
        let candidate = LearningCandidate {
            category: LearningCategory::Dependency,
            summary: "Redis requires SCAN for large keyspace iteration".into(),
            detail: "KEYS command blocks the server...".into(),
            criteria_met: vec![WriteGateCriterion::StableFact],
            tags: vec!["redis".into()],
            scope: LearningScope::Project,
        };

        let result = write_gate_filter(&candidate);
        assert!(result.passed);
    }

    #[test]
    fn accepts_explicit_request_criterion() {
        let candidate = LearningCandidate {
            category: LearningCategory::Convention,
            summary: "Use kebab-case for API endpoints".into(),
            detail: "Team decided on kebab-case...".into(),
            criteria_met: vec![WriteGateCriterion::ExplicitRequest],
            tags: vec!["api".into()],
            scope: LearningScope::Project,
        };

        let result = write_gate_filter(&candidate);
        assert!(result.passed);
    }

    #[test]
    fn rejects_no_criteria() {
        let candidate = LearningCandidate {
            category: LearningCategory::Domain,
            summary: "The codebase uses Rust".into(),
            detail: "This project is written in Rust...".into(),
            criteria_met: vec![],
            tags: vec!["general".into()],
            scope: LearningScope::Project,
        };

        let result = write_gate_filter(&candidate);
        assert!(!result.passed);
    }

    #[test]
    fn accepts_multiple_criteria() {
        let candidate = LearningCandidate {
            category: LearningCategory::Pitfall,
            summary: "Avoid N+1 queries in GraphQL resolvers".into(),
            detail: "Use dataloader pattern to batch...".into(),
            criteria_met: vec![
                WriteGateCriterion::BehaviorChanging,
                WriteGateCriterion::StableFact,
            ],
            tags: vec!["graphql".into(), "performance".into()],
            scope: LearningScope::Project,
        };

        let result = write_gate_filter(&candidate);
        assert!(result.passed);
    }
}
```

### 3.3 Schema Validation Tests

```rust
#[cfg(test)]
mod schema_validation_tests {
    use super::*;

    #[test]
    fn valid_learning_passes() {
        let candidate = LearningCandidate {
            category: LearningCategory::Pattern,
            summary: "This is a valid summary that meets length requirements".into(),
            detail: "This is a valid detail that is longer than the summary and provides context".into(),
            criteria_met: vec![WriteGateCriterion::BehaviorChanging],
            tags: vec!["valid-tag".into()],
            scope: LearningScope::Project,
        };

        let result = validate_schema(&candidate);
        assert!(result.is_ok());
    }

    #[test]
    fn rejects_short_summary() {
        let candidate = LearningCandidate {
            category: LearningCategory::Pattern,
            summary: "Short".into(), // < 10 chars
            detail: "This detail is long enough to pass validation".into(),
            criteria_met: vec![WriteGateCriterion::BehaviorChanging],
            tags: vec!["tag".into()],
            scope: LearningScope::Project,
        };

        let result = validate_schema(&candidate);
        assert!(matches!(result, Err(SchemaValidationError::SummaryTooShort)));
    }

    #[test]
    fn rejects_long_summary() {
        let candidate = LearningCandidate {
            category: LearningCategory::Pattern,
            summary: "x".repeat(201), // > 200 chars
            detail: "This detail is valid".into(),
            criteria_met: vec![WriteGateCriterion::BehaviorChanging],
            tags: vec!["tag".into()],
            scope: LearningScope::Project,
        };

        let result = validate_schema(&candidate);
        assert!(matches!(result, Err(SchemaValidationError::SummaryTooLong)));
    }

    #[test]
    fn rejects_short_detail() {
        let candidate = LearningCandidate {
            category: LearningCategory::Pattern,
            summary: "This summary is valid and long enough".into(),
            detail: "Too short".into(), // < 20 chars
            criteria_met: vec![WriteGateCriterion::BehaviorChanging],
            tags: vec!["tag".into()],
            scope: LearningScope::Project,
        };

        let result = validate_schema(&candidate);
        assert!(matches!(result, Err(SchemaValidationError::DetailTooShort)));
    }

    #[test]
    fn rejects_identical_summary_and_detail() {
        let text = "This text is used for both summary and detail fields";
        let candidate = LearningCandidate {
            category: LearningCategory::Pattern,
            summary: text.into(),
            detail: text.into(),
            criteria_met: vec![WriteGateCriterion::BehaviorChanging],
            tags: vec!["tag".into()],
            scope: LearningScope::Project,
        };

        let result = validate_schema(&candidate);
        assert!(matches!(result, Err(SchemaValidationError::SummaryEqualsDetail)));
    }

    #[test]
    fn rejects_empty_tags() {
        let candidate = LearningCandidate {
            category: LearningCategory::Pattern,
            summary: "This summary is valid and long enough".into(),
            detail: "This detail is also valid and provides context".into(),
            criteria_met: vec![WriteGateCriterion::BehaviorChanging],
            tags: vec![],
            scope: LearningScope::Project,
        };

        let result = validate_schema(&candidate);
        assert!(matches!(result, Err(SchemaValidationError::NoTags)));
    }

    #[test]
    fn rejects_too_many_tags() {
        let candidate = LearningCandidate {
            category: LearningCategory::Pattern,
            summary: "This summary is valid and long enough".into(),
            detail: "This detail is also valid and provides context".into(),
            criteria_met: vec![WriteGateCriterion::BehaviorChanging],
            tags: (0..11).map(|i| format!("tag-{}", i)).collect(),
            scope: LearningScope::Project,
        };

        let result = validate_schema(&candidate);
        assert!(matches!(result, Err(SchemaValidationError::TooManyTags)));
    }

    #[test]
    fn rejects_no_criteria_claimed() {
        let candidate = LearningCandidate {
            category: LearningCategory::Pattern,
            summary: "This summary is valid and long enough".into(),
            detail: "This detail is also valid and provides context".into(),
            criteria_met: vec![],
            tags: vec!["tag".into()],
            scope: LearningScope::Project,
        };

        let result = validate_schema(&candidate);
        assert!(matches!(result, Err(SchemaValidationError::NoCriteriaClaimed)));
    }
}
```

### 3.4 Near-Duplicate Detection Tests

```rust
#[cfg(test)]
mod duplicate_tests {
    use super::*;

    #[test]
    fn detects_exact_duplicate() {
        let existing = vec![
            CompoundLearning {
                id: "L001".into(),
                summary: "Always validate user input".into(),
                status: LearningStatus::Active,
                ..Default::default()
            },
        ];

        let candidate = LearningCandidate {
            summary: "Always validate user input".into(),
            ..Default::default()
        };

        let result = check_near_duplicate(&candidate, &existing);
        assert!(result.is_duplicate);
        assert_eq!(result.duplicate_of, Some("L001".into()));
    }

    #[test]
    fn detects_case_insensitive_duplicate() {
        let existing = vec![
            CompoundLearning {
                id: "L001".into(),
                summary: "Always Validate User Input".into(),
                status: LearningStatus::Active,
                ..Default::default()
            },
        ];

        let candidate = LearningCandidate {
            summary: "always validate user input".into(),
            ..Default::default()
        };

        let result = check_near_duplicate(&candidate, &existing);
        assert!(result.is_duplicate);
    }

    #[test]
    fn ignores_archived_learnings() {
        let existing = vec![
            CompoundLearning {
                id: "L001".into(),
                summary: "Always validate user input".into(),
                status: LearningStatus::Archived,
                ..Default::default()
            },
        ];

        let candidate = LearningCandidate {
            summary: "Always validate user input".into(),
            ..Default::default()
        };

        let result = check_near_duplicate(&candidate, &existing);
        assert!(!result.is_duplicate);
    }

    #[test]
    fn allows_different_summaries() {
        let existing = vec![
            CompoundLearning {
                id: "L001".into(),
                summary: "Always validate user input".into(),
                status: LearningStatus::Active,
                ..Default::default()
            },
        ];

        let candidate = LearningCandidate {
            summary: "Use parameterized queries for SQL".into(),
            ..Default::default()
        };

        let result = check_near_duplicate(&candidate, &existing);
        assert!(!result.is_duplicate);
    }
}
```

### 3.5 Stats Event Tests

```rust
#[cfg(test)]
mod stats_tests {
    use super::*;

    #[test]
    fn parses_surfaced_event() {
        let json = r#"{"v":1,"ts":"2026-02-06T10:00:00Z","event":"surfaced","learning_id":"L001","session_id":"abc"}"#;
        let event: StatsEvent = serde_json::from_str(json).unwrap();

        assert_eq!(event.version, 1);
        assert!(matches!(event.event_type, StatsEventType::Surfaced { .. }));
    }

    #[test]
    fn parses_referenced_event() {
        let json = r#"{"v":1,"ts":"2026-02-06T10:00:00Z","event":"referenced","learning_id":"L001","session_id":"abc","ticket_id":"T042"}"#;
        let event: StatsEvent = serde_json::from_str(json).unwrap();

        assert!(matches!(event.event_type, StatsEventType::Referenced { .. }));
    }

    #[test]
    fn parses_dismissed_event() {
        let json = r#"{"v":1,"ts":"2026-02-06T10:00:00Z","event":"dismissed","learning_id":"L003","session_id":"abc"}"#;
        let event: StatsEvent = serde_json::from_str(json).unwrap();

        assert!(matches!(event.event_type, StatsEventType::Dismissed { .. }));
    }

    #[test]
    fn parses_reflection_event() {
        let json = r#"{"v":1,"ts":"2026-02-06T10:00:00Z","event":"reflection","session_id":"abc","candidates":5,"accepted":3,"categories":["pitfall","pattern"],"ticket_id":"T042","backend":"markdown"}"#;
        let event: StatsEvent = serde_json::from_str(json).unwrap();

        assert!(matches!(event.event_type, StatsEventType::Reflection { .. }));
    }

    #[test]
    fn parses_skip_event() {
        let json = r#"{"v":1,"ts":"2026-02-06T10:00:00Z","event":"skip","session_id":"abc","reason":"auto: 2 lines","decider":"agent","lines_changed":2}"#;
        let event: StatsEvent = serde_json::from_str(json).unwrap();

        assert!(matches!(event.event_type, StatsEventType::Skip { .. }));
    }

    #[test]
    fn cache_calculates_hit_rate() {
        let mut cache = StatsCache::default();

        // Surface L001 twice
        cache.apply(&StatsEvent::surfaced("L001", "s1"));
        cache.apply(&StatsEvent::surfaced("L001", "s2"));

        // Reference L001 once
        cache.apply(&StatsEvent::referenced("L001", "s1", None));

        let stats = cache.learnings.get("L001").unwrap();
        assert_eq!(stats.surfaced, 2);
        assert_eq!(stats.referenced, 1);
        assert_eq!(stats.hit_rate, 0.5);
    }

    #[test]
    fn cache_tracks_cross_pollination() {
        let mut cache = StatsCache::default();

        // L001 originated from T001
        cache.apply(&StatsEvent::reflection("s1", 1, 1, vec!["pattern"], Some("T001"), "markdown"));

        // L001 referenced in T042 (different ticket)
        cache.apply(&StatsEvent::referenced("L001", "s2", Some("T042")));

        assert!(cache.cross_pollination.iter().any(|cp|
            cp.learning_id == "L001" && cp.referenced_in.contains(&"T042".to_string())
        ));
    }
}
```

## 4. Integration Tests

### 4.1 Gate Flow Integration Test

```rust
#[cfg(test)]
mod gate_flow_integration {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn full_gate_lifecycle() {
        let temp = TempDir::new().unwrap();
        let store = FileSessionStore::new(temp.path());

        // Session start
        let session_id = "test-session-001";
        let mut session = SessionState::new(session_id);
        store.put(&session).unwrap();

        // Ticket detected
        session.gate.handle(GateEvent::TicketDetected {
            ticket_id: "T001".into(),
            source: TicketSource::Tissue,
        });
        assert_eq!(session.gate.status, GateStatus::Active);
        store.put(&session).unwrap();

        // Ticket closed
        session.gate.handle(GateEvent::TicketClosed);
        assert_eq!(session.gate.status, GateStatus::Pending);
        store.put(&session).unwrap();

        // Stop hook (should block)
        let decision = session.gate.handle(GateEvent::StopHook);
        assert!(matches!(decision, GateDecision::Block { .. }));
        assert_eq!(session.gate.status, GateStatus::Blocked);
        store.put(&session).unwrap();

        // Reflection completes
        session.gate.handle(GateEvent::ReflectionComplete {
            learnings: vec![],
            rejected: vec![],
        });
        assert_eq!(session.gate.status, GateStatus::Reflected);
        store.put(&session).unwrap();

        // Stop hook (should approve)
        let decision = session.gate.handle(GateEvent::StopHook);
        assert!(matches!(decision, GateDecision::Approve { forced: false, .. }));
    }
}
```

### 4.2 Markdown Backend Integration Test

```rust
#[cfg(test)]
mod markdown_backend_integration {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_and_search_learning() {
        let temp = TempDir::new().unwrap();
        let learnings_path = temp.path().join(".grove").join("learnings.md");
        std::fs::create_dir_all(learnings_path.parent().unwrap()).unwrap();

        let backend = MarkdownBackend::new(&learnings_path);

        // Write a learning
        let learning = CompoundLearning {
            id: "cl_20260206_001".into(),
            schema_version: 1,
            category: LearningCategory::Pitfall,
            summary: "Avoid N+1 queries in UserDashboard".into(),
            detail: "The dashboard was loading users then iterating to load posts...".into(),
            scope: LearningScope::Project,
            criteria_met: vec![WriteGateCriterion::BehaviorChanging],
            tags: vec!["performance".into(), "database".into()],
            session_id: "test-session".into(),
            ticket_id: Some("T001".into()),
            timestamp: Utc::now(),
            context_files: Some(vec!["src/dashboard.rs".into()]),
            status: LearningStatus::Active,
        };

        backend.write(&learning).unwrap();

        // Verify file exists
        assert!(learnings_path.exists());

        // Search for the learning
        let query = SearchQuery {
            tags: vec!["performance".into()],
            files: vec![],
            keywords: vec![],
        };
        let results = backend.search(&query, &SearchFilters::default()).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "cl_20260206_001");
    }

    #[test]
    fn archive_learning() {
        let temp = TempDir::new().unwrap();
        let learnings_path = temp.path().join(".grove").join("learnings.md");
        std::fs::create_dir_all(learnings_path.parent().unwrap()).unwrap();

        let backend = MarkdownBackend::new(&learnings_path);

        let learning = CompoundLearning {
            id: "cl_20260206_001".into(),
            status: LearningStatus::Active,
            ..Default::default()
        };

        backend.write(&learning).unwrap();
        backend.archive("cl_20260206_001").unwrap();

        let learnings = backend.parse_learnings().unwrap();
        assert_eq!(learnings[0].status, LearningStatus::Archived);
    }
}
```

### 4.3 Stats Log Integration Test

```rust
#[cfg(test)]
mod stats_integration {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn append_and_rebuild_cache() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join(".grove").join("stats.log");
        let cache_path = temp.path().join("stats-cache.json");
        std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();

        let tracker = StatsTracker::new(&log_path, &cache_path);

        // Append events
        tracker.append_surfaced("L001", "s1").unwrap();
        tracker.append_surfaced("L001", "s2").unwrap();
        tracker.append_referenced("L001", "s1", None).unwrap();
        tracker.append_dismissed("L002", "s1").unwrap();

        // Rebuild cache
        let cache = tracker.rebuild_cache().unwrap();

        assert_eq!(cache.log_entries_processed, 4);
        assert_eq!(cache.learnings.get("L001").unwrap().hit_rate, 0.5);
        assert_eq!(cache.learnings.get("L002").unwrap().dismissed, 1);
    }

    #[test]
    fn detects_stale_cache() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join(".grove").join("stats.log");
        let cache_path = temp.path().join("stats-cache.json");
        std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();

        let tracker = StatsTracker::new(&log_path, &cache_path);

        // Create initial cache
        tracker.append_surfaced("L001", "s1").unwrap();
        let cache = tracker.rebuild_cache().unwrap();
        tracker.save_cache(&cache).unwrap();

        // Append more events
        tracker.append_surfaced("L001", "s2").unwrap();

        // Cache should be stale
        assert!(tracker.is_cache_stale().unwrap());
    }
}
```

## 5. Hook Simulation Tests

### 5.1 Session Start Hook

```bash
#!/bin/bash
# test/integration/session_start.sh

export GROVE_HOME=$(mktemp -d)
mkdir -p "$GROVE_HOME/sessions"

# Initialize project
mkdir -p .grove
echo "" > .grove/learnings.md

SESSION_ID="test-$(uuidgen)"

# Simulate session start
echo "{\"session_id\":\"$SESSION_ID\",\"cwd\":\"$(pwd)\",\"source\":\"startup\"}" \
  | grove hook session-start \
  | jq -e '.additionalContext != null'

# Verify session file created
test -f "$GROVE_HOME/sessions/$SESSION_ID.json"

rm -rf "$GROVE_HOME"
```

### 5.2 Gate Block Flow

```bash
#!/bin/bash
# test/integration/gate_block_flow.sh

export GROVE_HOME=$(mktemp -d)
mkdir -p "$GROVE_HOME/sessions"

# Initialize with tissue ticketing
mkdir -p .tissue
mkdir -p .grove
echo "" > .grove/learnings.md

SESSION_ID="test-$(uuidgen)"

# Session start
echo "{\"session_id\":\"$SESSION_ID\",\"cwd\":\"$(pwd)\",\"source\":\"startup\"}" \
  | grove hook session-start

# Simulate ticket close detection
echo "{\"session_id\":\"$SESSION_ID\",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"tissue status 1 closed\"}}" \
  | grove hook pre-tool-use

# Simulate successful ticket close
echo "{\"session_id\":\"$SESSION_ID\",\"tool_name\":\"Bash\",\"tool_response\":{\"success\":true}}" \
  | grove hook post-tool-use

# Stop should block
RESULT=$(echo "{\"session_id\":\"$SESSION_ID\"}" | grove hook stop)
echo "$RESULT" | jq -e '.decision == "block"'

# Run reflection
grove reflect --session "$SESSION_ID" << 'EOF'
[
  {
    "category": "pitfall",
    "summary": "Always check return values from tissue commands",
    "detail": "The tissue CLI can fail silently if the issue doesn't exist.",
    "criteria_met": ["behavior_changing"],
    "tags": ["tissue", "cli"],
    "scope": "project"
  }
]
EOF

# Stop should now approve
echo "{\"session_id\":\"$SESSION_ID\"}" \
  | grove hook stop \
  | jq -e '.decision == "approve"'

rm -rf "$GROVE_HOME" .tissue
```

### 5.3 Circuit Breaker Flow

```bash
#!/bin/bash
# test/integration/circuit_breaker.sh

export GROVE_HOME=$(mktemp -d)
mkdir -p "$GROVE_HOME/sessions" .grove
echo "" > .grove/learnings.md

SESSION_ID="test-$(uuidgen)"

# Session start with ticket
echo "{\"session_id\":\"$SESSION_ID\",\"cwd\":\"$(pwd)\",\"source\":\"startup\"}" \
  | grove hook session-start

# Force gate to Pending state
grove debug "$SESSION_ID" --set-gate pending

# Block 3 times (max_blocks default)
for i in {1..3}; do
  echo "{\"session_id\":\"$SESSION_ID\"}" \
    | grove hook stop \
    | jq -e '.decision == "block"'
done

# 4th stop should trigger circuit breaker
echo "{\"session_id\":\"$SESSION_ID\"}" \
  | grove hook stop \
  | jq -e '.decision == "approve" and .forced == true'

rm -rf "$GROVE_HOME"
```

## 6. Test Matrix

### 6.1 Platforms

| Platform | CI | Local |
|----------|-----|-------|
| Linux x86_64 | GitHub Actions | Required |
| Linux ARM64 | GitHub Actions | Required |
| macOS x86_64 | GitHub Actions | Optional |
| macOS ARM64 | GitHub Actions | Required |
| Windows | Not supported | Not supported |

### 6.2 Rust Versions

| Version | Status |
|---------|--------|
| Stable (latest) | Required |
| Beta | Optional (nightly CI) |
| Nightly | Not tested |
| MSRV | 1.75.0 (edition 2021 features) |

### 6.3 Coverage Requirements

| Module | Minimum Coverage |
|--------|------------------|
| `core/gate` | 90% |
| `core/reflect` | 85% |
| `core/learning` | 80% |
| `backends/markdown` | 80% |
| `storage/*` | 75% |
| `stats/tracker` | 75% |
| `hooks/*` | 75% |
| `cli/*` | 60% |
| Overall | 70% |

## Related Documents

- [Overview](./00-overview.md) - Vision, core concepts, design principles
- [Architecture](./01-architecture.md) - System diagrams, domain model, sequences
- [Implementation](./02-implementation.md) - Rust types, storage, hooks, CLI commands
- [Stats and Quality](./03-stats-and-quality.md) - Quality tracking model,
  retrieval scoring
- [CI](./05-ci.md) - Version management and release workflow
