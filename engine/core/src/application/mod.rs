//! Use-cases. Ports `src/vogt/application/`.
//!
//! `context`, `writes` and `resolve` are the seams every later service uses.
//! `instance` is the bootstrap use-case.

pub mod brief;
pub mod collect;
pub mod context;
pub mod contracts;
pub mod coverage;
pub mod deployed;
#[cfg(test)]
mod disposition;
pub mod drift;
pub mod forge_transport;
pub mod git_signals;
pub mod git_story;
pub mod grants;
pub mod instance;
pub mod observations;
pub mod place;
pub mod projects;
pub mod resolve;
pub mod services;
pub mod session_blocked;
pub mod session_changes;
pub mod session_reads;
pub mod session_work;
pub mod suppressions;
pub mod taxonomy;
pub mod upstream;
pub mod views;
pub mod writes;
#[cfg(test)]
mod writes_test;
