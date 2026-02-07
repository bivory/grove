//! CLI commands for Grove.
//!
//! This module provides CLI commands for Grove, organized into:
//! - **Core commands**: reflect, skip, observe (agent interaction)
//! - **User commands**: search, list, stats, maintain (user-facing)
//! - **Utility commands**: init, backends, tickets, debug, trace, clean
//! - **Hook command**: hook (Claude Code integration)

// Core commands
pub mod observe;
pub mod reflect;
pub mod skip;

// User commands
pub mod list;
pub mod maintain;
pub mod search;
pub mod stats;

// Utility commands
pub mod backends_cmd;
pub mod clean;
pub mod debug;
pub mod init;
pub mod tickets_cmd;
pub mod trace;

pub use backends_cmd::BackendsCommand;
pub use clean::CleanCommand;
pub use debug::DebugCommand;
pub use init::InitCommand;
pub use list::ListCommand;
pub use maintain::MaintainCommand;
pub use observe::ObserveCommand;
pub use reflect::ReflectCommand;
pub use search::SearchCommand;
pub use skip::SkipCommand;
pub use stats::StatsCommand;
pub use tickets_cmd::TicketsCommand;
pub use trace::TraceCommand;
