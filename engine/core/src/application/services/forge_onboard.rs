//! `forge.onboard`. Ports `writeback.py`'s `onboard`.
//!
//! Consolidation is "reset the watermark and sync now": the issue and pull
//! sync collectors walk the repository's history from the start, and the
//! labels and releases reads run once beside them. Every collector here only
//! reads. Nothing is created, edited or closed upstream, and the audit row
//! records the run whichever way it ended.

use std::rc::Rc;

use serde_json::{json, Value};

use crate::adapters::forge::registry::unsupported_reason;
use crate::adapters::forge::{
    ForgeClients, ForgeDirectory, ForgeLabelsCollector, ForgeReleasesCollector, ForgeSyncCollector,
    KIND_ISSUE, KIND_LABEL, KIND_PULL_REQUEST, KIND_RELEASE,
};
use crate::application::collect::{ForgeSyncKind, OwnedForgeSync};
use crate::application::context::{write_of, AppContext, Built};
use crate::application::services::{dispatch, now_of};
use crate::application::writes::audited_action;
use crate::collectors::{Collection, Collector, CollectorContext, Registered, Sweeper};
use crate::core::{Clock, IdFactory, Moment, Project};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

pub const FORGE_ONBOARDED_EVENT: &str = "forge.onboarded";

/// The default walk depth, and the most `max_pages` may ask for.
const DEFAULT_MAX_PAGES: i64 = 10;
const MAX_MAX_PAGES: i64 = 100;

/// Above any repository's observation count, so the count is never clipped.
const COUNT_LIMIT: i64 = 100_000;

pub fn onboard_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, onboard, params)
}

/// Consolidate one project's existing forge state into observations.
///
/// A project on a host this instance cannot read, or on a host with no usable
/// token, comes back as a result that says so: zeros on their own would read as
/// "there is nothing there", which is not what happened.
fn onboard<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let slug = present_string(&params, "project")?;
    let reason = present_string(&params, "reason")?;
    max_pages_of(&params)?;
    let project = {
        let view = ctx.declared.read()?;
        view.project_by_slug(&slug)?
            .ok_or_else(|| VogtError::NotFound(format!("no project {slug:?}")))?
    };

    // Asked before the adapter is built: "this host is not supported" and
    // "the adapter is not configured" are different answers, and both differ
    // from "there is nothing there".
    if let Some(detail) = unsupported_reason(project.repo_url.as_deref(), Some(&ctx.config)) {
        let refused = result(&project, false, Some(detail), Tally::default());
        record(ctx, &project, &reason, &refused)?;
        return Ok(refused);
    }

    let directory = Rc::new(ForgeClients::open(
        ctx.config.clone(),
        ctx.forge_transport(),
    )?);
    if directory
        .provider_for(project.repo_url.as_deref())
        .is_none()
    {
        let unconfigured = result(
            &project,
            true,
            Some(
                "the forge adapter is not configured, so nothing was read — \
                 which is 'not collected', not 'there is nothing'"
                    .to_owned(),
            ),
            Tally::default(),
        );
        record(ctx, &project, &reason, &unconfigured)?;
        return Ok(unconfigured);
    }

    // Reset the watermark, then sweep the same read path from the start of
    // history. `max_pages` is checked for the contract; Python does not pass
    // it to the walk either, so it does not bound the walk here.
    let now = now_of(&ctx.clock);
    for kind in [ForgeSyncKind::Issues, ForgeSyncKind::Pulls] {
        reset_watermark(ctx, &directory, kind, &project.id, now)?;
    }

    let collectors = vec![
        sync_collector(ctx, &directory, ForgeSyncKind::Issues),
        sync_collector(ctx, &directory, ForgeSyncKind::Pulls),
        read_collector(&directory, ForgeReadKind::Labels),
        read_collector(&directory, ForgeReadKind::Releases),
    ];
    let reports = {
        let mut clock = ctx
            .clock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Sweeper::new(
            &ctx.observed,
            CollectorContext {
                config: &ctx.config,
            },
            &mut *clock,
        )
        .run(collectors, std::slice::from_ref(&project))?
    };
    ctx.observed.rebuild_latest()?;

    let tally = Tally {
        new: reports.iter().map(|report| report.new).sum(),
        unchanged: reports.iter().map(|report| report.unchanged).sum(),
        issues: count_latest(ctx, &project.id, KIND_ISSUE)?,
        pull_requests: count_latest(ctx, &project.id, KIND_PULL_REQUEST)?,
        labels: count_latest(ctx, &project.id, KIND_LABEL)?,
        releases: count_latest(ctx, &project.id, KIND_RELEASE)?,
    };
    let done = result(&project, true, None, tally);
    record(ctx, &project, &reason, &done)?;
    Ok(done)
}

