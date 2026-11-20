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
    /// Add an account the pool did not start with, such as a profile found
    /// under the accounts root (WI-2734), at the default cap. It is only ever
    /// taken by name: unpinned sessions still spread over the configured names.
    pub fn register(&self, name: &str) {
        if let Ok(mut counts) = self.counts.lock() {
            counts.entry(name.to_string()).or_insert(0);
        }
    }

    pub fn reserve(&self, pinned: Option<&str>) -> Result<Option<String>, AtCapacity> {
        let mut counts = self.counts.lock().expect("account pool lock poisoned");
        let selected = match pinned {
            Some(name) if counts.contains_key(name) => name.to_string(),
            Some(_) => {
                return Err(AtCapacity {
                    counts: counts.clone(),
                    cap: self.cap,
                })
            }
            None if self.names.is_empty() => return Ok(None),
            None => self
                .names
                .iter()
                .min_by_key(|n| (counts[*n], n.as_str()))
                .expect("account candidates")
                .clone(),
        };
        let count = counts.get_mut(&selected).expect("account count");
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

    #[test]
    fn three_accounts_spread_least_loaded_then_refuse_with_counts() {
        let names = ["acct1", "acct2", "acct3"].map(String::from);
        let pool = Arc::new(AccountPool::new(names, 2));
        let mut leases: Vec<_> = (0..6).map(|_| pool.lease(None).unwrap()).collect();
        let picked: Vec<_> = leases.iter().map(|l| l.account().unwrap()).collect();
        assert_eq!(
            picked,
            ["acct1", "acct2", "acct3", "acct1", "acct2", "acct3"]
        );
        let full = pool.lease(None).unwrap_err();
        assert_eq!(full.cap, 2);
        assert!(full.counts.values().all(|&c| c == 2));
        assert_eq!(full.counts.len(), 3);
        // A pinned request to a full account is refused even with others free.
        drop(leases.remove(1));
        assert!(pool.lease(Some("acct1")).is_err());
        assert_eq!(pool.lease(None).unwrap().account(), Some("acct2"));
    }

    #[test]
    fn a_registered_account_is_taken_by_name_at_the_default_cap() {
        // WI-2734: a discovered profile is refused until registered, then it
        // holds the same cap as a configured one, and unpinned work still
        // spreads over the configured names only.
        let pool = AccountPool::new(["acct1".into()], 1);
        assert!(pool.reserve(Some("found")).is_err());
        pool.register("found");
        assert_eq!(pool.reserve(Some("found")).unwrap(), Some("found".into()));
        assert!(pool.reserve(Some("found")).is_err());
        assert_eq!(pool.reserve(None).unwrap(), Some("acct1".into()));
    }

    #[test]
    fn unknown_pinned_account_is_refused() {
        let pool = AccountPool::new(["acct1".into()], 8);
        assert!(pool.reserve(Some("nope")).is_err());
        assert_eq!(pool.counts()["acct1"], 0);
    }
}
