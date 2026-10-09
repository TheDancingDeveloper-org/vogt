//! Transport adapters. Ports `src/vogt/adapters/`.
//!
//! Only `http` has behaviour in this chunk: the health routes.

//! `auth_gate` is the one authorization both the HTTP and MCP adapters use.

pub mod auth_gate;
pub mod cli;
pub mod engine;
pub mod forge;
pub mod git;
pub mod github;
pub mod http;
pub mod mcp;
pub mod peer;
pub mod text;
pub mod transcripts;
