//! Storage. Ports `src/vogt/storage/`.
//!
//! `interface` is the contract the application layer may know about. SQL lives
//! in `sqlite` and is the only place it may.

pub mod interface;
pub mod observed_types;
pub mod sqlite;
