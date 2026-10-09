//! The forge read collectors, behind the provider. Ports
//! `adapters/forge/collectors.py`.
//!
//! Checks, releases, labels, posture and notifications — the read surface that
//! was GitHub-specific scraping. Each resolves its provider per project and
//! calls the [`ForgeProvider`](super::provider::ForgeProvider) contract, so
//! nothing here knows which forge answered. Capability-gated: a provider that
//! declares it cannot offer posture or per-repo notifications yields a
//! `not_supported` receipt rather than an empty success.
//!
//! These carry no watermark — they are current-state reads, not incremental
//! history — so unlike the issue/PR sync they write nothing.

use serde_json::{Map, Value};

use super::kinds::{
    COLLECTOR_CHECKS, COLLECTOR_LABELS, COLLECTOR_NOTIFICATIONS, COLLECTOR_POSTURE,
    COLLECTOR_RELEASES, KIND_CHECK, KIND_LABEL, KIND_NOTIFICATION, KIND_POSTURE, KIND_RELEASE,
    KIND_SYNC,
};
use super::models::{ForgeCheck, ForgeNotification, RepoRef};
use super::provider::ForgeProvider;
use super::sync::{finding, ForgeDirectory};
use crate::config::VogtConfig;
use crate::core::{Observation, Project};
use crate::errors::VogtError;
use crate::storage::observed_types::PendingObservation;

/// Conclusions that are a failure worth an alert. Mirrors
/// `core/ci_alerts.py`'s `FAILING_CONCLUSIONS`; the alert module itself is a
/// later port.
const FAILING_CONCLUSIONS: [&str; 5] = [
    "failure",
    "timed_out",
    "startup_failure",
    "action_required",
    "error",
];

/// Runs a pull request started. They are the PR's own business, so they never
/// raise a watched-ref alert.
const PULL_REQUEST_EVENTS: [&str; 3] = ["pull_request", "pull_request_target", "merge_group"];

/// Runs GitHub starts from its own managed workflows.
const GITHUB_MANAGED_EVENTS: [&str; 1] = ["dynamic"];

/// How many failed runs per project per sweep may have their jobs looked up
/// (one forge call each). A job list never changes once a run concludes, so a
/// looked-up answer is carried forward and this budget is only spent on runs
/// that are new.
const JOB_LOOKUP_BUDGET: i64 = 5;

/// How many notification authors one sweep may resolve. The rest wait.
///
/// [`super::actors::RESOLVE_BUDGET`]: a first sweep over a busy repository must
/// not spend the hourly rate limit, so the remainder stay unknown this sweep
/// and are resolved on the next. The constant lives with the resolver; this is
/// the collector's starting budget.
const RESOLVE_BUDGET: i64 = super::actors::RESOLVE_BUDGET;

/// The previous observation of a subject, for the caches the collectors carry
/// forward (failed jobs, notification authors). Read-only: nothing here writes.
pub trait PriorObservations {
    fn latest(&self, kind: &str, project_id: &str, limit: i64) -> Vec<Observation>;
}

/// Who caused a notification, resolved at collect time (never on a read path).
///
/// [`super::actors::ActorResolver`] implements this. `None` means "not resolved
/// this sweep".
pub trait ActorBlock {
    fn actor_block(
        &self,
        provider: &dyn ForgeProvider,
        repo: &RepoRef,
        note: &ForgeNotification,
        prior: Option<&Value>,
        budget: &mut i64,
    ) -> Option<Value>;
}

/// The lane a run belongs to, or `None` when it is not watched.
///
/// A run with no ref, one a pull request started, or one GitHub started from a
/// managed workflow is never watched. A ref matching a branch pattern is a
/// branch even if a tag pattern would also match it.
pub fn watched_ref(branch: Option<&str>, event: Option<&str>, config: &VogtConfig) -> bool {
    let Some(branch) = branch.filter(|branch| !branch.is_empty()) else {
        return false;
    };
    if let Some(event) = event {
        if PULL_REQUEST_EVENTS.contains(&event) || GITHUB_MANAGED_EVENTS.contains(&event) {
            return false;
        }
    }
    config
        .ci_alert_branches
        .iter()
        .any(|pattern| glob_match(pattern, branch))
        || config
            .ci_alert_tags
            .iter()
            .any(|pattern| glob_match(pattern, branch))
}

