//! Storage. Ports `src/vogt/storage/`.
//!
//! `interface` is the contract the application layer may know about. SQL lives
//! in `sqlite` and is the only place it may.

#[allow(dead_code)]
pub mod interface;
#[allow(dead_code)]
pub mod observed_types;
pub mod sqlite;
