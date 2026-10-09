//! Incremental all-state forge sync. Ports `adapters/forge/sync.py`.
//!
//! The collectors this replaces asked the forge for its *open* issues and PRs
//! and nothing else, so a closure upstream was never an event Vogt could see.
//! This reads every state incrementally instead: each sweep asks "what changed
//! since I last looked", so a close arrives like any other fact.
//!
//! Two pieces of per-collector bookkeeping make that work, both in the observed
//! store because neither is a fact a person asserted:
//!
//! - a **watermark** per (collector, project): the max upstream `updated_at`
//!   seen, so the next sweep fetches only what moved since (with a small
//!   overlap; digest dedup absorbs the replays);
//! - **subject confirmation**: every subject confirmed to still exist this
//!   sweep, so trust can be read from "last confirmed" rather than "last
//!   changed".
//!
//! Both are written *after* the append commits (`after_append`), never before:
//! a watermark that advanced past observations that failed to persist would
//! skip them forever.
//!
//! The provider is resolved per project through [`ForgeDirectory`], so
//! registration is not per host. A project whose forge is not configured
//! yields a receipt saying so — a zero count is never "there is nothing".

use std::cell::RefCell;
use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::edges::parse_edges;
use super::kinds::{COLLECTOR_ISSUES, COLLECTOR_PULLS, KIND_ISSUE, KIND_PULL_REQUEST, KIND_SYNC};
use super::models::{ForgeIssue, ForgePull, RepoRef};
use super::provider::ForgeProvider;
use crate::core::{from_iso, to_iso, Moment, Project};
use crate::decisions::digest_of;
use crate::errors::VogtError;
use crate::storage::observed_types::PendingObservation;

/// The bookkeeping the sync collector needs from the observed store.
///
/// The full [`crate::storage::interface::ObservedStore`] is far wider than a
/// collector may use, and the SQLite backend implements it. This is the slice
/// the watermark and the subject confirmations actually touch, so a test can
/// stand in for the store without re-implementing it.
pub trait SyncBook {
    fn get_watermark(&self, collector: &str, project_id: &str)
        -> Result<Option<String>, VogtError>;
    fn set_watermark(
        &self,
        collector: &str,
        project_id: &str,
        watermark: Option<&str>,
        at: crate::core::Moment,
    ) -> Result<(), VogtError>;
    fn touch_subjects(
        &self,
        subject_keys: &[String],
        at: crate::core::Moment,
    ) -> Result<(), VogtError>;
}

impl<S: crate::storage::interface::ObservedStore> SyncBook for S {
    fn get_watermark(
        &self,
        collector: &str,
        project_id: &str,
    ) -> Result<Option<String>, VogtError> {
        crate::storage::interface::ObservedStore::get_watermark(self, collector, project_id)
    }
    fn set_watermark(
        &self,
        collector: &str,
        project_id: &str,
        watermark: Option<&str>,
        at: crate::core::Moment,
    ) -> Result<(), VogtError> {
        crate::storage::interface::ObservedStore::set_watermark(
            self, collector, project_id, watermark, at,
        )
    }
    fn touch_subjects(
        &self,
        subject_keys: &[String],
        at: crate::core::Moment,
    ) -> Result<(), VogtError> {
        crate::storage::interface::ObservedStore::touch_subjects(self, subject_keys, at)
    }
}

/// Re-ask for a small window before the watermark, so a subject updated in the
/// same second as the boundary is not skipped. Digest dedup means the overlap
/// costs nothing but a few unchanged reads.
const OVERLAP_SECONDS: i64 = 60;

/// A full page means the forge may hold more that moved since the watermark;
/// the receipt says so and the next sweep continues from where this one reached.
const PAGE: usize = 100;

/// A project's watermark advance and confirmed subjects, awaiting commit.
struct Pending {
    watermark: Option<String>,
    subject_keys: Vec<String>,
}

struct ReceiptArgs<'a> {
    project: &'a Project,
    repo: Option<&'a RepoRef>,
    supported: bool,
    count: usize,
    truncated: bool,
    watermark: Option<String>,
    detail: Option<String>,
}