/// `fnmatchcase`: `*`, `?` and `[seq]`/`[!seq]`, case-sensitive, no path
/// separator rules. `?` and a character class match one Unicode scalar, not one
/// byte. `ci_alerts` wants the same matching, so this is the one copy.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn rec(pattern: &[char], text: &[char]) -> bool {
        match (pattern, text) {
            ([], []) => true,
            ([head, rest @ ..], text) if *head == '*' => {
                (0..=text.len()).any(|at| rec(rest, &text[at..]))
            }
            ([head, rest @ ..], [_, tail @ ..]) if *head == '?' => rec(rest, tail),
            ([head, rest @ ..], text) if *head == '[' => match character_class(rest) {
                Some((class, after)) => {
                    !text.is_empty() && class_matches(&class, text[0]) && rec(after, &text[1..])
                }
                None => text.first().is_some_and(|other| *head == *other) && rec(rest, &text[1..]),
            },
            ([head, rest @ ..], [other, tail @ ..]) if head == other => rec(rest, tail),
            _ => false,
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    rec(&pattern, &text)
}

/// The body of a `[...]` class and what follows it, or `None` when the bracket
/// never closes — which is a literal `[`, the way `fnmatch` reads it.
fn character_class(pattern: &[char]) -> Option<(Vec<char>, &[char])> {
    let end = pattern.iter().position(|char| *char == ']')?;
    Some((pattern[..end].to_vec(), &pattern[end + 1..]))
}

fn class_matches(class: &[char], candidate: char) -> bool {
    let (negated, class) = match class.first() {
        Some('!') | Some('^') => (true, &class[1..]),
        _ => (false, class),
    };
    let mut matched = false;
    let mut chars = class.iter().peekable();
    while let Some(start) = chars.next() {
        if chars.peek() == Some(&&'-') {
            chars.next();
            if let Some(end) = chars.next() {
                matched |= (*start..=*end).contains(&candidate);
                continue;
            }
        }
        matched |= *start == candidate;
    }
    matched != negated
}

/// One workflow on one revision *and ref*.
///
/// The ref joins the key because one commit is routinely built twice by the
/// same workflow — on the default branch and again on the release tag cut from
/// it — and without it the two runs shared a subject, so a tag's failed
/// release could be hidden by the branch's green build of the same commit.
pub fn check_subject_key(slug: &str, check: &ForgeCheck) -> String {
    let base = format!("ci:{slug}@{}:{}", check.revision, check.check);
    match &check.branch {
        Some(branch) if !branch.is_empty() => format!("{base}@{branch}"),
        _ => base,
    }
}

fn run_identity(check: &ForgeCheck) -> (String, String, String, String, String) {
    if let Some(run_id) = check.run_id {
        return (
            "id".into(),
            run_id.to_string(),
            String::new(),
            String::new(),
            String::new(),
        );
    }
    (
        check.revision.clone(),
        check.check.clone(),
        check.branch.clone().unwrap_or_default(),
        check.event.clone().unwrap_or_default(),
        check
            .run_number
            .map(|number| number.to_string())
            .unwrap_or_default(),
    )
}

fn receipt(
    name: &str,
    project: &Project,
    repo: Option<&RepoRef>,
    supported: bool,
    count: i64,
    detail: Option<String>,
) -> PendingObservation {
    let mut payload = Map::new();
    payload.insert("collector".into(), Value::String(name.to_string()));
    payload.insert("supported".into(), Value::Bool(supported));
    payload.insert("count".into(), Value::from(count));
    payload.insert(
        "repo".into(),
        repo.map(|repo| Value::String(repo.slug()))
            .unwrap_or(Value::Null),
    );
    payload.insert(
        "detail".into(),
        detail.map(Value::String).unwrap_or(Value::Null),
    );
    finding(
        KIND_SYNC,
        format!(
            "sync:{name}/{}",
            repo.map(RepoRef::slug)
                .unwrap_or_else(|| project.id.clone())
        ),
        Value::Object(payload),
        Some(&project.id),
        None,
        false,
    )
}

/// CI checks, per revision and ref.
pub struct ForgeChecksCollector<'a> {
    directory: &'a dyn ForgeDirectory,
    prior: Option<&'a dyn PriorObservations>,
}

impl<'a> ForgeChecksCollector<'a> {
    pub fn new(
        directory: &'a dyn ForgeDirectory,
        prior: Option<&'a dyn PriorObservations>,
    ) -> Self {
        Self { directory, prior }
    }

