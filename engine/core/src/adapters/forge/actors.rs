//! Resolve who caused a notification, at collect time and bounded. Ports
//! `adapters/forge/actors.py`.
//!
//! A forge notification thread names no author. The collector follows the
//! thread's `latest_comment_url` (or, without one, the subject itself) to read
//! the author, and the owning org's member list to tell a member from an
//! outsider. Both reads go through the provider, so both are cached and both
//! happen here — at collect time — and never on a read path.
//!
//! - **Per thread** — an author is resolved once per `(url, updated_at)`. The
//!   previous observation of the same thread is the cache that survives a
//!   restart; a bounded in-process map covers the rest.
//! - **Per sweep** — at most [`RESOLVE_BUDGET`] author reads per project. The
//!   remainder stay unknown this sweep and are resolved on the next.
//! - **Org lists** — one read per `(host, owner)` per [`ORG_TTL_SECONDS`], and
//!   a failed read is remembered briefly as "unknown" rather than retried per
//!   thread.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Instant;

use serde_json::{json, Value};

use super::collectors::ActorBlock;
use super::models::{ForgeActor, ForgeNotification, RepoRef};
use super::provider::ForgeProvider;
use crate::errors::VogtError;

/// Author reads one project's notification sweep may spend.
pub const RESOLVE_BUDGET: i64 = 30;
/// How long an org member list is trusted before it is read again.
pub const ORG_TTL_SECONDS: f64 = 3600.0;
/// How long a failed org list read is remembered as "unknown".
pub const ORG_FAILURE_TTL_SECONDS: f64 = 300.0;
const AUTHOR_CACHE_SIZE: usize = 4096;

/// GitHub reports workflow runs as `CheckSuite` threads with no subject URL;
/// the author of a CI notification is the Actions app.
fn check_suite_actor() -> ForgeActor {
    ForgeActor {
        login: Some("github-actions[bot]".to_owned()),
        user_type: Some("Bot".to_owned()),
        association: None,
    }
}

struct OrgEntry {
    members: Option<Vec<String>>,
    expires: f64,
}

/// Process-wide caches for author and org-membership reads.
///
/// `started` is the monotonic origin, the way Python's `time.monotonic` is, so
/// a member list lasts an hour and a failed read five minutes. `override_now`
/// replaces it for a test that wants an entry to expire without waiting.
pub struct ActorResolver {
    started: Instant,
    override_now: RefCell<Option<f64>>,
    authors: RefCell<VecDeque<((String, String), ForgeActor)>>,
    orgs: RefCell<Vec<(String, String, OrgEntry)>>,
}

impl ActorResolver {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            override_now: RefCell::new(None),
            authors: RefCell::new(VecDeque::new()),
            orgs: RefCell::new(Vec::new()),
        }
    }

    /// Seconds since construction, unless a test has pinned the clock.
    fn now(&self) -> f64 {
        self.override_now
            .borrow()
            .unwrap_or_else(|| self.started.elapsed().as_secs_f64())
    }

    /// Pin the clock, for a test that wants a cache entry to expire. Production
    /// never calls this; the monotonic clock above is what a sweep sees.
    pub fn set_now(&self, now: f64) {
        *self.override_now.borrow_mut() = Some(now);
    }

    pub fn clear(&self) {
        self.authors.borrow_mut().clear();
        self.orgs.borrow_mut().clear();
    }

    /// The org's members, or `None` when the list could not be read.
    ///
    /// A failure is cached for [`ORG_FAILURE_TTL_SECONDS`], so one refused read
    /// does not become one retry per thread.
    pub fn org_members(&self, provider: &dyn ForgeProvider, repo: &RepoRef) -> Option<Vec<String>> {
        let key = (repo.host.clone(), repo.owner.to_ascii_lowercase());
        let now = self.now();
        let mut orgs = self.orgs.borrow_mut();
        if let Some((_, _, entry)) = orgs
            .iter()
            .find(|(host, owner, entry)| host == &key.0 && owner == &key.1 && entry.expires > now)
        {
            return entry.members.clone();
        }
        let (members, ttl) = match provider.org_members(&repo.owner) {
            Ok(members) => (members, ORG_TTL_SECONDS),
            Err(_error) => (None, ORG_FAILURE_TTL_SECONDS),
        };
        orgs.retain(|(host, owner, _)| host != &key.0 || owner != &key.1);
        orgs.push((
            key.0,
            key.1,
            OrgEntry {
                members: members.clone(),
                expires: now + ttl,
            },
        ));
        members
    }

    fn cached_author(&self, url: &str, stamp: &str) -> Option<ForgeActor> {
        let mut authors = self.authors.borrow_mut();
        let at = authors.iter().position(|((cached_url, cached_stamp), _)| {
            cached_url == url && cached_stamp == stamp
        })?;
        let entry = authors.remove(at).expect("the position was just found");
        let actor = entry.1.clone();
        authors.push_back(entry);
        Some(actor)
    }

    fn remember_author(&self, url: &str, stamp: &str, actor: ForgeActor) {
        let mut authors = self.authors.borrow_mut();
        authors.push_back(((url.to_owned(), stamp.to_owned()), actor));
        while authors.len() > AUTHOR_CACHE_SIZE {
            authors.pop_front();
        }
    }
}

