//! Specificity heuristics for the write gate.
//!
//! This module implements three heuristic checks to assess learning quality:
//! 1. Named Entity Density (NED) - code-specific entities per 100 words
//! 2. Project-Specific Term Frequency (PSTF) - ratio of specific tags
//! 3. Generic Phrase Detection (GPD) - count of generic advice phrases
//!
//! These are combined into a composite specificity score used by the write gate
//! to reject or flag low-quality learnings.

use serde::{Deserialize, Serialize};

use crate::core::learning::CompoundLearning;

// =============================================================================
// Types
// =============================================================================

/// Specificity score for a learning, combining multiple heuristic signals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecificityScore {
    /// Named Entity Density: code-specific entities per 100 words.
    pub ned: f64,
    /// Project-Specific Term Frequency: ratio of specific tags (0.0 to 1.0).
    pub pstf: f64,
    /// Generic phrase count: number of generic advice phrases found.
    pub generic_count: u32,
    /// Weighted composite score (0.0 to 5.0).
    pub composite: f64,
}

/// Quality check mode for the write gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityCheckMode {
    /// Enforce specificity check: reject low-quality learnings.
    #[default]
    Enforce,
    /// Warn about low-quality learnings but still accept them.
    Warn,
    /// Skip specificity check entirely.
    Disabled,
}

impl QualityCheckMode {
    /// Parse mode from config string value.
    pub fn from_config(s: &str) -> Self {
        match s {
            "warn" => Self::Warn,
            "disabled" => Self::Disabled,
            _ => Self::Enforce, // Default to enforce for unknown values
        }
    }
}

// =============================================================================
// Generic Phrases
// =============================================================================

/// Phrases that indicate generic, non-specific advice.
const GENERIC_PHRASES: &[&str] = &[
    "always test",
    "write tests",
    "test your code",
    "remember to",
    "make sure to",
    "don't forget",
    "be careful",
    "pay attention",
    "keep in mind",
    "best practice",
    "good practice",
    "important to note",
    "worth noting",
    "in general",
    "as a rule",
];

// =============================================================================
// File Extensions for NED
// =============================================================================

/// Common file extensions that indicate a file path entity.
const FILE_EXTENSIONS: &[&str] = &[
    ".rs", ".ex", ".exs", ".ts", ".tsx", ".js", ".jsx", ".py", ".rb", ".go", ".java", ".kt",
    ".swift", ".c", ".cpp", ".h", ".hpp", ".cs", ".toml", ".yaml", ".yml", ".json", ".xml",
    ".html", ".css", ".scss", ".sql", ".sh", ".bash", ".zsh", ".md", ".txt", ".cfg", ".ini",
    ".env", ".lock", ".vue", ".svelte",
];

// =============================================================================
// NED: Named Entity Density
// =============================================================================

/// Count code-specific entities in text and return density per 100 words.
///
/// Entities detected:
/// - camelCase identifiers (e.g., `parseJson`, `loadTeams`)
/// - snake_case identifiers (e.g., `parse_json`, `load_teams`)
/// - PascalCase identifiers (e.g., `LiveView`, `GameLive`)
/// - File paths (containing `/` or common extensions)
/// - Version numbers (e.g., `v4.1.0`, `2.0`)
/// - Quantities with units (e.g., `300s`, `5MB`, `80ms`)
/// - Port numbers / status codes (3-5 digit numbers like `4000`, `404`)
pub fn compute_ned(text: &str) -> f64 {
    let words: Vec<&str> = text.split_whitespace().collect();
    let word_count = words.len();
    if word_count == 0 {
        return 0.0;
    }

    let mut entity_count: usize = 0;

    for word in &words {
        // Strip common punctuation from word edges for analysis
        let cleaned = word.trim_matches(|c: char| {
            matches!(c, ',' | '.' | ';' | ':' | '(' | ')' | '`' | '\'' | '"')
        });
        if cleaned.is_empty() {
            continue;
        }

        let is_entity = is_file_path(cleaned)
            || is_version_number(cleaned)
            || is_quantity_with_unit(cleaned)
            || is_port_or_status_code(cleaned)
            || is_camel_case(cleaned)
            || is_snake_case(cleaned)
            || is_pascal_case(cleaned);

        if is_entity {
            entity_count += 1;
        }
    }

    (entity_count as f64 / word_count as f64) * 100.0
}

