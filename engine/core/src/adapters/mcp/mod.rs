//! MCP adapter. Ports `src/vogt/adapters/mcp/`.
//!
//! The JSON-RPC framing is shared by the stdio transport and the streamable
//! HTTP route, so it lives on its own. The transports themselves — reading
//! stdin, and the HTTP route with its session and grant filtering — sit on top
//! of it.

pub mod bridge;
pub mod framing;
pub mod http;