    pub fn collect(
        &self,
        config: &VogtConfig,
        project: &Project,
    ) -> Result<Vec<PendingObservation>, VogtError> {
        let Some((provider, repo)) = resolve(self.directory, project) else {
            return Ok(vec![receipt(
                COLLECTOR_CHECKS,
                project,
                None,
                false,
                0,
                Some(
                    self.directory
                        .unsupported_reason(project.repo_url.as_deref()),
                ),
            )]);
        };
        let mut runs: Vec<ForgeCheck> = Vec::new();
        let mut seen: Vec<(String, String, String, String, String)> = Vec::new();
        for check in provider
            .checks(&repo)?
            .into_iter()
            .chain(provider.watched_ref_checks(&repo)?)
        {
            let identity = run_identity(&check);
            if !seen.contains(&identity) {
                seen.push(identity);
                runs.push(check);
            }
        }
        let cached = self.cached(project, runs.len() as i64);
        let mut budget = JOB_LOOKUP_BUDGET;
        let mut out = Vec::with_capacity(runs.len() + 1);
        for check in &runs {
            let subject_key = check_subject_key(&repo.slug(), check);
            out.push(check_finding(
                &mut CheckContext {
                    config,
                    provider,
                    repo: &repo,
                    project,
                    prior: cached.get(&subject_key).map(Vec::as_slice),
                    budget: &mut budget,
                },
                check,
                &subject_key,
            )?);
        }
        out.push(receipt(
            COLLECTOR_CHECKS,
            project,
            Some(&repo),
            true,
            runs.len() as i64,
            None,
        ));
        Ok(out)
    }

    fn cached(
        &self,
        project: &Project,
        count: i64,
    ) -> std::collections::BTreeMap<String, Vec<Observation>> {
        let mut out = std::collections::BTreeMap::new();
        if count == 0 {
            return out;
        }
        if let Some(prior) = self.prior {
            for observation in prior.latest(KIND_CHECK, &project.id, count * 4 + 200) {
                out.entry(observation.subject_key.clone())
                    .or_insert_with(Vec::new)
                    .push(observation);
            }
        }
        out
    }
}

struct CheckContext<'a> {
    config: &'a VogtConfig,
    provider: &'a dyn ForgeProvider,
    repo: &'a RepoRef,
    project: &'a Project,
    prior: Option<&'a [Observation]>,
    budget: &'a mut i64,
}

fn check_finding(
    ctx: &mut CheckContext<'_>,
    check: &ForgeCheck,
    subject_key: &str,
) -> Result<PendingObservation, VogtError> {
    let repo = ctx.repo;
    let mut payload = Map::new();
    payload.insert("revision".into(), Value::String(check.revision.clone()));
    payload.insert("check".into(), Value::String(check.check.clone()));
    payload.insert("status".into(), opt(check.status.clone()));
    payload.insert("conclusion".into(), opt(check.conclusion.clone()));
    payload.insert("branch".into(), opt(check.branch.clone()));
    payload.insert("event".into(), opt(check.event.clone()));
    payload.insert(
        "run_number".into(),
        check.run_number.map(Value::from).unwrap_or(Value::Null),
    );
    payload.insert("updated_at".into(), opt(check.updated_at.clone()));
    payload.insert("repo".into(), Value::String(repo.slug()));
    if let Some(path) = &check.workflow_path {
        payload.insert("workflow_path".into(), Value::String(path.clone()));
    }
    if let Some(run_id) = check.run_id {
        payload.insert("run_id".into(), Value::from(run_id));
        payload.insert(
            "run_attempt".into(),
            check.run_attempt.map(Value::from).unwrap_or(Value::Null),
        );
    }
    if let Some(jobs) = failed_jobs(ctx, check)? {
        payload.insert("failed_jobs".into(), Value::Array(jobs));
    }
    Ok(finding(
        KIND_CHECK,
        subject_key,
        Value::Object(payload),
        Some(&ctx.project.id),
        check.source_url.clone(),
        false,
    ))
}