/// Check if a token looks like a camelCase identifier.
/// Must start with lowercase, contain at least one uppercase letter after the first char.
fn is_camel_case(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 2 {
        return false;
    }
    if !chars[0].is_ascii_lowercase() {
        return false;
    }
    // Must have at least one uppercase letter after the first
    let has_upper = chars[1..].iter().any(|c| c.is_ascii_uppercase());
    // Must be all alphanumeric
    let all_alnum = chars.iter().all(|c| c.is_ascii_alphanumeric());
    has_upper && all_alnum
}

/// Check if a token looks like a snake_case identifier.
/// Must contain at least one underscore, with alphanumeric segments on both sides.
fn is_snake_case(s: &str) -> bool {
    if !s.contains('_') {
        return false;
    }
    let parts: Vec<&str> = s.split('_').collect();
    // Must have at least 2 non-empty parts
    if parts.len() < 2 {
        return false;
    }
    // All parts must be non-empty and alphanumeric
    parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Check if a token looks like a PascalCase identifier.
/// Must start with uppercase, have at least 2 chars, contain a lowercase after a capital,
/// and have at least two capital letters (to distinguish from regular words).
fn is_pascal_case(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 2 {
        return false;
    }
    if !chars[0].is_ascii_uppercase() {
        return false;
    }
    // Must be all alphanumeric
    if !chars.iter().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    // Count uppercase letters - need at least 2 to distinguish from regular capitalized words
    let upper_count = chars.iter().filter(|c| c.is_ascii_uppercase()).count();
    if upper_count < 2 {
        return false;
    }
    // Must have at least one lowercase letter (not all caps)
    let has_lower = chars.iter().any(|c| c.is_ascii_lowercase());
    has_lower
}

/// Check if a token looks like a file path.
fn is_file_path(s: &str) -> bool {
    // Contains a forward slash (path separator)
    if s.contains('/') && s.len() > 1 {
        return true;
    }
    // Has a known file extension
    let lower = s.to_lowercase();
    FILE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(ext) && lower.len() > ext.len())
}