impl ActorBlock for ActorResolver {
    fn actor_block(
        &self,
        provider: &dyn ForgeProvider,
        repo: &RepoRef,
        note: &ForgeNotification,
        prior: Option<&Value>,
        budget: &mut i64,
    ) -> Option<Value> {
        self.resolve(provider, repo, note, prior, budget)
    }
}

impl ActorResolver {
    fn resolve(
        &self,
        provider: &dyn ForgeProvider,
        repo: &RepoRef,
        note: &ForgeNotification,
        prior: Option<&Value>,
        budget: &mut i64,
    ) -> Option<Value> {
        let url = note
            .latest_comment_api_url
            .as_deref()
            .or(note.subject_api_url.as_deref());
        let stamp = note.updated_at.clone().unwrap_or_default();
        let actor = if let Some(url) = url {
            let found = from_prior(prior, url, &stamp).or_else(|| self.cached_author(url, &stamp));
            match found {
                Some(actor) => Some(actor),
                None if *budget <= 0 => return None,
                None => {
                    *budget -= 1;
                    match provider.resolve_actor(url) {
                        Err(_error) => {
                            // Refused once: stop spending on this project for
                            // this sweep. The threads stay unknown.
                            *budget = 0;
                            return None;
                        }
                        Ok(None) => return None,
                        Ok(Some(actor)) => {
                            // Only an answer is cached. "No author" is retried
                            // on a later sweep, within the same budget.
                            self.remember_author(url, &stamp, actor.clone());
                            Some(actor)
                        }
                    }
                }
            }
        } else if note.subject_type.as_deref() == Some("CheckSuite") {
            Some(check_suite_actor())
        } else {
            return None;
        }?;
        let org_member = match &actor.login {
            Some(login)
                if !login.is_empty()
                    && !actor
                        .user_type
                        .as_deref()
                        .unwrap_or("")
                        .eq_ignore_ascii_case("bot") =>
            {
                self.org_members(provider, repo).map(|members| {
                    members
                        .iter()
                        .any(|member| member.eq_ignore_ascii_case(login))
                })
            }
            _ => None,
        };
        Some(json!({
            "login": actor.login,
            "user_type": actor.user_type,
            "association": actor.association,
            "org_member": org_member,
            "resolved_from": url,
            "resolved_for": (!stamp.is_empty()).then_some(stamp),
        }))
    }
}

