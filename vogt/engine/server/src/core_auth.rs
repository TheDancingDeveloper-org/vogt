//! Who a bearer is, as vogt-core decides it.
//!
//! The front door holds no token table of its own. Every credential a client
//! presents — a browser session minted by a password login, an agent's API
//! token, the stack secret the core adopted at init — is a core token, and
//! the core is the only party that can say whose it is. The engine asks
//! (`GET /api/auth/whoami` with the bearer forwarded) and caches the answer
//! briefly, keyed by a digest of the bearer so the cache never holds a
//! credential in the clear.
//!
//! What the cache is for: a PWA polls `/api/status` and `/api/sessions`
//! every few seconds, and a whoami round trip per request would double the
//! core's load for no new information. What it is *not*: a second source of
//! truth. A positive answer lives seconds, a refusal fewer, and a logout
//! evicts its entry outright (`vogt_core::api`), so a revoked session stops
//! opening engine routes within one poll interval — and never opens a core
//! route at all, because `/api/vogt/*` forwards the bearer for the core to
//! judge afresh.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// How long a resolved identity is trusted before the core is asked again.
pub const POSITIVE_TTL: Duration = Duration::from_secs(15);
/// How long a refusal is remembered. Short: a token issued a moment after a
/// guess must not inherit the guess's refusal for long.
pub const NEGATIVE_TTL: Duration = Duration::from_secs(3);
/// Entries beyond this are pruned oldest-first on insert; a front door has
/// tens of live credentials, not thousands.
const MAX_ENTRIES: usize = 1024;

/// What the core said about a bearer.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CoreIdentity {
    pub identity_ref: String,
    pub kind: String,
    pub display_name: String,
    /// The effective scope set, implications applied — the core does that
    /// so no consumer re-derives it.
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// Why a bearer could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// The core answered and does not recognise the bearer: a 401.
    Rejected,
    /// The core could not be asked, or answered nonsense. Not a 401 —
    /// telling a browser its credential is wrong because the core is
    /// restarting is exactly the collapse of "offline" into "unauthorized"
    /// the PWA guards against.
    Unavailable(String),
}

#[derive(Debug, Clone)]
struct Entry {
    outcome: Result<CoreIdentity, ResolveError>,
    expires: Instant,
    inserted: Instant,
}

/// The cache. One per engine, shared by the bearer gate and the WebSocket
/// attach handshake.
#[derive(Default)]
pub struct CoreIdentityCache {
    entries: Mutex<HashMap<[u8; 32], Entry>>,
}

impl CoreIdentityCache {
    /// A remembered answer for this bearer, if one is still fresh.
    pub fn get(&self, bearer: &str) -> Option<Result<CoreIdentity, ResolveError>> {
        let key = digest(bearer);
        let entries = self.entries.lock();
        let entry = entries.get(&key)?;
        (Instant::now() < entry.expires).then(|| entry.outcome.clone())
    }

    /// Remember an answer. Refusals are kept briefly; transport failures are
    /// not kept at all, so a core that comes back is noticed on the next
    /// request rather than after a timeout.
    pub fn put(&self, bearer: &str, outcome: Result<CoreIdentity, ResolveError>) {
        let ttl = match &outcome {
            Ok(_) => POSITIVE_TTL,
            Err(ResolveError::Rejected) => NEGATIVE_TTL,
            Err(ResolveError::Unavailable(_)) => return,
        };
        let now = Instant::now();
        let mut entries = self.entries.lock();
        if entries.len() >= MAX_ENTRIES {
            entries.retain(|_, e| now < e.expires);
            if entries.len() >= MAX_ENTRIES {
                if let Some(oldest) = entries
                    .iter()
                    .min_by_key(|(_, e)| e.inserted)
                    .map(|(k, _)| *k)
                {
                    entries.remove(&oldest);
                }
            }
        }
        entries.insert(
            digest(bearer),
            Entry {
                outcome,
                expires: now + ttl,
                inserted: now,
            },
        );
    }

    /// Forget one bearer — what a logout does, so the session it revoked
    /// stops opening engine routes at once rather than at the TTL.
    pub fn evict(&self, bearer: &str) {
        self.entries.lock().remove(&digest(bearer));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().len()
    }
}

fn digest(bearer: &str) -> [u8; 32] {
    Sha256::digest(bearer.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str) -> CoreIdentity {
        CoreIdentity {
            identity_ref: format!("human:{name}"),
            kind: "human".into(),
            display_name: name.into(),
            scopes: vec!["read".into()],
        }
    }

    #[test]
    fn a_resolved_identity_is_remembered_and_evicted() {
        let cache = CoreIdentityCache::default();
        assert!(cache.get("bearer-a").is_none());
        cache.put("bearer-a", Ok(identity("ada")));
        assert_eq!(cache.get("bearer-a"), Some(Ok(identity("ada"))));
        cache.evict("bearer-a");
        assert!(cache.get("bearer-a").is_none());
    }

    #[test]
    fn a_refusal_is_remembered_but_an_outage_is_not() {
        let cache = CoreIdentityCache::default();
        cache.put("guess", Err(ResolveError::Rejected));
        assert_eq!(cache.get("guess"), Some(Err(ResolveError::Rejected)));
        cache.put("later", Err(ResolveError::Unavailable("down".into())));
        assert!(cache.get("later").is_none());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn the_cache_holds_digests_not_bearers() {
        let cache = CoreIdentityCache::default();
        cache.put("the-secret-value", Ok(identity("ada")));
        let entries = cache.entries.lock();
        for key in entries.keys() {
            assert_ne!(key.as_slice(), b"the-secret-value");
        }
    }
}