/// Resolves the provider a repository belongs to.
///
/// `None` means no configured forge reads that URL. `unsupported_reason` names
/// why, so a receipt can tell "no forge reads this" from "reads it, but no
/// token". The registry that implements this lands with the next chunk; the
/// collectors only need the seam.
pub trait ForgeDirectory {
    fn provider_for(&self, repo_url: Option<&str>) -> Option<&dyn ForgeProvider>;
    fn unsupported_reason(&self, repo_url: Option<&str>) -> String;
}

/// Which read a sync collector performs.
enum SyncKind {
    Issues,
    Pulls,
}

/// Shared incremental-sync plumbing; `kind` picks issues vs PRs.
pub struct ForgeSyncCollector<'a, S: SyncBook> {
    name: &'static str,
    kind: SyncKind,
    store: &'a S,
    directory: &'a dyn ForgeDirectory,
    pending: BTreeMap<String, Pending>,
}

impl<'a, S: SyncBook> ForgeSyncCollector<'a, S> {
    pub fn issues(store: &'a S, directory: &'a dyn ForgeDirectory) -> Self {
        Self::new(COLLECTOR_ISSUES, SyncKind::Issues, store, directory)
    }

    pub fn pulls(store: &'a S, directory: &'a dyn ForgeDirectory) -> Self {
        Self::new(COLLECTOR_PULLS, SyncKind::Pulls, store, directory)
    }

    fn new(
        name: &'static str,
        kind: SyncKind,
        store: &'a S,
        directory: &'a dyn ForgeDirectory,
    ) -> Self {
        Self {
            name,
            kind,
            store,
            directory,
            pending: BTreeMap::new(),
        }
    }

    pub fn requires_network(&self) -> bool {
        true
    }

    /// One project's findings, receipt last. A provider error propagates: the
    /// watermark stays where it was, because nothing here records it until
    /// [`after_append`](Self::after_append).
    pub fn collect(&mut self, project: &Project) -> Result<Vec<PendingObservation>, VogtError> {
        let provider = self.directory.provider_for(project.repo_url.as_deref());
        let repo = provider.and_then(|provider| provider.parse(project.repo_url.as_deref()));
        let (Some(provider), Some(repo)) = (provider, repo) else {
            return Ok(vec![self.receipt(ReceiptArgs {
                project,
                repo: None,
                supported: false,
                count: 0,
                truncated: false,
                watermark: None,
                detail: Some(
                    self.directory
                        .unsupported_reason(project.repo_url.as_deref()),
                ),
            })]);
        };

        let watermark = self.store.get_watermark(self.name, &project.id)?;
        let since = since_of(watermark.as_deref());
        let (findings, subject_keys, newest) = match self.kind {
            SyncKind::Issues => {
                let items = provider.issues_updated_since(&repo, since.as_deref())?;
                self.fold_issues(provider, &repo, project, &items)
            }
            SyncKind::Pulls => {
                let items = provider.pulls_updated_since(&repo, since.as_deref())?;
                self.fold_pulls(provider, &repo, project, &items)
            }
        };
        let count = subject_keys.len();
        // The stored watermark seeds the comparison, so an overlap replay of an
        // older item can never move it backwards. A watermark that does not
        // parse is treated as absent: there is no honest moment to compare
        // against, and the next sweep re-reads the window.
        let stored = watermark.as_deref().and_then(|stamp| from_iso(stamp).ok());
        let advanced = match (newest, stored) {
            (Some(seen), Some(kept)) => Some(later(seen, kept)),
            (seen, kept) => seen.or(kept),
        };
        self.pending.insert(
            project.id.clone(),
            Pending {
                watermark: advanced.map(to_iso),
                subject_keys,
            },
        );
        let mut out = findings;
        out.push(self.receipt(ReceiptArgs {
            project,
            repo: Some(&repo),
            supported: true,
            count,
            truncated: count >= PAGE,
            watermark: advanced.map(to_iso),
            detail: None,
        }));
        Ok(out)
    }

    /// Commit each project's watermark and confirmations post-append.
    pub fn after_append(&mut self, at: Moment) -> Result<(), VogtError> {
        for (project_id, pending) in &self.pending {
            if let Some(watermark) = &pending.watermark {
                self.store
                    .set_watermark(self.name, project_id, Some(watermark), at)?;
            }
            self.store.touch_subjects(&pending.subject_keys, at)?;
        }
        self.pending.clear();
        Ok(())
    }

