//! The collector plugin registry. Ports `src/vogt/collectors/registry.py`.
//!
//! Core collectors require no network and are always present. Optional ones
//! register themselves only when configured, and their absence yields "not
//! collected", never failure (NFR-PO1).

use crate::errors::VogtError;

use super::base::{ActivityIndexer, Collector};
use super::contract_checker::ContractCheckerCollector;
use super::dep_refs::DepRefCollector;
use super::git_local::GitLocalCollector;
use super::source_markers::SourceMarkerCollector;

/// Whether this collector opts out of unnamed sweeps.
pub fn is_on_demand(collector: &dyn Collector) -> bool {
    collector.on_demand_only()
}

/// The offline collectors every instance has.
///
/// `contract-checker` is `on_demand_only`, so a plain `sweep` does not run it.
/// `mirrored-source` needs the registered project list, so the application
/// layer adds it after this set is built. Until the remaining collectors land,
/// an unnamed sweep selects `dep-refs`, `git-local`, `source-markers` and
/// `mirrored-source`.
pub fn core_collectors() -> Vec<Box<dyn Collector>> {
    vec![
        Box::new(ContractCheckerCollector),
        Box::new(DepRefCollector),
        Box::new(GitLocalCollector),
        Box::new(SourceMarkerCollector),
    ]
}

/// What the sweeper holds: an ordinary collector, or one that indexes
/// transcripts. A collector with post-append bookkeeping still answers
/// `after_append`; the sweeper calls it.
pub enum Registered {
    Collector(Box<dyn Collector>),
    Indexer(Box<dyn ActivityIndexer>),
}

impl Registered {
    pub fn name(&self) -> &str {
        match self {
            Self::Collector(c) => c.name(),
            Self::Indexer(c) => c.name(),
        }
    }

    fn requires_network(&self) -> bool {
        match self {
            Self::Collector(c) => c.requires_network(),
            Self::Indexer(c) => c.requires_network(),
        }
    }

    fn on_demand_only(&self) -> bool {
        match self {
            Self::Collector(c) => c.on_demand_only(),
            Self::Indexer(c) => c.on_demand_only(),
        }
    }

    pub fn project_scoped(&self) -> bool {
        match self {
            Self::Collector(c) => c.project_scoped(),
            Self::Indexer(c) => c.project_scoped(),
        }
    }
}

/// The collectors this instance can run.
pub struct CollectorRegistry {
    collectors: std::collections::BTreeMap<String, Registered>,
}

impl CollectorRegistry {
    /// `None` starts from [`core_collectors`]; `Some` starts empty of them.
    pub fn new(collectors: Option<Vec<Registered>>) -> Result<Self, String> {
        let mut registry = Self {
            collectors: std::collections::BTreeMap::new(),
        };
        match collectors {
            Some(collectors) => {
                for collector in collectors {
                    registry.add(collector)?;
                }
            }
            None => {
                for collector in core_collectors() {
                    registry.add(Registered::Collector(collector))?;
                }
            }
        }
        Ok(registry)
    }

    pub fn add(&mut self, collector: Registered) -> Result<(), String> {
        let name = collector.name().to_string();
        if self.collectors.contains_key(&name) {
            return Err(format!("duplicate collector name: {name}"));
        }
        self.collectors.insert(name, collector);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.collectors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.collectors.is_empty()
    }

    pub fn names(&self) -> Vec<String> {
        self.collectors.keys().cloned().collect()
    }

    /// Only the collectors that touch nothing outside this machine.
    pub fn offline(&self) -> Vec<&Registered> {
        self.collectors
            .values()
            .filter(|collector| !collector.requires_network())
            .collect()
    }

    pub fn get(&self, name: &str) -> Result<&Registered, VogtError> {
        self.collectors.get(name).ok_or_else(|| {
            let available = self.names().join(", ");
            VogtError::NotFound(format!(
                "no collector named '{name}' (available: {})",
                if available.is_empty() {
                    "none"
                } else {
                    &available
                }
            ))
        })
    }

    /// Resolve a caller's choice of collectors.
    ///
    /// An unnamed sweep skips on-demand-only collectors. That is the mechanism
    /// behind "the contract is evaluated when someone asks".
    pub fn select(
        &self,
        names: &[String],
        offline_only: bool,
    ) -> Result<Vec<&Registered>, VogtError> {
        if !names.is_empty() {
            return names
                .iter()
                .map(|name| self.get(name))
                .filter(|found| match found {
                    Ok(collector) => !(offline_only && collector.requires_network()),
                    Err(_) => true,
                })
                .collect();
        }
        Ok(self
            .collectors
            .values()
            .filter(|collector| {
                !collector.on_demand_only() && !(offline_only && collector.requires_network())
            })
            .collect())
    }

    /// Take the named collectors out, in the order asked for.
    pub fn take(&mut self, names: &[String]) -> Result<Vec<Registered>, VogtError> {
        let mut taken = Vec::with_capacity(names.len());
        for name in names {
            let collector = self.collectors.remove(name).ok_or_else(|| {
                let available = self.names().join(", ");
                VogtError::NotFound(format!(
                    "no collector named '{name}' (available: {})",
                    if available.is_empty() {
                        "none"
                    } else {
                        &available
                    }
                ))
            })?;
            taken.push(collector);
        }
        Ok(taken)
    }
}
