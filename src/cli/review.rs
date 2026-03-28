//! Review command for Grove.
//!
//! Provides a developer feedback loop for learning quality calibration.
//! Samples learnings for binary thumbs up/down rating. Ratings are persisted
//! in the stats log for threshold calibration.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::backends::MemoryBackend;
use crate::config::project_stats_log_path;
use crate::error::FailOpen;
use crate::stats::StatsLogger;

/// Options for the review command.
#[derive(Debug, Clone, Default)]
pub struct ReviewOptions {
    /// Output as JSON.
    pub json: bool,
    /// Suppress output.
    pub quiet: bool,
    /// Number of learnings to sample for review.
    pub count: usize,
}

/// Input format for the review command (JSON from stdin).
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewInput {
    /// Ratings for sampled learnings.
    pub ratings: Vec<ReviewRating>,
}

/// A single rating in the review input.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReviewRating {
    /// The learning ID being rated.
    pub id: String,
    /// Whether the learning was useful (true = thumbs up, false = thumbs down).
    pub useful: bool,
}

/// A learning presented for review.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewCandidate {
    /// The learning ID.
    pub id: String,
    /// The learning summary.
    pub summary: String,
    /// The learning category.
    pub category: String,
    /// The learning tags.
    pub tags: Vec<String>,
}

/// Output format for the review command.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewOutput {
    /// Whether the review was successful.
    pub success: bool,
    /// Learnings sampled for review (when no ratings provided).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<ReviewCandidate>>,
    /// Number of ratings recorded.
    pub ratings_recorded: usize,
    /// Error message if review failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ReviewOutput {
    /// Create a successful output with candidates for review.
    pub fn candidates(candidates: Vec<ReviewCandidate>) -> Self {
        Self {
            success: true,
            candidates: Some(candidates),
            ratings_recorded: 0,
            error: None,
        }
    }

    /// Create a successful output with recorded ratings.
    pub fn rated(count: usize) -> Self {
        Self {
            success: true,
            candidates: None,
            ratings_recorded: count,
            error: None,
        }
    }

    /// Create a failed output.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            candidates: None,
            ratings_recorded: 0,
            error: Some(error.into()),
        }
    }
}

/// The review command implementation.
pub struct ReviewCommand<B: MemoryBackend> {
    backend: B,
}

impl<B: MemoryBackend> ReviewCommand<B> {
    /// Create a new review command.
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// Run the review command.
    ///
    /// Two modes:
    /// - Without stdin: sample learnings and output candidates for rating
    /// - With stdin: record ratings from the provided JSON input
    pub fn run(&self, options: &ReviewOptions, cwd: &Path) -> ReviewOutput {
        // Try to read stdin (non-blocking check)
        let stdin_input = self.try_read_stdin();

        match stdin_input {
            Some(input) => self.record_ratings(&input, cwd),
            None => self.sample_candidates(options),
        }
    }

    /// Run with explicit input (for testing).
    pub fn run_with_input(&self, input: &ReviewInput, cwd: &Path) -> ReviewOutput {
        self.record_ratings(input, cwd)
    }

    /// Run without input to get candidates (for testing).
    pub fn run_sample(&self, options: &ReviewOptions) -> ReviewOutput {
        self.sample_candidates(options)
    }

    /// Sample learnings for review.
    fn sample_candidates(&self, options: &ReviewOptions) -> ReviewOutput {
        let learnings = self
            .backend
            .list_all()
            .fail_open_default("listing learnings for review");

        if learnings.is_empty() {
            return ReviewOutput::failure("No learnings available for review");
        }

        let count = if options.count > 0 { options.count } else { 5 };

        // Deterministic sampling: take evenly spaced learnings
        // This avoids requiring `rand` as a dependency
        let total = learnings.len();
        let sample_count = count.min(total);
        let mut candidates = Vec::with_capacity(sample_count);

        if sample_count >= total {
            // Take all learnings
            for learning in &learnings {
                candidates.push(ReviewCandidate {
                    id: learning.id.clone(),
                    summary: learning.summary.clone(),
                    category: format!("{:?}", learning.category),
                    tags: learning.tags.clone(),
                });
            }
        } else {
            // Stride-based sampling for even distribution
            let stride = total as f64 / sample_count as f64;
            for i in 0..sample_count {
                let idx = (i as f64 * stride) as usize;
                let learning = &learnings[idx];
                candidates.push(ReviewCandidate {
                    id: learning.id.clone(),
                    summary: learning.summary.clone(),
                    category: format!("{:?}", learning.category),
                    tags: learning.tags.clone(),
                });
            }
        }

        ReviewOutput::candidates(candidates)
    }

