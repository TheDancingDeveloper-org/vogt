//! Storage. Ports `src/vogt/storage/`.
//!
//! `interface` is the contract the application layer may know about. SQL lives
//! in `sqlite` and is the only place it may.

#[expect(dead_code, reason = "ported ahead of the callers that will wire it in")]
pub mod interface;
#[expect(dead_code, reason = "ported ahead of the callers that will wire it in")]
pub mod observed_types;
pub mod sqlite;
