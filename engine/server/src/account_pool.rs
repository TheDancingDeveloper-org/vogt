//! Sticky Claude account assignment and concurrency accounting.
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtCapacity {
    pub counts: BTreeMap<String, usize>,
    pub cap: usize,
}

#[derive(Debug)]
pub struct AccountPool {
    names: Vec<String>,
    cap: usize,
    counts: Mutex<BTreeMap<String, usize>>,
}

impl AccountPool {
    pub fn new(names: impl IntoIterator<Item = String>, cap: usize) -> Self {
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort();
        names.dedup();
        let counts = names.iter().map(|n| (n.clone(), 0)).collect();
        Self {
            names,
            cap,
            counts: Mutex::new(counts),
        }
    }
    pub fn reserve(&self, pinned: Option<&str>) -> Result<Option<String>, AtCapacity> {
        if self.names.is_empty() {
            return Ok(None);
        }
        let mut counts = self.counts.lock().expect("account pool lock poisoned");
        let selected = match pinned {
            Some(name) if self.names.iter().any(|known| known == name) => name,
            Some(_) => {
                return Err(AtCapacity {
                    counts: counts.clone(),
                    cap: self.cap,
                })
            }
            None => self
                .names
                .iter()
                .min_by_key(|n| (counts[*n], n.as_str()))
                .expect("account candidates"),
        };
        let count = counts.get_mut(selected).expect("account count");
        if *count >= self.cap {
            return Err(AtCapacity {
                counts: counts.clone(),
                cap: self.cap,
            });
        }
        *count += 1;
        Ok(Some(selected.to_string()))
    }

    /// Reserve a slot whose lifetime is tied to the returned value.  This is
    /// the only reservation API used by session startup: dropping it releases
    /// the slot, including when a later validation or spawn fails.
    pub fn lease(self: &Arc<Self>, pinned: Option<&str>) -> Result<AccountLease, AtCapacity> {
        let account = self.reserve(pinned)?;
        Ok(AccountLease {
            pool: Arc::clone(self),
            account,
            released: AtomicBool::new(false),
        })
    }

    /// Rebuild the reservation of a session which was live when the engine
    /// was restarted.  Unlike `reserve`, this preserves the recorded
    /// assignment.  The slot is owned by the returned lease, so it can only be
    /// taken once and is returned when the lease is dropped or released; there
    /// is no way to hold a restored slot without a lease to give it back.
    pub fn restore(self: &Arc<Self>, account: Option<&str>) -> Option<AccountLease> {
        let account = account?;
        let mut counts = self.counts.lock().ok()?;
        let count = counts.get_mut(account)?;
        *count += 1;
        Some(AccountLease {
            pool: Arc::clone(self),
            account: Some(account.to_string()),
            released: AtomicBool::new(false),
        })
    }
    fn release(&self, account: Option<&str>) {
        if let Some(account) = account {
            if let Ok(mut counts) = self.counts.lock() {
                if let Some(count) = counts.get_mut(account) {
                    *count = count.saturating_sub(1);
                }
            }
        }
    }
    #[cfg(test)]
    pub fn counts(&self) -> BTreeMap<String, usize> {
        self.counts.lock().unwrap().clone()
    }
}

#[derive(Debug)]
pub struct AccountLease {
    pool: Arc<AccountPool>,
    account: Option<String>,
    released: AtomicBool,
}

impl AccountLease {
    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }
    pub fn release(&self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            self.pool.release(self.account.as_deref());
        }
    }
}

impl Drop for AccountLease {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn least_loaded_and_cap() {
        let pool = AccountPool::new(["a".into(), "b".into()], 1);
        assert_eq!(pool.reserve(None).unwrap(), Some("a".into()));
        assert_eq!(pool.reserve(None).unwrap(), Some("b".into()));
        assert_eq!(pool.reserve(None).unwrap_err().counts["a"], 1);
        pool.release(Some("a"));
        assert_eq!(pool.reserve(Some("a")).unwrap(), Some("a".into()));
    }
    #[test]
    fn pinned_account_does_not_switch() {
        let pool = AccountPool::new(["a".into(), "b".into()], 1);
        assert_eq!(pool.reserve(Some("a")).unwrap(), Some("a".into()));
        assert!(pool.reserve(Some("a")).is_err());
    }

    #[test]
    fn lease_releases_once() {
        let pool = Arc::new(AccountPool::new(["a".into()], 1));
        let lease = pool.lease(None).unwrap();
        lease.release();
        lease.release();
        assert_eq!(pool.counts()["a"], 0);
    }
}