    /// Record ratings from input.
    fn record_ratings(&self, input: &ReviewInput, cwd: &Path) -> ReviewOutput {
        if input.ratings.is_empty() {
            return ReviewOutput::rated(0);
        }

        let stats_path = project_stats_log_path(cwd);
        let stats_logger = StatsLogger::new(&stats_path);

        let mut recorded = 0;
        for rating in &input.ratings {
            stats_logger
                .append_rated(&rating.id, rating.useful, "review")
                .fail_open_default("logging review rating");
            recorded += 1;
        }

        ReviewOutput::rated(recorded)
    }

    /// Try to read JSON from stdin (returns None if stdin is a terminal).
    fn try_read_stdin(&self) -> Option<ReviewInput> {
        use std::io::{IsTerminal, Read};

        // If stdin is a terminal (interactive), no piped input
        if io::stdin().is_terminal() {
            return None;
        }

        let stdin = io::stdin();
        let mut input = String::new();

        if stdin.lock().read_to_string(&mut input).is_err() {
            return None;
        }

        if input.trim().is_empty() {
            return None;
        }

        serde_json::from_str(&input).ok()
    }

    /// Format output based on options.
    pub fn format_output(&self, output: &ReviewOutput, options: &ReviewOptions) -> String {
        if options.quiet {
            return String::new();
        }

        if options.json {
            serde_json::to_string_pretty(output).unwrap_or_else(|_| "{}".to_string())
        } else {
            self.format_human_readable(output)
        }
    }

