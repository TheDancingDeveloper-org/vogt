//! Collectors. Ports `src/vogt/collectors/`.
//!
//! A collector answers one question about one registered project and returns
//! findings. It does not decide whether they are interesting, does not write
//! anything, and does not know whether it is being run on a schedule or on
//! demand. The sweeper is the only thing here that touches the observed store.

mod agent_activity;
mod base;
mod contract_checker;
mod dep_refs;
mod git_local;
mod mirrored_source;
pub use mirrored_source::{MirroredSourceCollector, RegisteredProject};
mod registry;
mod session_outcomes;
mod source_markers;
mod sweeper;

pub use base::CollectorContext;
pub use registry::{CollectorRegistry, Registered};
pub use sweeper::Sweeper;
