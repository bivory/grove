//! Total Recall format constants and helpers.
//!
//! Centralizes all format-related constants for Total Recall integration.
//! If Total Recall changes its format, updates should only be needed here.
//!
//! ## Format Version
//!
//! This module targets the Total Recall format as of 2026-02. The format is
//! not versioned by Total Recall itself, so Grove must track compatibility
//! independently.

/// Current format version identifier (internal to Grove).
/// Increment this when making breaking format changes.
pub const FORMAT_VERSION: u32 = 1;

// =============================================================================
// Date and Time Formats
// =============================================================================

/// Time format for entry timestamps: `[HH:MM]`
pub const TIME_FORMAT: &str = "%H:%M";

/// Date format for daily log filenames: `YYYY-MM-DD.md`
pub const DATE_FORMAT: &str = "%Y-%m-%d";

/// Full timestamp format for personal learnings: `2026-01-15T14:30:00Z`
pub const TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H:%M:%SZ";

// =============================================================================
// ID and Prefix Markers
// =============================================================================

/// Prefix for Grove learning IDs: `grove:`
pub const GROVE_ID_PREFIX: &str = "grove:";

// =============================================================================
// Section Headers (Daily Log Structure)
// =============================================================================

/// The section where Grove learnings are appended.
pub const SECTION_LEARNINGS: &str = "## Learnings";

/// All standard daily log sections in order.
pub const DAILY_LOG_SECTIONS: &[&str] = &[
    "## Decisions",
    "## Corrections",
    "## Commitments",
    "## Open Loops",
    "## Notes",
    "## Learnings",
];

// =============================================================================
// Entry Format Markers
// =============================================================================

/// Blockquote prefix for detail lines.
pub const BLOCKQUOTE_PREFIX: &str = "> ";

/// Separator between entries in output.
pub const ENTRY_SEPARATOR: &str = "---";

/// Section header prefix (used for parsing next section).
pub const SECTION_PREFIX: &str = "\n## ";

/// Separator between metadata fields: ` | `
pub const METADATA_SEPARATOR: &str = " | ";

// =============================================================================
// Metadata Field Labels
// =============================================================================

/// Label for tags metadata.
pub const LABEL_TAGS: &str = "Tags:";

/// Label for confidence metadata.
pub const LABEL_CONFIDENCE: &str = "Confidence:";

/// Label for ticket metadata.
pub const LABEL_TICKET: &str = "Ticket:";

/// Label for files metadata.
pub const LABEL_FILES: &str = "Files:";

/// Label for category in personal learnings.
pub const LABEL_CATEGORY: &str = "**Category:**";

/// Label for summary in personal learnings.
pub const LABEL_SUMMARY: &str = "**Summary:**";

/// Label for created date in personal learnings.
pub const LABEL_CREATED: &str = "**Created:**";

// =============================================================================
// Category Markers (for parsing)
// =============================================================================

/// Bold category markers used in entry headers.
pub const CATEGORY_MARKERS: &[(&str, &str)] = &[
    ("**Pattern**", "Pattern"),
    ("**Pitfall**", "Pitfall"),
    ("**Convention**", "Convention"),
    ("**Dependency**", "Dependency"),
    ("**Process**", "Process"),
    ("**Domain**", "Domain"),
    ("**Debugging**", "Debugging"),
];

// =============================================================================
// Limits and Thresholds
// =============================================================================

/// Maximum number of daily log files to search.
pub const DAILY_LOG_SEARCH_LIMIT: usize = 14;

/// Maximum ID length when parsing (prevents runaway parsing).
pub const MAX_ID_LENGTH: usize = 30;

// =============================================================================
// Template Generation
// =============================================================================

/// Generate a daily log template for the given date.
///
/// The template includes all standard sections in the order Total Recall expects.
pub fn daily_log_template(date: &str) -> String {
    format!(
        r#"# {date}

## Decisions

## Corrections

## Commitments

## Open Loops

## Notes

## Learnings

"#
    )
}

// =============================================================================
// Format Detection (for future version migration)
// =============================================================================

/// Markers that indicate a file is a Total Recall daily log.
pub const TR_DAILY_LOG_MARKERS: &[&str] = &[
    "## Decisions",
    "## Corrections",
    "## Commitments",
    "## Open Loops",
];

/// Markers that indicate a file is a Total Recall register.
pub const TR_REGISTER_MARKERS: &[&str] = &["## Summary", "## Key Facts", "## History"];

/// Check if content appears to be a Total Recall daily log.
pub fn looks_like_daily_log(content: &str) -> bool {
    // Require at least 2 of the standard sections
    TR_DAILY_LOG_MARKERS
        .iter()
        .filter(|marker| content.contains(*marker))
        .count()
        >= 2
}

/// Check if content contains Grove entries.
pub fn has_grove_entries(content: &str) -> bool {
    content.contains(GROVE_ID_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daily_log_template_contains_all_sections() {
        let template = daily_log_template("2026-02-10");

        assert!(template.starts_with("# 2026-02-10"));
        for section in DAILY_LOG_SECTIONS {
            assert!(template.contains(section), "Missing section: {}", section);
        }
    }

    #[test]
    fn test_looks_like_daily_log_positive() {
        let content = "# 2026-02-10\n\n## Decisions\n\n## Corrections\n\n## Notes\n";
        assert!(looks_like_daily_log(content));
    }

    #[test]
    fn test_looks_like_daily_log_negative() {
        let content = "# Random Notes\n\nSome random content here.\n";
        assert!(!looks_like_daily_log(content));
    }

    #[test]
    fn test_has_grove_entries() {
        assert!(has_grove_entries("Some text grove:cl_001 more text"));
        assert!(!has_grove_entries("No grove entries here"));
    }

    #[test]
    fn test_format_constants_consistent() {
        // Verify related constants are consistent
        assert!(SECTION_LEARNINGS.starts_with("## "));
        assert!(BLOCKQUOTE_PREFIX.ends_with(' '));
        assert!(GROVE_ID_PREFIX.ends_with(':'));
    }
}