    /// Format output as human-readable text.
    fn format_human_readable(&self, output: &ReviewOutput) -> String {
        let mut result = String::new();

        if !output.success {
            result.push_str(&format!(
                "Review failed: {}\n",
                output.error.as_deref().unwrap_or("unknown error")
            ));
            return result;
        }

        if let Some(candidates) = &output.candidates {
            result.push_str(&format!("Review {} learning(s):\n\n", candidates.len()));
            for (i, c) in candidates.iter().enumerate() {
                result.push_str(&format!(
                    "{}. [{}] {} ({})\n   Tags: {}\n\n",
                    i + 1,
                    c.id,
                    c.summary,
                    c.category,
                    if c.tags.is_empty() {
                        "none".to_string()
                    } else {
                        c.tags.join(", ")
                    }
                ));
            }
            result.push_str(
                "To rate: pipe JSON to stdin with {\"ratings\": [{\"id\": \"...\", \"useful\": true/false}]}\n",
            );
        } else {
            result.push_str(&format!("Recorded {} rating(s)\n", output.ratings_recorded));
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::{SearchFilters, SearchQuery, SearchResult, WriteResult};

    use crate::core::{
        CompoundLearning, Confidence, LearningCategory, LearningScope, WriteGateCriterion,
    };
    use crate::error::Result;
    use crate::stats::StatsEventType;
    use tempfile::TempDir;

    /// A test backend that returns preconfigured learnings.
    struct MockBackend {
        learnings: Vec<CompoundLearning>,
    }

    impl MockBackend {
        fn new(learnings: Vec<CompoundLearning>) -> Self {
            Self { learnings }
        }
    }

    impl MemoryBackend for MockBackend {
        fn write(&self, _learning: &CompoundLearning) -> Result<WriteResult> {
            Ok(WriteResult::success("test", "memory"))
        }

        fn search(
            &self,
            _query: &SearchQuery,
            _filters: &SearchFilters,
        ) -> Result<Vec<SearchResult>> {
            Ok(Vec::new())
        }

        fn ping(&self) -> bool {
            true
        }

        fn name(&self) -> &'static str {
            "mock"
        }

        fn list_all(&self) -> Result<Vec<CompoundLearning>> {
            Ok(self.learnings.clone())
        }
    }

    fn make_learning(id: &str, summary: &str, category: LearningCategory) -> CompoundLearning {
        CompoundLearning::new(
            category,
            summary,
            "Detail text for testing.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["test".to_string()],
            "test-session",
        )
        .with_id(id)
    }

    #[test]
    fn test_review_sample_candidates_default_count() {
        let learnings: Vec<CompoundLearning> = (0..10)
            .map(|i| {
                make_learning(
                    &format!("cl_{:03}", i),
                    &format!("Learning {}", i),
                    LearningCategory::Pattern,
                )
            })
            .collect();

        let backend = MockBackend::new(learnings);

        let cmd = ReviewCommand::new(backend);
        let options = ReviewOptions::default();

        let output = cmd.run_sample(&options);
        assert!(output.success);
        let candidates = output.candidates.unwrap();
        assert_eq!(candidates.len(), 5); // default count
    }

    #[test]
    fn test_review_sample_candidates_custom_count() {
        let learnings: Vec<CompoundLearning> = (0..10)
            .map(|i| {
                make_learning(
                    &format!("cl_{:03}", i),
                    &format!("Learning {}", i),
                    LearningCategory::Pattern,
                )
            })
            .collect();

        let backend = MockBackend::new(learnings);

        let cmd = ReviewCommand::new(backend);
        let options = ReviewOptions {
            count: 3,
            ..Default::default()
        };

        let output = cmd.run_sample(&options);
        assert!(output.success);
        let candidates = output.candidates.unwrap();
        assert_eq!(candidates.len(), 3);
    }

    #[test]
    fn test_review_sample_fewer_than_count() {
        let learnings = vec![
            make_learning("cl_001", "Learning 1", LearningCategory::Pattern),
            make_learning("cl_002", "Learning 2", LearningCategory::Pitfall),
        ];

        let backend = MockBackend::new(learnings);

        let cmd = ReviewCommand::new(backend);
        let options = ReviewOptions {
            count: 5,
            ..Default::default()
        };

        let output = cmd.run_sample(&options);
        assert!(output.success);
        let candidates = output.candidates.unwrap();
        assert_eq!(candidates.len(), 2); // only 2 available
    }

    #[test]
    fn test_review_sample_empty_corpus() {
        let backend = MockBackend::new(Vec::new());

        let cmd = ReviewCommand::new(backend);
        let options = ReviewOptions::default();

        let output = cmd.run_sample(&options);
        assert!(!output.success);
        assert!(output.error.unwrap().contains("No learnings"));
    }

    #[test]
    fn test_review_record_ratings() {
        let dir = TempDir::new().unwrap();
        let grove_dir = dir.path().join(".grove");
        std::fs::create_dir_all(&grove_dir).unwrap();

        let backend = MockBackend::new(Vec::new());

        let cmd = ReviewCommand::new(backend);

        let input = ReviewInput {
            ratings: vec![
                ReviewRating {
                    id: "cl_001".to_string(),
                    useful: true,
                },
                ReviewRating {
                    id: "cl_002".to_string(),
                    useful: false,
                },
            ],
        };

        let output = cmd.run_with_input(&input, dir.path());
        assert!(output.success);
        assert_eq!(output.ratings_recorded, 2);

        // Verify stats were written
        let stats_path = project_stats_log_path(dir.path());
        let logger = StatsLogger::new(&stats_path);
        let events = logger.read_all().unwrap();

        assert_eq!(events.len(), 2);

        match &events[0].data {
            StatsEventType::Rated {
                learning_id,
                useful,
                context,
            } => {
                assert_eq!(learning_id, "cl_001");
                assert!(useful);
                assert_eq!(context, "review");
            }
            other => panic!("Expected Rated event, got {:?}", other),
        }

        match &events[1].data {
            StatsEventType::Rated {
                learning_id,
                useful,
                context,
            } => {
                assert_eq!(learning_id, "cl_002");
                assert!(!useful);
                assert_eq!(context, "review");
            }
            other => panic!("Expected Rated event, got {:?}", other),
        }
    }

    #[test]
    fn test_review_record_empty_ratings() {
        let dir = TempDir::new().unwrap();
        let backend = MockBackend::new(Vec::new());

        let cmd = ReviewCommand::new(backend);

        let input = ReviewInput {
            ratings: Vec::new(),
        };

        let output = cmd.run_with_input(&input, dir.path());
        assert!(output.success);
        assert_eq!(output.ratings_recorded, 0);
    }

    #[test]
    fn test_review_output_json_format() {
        let backend = MockBackend::new(Vec::new());

        let cmd = ReviewCommand::new(backend);

        let output = ReviewOutput::rated(3);
        let options = ReviewOptions {
            json: true,
            ..Default::default()
        };
        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.contains("\"ratings_recorded\": 3"));
    }

    #[test]
    fn test_review_output_quiet() {
        let backend = MockBackend::new(Vec::new());

        let cmd = ReviewCommand::new(backend);

        let output = ReviewOutput::rated(3);
        let options = ReviewOptions {
            quiet: true,
            ..Default::default()
        };
        let formatted = cmd.format_output(&output, &options);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_review_candidate_fields() {
        let learnings = vec![make_learning(
            "cl_001",
            "Use builder pattern",
            LearningCategory::Convention,
        )];

        let backend = MockBackend::new(learnings);

        let cmd = ReviewCommand::new(backend);
        let options = ReviewOptions {
            count: 1,
            ..Default::default()
        };

        let output = cmd.run_sample(&options);
        let candidates = output.candidates.unwrap();
        assert_eq!(candidates[0].id, "cl_001");
        assert_eq!(candidates[0].summary, "Use builder pattern");
        assert_eq!(candidates[0].category, "Convention");
        assert_eq!(candidates[0].tags, vec!["test"]);
    }
}