#[derive(Default)]
struct Tally {
    issues: i64,
    pull_requests: i64,
    labels: i64,
    releases: i64,
    new: i64,
    unchanged: i64,
}

fn result(project: &Project, supported: bool, detail: Option<String>, tally: Tally) -> Value {
    json!({
        "project": project.slug,
        "repo": project.repo_url,
        "issues": tally.issues,
        "pull_requests": tally.pull_requests,
        "labels": tally.labels,
        "releases": tally.releases,
        "new": tally.new,
        "unchanged": tally.unchanged,
        "mutations": 0,
        "supported": supported,
        "detail": detail,
    })
}

/// Attribute one consolidation run to the principal who asked for it. Every
/// return path records, including the ones that read nothing.
fn record<C, I>(
    ctx: &AppContext<C, I>,
    project: &Project,
    reason: &str,
    outcome: &Value,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        "forge.onboard",
        reason,
        "project",
        &project.id,
        outcome,
        FORGE_ONBOARDED_EVENT,
        None,
    )
}

fn count_latest<C, I>(
    ctx: &AppContext<C, I>,
    project_id: &str,
    kind: &str,
) -> Result<i64, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let rows = ctx.observed.latest(
        &[kind.to_owned()],
        Some(project_id),
        false,
        false,
        COUNT_LIMIT,
    )?;
    Ok(i64::try_from(rows.len()).unwrap_or(i64::MAX))
}

fn reset_watermark<C, I>(
    ctx: &AppContext<C, I>,
    directory: &Rc<ForgeClients>,
    kind: ForgeSyncKind,
    project_id: &str,
    now: Moment,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let dir: &dyn ForgeDirectory = &**directory;
    match kind {
        ForgeSyncKind::Issues => {
            ForgeSyncCollector::issues(&ctx.observed, dir).reset_watermark(project_id, now)
        }
        ForgeSyncKind::Pulls => {
            ForgeSyncCollector::pulls(&ctx.observed, dir).reset_watermark(project_id, now)
        }
    }
}

fn sync_collector<C, I>(
    ctx: &AppContext<C, I>,
    directory: &Rc<ForgeClients>,
    kind: ForgeSyncKind,
) -> Registered
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    Registered::Collector(Box::new(OwnedForgeSync::new(
        kind,
        ctx.observed.clone(),
        Rc::clone(directory),
    )))
}

/// Which current-state read onboarding runs.
#[derive(Clone, Copy)]
enum ForgeReadKind {
    Labels,
    Releases,
}

/// A provider read the sweep's registry can hold. It owns a share of the
/// directory, the same way [`OwnedForgeSync`] does, so the collector is
/// `'static` and the borrowing read collector is built per call.
struct OwnedForgeRead {
    name: &'static str,
    kind: ForgeReadKind,
    directory: Rc<ForgeClients>,
}

fn read_collector(directory: &Rc<ForgeClients>, kind: ForgeReadKind) -> Registered {
    let name = match kind {
        ForgeReadKind::Labels => "forge-labels",
        ForgeReadKind::Releases => "forge-releases",
    };
    Registered::Collector(Box::new(OwnedForgeRead {
        name,
        kind,
        directory: Rc::clone(directory),
    }))
}

impl Collector for OwnedForgeRead {
    fn name(&self) -> &str {
        self.name
    }

    fn requires_network(&self) -> bool {
        true
    }

    fn collect(
        &mut self,
        _ctx: &mut CollectorContext<'_>,
        project: &Project,
        _now: &mut dyn FnMut() -> Moment,
    ) -> Collection {
        let directory: &dyn ForgeDirectory = &*self.directory;
        let found = match self.kind {
            ForgeReadKind::Labels => ForgeLabelsCollector::new(directory).collect(project),
            ForgeReadKind::Releases => ForgeReleasesCollector::new(directory).collect(project),
        };
        found.into()
    }
}

fn max_pages_of(params: &Value) -> Result<i64, VogtError> {
    let max_pages = match params.get("max_pages") {
        None | Some(Value::Null) => DEFAULT_MAX_PAGES,
        Some(value) => value
            .as_i64()
            .ok_or_else(|| VogtError::InvalidRequest("max_pages must be an integer".to_owned()))?,
    };
    if !(1..=MAX_MAX_PAGES).contains(&max_pages) {
        return Err(VogtError::InvalidRequest(format!(
            "max_pages must be between 1 and {MAX_MAX_PAGES}, not {max_pages}"
        )));
    }
    Ok(max_pages)
}

fn present_string(params: &Value, field: &str) -> Result<String, VogtError> {
    match params.get(field) {
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(text.clone()),
        _ => Err(VogtError::InvalidRequest(format!("{field} is required"))),
    }
}