/// The author the previous observation resolved for the same `(url, stamp)` —
/// the cache that survives a restart.
fn from_prior(prior: Option<&Value>, url: &str, stamp: &str) -> Option<ForgeActor> {
    let block = prior?.get("actor")?;
    if block.get("resolved_from").and_then(Value::as_str) != Some(url) {
        return None;
    }
    if block
        .get("resolved_for")
        .and_then(Value::as_str)
        .unwrap_or("")
        != stamp
    {
        return None;
    }
    let text = |name: &str| block.get(name).and_then(Value::as_str).map(str::to_owned);
    Some(ForgeActor {
        login: text("login"),
        user_type: text("user_type"),
        association: text("association"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Stub {
        actor: Cell<Option<Result<Option<ForgeActor>, String>>>,
        members: Cell<Option<Result<Option<Vec<String>>, String>>>,
        actor_reads: Cell<i64>,
        member_reads: Cell<i64>,
    }

    impl Stub {
        fn new() -> Self {
            Self {
                actor: Cell::new(None),
                members: Cell::new(None),
                actor_reads: Cell::new(0),
                member_reads: Cell::new(0),
            }
        }
    }

    impl ForgeProvider for Stub {
        fn capabilities(&self) -> &super::super::models::ForgeCapabilities {
            unimplemented!("the resolver never reads capabilities")
        }
        fn list_repos(&self) -> Result<Vec<super::super::models::ForgeRepo>, VogtError> {
            Ok(Vec::new())
        }
        fn parse(&self, _: Option<&str>) -> Option<RepoRef> {
            None
        }
        fn subject_key(&self, repo: &RepoRef, number: i64) -> String {
            format!("gh:{}/{}#{number}", repo.owner, repo.repo)
        }
        fn number_of(&self, _: Option<&str>) -> Option<i64> {
            None
        }
        fn clone_url(&self, _: &RepoRef) -> String {
            String::new()
        }
        fn web_url(&self, _: &RepoRef) -> String {
            String::new()
        }
        fn describe(
            &self,
            _: &RepoRef,
        ) -> Result<Option<serde_json::Map<String, Value>>, VogtError> {
            Ok(None)
        }
        fn clone_token(&self) -> Option<&str> {
            None
        }
        fn identity(&self) -> Result<Option<(String, String)>, VogtError> {
            Ok(None)
        }
        fn issues_updated_since(
            &self,
            _: &RepoRef,
            _: Option<&str>,
        ) -> Result<Vec<super::super::models::ForgeIssue>, VogtError> {
            Ok(Vec::new())
        }
        fn pulls_updated_since(
            &self,
            _: &RepoRef,
            _: Option<&str>,
        ) -> Result<Vec<super::super::models::ForgePull>, VogtError> {
            Ok(Vec::new())
        }
        fn releases(
            &self,
            _: &RepoRef,
        ) -> Result<Vec<super::super::models::ForgeRelease>, VogtError> {
            Ok(Vec::new())
        }
        fn checks(&self, _: &RepoRef) -> Result<Vec<super::super::models::ForgeCheck>, VogtError> {
            Ok(Vec::new())
        }
        fn watched_ref_checks(
            &self,
            _: &RepoRef,
        ) -> Result<Vec<super::super::models::ForgeCheck>, VogtError> {
            Ok(Vec::new())
        }
        fn failed_jobs(
            &self,
            _: &RepoRef,
            _: i64,
        ) -> Result<Vec<super::super::models::ForgeJob>, VogtError> {
            Ok(Vec::new())
        }
        fn read_file(&self, _: &RepoRef, _: &str) -> Result<Option<Vec<u8>>, VogtError> {
            Ok(None)
        }
        fn compare(
            &self,
            _: &RepoRef,
            _: &str,
            _: &str,
        ) -> Result<Option<super::super::models::ForgeComparison>, VogtError> {
            Ok(Some(super::super::models::ForgeComparison {
                base: String::new(),
                head: String::new(),
                head_sha: None,
                status: None,
                ahead_by: 0,
                behind_by: 0,
                commits: Vec::new(),
            }))
        }
        fn labels(&self, _: &RepoRef) -> Result<Vec<super::super::models::ForgeLabel>, VogtError> {
            Ok(Vec::new())
        }
        fn posture(&self, _: &RepoRef) -> Result<super::super::models::ForgePosture, VogtError> {
            Ok(super::super::models::ForgePosture {
                version_updates_config: None,
                vulnerability_alerts: None,
                automated_security_fixes: None,
                repo: String::new(),
            })
        }
        fn notifications(&self, _: &RepoRef) -> Result<Vec<ForgeNotification>, VogtError> {
            Ok(Vec::new())
        }
        fn comment(
            &self,
            _: &RepoRef,
            _: i64,
            _: &str,
        ) -> Result<super::super::writeback::WriteBackResult, VogtError> {
            unimplemented!()
        }
        fn create_issue(
            &self,
            _: &RepoRef,
            _: &str,
            _: &str,
            _: Option<&[String]>,
        ) -> Result<super::super::writeback::WriteBackResult, VogtError> {
            unimplemented!()
        }
        fn add_labels(
            &self,
            _: &RepoRef,
            _: i64,
            _: &[String],
        ) -> Result<super::super::writeback::WriteBackResult, VogtError> {
            unimplemented!()
        }
        fn set_state(
            &self,
            _: &RepoRef,
            _: i64,
            _: &str,
        ) -> Result<super::super::writeback::WriteBackResult, VogtError> {
            unimplemented!()
        }
        fn update_issue_body(
            &self,
            _: &RepoRef,
            _: i64,
            _: &str,
        ) -> Result<super::super::writeback::WriteBackResult, VogtError> {
            unimplemented!()
        }
        fn create_repo(
            &self,
            _: &str,
            _: bool,
            _: Option<&str>,
        ) -> Result<super::super::models::ForgeRepo, VogtError> {
            unimplemented!()
        }
        fn resolve_actor(&self, _: &str) -> Result<Option<ForgeActor>, VogtError> {
            self.actor_reads.set(self.actor_reads.get() + 1);
            match self.actor.take() {
                Some(Ok(actor)) => Ok(actor),
                Some(Err(message)) => Err(VogtError::InvalidRequest(message)),
                None => Ok(None),
            }
        }
        fn org_members(&self, _: &str) -> Result<Option<Vec<String>>, VogtError> {
            self.member_reads.set(self.member_reads.get() + 1);
            match self.members.take() {
                Some(Ok(members)) => Ok(members),
                Some(Err(message)) => Err(VogtError::InvalidRequest(message)),
                None => Ok(None),
            }
        }
    }

    fn note(url: Option<&str>, stamp: Option<&str>, kind: Option<&str>) -> ForgeNotification {
        ForgeNotification {
            thread: "1".into(),
            repo: "acme/widget".into(),
            reason: None,
            unread: true,
            title: "t".into(),
            subject_type: kind.map(str::to_owned),
            updated_at: stamp.map(str::to_owned),
            last_read_at: None,
            source_url: None,
            subject_api_url: url.map(str::to_owned),
            latest_comment_api_url: None,
        }
    }

    fn repo() -> RepoRef {
        RepoRef {
            host: "github.com".into(),
            owner: "Acme".into(),
            repo: "widget".into(),
        }
    }

    fn actor(login: &str) -> ForgeActor {
        ForgeActor {
            login: Some(login.into()),
            user_type: Some("User".into()),
            association: None,
        }
    }

    #[test]
    fn a_resolved_author_is_cached_and_the_budget_is_spent_once() {
        let stub = Stub::new();
        stub.actor.set(Some(Ok(Some(actor("ada")))));
        stub.members.set(Some(Ok(Some(vec!["ada".into()]))));
        let resolver = ActorResolver::new();
        resolver.set_now(0.0);
        let mut budget = RESOLVE_BUDGET;
        let first = resolver
            .resolve(
                &stub,
                &repo(),
                &note(Some("/c/1"), Some("t1"), None),
                None,
                &mut budget,
            )
            .unwrap();
        assert_eq!(first["login"], "ada");
        assert_eq!(first["org_member"], true);
        assert_eq!(budget, RESOLVE_BUDGET - 1);

        let second = resolver
            .resolve(
                &stub,
                &repo(),
                &note(Some("/c/1"), Some("t1"), None),
                None,
                &mut budget,
            )
            .unwrap();
        assert_eq!(second["login"], "ada");
        assert_eq!(
            stub.actor_reads.get(),
            1,
            "the second read came from the cache"
        );
        assert_eq!(stub.member_reads.get(), 1, "the org list is cached too");
    }

    #[test]
    fn an_exhausted_budget_leaves_the_thread_unresolved() {
        let stub = Stub::new();
        stub.actor.set(Some(Ok(Some(actor("ada")))));
        let resolver = ActorResolver::new();
        resolver.set_now(0.0);
        let mut budget = 0;
        assert!(resolver
            .resolve(
                &stub,
                &repo(),
                &note(Some("/c/1"), None, None),
                None,
                &mut budget
            )
            .is_none());
        assert_eq!(stub.actor_reads.get(), 0);
    }

    #[test]
    fn a_refused_read_stops_the_sweep_spending() {
        let stub = Stub::new();
        stub.actor.set(Some(Err("rate limited".into())));
        let resolver = ActorResolver::new();
        resolver.set_now(0.0);
        let mut budget = 5;
        assert!(resolver
            .resolve(
                &stub,
                &repo(),
                &note(Some("/c/1"), None, None),
                None,
                &mut budget
            )
            .is_none());
        assert_eq!(budget, 0);
    }

    #[test]
    fn a_member_list_expires_and_is_read_again() {
        let stub = Stub::new();
        stub.actor.set(Some(Ok(Some(actor("ada")))));
        stub.members.set(Some(Ok(Some(vec!["ada".into()]))));
        let resolver = ActorResolver::new();
        resolver.set_now(0.0);
        let mut budget = 2;
        resolver
            .resolve(
                &stub,
                &repo(),
                &note(Some("/c/1"), None, None),
                None,
                &mut budget,
            )
            .unwrap();
        assert_eq!(stub.member_reads.get(), 1);
        // An hour later the list is stale and read again.
        resolver.set_now(ORG_TTL_SECONDS + 1.0);
        stub.actor.set(Some(Ok(Some(actor("bea")))));
        stub.members.set(Some(Ok(Some(vec!["bea".into()]))));
        resolver
            .resolve(
                &stub,
                &repo(),
                &note(Some("/c/2"), None, None),
                None,
                &mut budget,
            )
            .unwrap();
        assert_eq!(stub.member_reads.get(), 2);
    }

    #[test]
    fn an_empty_login_is_not_a_member_lookup() {
        let stub = Stub::new();
        let mut empty = actor("ada");
        empty.login = Some(String::new());
        stub.actor.set(Some(Ok(Some(empty))));
        stub.members.set(Some(Ok(Some(vec!["ada".into()]))));
        let resolver = ActorResolver::new();
        resolver.set_now(0.0);
        let mut budget = 1;
        let block = resolver
            .resolve(
                &stub,
                &repo(),
                &note(Some("/c/1"), None, None),
                None,
                &mut budget,
            )
            .unwrap();
        assert_eq!(block["org_member"], Value::Null);
        assert_eq!(stub.member_reads.get(), 0);
    }

    #[test]
    fn the_prior_observation_is_the_cache_that_survives_a_restart() {
        let stub = Stub::new();
        let prior = json!({"actor": {"login": "ada", "user_type": "User",
            "resolved_from": "/c/1", "resolved_for": "t1"}});
        let resolver = ActorResolver::new();
        resolver.set_now(0.0);
        let mut budget = 1;
        let block = resolver
            .resolve(
                &stub,
                &repo(),
                &note(Some("/c/1"), Some("t1"), None),
                Some(&prior),
                &mut budget,
            )
            .unwrap();
        assert_eq!(block["login"], "ada");
        assert_eq!(stub.actor_reads.get(), 0);
        assert_eq!(budget, 1);
    }

    #[test]
    fn a_check_suite_with_no_url_is_the_actions_bot() {
        let stub = Stub::new();
        let resolver = ActorResolver::new();
        resolver.set_now(0.0);
        let mut budget = 1;
        let block = resolver
            .resolve(
                &stub,
                &repo(),
                &note(None, None, Some("CheckSuite")),
                None,
                &mut budget,
            )
            .unwrap();
        assert_eq!(block["login"], "github-actions[bot]");
        assert_eq!(
            block["org_member"],
            Value::Null,
            "a bot is not an org member"
        );
        assert_eq!(stub.actor_reads.get(), 0);
    }

    #[test]
    fn a_failed_org_read_is_remembered_as_unknown() {
        let stub = Stub::new();
        stub.actor.set(Some(Ok(Some(actor("ada")))));
        stub.members.set(Some(Err("unavailable".into())));
        let resolver = ActorResolver::new();
        resolver.set_now(0.0);
        let mut budget = 2;
        let block = resolver
            .resolve(
                &stub,
                &repo(),
                &note(Some("/c/1"), None, None),
                None,
                &mut budget,
            )
            .unwrap();
        assert_eq!(block["org_member"], Value::Null);
        // A second thread reuses the remembered failure.
        stub.actor.set(Some(Ok(Some(actor("bea")))));
        resolver
            .resolve(
                &stub,
                &repo(),
                &note(Some("/c/2"), None, None),
                None,
                &mut budget,
            )
            .unwrap();
        assert_eq!(stub.member_reads.get(), 1);
    }
}