    /// Forget progress for one project, so the next sync backfills it.
    ///
    /// `forge onboard` is exactly "reset the watermark and sync now": the same
    /// read path a sweep uses, walked from the start of history.
    pub fn reset_watermark(&self, project_id: &str, at: Moment) -> Result<(), VogtError> {
        // `None` clears it, so the next sync backfills from the start of history.
        self.store.set_watermark(self.name, project_id, None, at)
    }

    fn fold_issues(
        &self,
        provider: &dyn ForgeProvider,
        repo: &RepoRef,
        project: &Project,
        items: &[ForgeIssue],
    ) -> (Vec<PendingObservation>, Vec<String>, Option<Moment>) {
        let mut findings = Vec::with_capacity(items.len());
        let mut keys = Vec::with_capacity(items.len());
        let mut newest: Option<Moment> = None;
        for item in items {
            let key = provider.subject_key(repo, item.number);
            keys.push(key.clone());
            if let Some(moved) = moment_of(item.updated_at.as_deref()) {
                if newest.is_none_or(|seen| {
                    moved.unix_seconds() > seen.unix_seconds()
                        || (moved.unix_seconds() == seen.unix_seconds()
                            && moved.nanos() > seen.nanos())
                }) {
                    newest = Some(moved);
                }
            }
            findings.push(issue_finding(provider, repo, project, item, &key));
        }
        (findings, keys, newest)
    }

    fn fold_pulls(
        &self,
        provider: &dyn ForgeProvider,
        repo: &RepoRef,
        project: &Project,
        items: &[ForgePull],
    ) -> (Vec<PendingObservation>, Vec<String>, Option<Moment>) {
        let mut findings = Vec::with_capacity(items.len());
        let mut keys = Vec::with_capacity(items.len());
        let mut newest: Option<Moment> = None;
        for item in items {
            let key = provider.subject_key(repo, item.number);
            keys.push(key.clone());
            if let Some(moved) = moment_of(item.updated_at.as_deref()) {
                if newest.is_none_or(|seen| {
                    moved.unix_seconds() > seen.unix_seconds()
                        || (moved.unix_seconds() == seen.unix_seconds()
                            && moved.nanos() > seen.nanos())
                }) {
                    newest = Some(moved);
                }
            }
            findings.push(pull_finding(provider, repo, project, item, &key));
        }
        (findings, keys, newest)
    }

    fn receipt(&self, args: ReceiptArgs<'_>) -> PendingObservation {
        let ReceiptArgs {
            project,
            repo,
            supported,
            count,
            truncated,
            watermark,
            detail,
        } = args;
        let mut payload = Map::new();
        payload.insert("collector".into(), Value::String(self.name.to_string()));
        payload.insert("supported".into(), Value::Bool(supported));
        payload.insert("count".into(), Value::from(count as i64));
        payload.insert("truncated".into(), Value::Bool(truncated));
        payload.insert("watermark".into(), option_string(watermark));
        payload.insert(
            "repo".into(),
            repo.map(|repo| Value::String(repo.slug()))
                .unwrap_or(Value::Null),
        );
        payload.insert("detail".into(), option_string(detail));
        finding(
            KIND_SYNC,
            format!(
                "sync:{}/{}",
                self.name,
                repo.map(RepoRef::slug)
                    .unwrap_or_else(|| project.id.clone())
            ),
            Value::Object(payload),
            Some(&project.id),
            None,
            false,
        )
    }
}

/// The incremental sync collectors — always the pair; the provider is resolved
/// per project, so registration is not per host.
pub fn forge_sync_collectors<'a, S: SyncBook>(
    store: &'a S,
    directory: &'a dyn ForgeDirectory,
) -> Vec<ForgeSyncCollector<'a, S>> {
    vec![
        ForgeSyncCollector::issues(store, directory),
        ForgeSyncCollector::pulls(store, directory),
    ]
}

/// The later of two moments, by whole seconds then the remainder.
fn later(left: Moment, right: Moment) -> Moment {
    if left.unix_seconds() > right.unix_seconds()
        || (left.unix_seconds() == right.unix_seconds() && left.nanos() > right.nanos())
    {
        left
    } else {
        right
    }
}
/// The `since` to ask the forge for: the watermark, less the overlap, in the
/// `Z`-suffixed form the forge `since` parameters expect.
fn since_of(watermark: Option<&str>) -> Option<String> {
    let moment = from_iso(watermark?).ok()?;
    Some(github_iso(rewind(moment, OVERLAP_SECONDS)))
}