/// The failed jobs of a failed watched-ref run, or `None` when the run is not
/// one, or its jobs are not known yet (budget spent this sweep).
fn failed_jobs(
    ctx: &mut CheckContext<'_>,
    check: &ForgeCheck,
) -> Result<Option<Vec<Value>>, VogtError> {
    let Some(run_id) = check.run_id else {
        return Ok(None);
    };
    if !check
        .conclusion
        .as_deref()
        .is_some_and(|conclusion| FAILING_CONCLUSIONS.contains(&conclusion))
        || !watched_ref(check.branch.as_deref(), check.event.as_deref(), ctx.config)
    {
        return Ok(None);
    }
    if let Some(prior) = ctx.prior.and_then(|rows| rows.first()) {
        let same = prior.payload.get("run_id") == Some(&Value::from(run_id))
            && prior.payload.get("run_attempt")
                == Some(&check.run_attempt.map(Value::from).unwrap_or(Value::Null))
            && prior.payload.get("conclusion") == Some(&opt(check.conclusion.clone()))
            && prior
                .payload
                .get("failed_jobs")
                .and_then(Value::as_array)
                .is_some();
        if same {
            return Ok(prior
                .payload
                .get("failed_jobs")
                .and_then(Value::as_array)
                .cloned());
        }
    }
    if *ctx.budget <= 0 {
        return Ok(None);
    }
    *ctx.budget -= 1;
    Ok(Some(
        ctx.provider
            .failed_jobs(ctx.repo, run_id)?
            .into_iter()
            .map(|job| {
                let mut row = Map::new();
                row.insert("name".into(), Value::String(job.name));
                row.insert("conclusion".into(), opt(job.conclusion));
                row.insert("url".into(), opt(job.source_url));
                Value::Object(row)
            })
            .collect(),
    ))
}

/// Releases, where an observed version comes from.
pub struct ForgeReleasesCollector<'a> {
    directory: &'a dyn ForgeDirectory,
}

impl<'a> ForgeReleasesCollector<'a> {
    pub fn new(directory: &'a dyn ForgeDirectory) -> Self {
        Self { directory }
    }

    pub fn collect(&self, project: &Project) -> Result<Vec<PendingObservation>, VogtError> {
        read_all(
            self.directory,
            project,
            COLLECTOR_RELEASES,
            None,
            None,
            |provider, repo| {
                Ok(provider
                    .releases(repo)?
                    .into_iter()
                    .map(|release| {
                        let mut payload = Map::new();
                        payload.insert("tag".into(), Value::String(release.tag.clone()));
                        payload.insert("name".into(), opt(release.name));
                        payload.insert("draft".into(), Value::Bool(release.draft));
                        payload.insert("prerelease".into(), Value::Bool(release.prerelease));
                        payload.insert("published_at".into(), opt(release.published_at));
                        payload.insert("repo".into(), Value::String(repo.slug()));
                        payload.insert("source".into(), Value::String("forge release".into()));
                        finding(
                            KIND_RELEASE,
                            format!("release:{}@{}", repo.slug(), release.tag),
                            Value::Object(payload),
                            Some(&project.id),
                            release.source_url,
                            false,
                        )
                    })
                    .collect())
            },
        )
    }
}

/// Repository labels.
pub struct ForgeLabelsCollector<'a> {
    directory: &'a dyn ForgeDirectory,
}

impl<'a> ForgeLabelsCollector<'a> {
    pub fn new(directory: &'a dyn ForgeDirectory) -> Self {
        Self { directory }
    }

    pub fn collect(&self, project: &Project) -> Result<Vec<PendingObservation>, VogtError> {
        read_all(
            self.directory,
            project,
            COLLECTOR_LABELS,
            None,
            None,
            |provider, repo| {
                Ok(provider
                    .labels(repo)?
                    .into_iter()
                    .map(|label| {
                        let mut payload = Map::new();
                        payload.insert("name".into(), Value::String(label.name.clone()));
                        payload.insert("color".into(), opt(label.color));
                        payload.insert("description".into(), opt(label.description));
                        payload.insert("repo".into(), Value::String(repo.slug()));
                        finding(
                            KIND_LABEL,
                            // Unchanged from the retired consolidator so observations
                            // share a subject with any already stored.
                            format!("ghlabel:{}/{}", repo.slug(), label.name),
                            Value::Object(payload),
                            Some(&project.id),
                            None,
                            false,
                        )
                    })
                    .collect())
            },
        )
    }
}

/// Update-automation posture, three facts.
pub struct ForgePostureCollector<'a> {
    directory: &'a dyn ForgeDirectory,
}

impl<'a> ForgePostureCollector<'a> {
    pub fn new(directory: &'a dyn ForgeDirectory) -> Self {
        Self { directory }
    }