/// Check if a token looks like a version number (e.g., v4.1.0, 2.0, 1.2.3).
fn is_version_number(s: &str) -> bool {
    let to_check = if s.starts_with('v') || s.starts_with('V') {
        &s[1..]
    } else {
        s
    };
    if to_check.is_empty() {
        return false;
    }
    // Must contain at least one dot
    if !to_check.contains('.') {
        return false;
    }
    // All parts must be numeric
    to_check
        .split('.')
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

/// Check if a token looks like a quantity with a unit (e.g., 300s, 5MB, 80ms, 10GB).
fn is_quantity_with_unit(s: &str) -> bool {
    if s.len() < 2 {
        return false;
    }
    // Find the boundary between digits and the unit suffix
    let digit_end = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_end == 0 || digit_end == s.len() {
        return false;
    }
    let unit = &s[digit_end..];
    // Common units
    let units = [
        "s", "ms", "us", "ns", "m", "h", "d", "b", "kb", "mb", "gb", "tb", "B", "KB", "MB", "GB",
        "TB", "k", "K", "M", "G", "px", "em", "rem", "pt", "%",
    ];
    units
        .iter()
        .any(|u| unit.eq_ignore_ascii_case(u) || unit == *u)
}

/// Check if a token looks like a port number or HTTP status code (3-5 digit number).
fn is_port_or_status_code(s: &str) -> bool {
    if s.len() < 3 || s.len() > 5 {
        return false;
    }
    s.chars().all(|c| c.is_ascii_digit())
}

// =============================================================================
// PSTF: Project-Specific Term Frequency
// =============================================================================

/// Compute the ratio of project-specific tags to total tags.
///
/// A tag is considered project-specific if it:
/// - Looks like a code identifier (snake_case, camelCase, PascalCase)
/// - Looks like a file extension or technology name
/// - Appears in the learning's context_files
pub fn compute_pstf(tags: &[String], context_files: Option<&[String]>) -> f64 {
    if tags.is_empty() {
        return 0.0;
    }

    let specific_count = tags
        .iter()
        .filter(|tag| is_specific_tag(tag, context_files))
        .count();
    specific_count as f64 / tags.len() as f64
}

/// Check if a tag appears to be project-specific rather than generic.
fn is_specific_tag(tag: &str, context_files: Option<&[String]>) -> bool {
    // Check if it looks like a code identifier
    if is_camel_case(tag) || is_snake_case(tag) || is_pascal_case(tag) {
        return true;
    }

    // Check if it looks like a file extension
    if tag.starts_with('.') && tag.len() > 1 && tag[1..].chars().all(|c| c.is_ascii_alphanumeric())
    {
        return true;
    }

    // Check if it contains a hyphen (framework/library-style names like "live-view")
    if tag.contains('-') && tag.len() > 3 {
        let parts: Vec<&str> = tag.split('-').collect();
        if parts.len() >= 2 && parts.iter().all(|p| !p.is_empty()) {
            return true;
        }
    }

    // Check if it appears in context_files (file name or path component)
    if let Some(files) = context_files {
        let tag_lower = tag.to_lowercase();
        for file in files {
            let file_lower = file.to_lowercase();
            // Check if tag matches a filename or path component
            if file_lower.contains(&tag_lower) && tag_lower.len() > 2 {
                return true;
            }
        }
    }

    // Generic words that commonly appear as tags but don't indicate specificity
    let generic_tags = [
        "code",
        "testing",
        "general",
        "development",
        "programming",
        "software",
        "design",
        "architecture",
        "performance",
        "security",
        "error",
        "bug",
        "feature",
        "refactoring",
        "cleanup",
        "documentation",
        "docs",
        "api",
        "database",
        "frontend",
        "backend",
        "deployment",
        "configuration",
        "debugging",
        "logging",
        "monitoring",
        "test",
        "tests",
        "fix",
        "update",
        "improvement",
        "optimization",
        "pattern",
        "convention",
        "workflow",
        "process",
        "tool",
        "tools",
        "setup",
        "config",
    ];

    let tag_lower = tag.to_lowercase();
    if generic_tags.contains(&tag_lower.as_str()) {
        return false;
    }

    // If it doesn't match any generic pattern and has some length, consider it specific
    // (framework names, library names, etc.)
    tag.len() > 3
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// =============================================================================
// GPD: Generic Phrase Detection
// =============================================================================

/// Count generic advice phrases in the combined summary and detail text.
pub fn compute_generic_phrase_count(summary: &str, detail: &str) -> u32 {
    let combined = format!("{} {}", summary, detail).to_lowercase();
    let mut count: u32 = 0;

    for phrase in GENERIC_PHRASES {
        if combined.contains(phrase) {
            count += 1;
        }
    }

    count
}

// =============================================================================
// Composite Score
// =============================================================================

/// Default minimum composite specificity score.
pub const DEFAULT_MIN_SPECIFICITY_SCORE: f64 = 1.5;

/// Compute the composite specificity score from individual signals.
///
/// Formula: `ned * 0.4 + pstf * 5.0 * 0.4 + (5.0 - generic_penalty) * 0.2`
/// where `generic_penalty = min(generic_count * 1.5, 5.0)`
fn compute_composite(ned: f64, pstf: f64, generic_count: u32) -> f64 {
    let generic_penalty = (generic_count as f64 * 1.5).min(5.0);
    let raw = ned * 0.4 + pstf * 5.0 * 0.4 + (5.0 - generic_penalty) * 0.2;
    raw.clamp(0.0, 5.0)
}

/// Assess the specificity of a learning using NED, PSTF, and GPD heuristics.
///
/// This is the main entry point for specificity scoring. It analyzes the
/// learning's summary, detail, tags, and context files to produce a composite
/// score indicating how specific (vs. generic) the learning is.
pub fn assess_specificity(learning: &CompoundLearning) -> SpecificityScore {
    let combined_text = format!("{} {}", learning.summary, learning.detail);

    let ned = compute_ned(&combined_text);
    let pstf = compute_pstf(&learning.tags, learning.context_files.as_deref());
    let generic_count = compute_generic_phrase_count(&learning.summary, &learning.detail);
    let composite = compute_composite(ned, pstf, generic_count);

    SpecificityScore {
        ned,
        pstf,
        generic_count,
        composite,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::learning::{
        CompoundLearning, Confidence, LearningCategory, LearningScope, WriteGateCriterion,
    };

    // =========================================================================
    // NED tests
    // =========================================================================

    #[test]
    fn test_ned_code_identifiers() {
        // camelCase, snake_case, PascalCase
        let text = "The parseJson function in load_teams module uses LiveView for rendering GameLive components";
        let ned = compute_ned(text);
        // 4 entities in 13 words => ~30.8 per 100
        assert!(
            ned >= 3.0,
            "NED should be >= 3.0 for text with code identifiers, got {}",
            ned
        );
    }

    #[test]
    fn test_ned_camel_case() {
        assert!(is_camel_case("parseJson"));
        assert!(is_camel_case("loadTeams"));
        assert!(is_camel_case("getA"));
        assert!(!is_camel_case("Parse")); // starts with uppercase
        assert!(!is_camel_case("parse")); // no uppercase
        assert!(!is_camel_case("a")); // too short
        assert!(!is_camel_case("parse_json")); // has underscore
    }

    #[test]
    fn test_ned_snake_case() {
        assert!(is_snake_case("parse_json"));
        assert!(is_snake_case("load_teams"));
        assert!(is_snake_case("get_user_data"));
        assert!(!is_snake_case("parse")); // no underscore
        assert!(!is_snake_case("_leading")); // empty first part
        assert!(!is_snake_case("trailing_")); // empty last part
    }

    #[test]
    fn test_ned_pascal_case() {
        assert!(is_pascal_case("LiveView"));
        assert!(is_pascal_case("GameLive"));
        assert!(is_pascal_case("HttpClient"));
        assert!(!is_pascal_case("always")); // lowercase start
        assert!(!is_pascal_case("ALLCAPS")); // no lowercase
        assert!(!is_pascal_case("Hello")); // only one uppercase
        assert!(!is_pascal_case("A")); // too short
    }

    #[test]
    fn test_ned_file_paths() {
        let text = "Check src/main.rs and config.toml for the endpoint at /api/users";
        let ned = compute_ned(text);
        // 3 entities (src/main.rs, config.toml, /api/users) in 10 words => 30 per 100
        assert!(
            ned >= 3.0,
            "NED should be >= 3.0 for text with file paths, got {}",
            ned
        );
    }

    #[test]
    fn test_ned_versions_and_quantities() {
        let text = "Upgraded to v4.1.0 with 300s timeout and 5MB limit running on port 4000";
        let ned = compute_ned(text);
        // v4.1.0, 300s, 5MB, 4000 => 4 entities in 13 words => ~30.8
        assert!(
            ned >= 3.0,
            "NED should be >= 3.0 for text with versions and quantities, got {}",
            ned
        );
    }

    #[test]
    fn test_ned_generic_text() {
        let text = "Always write tests for your code and make sure everything works properly";
        let ned = compute_ned(text);
        assert!(
            ned < 1.0,
            "NED should be < 1.0 for generic text, got {}",
            ned
        );
    }

    #[test]
    fn test_ned_empty_text() {
        assert_eq!(compute_ned(""), 0.0);
    }

    #[test]
    fn test_ned_version_number_detection() {
        assert!(is_version_number("v4.1.0"));
        assert!(is_version_number("2.0"));
        assert!(is_version_number("1.2.3"));
        assert!(is_version_number("V1.0"));
        assert!(!is_version_number("v4")); // no dot
        assert!(!is_version_number("hello")); // not a version
        assert!(!is_version_number("")); // empty
    }

    #[test]
    fn test_ned_quantity_with_unit() {
        assert!(is_quantity_with_unit("300s"));
        assert!(is_quantity_with_unit("5MB"));
        assert!(is_quantity_with_unit("80ms"));
        assert!(is_quantity_with_unit("10GB"));
        assert!(is_quantity_with_unit("100px"));
        assert!(!is_quantity_with_unit("hello")); // no digits
        assert!(!is_quantity_with_unit("42")); // no unit
        assert!(!is_quantity_with_unit("s")); // too short
    }

    #[test]
    fn test_ned_port_or_status_code() {
        assert!(is_port_or_status_code("4000"));
        assert!(is_port_or_status_code("404"));
        assert!(is_port_or_status_code("8080"));
        assert!(is_port_or_status_code("200"));
        assert!(is_port_or_status_code("50000"));
        assert!(!is_port_or_status_code("42")); // too short (2 digits)
        assert!(!is_port_or_status_code("123456")); // too long (6 digits)
        assert!(!is_port_or_status_code("abc")); // not digits
    }

    // =========================================================================
    // PSTF tests
    // =========================================================================

    #[test]
    fn test_pstf_specific_tags() {
        let tags = vec![
            "phoenix".to_string(),
            "LiveView".to_string(),
            "router".to_string(),
        ];
        let pstf = compute_pstf(&tags, None);
        // "phoenix" (>3 chars, alnum), "LiveView" (PascalCase), "router" (>3 chars, alnum)
        assert!(
            pstf > 0.5,
            "PSTF should be > 0.5 for specific tags, got {}",
            pstf
        );
    }

    #[test]
    fn test_pstf_generic_tags() {
        let tags = vec![
            "code".to_string(),
            "testing".to_string(),
            "general".to_string(),
        ];
        let pstf = compute_pstf(&tags, None);
        assert!(
            pstf < 0.5,
            "PSTF should be < 0.5 for generic tags, got {}",
            pstf
        );
    }

    #[test]
    fn test_pstf_mixed_tags() {
        let tags = vec![
            "LiveView".to_string(),   // specific (PascalCase)
            "code".to_string(),       // generic
            "parse_json".to_string(), // specific (snake_case)
        ];
        let pstf = compute_pstf(&tags, None);
        // 2 out of 3 are specific => ~0.67
        assert!(
            pstf > 0.5,
            "PSTF should be > 0.5 for mixed tags with majority specific, got {}",
            pstf
        );
    }

    #[test]
    fn test_pstf_empty_tags() {
        let tags: Vec<String> = vec![];
        let pstf = compute_pstf(&tags, None);
        assert_eq!(pstf, 0.0);
    }

    #[test]
    fn test_pstf_with_context_files() {
        let tags = vec!["router".to_string(), "testing".to_string()];
        let context_files = vec!["src/router.rs".to_string()];
        let pstf = compute_pstf(&tags, Some(&context_files));
        // "router" matches context file, "testing" is generic
        // At least 1 out of 2 should be specific
        assert!(
            pstf >= 0.5,
            "PSTF should be >= 0.5 when tag matches context file, got {}",
            pstf
        );
    }

    #[test]
    fn test_pstf_hyphenated_tags() {
        let tags = vec!["live-view".to_string(), "hot-reload".to_string()];
        let pstf = compute_pstf(&tags, None);
        assert!(
            pstf > 0.5,
            "PSTF should count hyphenated tags as specific, got {}",
            pstf
        );
    }

    // =========================================================================
    // GPD tests
    // =========================================================================

    #[test]
    fn test_generic_phrase_detection() {
        let count = compute_generic_phrase_count(
            "Always test your code",
            "Remember to write tests and make sure to check everything. As a rule, be careful.",
        );
        // Matches: "always test", "test your code", "remember to", "write tests",
        //          "make sure to", "as a rule", "be careful"
        assert!(
            count >= 5,
            "Should detect multiple generic phrases, got {}",
            count
        );
    }

    #[test]
    fn test_generic_phrase_detection_none() {
        let count = compute_generic_phrase_count(
            "Phoenix LiveView uses WebSocket for real-time updates",
            "The LiveView.mount/3 callback initializes socket assigns with default values from the database query.",
        );
        assert_eq!(
            count, 0,
            "Should detect no generic phrases in specific text"
        );
    }

    #[test]
    fn test_generic_phrase_detection_case_insensitive() {
        let count =
            compute_generic_phrase_count("ALWAYS TEST your code", "Best Practice for development.");
        assert!(
            count >= 2,
            "Should detect phrases case-insensitively, got {}",
            count
        );
    }

    // =========================================================================
    // Composite score tests
    // =========================================================================

    #[test]
    fn test_composite_score_specific_learning() {
        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            "Phoenix LiveView mount/3 requires socket assigns initialization",
            "The LiveView.mount/3 callback in router.ex must set all assigns used by render/1. Missing assigns cause KeyError at runtime. Use assign_new/3 for defaults.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["phoenix".to_string(), "LiveView".to_string(), "elixir".to_string()],
            "session-1",
        );

        let score = assess_specificity(&learning);
        assert!(
            score.composite >= 1.5,
            "Specific learning should score >= 1.5, got {}",
            score.composite
        );
        assert!(
            score.ned >= 1.0,
            "Should detect code entities, got NED={}",
            score.ned
        );
        assert!(
            score.generic_count == 0,
            "Should have no generic phrases, got {}",
            score.generic_count
        );
    }

    #[test]
    fn test_composite_score_generic_learning() {
        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            "Always test your code before deploying",
            "Remember to write tests and make sure to check everything. Best practice is to always test before deployment. Pay attention to edge cases.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["testing".to_string(), "general".to_string()],
            "session-1",
        );

        let score = assess_specificity(&learning);
        assert!(
            score.composite < 1.5,
            "Generic learning should score < 1.5, got {}",
            score.composite
        );
        assert!(
            score.generic_count >= 3,
            "Should detect generic phrases, got {}",
            score.generic_count
        );
    }

    #[test]
    fn test_composite_formula() {
        // NED=5.0, PSTF=0.8, generic_count=0
        // composite = 5.0 * 0.4 + 0.8 * 5.0 * 0.4 + (5.0 - 0.0) * 0.2
        //           = 2.0 + 1.6 + 1.0 = 4.6
        let composite = compute_composite(5.0, 0.8, 0);
        assert!(
            (composite - 4.6).abs() < 0.01,
            "Expected ~4.6, got {}",
            composite
        );
    }

    #[test]
    fn test_composite_formula_with_generics() {
        // NED=0.0, PSTF=0.0, generic_count=4
        // generic_penalty = min(4 * 1.5, 5.0) = 5.0
        // composite = 0.0 * 0.4 + 0.0 * 5.0 * 0.4 + (5.0 - 5.0) * 0.2
        //           = 0.0 + 0.0 + 0.0 = 0.0
        let composite = compute_composite(0.0, 0.0, 4);
        assert!(
            (composite - 0.0).abs() < 0.01,
            "Expected ~0.0, got {}",
            composite
        );
    }

    #[test]
    fn test_composite_clamps_to_range() {
        // Very high NED should still clamp to 5.0
        let composite = compute_composite(100.0, 1.0, 0);
        assert!(
            composite <= 5.0,
            "Composite should be clamped to 5.0, got {}",
            composite
        );

        // Negative intermediate shouldn't go below 0.0
        let composite = compute_composite(0.0, 0.0, 10);
        assert!(
            composite >= 0.0,
            "Composite should be >= 0.0, got {}",
            composite
        );
    }

    // =========================================================================
    // QualityCheckMode tests
    // =========================================================================

    #[test]
    fn test_quality_check_mode_from_config() {
        assert_eq!(
            QualityCheckMode::from_config("enforce"),
            QualityCheckMode::Enforce
        );
        assert_eq!(
            QualityCheckMode::from_config("warn"),
            QualityCheckMode::Warn
        );
        assert_eq!(
            QualityCheckMode::from_config("disabled"),
            QualityCheckMode::Disabled
        );
        assert_eq!(
            QualityCheckMode::from_config("unknown"),
            QualityCheckMode::Enforce
        );
        assert_eq!(QualityCheckMode::from_config(""), QualityCheckMode::Enforce);
    }

    // =========================================================================
    // is_specific_tag tests
    // =========================================================================

    #[test]
    fn test_specific_tag_code_identifiers() {
        assert!(is_specific_tag("parseJson", None)); // camelCase
        assert!(is_specific_tag("parse_json", None)); // snake_case
        assert!(is_specific_tag("LiveView", None)); // PascalCase
    }

    #[test]
    fn test_specific_tag_file_extension() {
        assert!(is_specific_tag(".rs", None));
        assert!(is_specific_tag(".toml", None));
    }

    #[test]
    fn test_specific_tag_generic() {
        assert!(!is_specific_tag("code", None));
        assert!(!is_specific_tag("testing", None));
        assert!(!is_specific_tag("general", None));
        assert!(!is_specific_tag("bug", None));
    }

    #[test]
    fn test_specific_tag_context_file_match() {
        let files = vec!["src/router.rs".to_string()];
        assert!(is_specific_tag("router", Some(&files)));
    }

    // =========================================================================
    // Full assess_specificity tests
    // =========================================================================

    #[test]
    fn test_assess_specificity_with_context_files() {
        let mut learning = CompoundLearning::new(
            LearningCategory::Debugging,
            "The parse_config function in config.rs panics on empty input",
            "Found that parse_config in src/config.rs does not handle empty string input, causing a panic at line 42. Added a guard clause to return Err instead.",
            LearningScope::Project,
            Confidence::High,
            vec![WriteGateCriterion::StableFact],
            vec!["config".to_string(), "parse_config".to_string()],
            "session-1",
        );
        learning.context_files = Some(vec!["src/config.rs".to_string()]);

        let score = assess_specificity(&learning);
        assert!(
            score.ned >= 1.0,
            "Should detect code entities, got NED={}",
            score.ned
        );
        assert!(score.pstf >= 0.5, "Should have specific tags (config matches context file, parse_config is snake_case), got PSTF={}", score.pstf);
    }

    #[test]
    fn test_assess_specificity_returns_zero_for_empty_learning() {
        let learning = CompoundLearning::new(
            LearningCategory::Pattern,
            "Short note", // Will fail schema validation normally, but test scoring independently
            "A very generic explanation of something that everyone knows about programming.",
            LearningScope::Project,
            Confidence::Low,
            vec![WriteGateCriterion::BehaviorChanging],
            vec!["general".to_string()],
            "session-1",
        );

        let score = assess_specificity(&learning);
        assert!(
            score.ned < 3.0,
            "Generic learning should have low NED, got {}",
            score.ned
        );
    }
}
