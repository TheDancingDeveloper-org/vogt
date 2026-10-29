//! The path the streamable-HTTP route is mounted on.
//!
//! The route itself lives in `adapters/http/mcp.rs` and authenticates through
//! the shared gate. What used to live here — a second grant, a second
//! recorder and a response built from a raw body — answered the same request
//! a second way, and the mounted route never called it.

/// Where the route is mounted, matching the Python default.
pub const MCP_PATH: &str = "/mcp";