    pub fn collect(&self, project: &Project) -> Result<Vec<PendingObservation>, VogtError> {
        const MISSING: &str = "this forge does not expose repository update-automation \
posture, so it is not collected here — not that automation is off";
        read_all(
            self.directory,
            project,
            COLLECTOR_POSTURE,
            Some("supports_posture"),
            Some(MISSING),
            |provider, repo| {
                let posture = provider.posture(repo)?;
                let mut payload = Map::new();
                payload.insert(
                    "version_updates_config".into(),
                    opt(posture.version_updates_config.clone()),
                );
                payload.insert(
                    "version_updates".into(),
                    Value::Bool(posture.version_updates()),
                );
                payload.insert(
                    "vulnerability_alerts".into(),
                    posture
                        .vulnerability_alerts
                        .map(Value::Bool)
                        .unwrap_or(Value::Null),
                );
                payload.insert(
                    "automated_security_fixes".into(),
                    posture
                        .automated_security_fixes
                        .map(Value::Bool)
                        .unwrap_or(Value::Null),
                );
                payload.insert("repo".into(), Value::String(repo.slug()));
                Ok(vec![finding(
                    KIND_POSTURE,
                    format!("posture:{}", repo.slug()),
                    Value::Object(payload),
                    Some(&project.id),
                    None,
                    false,
                )])
            },
        )
    }
}

/// Per-repository notifications.
pub struct ForgeNotificationsCollector<'a> {
    directory: &'a dyn ForgeDirectory,
    prior: Option<&'a dyn PriorObservations>,
    actors: &'a dyn ActorBlock,
}

impl<'a> ForgeNotificationsCollector<'a> {
    pub fn new(
        directory: &'a dyn ForgeDirectory,
        prior: Option<&'a dyn PriorObservations>,
        actors: &'a dyn ActorBlock,
    ) -> Self {
        Self {
            directory,
            prior,
            actors,
        }
    }

    pub fn collect(&self, project: &Project) -> Result<Vec<PendingObservation>, VogtError> {
        const MISSING: &str = "this forge does not expose per-repository notifications, \
so they are not collected here";
        let Some((provider, repo)) = resolve(self.directory, project) else {
            return Ok(vec![receipt(
                COLLECTOR_NOTIFICATIONS,
                project,
                None,
                false,
                0,
                Some(
                    self.directory
                        .unsupported_reason(project.repo_url.as_deref()),
                ),
            )]);
        };
        if !provider.capabilities().supports_notifications {
            return Ok(vec![receipt(
                COLLECTOR_NOTIFICATIONS,
                project,
                Some(&repo),
                false,
                0,
                Some(MISSING.into()),
            )]);
        }
        let mut notes = provider.notifications(&repo)?;
        // Newest first, so a spent budget leaves the oldest threads unknown.
        notes.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        let cached: Vec<Observation> = match self.prior {
            Some(prior) if !notes.is_empty() => {
                prior.latest(KIND_NOTIFICATION, &project.id, notes.len() as i64 * 4 + 100)
            }
            _ => Vec::new(),
        };
        let mut budget = RESOLVE_BUDGET;
        let mut out = Vec::with_capacity(notes.len() + 1);
        for note in &notes {
            let subject_key = format!("gh:{}!{}", repo.slug(), note.thread);
            let prior_payload = cached
                .iter()
                .find(|observation| observation.subject_key == subject_key)
                .map(|observation| &observation.payload);
            let actor = self
                .actors
                .actor_block(provider, &repo, note, prior_payload, &mut budget);
            let mut payload = Map::new();
            payload.insert("thread".into(), Value::String(note.thread.clone()));
            payload.insert("reason".into(), opt(note.reason.clone()));
            payload.insert("unread".into(), Value::Bool(note.unread));
            payload.insert("title".into(), Value::String(note.title.clone()));
            payload.insert("subject_type".into(), opt(note.subject_type.clone()));
            payload.insert("updated_at".into(), opt(note.updated_at.clone()));
            payload.insert("last_read_at".into(), opt(note.last_read_at.clone()));
            payload.insert("repo".into(), Value::String(repo.slug()));
            payload.insert("source".into(), Value::String("forge notification".into()));
            // Who caused it; `None` when it could not be resolved this sweep.
            payload.insert("actor".into(), actor.unwrap_or(Value::Null));
            out.push(finding(
                KIND_NOTIFICATION,
                subject_key,
                Value::Object(payload),
                Some(&project.id),
                note.source_url.clone(),
                false,
            ));
        }
        out.push(receipt(
            COLLECTOR_NOTIFICATIONS,
            project,
            Some(&repo),
            true,
            notes.len() as i64,
            None,
        ));
        Ok(out)
    }
}

