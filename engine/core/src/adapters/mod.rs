//! Transport adapters. Ports `src/vogt/adapters/`.
//!
//! Only `http` has behaviour in this chunk: the health routes.

pub mod engine;
pub mod forge;
pub mod git;
pub mod github;
pub mod http;
pub mod mcp;
