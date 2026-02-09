//! Discovery module for Grove.
//!
//! This module handles auto-detection of:
//! - Ticketing systems (tissue, beads, tasks, session)
//! - Memory backends (markdown, total-recall, mcp)
//!
//! Discovery order is configurable via the Grove config file.
//! Individual systems can be enabled or disabled via overrides.

pub mod backends;
pub mod tickets;

pub use backends::{
    create_default_backend, create_primary_backend, detect_backends, probe_markdown, BackendInfo,
    BackendType,
};
pub use tickets::{
    detect_ticketing_system, match_close_command, probe_beads, probe_tissue, ClosePattern,
    TicketingInfo, TicketingSystem,
};
