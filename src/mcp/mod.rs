//! MCP surface: the tools Claude Code and Codex call.
//!
//! Everything here is a thin, stateless adapter over [`crate::jobs`]. The
//! returned values are compact JSON strings: MCP clients cap tool output, and
//! a planner only needs the fields it can act on.

mod server;

pub use server::GateServer;
