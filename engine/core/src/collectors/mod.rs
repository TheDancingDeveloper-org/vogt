//! Collectors. Ports `src/vogt/collectors/`.
//!
//! A collector answers one question about one registered project and returns
//! findings. It does not decide whether they are interesting, does not write
//! anything, and does not know whether it is being run on a schedule or on
//! demand. The sweeper is the only thing here that touches the observed store.

mod base;
mod registry;
mod sweeper;

pub use base::{
    collector_error, finding, typed_failure, walk_project, ActivityIndexer, Collector,
    CollectorContext, CollectorError, Finding,
};
pub use registry::{core_collectors, is_on_demand, CollectorRegistry, Registered};
pub use sweeper::{SweepOutcomeSet, Sweeper};