fn moment_of(updated_at: Option<&str>) -> Option<Moment> {
    let text = updated_at.filter(|text| !text.is_empty())?;
    from_iso(text).ok()
}

/// `Z`-suffixed ISO with no fractional seconds, matching Python's
/// `strftime("%Y-%m-%dT%H:%M:%SZ")`.
fn github_iso(moment: Moment) -> String {
    chrono::DateTime::from_timestamp(moment.unix_seconds(), 0)
        .expect("a moment built here is in range")
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn rewind(moment: Moment, seconds: i64) -> Moment {
    Moment::from_unix(moment.unix_seconds() - seconds, moment.nanos())
}

fn issue_finding(
    _provider: &dyn ForgeProvider,
    repo: &RepoRef,
    project: &Project,
    item: &ForgeIssue,
    key: &str,
) -> PendingObservation {
    let mut payload = Map::new();
    payload.insert("number".into(), Value::from(item.number));
    payload.insert("title".into(), Value::String(item.title.clone()));
    payload.insert("state".into(), Value::String(item.state.clone()));
    payload.insert("labels".into(), string_list(&item.labels));
    payload.insert("author".into(), option_string(item.author.clone()));
    // Who the author is, as the forge reported it: the account type (`Bot`
    // for an app) and their association with the repo.
    payload.insert(
        "author_type".into(),
        option_string(item.author_type.clone()),
    );
    payload.insert(
        "author_association".into(),
        option_string(item.author_association.clone()),
    );
    payload.insert("assignees".into(), string_list(&item.assignees));
    payload.insert("comments".into(), Value::from(item.comments));
    // Carried so an initiative tracking issue is observed body-and-all: a
    // person ticking a box upstream edits the body, and a sweep must be able
    // to see the tick.
    payload.insert("body".into(), option_string(item.body.clone()));
    payload.insert("updated_at".into(), option_string(item.updated_at.clone()));
    payload.insert("closed_at".into(), option_string(item.closed_at.clone()));
    payload.insert("repo".into(), Value::String(repo.slug()));
    finding(
        KIND_ISSUE,
        key,
        Value::Object(payload),
        Some(&project.id),
        item.source_url.clone(),
        // Open issues are backlog; closed history is context, not backlog.
        item.state == "open",
    )
}

fn pull_finding(
    provider: &dyn ForgeProvider,
    repo: &RepoRef,
    project: &Project,
    item: &ForgePull,
    key: &str,
) -> PendingObservation {
    let mut payload = Map::new();
    payload.insert("number".into(), Value::from(item.number));
    payload.insert("title".into(), Value::String(item.title.clone()));
    // `merged` is a distinct lifecycle from a bare `closed`: both read
    // `closed` upstream, and only the merged one shipped.
    payload.insert(
        "state".into(),
        Value::String(if item.merged {
            "merged".to_string()
        } else {
            item.state.clone()
        }),
    );
    payload.insert("draft".into(), Value::Bool(item.draft));
    payload.insert("author".into(), option_string(item.author.clone()));
    payload.insert(
        "author_type".into(),
        option_string(item.author_type.clone()),
    );
    payload.insert(
        "author_association".into(),
        option_string(item.author_association.clone()),
    );
    payload.insert("head".into(), option_string(item.head.clone()));
    payload.insert("head_ref".into(), option_string(item.head_ref.clone()));
    payload.insert("base".into(), option_string(item.base.clone()));
    // Reviewability rollups where the provider exposes them; `None` is an
    // honest "the forge did not say".
    payload.insert(
        "review_state".into(),
        option_string(item.review_state.clone()),
    );
    payload.insert("mergeable".into(), option_string(item.mergeable.clone()));
    payload.insert("checks".into(), option_string(item.checks.clone()));
    // The observed `implemented_by` edge, read from the PR's own closing
    // keywords and branch name, each with its provenance.
    payload.insert("implements".into(), implements(provider, repo, item));
    payload.insert("updated_at".into(), option_string(item.updated_at.clone()));
    payload.insert("closed_at".into(), option_string(item.closed_at.clone()));
    payload.insert("repo".into(), Value::String(repo.slug()));
    finding(
        KIND_PULL_REQUEST,
        key,
        Value::Object(payload),
        Some(&project.id),
        item.source_url.clone(),
        false,
    )
}

/// The `implemented_by` targets this PR asserts, resolved to subject keys.
///
/// A same-repo `#3` and a cross-repo `owner/repo#3` both land as the subject
/// the rest of Vogt speaks. Provenance travels with every edge.
fn implements(provider: &dyn ForgeProvider, repo: &RepoRef, pull: &ForgePull) -> Value {
    let edges = parse_edges(
        Some(&pull.title),
        pull.body.as_deref(),
        pull.head_ref.as_deref(),
    );
    Value::Array(
        edges
            .into_iter()
            .map(|edge| {
                let target = match (&edge.owner, &edge.repo) {
                    (Some(owner), Some(name)) => RepoRef {
                        host: repo.host.clone(),
                        owner: owner.clone(),
                        repo: name.clone(),
                    },
                    _ => repo.clone(),
                };
                let mut row = Map::new();
                row.insert(
                    "subject".into(),
                    Value::String(provider.subject_key(&target, edge.number)),
                );
                row.insert("number".into(), Value::from(edge.number));
                row.insert("repo".into(), Value::String(target.slug()));
                row.insert(
                    "provenance".into(),
                    Value::String(edge.provenance.to_string()),
                );
                Value::Object(row)
            })
            .collect(),
    )
}

pub(super) fn finding(
    kind: &str,
    subject_key: impl Into<String>,
    payload: Value,
    project_id: Option<&str>,
    source_url: Option<String>,
    promoted: bool,
) -> PendingObservation {
    PendingObservation {
        content_digest: digest_of(&payload),
        kind: kind.to_string(),
        subject_key: subject_key.into(),
        payload,
        project_id: project_id.map(str::to_string),
        source_url,
        promoted,
    }
}

fn option_string(value: Option<String>) -> Value {
    value.map_or(Value::Null, Value::String)
}

fn string_list(values: &[String]) -> Value {
    Value::Array(values.iter().cloned().map(Value::String).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingStore {
        watermark: RefCell<Option<String>>,
        written: RefCell<Vec<Option<String>>>,
        touched: RefCell<Vec<Vec<String>>>,
    }

    impl SyncBook for RecordingStore {
        fn get_watermark(&self, _: &str, _: &str) -> Result<Option<String>, VogtError> {
            Ok(self.watermark.borrow().clone())
        }
        fn set_watermark(
            &self,
            _: &str,
            _: &str,
            watermark: Option<&str>,
            _: Moment,
        ) -> Result<(), VogtError> {
            self.written
                .borrow_mut()
                .push(watermark.map(str::to_string));
            Ok(())
        }
        fn touch_subjects(&self, keys: &[String], _: Moment) -> Result<(), VogtError> {
            self.touched.borrow_mut().push(keys.to_vec());
            Ok(())
        }
    }

    struct StubProvider {
        issues: Vec<ForgeIssue>,
        pulls: Vec<ForgePull>,
    }

    impl ForgeProvider for StubProvider {
        fn capabilities(&self) -> &super::super::models::ForgeCapabilities {
            unimplemented!("the sync collector never reads capabilities")
        }
        fn list_repos(&self) -> Result<Vec<super::super::models::ForgeRepo>, VogtError> {
            Ok(Vec::new())
        }
        fn parse(&self, _: Option<&str>) -> Option<RepoRef> {
            Some(RepoRef {
                host: "github.com".into(),
                owner: "acme".into(),
                repo: "widgets".into(),
            })
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
        ) -> Result<Vec<ForgeIssue>, VogtError> {
            Ok(self.issues.clone())
        }
        fn pulls_updated_since(
            &self,
            _: &RepoRef,
            _: Option<&str>,
        ) -> Result<Vec<ForgePull>, VogtError> {
            Ok(self.pulls.clone())
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
            Ok(None)
        }
        fn labels(&self, _: &RepoRef) -> Result<Vec<super::super::models::ForgeLabel>, VogtError> {
            Ok(Vec::new())
        }
        fn posture(&self, _: &RepoRef) -> Result<super::super::models::ForgePosture, VogtError> {
            unimplemented!("not read by the sync collector")
        }
        fn notifications(
            &self,
            _: &RepoRef,
        ) -> Result<Vec<super::super::models::ForgeNotification>, VogtError> {
            Ok(Vec::new())
        }
        fn resolve_actor(
            &self,
            _: &str,
        ) -> Result<Option<super::super::models::ForgeActor>, VogtError> {
            Ok(None)
        }
        fn org_members(&self, _: &str) -> Result<Option<Vec<String>>, VogtError> {
            Ok(None)
        }
        fn comment(
            &self,
            _: &RepoRef,
            _: i64,
            _: &str,
        ) -> Result<super::super::writeback::WriteBackResult, VogtError> {
            unimplemented!("the sync collector never writes")
        }
        fn create_issue(
            &self,
            _: &RepoRef,
            _: &str,
            _: &str,
            _: Option<&[String]>,
        ) -> Result<super::super::writeback::WriteBackResult, VogtError> {
            unimplemented!("the sync collector never writes")
        }
        fn add_labels(
            &self,
            _: &RepoRef,
            _: i64,
            _: &[String],
        ) -> Result<super::super::writeback::WriteBackResult, VogtError> {
            unimplemented!("the sync collector never writes")
        }
        fn set_state(
            &self,
            _: &RepoRef,
            _: i64,
            _: &str,
        ) -> Result<super::super::writeback::WriteBackResult, VogtError> {
            unimplemented!("the sync collector never writes")
        }
        fn update_issue_body(
            &self,
            _: &RepoRef,
            _: i64,
            _: &str,
        ) -> Result<super::super::writeback::WriteBackResult, VogtError> {
            unimplemented!("the sync collector never writes")
        }
        fn create_repo(
            &self,
            _: &str,
            _: bool,
            _: Option<&str>,
        ) -> Result<super::super::models::ForgeRepo, VogtError> {
            unimplemented!("the sync collector never writes")
        }
    }

    struct Directory {
        provider: Option<StubProvider>,
        reason: String,
    }

    impl ForgeDirectory for Directory {
        fn provider_for(&self, _: Option<&str>) -> Option<&dyn ForgeProvider> {
            self.provider
                .as_ref()
                .map(|provider| provider as &dyn ForgeProvider)
        }
        fn unsupported_reason(&self, _: Option<&str>) -> String {
            self.reason.clone()
        }
    }

    fn project() -> Project {
        let mut project =
            Project::new("prj_1", "widgets", "Widgets", "/w", Moment::from_unix(0, 0));
        project.repo_url = Some("https://github.com/acme/widgets".into());
        project
    }

    #[test]
    fn an_unconfigured_repo_is_a_receipt_not_an_empty_success() {
        let store = RecordingStore {
            watermark: RefCell::new(None),
            written: RefCell::new(Vec::new()),
            touched: RefCell::new(Vec::new()),
        };
        let directory = Directory {
            provider: None,
            reason: "no forge reads this host".into(),
        };
        let mut collector = ForgeSyncCollector::issues(&store, &directory);
        let findings = collector.collect(&project()).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, KIND_SYNC);
        assert_eq!(findings[0].payload["supported"], Value::Bool(false));
        assert_eq!(findings[0].payload["count"], Value::from(0));
        assert_eq!(
            findings[0].payload["detail"],
            Value::String("no forge reads this host".into())
        );
        assert!(
            collector.pending.is_empty(),
            "nothing was fetched, so nothing advances"
        );
    }

    #[test]
    fn the_watermark_advances_to_the_newest_and_only_after_append() {
        let store = RecordingStore {
            watermark: RefCell::new(Some("2026-01-01T00:00:00+00:00".into())),
            written: RefCell::new(Vec::new()),
            touched: RefCell::new(Vec::new()),
        };
        let mut older = ForgeIssue::new(1, "old", "open", "acme/widgets");
        older.updated_at = Some("2026-02-01T00:00:00Z".into());
        let mut newer = ForgeIssue::new(2, "new", "closed", "acme/widgets");
        newer.updated_at = Some("2026-03-01T12:00:00Z".into());
        newer.body = Some("closes #7".into());
        let directory = Directory {
            provider: Some(StubProvider {
                issues: vec![older, newer],
                pulls: Vec::new(),
            }),
            reason: String::new(),
        };
        let mut collector = ForgeSyncCollector::issues(&store, &directory);
        let findings = collector.collect(&project()).unwrap();
        assert!(
            store.written.borrow().is_empty(),
            "the watermark waits for the append"
        );
        let open = findings
            .iter()
            .find(|finding| finding.kind == KIND_ISSUE)
            .unwrap();
        assert!(open.promoted, "an open issue is backlog");
        let closed = findings
            .iter()
            .find(|finding| finding.payload["number"] == 2)
            .unwrap();
        assert!(!closed.promoted, "closed history is context, not backlog");
        collector.after_append(Moment::from_unix(1, 0)).unwrap();
        assert_eq!(
            store.written.borrow().as_slice(),
            &[Some("2026-03-01T12:00:00+00:00".into())]
        );
        assert_eq!(
            store.touched.borrow()[0],
            vec!["gh:acme/widgets#1".to_string(), "gh:acme/widgets#2".into()]
        );
    }

    #[test]
    fn an_older_replay_inside_the_overlap_does_not_rewind_the_watermark() {
        let store = RecordingStore {
            watermark: RefCell::new(Some("2026-03-01T00:01:00+00:00".into())),
            written: RefCell::new(Vec::new()),
            touched: RefCell::new(Vec::new()),
        };
        let mut replayed = ForgeIssue::new(1, "seen", "open", "acme/widgets");
        replayed.updated_at = Some("2026-03-01T00:00:30Z".into());
        let directory = Directory {
            provider: Some(StubProvider {
                issues: vec![replayed],
                pulls: Vec::new(),
            }),
            reason: String::new(),
        };
        let mut collector = ForgeSyncCollector::issues(&store, &directory);
        let findings = collector.collect(&project()).unwrap();
        let receipt = findings.last().unwrap();
        assert_eq!(
            receipt.payload["watermark"],
            Value::String("2026-03-01T00:01:00+00:00".into()),
            "the stored watermark is the floor"
        );
        collector.after_append(Moment::from_unix(1, 0)).unwrap();
        assert_eq!(
            store.written.borrow().as_slice(),
            &[Some("2026-03-01T00:01:00+00:00".into())]
        );
    }

    #[test]
    fn a_pull_records_its_implemented_by_edges_with_provenance() {
        let store = RecordingStore {
            watermark: RefCell::new(None),
            written: RefCell::new(Vec::new()),
            touched: RefCell::new(Vec::new()),
        };
        let mut pull = ForgePull {
            number: 4,
            title: "Fixes acme/other#9".into(),
            state: "closed".into(),
            repo: "acme/widgets".into(),
            draft: false,
            merged: true,
            author: None,
            author_type: None,
            author_association: None,
            head: None,
            head_ref: Some("wi-3-thing".into()),
            base: None,
            body: None,
            labels: Vec::new(),
            review_state: None,
            mergeable: None,
            checks: None,
            updated_at: None,
            closed_at: None,
            source_url: None,
        };
        let _ = &mut pull;
        let directory = Directory {
            provider: Some(StubProvider {
                issues: Vec::new(),
                pulls: vec![pull],
            }),
            reason: String::new(),
        };
        let mut collector = ForgeSyncCollector::pulls(&store, &directory);
        let findings = collector.collect(&project()).unwrap();
        let pr = findings
            .iter()
            .find(|finding| finding.kind == KIND_PULL_REQUEST)
            .unwrap();
        assert_eq!(pr.payload["state"], Value::String("merged".into()));
        let edges = pr.payload["implements"].as_array().unwrap();
        assert!(
            edges
                .iter()
                .any(|edge| edge["subject"] == "gh:acme/other#9"),
            "a cross-repo keyword resolves against the named repo: {edges:?}"
        );
    }

    #[test]
    fn since_rewinds_the_watermark_by_the_overlap() {
        let since = since_of(Some("2026-05-01T00:01:30+00:00")).unwrap();
        assert_eq!(since, "2026-05-01T00:00:30Z");
        assert_eq!(since_of(None), None);
        assert_eq!(moment_of(Some("not a date")), None);
    }
}