/// The provider-backed read collectors. Registered whenever a forge is
/// configured; the provider is resolved per project.
pub struct ForgeReadCollectors<'a> {
    pub checks: ForgeChecksCollector<'a>,
    pub releases: ForgeReleasesCollector<'a>,
    pub labels: ForgeLabelsCollector<'a>,
    pub posture: ForgePostureCollector<'a>,
    pub notifications: ForgeNotificationsCollector<'a>,
}

pub fn forge_read_collectors<'a>(
    directory: &'a dyn ForgeDirectory,
    prior: Option<&'a dyn PriorObservations>,
    actors: &'a dyn ActorBlock,
) -> ForgeReadCollectors<'a> {
    ForgeReadCollectors {
        checks: ForgeChecksCollector::new(directory, prior),
        releases: ForgeReleasesCollector::new(directory),
        labels: ForgeLabelsCollector::new(directory),
        posture: ForgePostureCollector::new(directory),
        notifications: ForgeNotificationsCollector::new(directory, prior, actors),
    }
}

fn resolve<'a>(
    directory: &'a dyn ForgeDirectory,
    project: &Project,
) -> Option<(&'a dyn ForgeProvider, RepoRef)> {
    let provider = directory.provider_for(project.repo_url.as_deref())?;
    let repo = provider.parse(project.repo_url.as_deref())?;
    Some((provider, repo))
}

fn read_all(
    directory: &dyn ForgeDirectory,
    project: &Project,
    name: &str,
    capability: Option<&str>,
    missing: Option<&str>,
    read: impl FnOnce(&dyn ForgeProvider, &RepoRef) -> Result<Vec<PendingObservation>, VogtError>,
) -> Result<Vec<PendingObservation>, VogtError> {
    let Some((provider, repo)) = resolve(directory, project) else {
        return Ok(vec![receipt(
            name,
            project,
            None,
            false,
            0,
            Some(directory.unsupported_reason(project.repo_url.as_deref())),
        )]);
    };
    if let Some(capability) = capability {
        let declared = match capability {
            "supports_posture" => provider.capabilities().supports_posture,
            "supports_notifications" => provider.capabilities().supports_notifications,
            _ => true,
        };
        if !declared {
            return Ok(vec![receipt(
                name,
                project,
                Some(&repo),
                false,
                0,
                missing.map(str::to_string),
            )]);
        }
    }
    let mut out = read(provider, &repo)?;
    let count = out.len() as i64;
    out.push(receipt(name, project, Some(&repo), true, count, None));
    Ok(out)
}

fn opt(value: Option<String>) -> Value {
    value.map_or(Value::Null, Value::String)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VogtConfig;

    #[test]
    fn a_watched_ref_ignores_pull_requests_and_managed_runs() {
        let config = VogtConfig {
            ci_alert_branches: vec!["main".into()],
            ci_alert_tags: vec!["v*".into()],
            ..VogtConfig::default()
        };
        assert!(watched_ref(Some("main"), Some("push"), &config));
        assert!(watched_ref(Some("v1.2.3"), Some("push"), &config));
        assert!(!watched_ref(Some("main"), Some("pull_request"), &config));
        assert!(!watched_ref(Some("main"), Some("dynamic"), &config));
        assert!(!watched_ref(Some("feature"), Some("push"), &config));
        assert!(!watched_ref(None, Some("push"), &config));
    }

    #[test]
    fn the_ref_joins_the_check_subject_key() {
        let mut check = ForgeCheck::new("abc123", "build", "acme/widgets");
        assert_eq!(
            check_subject_key("acme/widgets", &check),
            "ci:acme/widgets@abc123:build"
        );
        check.branch = Some("main".into());
        assert_eq!(
            check_subject_key("acme/widgets", &check),
            "ci:acme/widgets@abc123:build@main"
        );
        check.branch = Some("v1.0.0".into());
        assert_ne!(
            check_subject_key("acme/widgets", &check),
            "ci:acme/widgets@abc123:build@main",
            "a tag run must not share a subject with the branch run of the same commit"
        );
    }

    #[test]
    fn glob_matching_is_case_sensitive_and_star_only() {
        assert!(glob_match("v*", "v1.2.3"));
        assert!(glob_match("main", "main"));
        assert!(!glob_match("Main", "main"));
        assert!(glob_match("release-?", "release-a"));
        assert!(!glob_match("release-?", "release-ab"));
        assert!(glob_match("v[0-9]*", "v1"));
        assert!(!glob_match("v[0-9]*", "va"));
        assert!(glob_match("v[!0-9]", "va"));
        assert!(glob_match("relé-?", "relé-β"));
        assert!(!glob_match("[unterminated", "x"));
    }
}
