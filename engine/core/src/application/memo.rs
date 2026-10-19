//! Share an expensive read between callers while the stores have not moved.
//! Ports `src/vogt/application/memo.py` (WI-1224).
//!
//! Some answers are pure functions of the two stores' contents: the shell's
//! badge counts (`place.metrics`) and the stored half of the Inbox. Every open
//! tab and device asks for them on the same event, so one answer is kept per
//! pair of stores, keyed on a [`StoreState`] that moves whenever either
//! store's content could have: the declared revision (every audited write),
//! the newest event (an observed-side publish) and the observed projection
//! generation (every `rebuild_latest`). The memo is process-wide, because a
//! context is built per request.
//!
//! Concurrent callers share one computation. A caller that finds one running
//! waits for it and takes its answer if that computation read the stores
//! *after the caller arrived*, which is all an uncached read promises too.
//! An answer is computed over the stores as they are after its key was read,
//! so it is never older than its key. Another process writing the same data
//! directory moves the same revision and generation, so its writes are seen.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::context::AppContext;
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

/// Pairs of stores remembered per memo. A server has one; a test suite builds
/// thousands of throwaway instances, and they must not accumulate.
pub const MEMO_STORES_MAX: usize = 4;

/// Everything a memoised answer's validity depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreState {
    pub instance_id: String,
    pub revision: i64,
    pub latest_event_seq: i64,
    pub latest_generation: i64,
    /// The configuration the answer was computed under, so two configurations
    /// over one data directory never share an answer.
    pub config: String,
}

/// Read the key: four cheap point reads, no scans.
pub fn store_state<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
) -> Result<StoreState, VogtError> {
    let view = ctx.declared.read()?;
    let instance_id = view.instance_id()?;
    let revision = view.current_revision()?;
    let latest_event_seq = view.latest_event_seq()?;
    drop(view);
    Ok(StoreState {
        instance_id,
        revision,
        latest_event_seq,
        latest_generation: ctx.observed.latest_generation()?,
        config: config_key(ctx),
    })
}

fn config_key<C, I>(ctx: &AppContext<C, I>) -> String {
    format!("{:?}", ctx.config)
}

type Stores = (PathBuf, PathBuf);

fn stores_of<C: Clock + 'static, I: IdFactory + 'static>(ctx: &AppContext<C, I>) -> Stores {
    (
        ctx.declared.path().to_path_buf(),
        ctx.observed.path().to_path_buf(),
    )
}

struct Answer<T> {
    state: StoreState,
    /// Taken just before `state` was read.
    read_at: Instant,
    value: T,
}

struct Inner<T> {
    flights: HashMap<Stores, Arc<Mutex<()>>>,
    /// Least recently stored first.
    answers: Vec<(Stores, Answer<T>)>,
}

/// One remembered answer per pair of stores, valid for one [`StoreState`].
pub struct StoreMemo<T> {
    inner: Mutex<Inner<T>>,
    limit: usize,
}

impl<T: Clone> Default for StoreMemo<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> StoreMemo<T> {
    pub fn new() -> Self {
        Self::with_limit(MEMO_STORES_MAX)
    }

    pub fn with_limit(limit: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                flights: HashMap::new(),
                answers: Vec::new(),
            }),
            limit,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner<T>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The remembered answer if the stores have not moved, else a new one.
    ///
    /// `keep` decides whether a fresh answer may be remembered: an answer
    /// built while part of it was unavailable is asked for again rather than
    /// served until the next write.
    pub fn get<C, I, F, K>(
        &self,
        ctx: &AppContext<C, I>,
        compute: F,
        keep: K,
    ) -> Result<T, VogtError>
    where
        C: Clock + 'static,
        I: IdFactory + 'static,
        F: FnOnce() -> Result<T, VogtError>,
        K: FnOnce(&T) -> bool,
    {
        let arrived = Instant::now();
        let stores = stores_of(ctx);
        let flight = Arc::clone(self.lock().flights.entry(stores.clone()).or_default());
        let _flying = flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let config = config_key(ctx);
        let remembered = self
            .lock()
            .answers
            .iter()
            .find(|(key, _)| *key == stores)
            .map(|(_, answer)| (answer.state.clone(), answer.read_at, answer.value.clone()));
        if let Some((state, read_at, value)) = &remembered {
            if *read_at >= arrived && state.config == config {
                // Computed by the caller this one waited behind, from stores
                // read after this one arrived.
                return Ok(value.clone());
            }
        }
        let read_at = Instant::now();
        let state = store_state(ctx)?;
        if let Some((remembered_state, _, value)) = remembered {
            if remembered_state == state {
                return Ok(value);
            }
        }
        let value = compute()?;
        if keep(&value) {
            let mut inner = self.lock();
            inner.answers.retain(|(key, _)| *key != stores);
            inner.answers.push((
                stores,
                Answer {
                    state,
                    read_at,
                    value: value.clone(),
                },
            ));
            while inner.answers.len() > self.limit {
                let (dropped, _) = inner.answers.remove(0);
                inner.flights.remove(&dropped);
            }
        }
        Ok(value)
    }

    /// Forget every answer (tests).
    pub fn clear(&self) {
        self.lock().answers.clear();
    }

    /// Forget the answers over the stores in `data_dir`, which a restore or
    /// clone is about to replace. Other data directories keep theirs.
    pub fn forget(&self, data_dir: &std::path::Path) {
        self.lock()
            .answers
            .retain(|((declared, _), _)| declared.parent() != Some(data_dir));
    }
}
