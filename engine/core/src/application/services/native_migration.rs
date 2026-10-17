//! Native-item migration on link and publish. Ports `native_migration.py`.
//!
//! When `forge.link` or `forge.publish` succeeds on a project that still holds
//! open native work items, each one is published upstream as an issue and
//! re-keyed. Its vogt-only fields fold into a work overlay under the new subject
//! key, and the native row is retired with a `superseded_by` marker. Closed and
//! archived native items stay historical and untouched.
//!
//! Migration is per item, not atomic. A provider failure stops the loop with a
//! typed error that names the items that migrated and the ones still native.
//! Re-running the link resumes from what is left, because the population is
//! recomputed each time.

use serde_json::{json, Value};

use crate::adapters::forge::registry::ForgeClient;
use crate::adapters::forge::{permits, ForgeProvider, RepoRef, WriteBackOutcome};
use crate::application::context::{write_of, AppContext};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{Clock, IdFactory, Project, WorkItem, WorkOverlay, WriteBackRecord};
use crate::errors::VogtError;
use crate::storage::interface::{ReadView, WorkFilter, WorkItemUpdate, WriteTxn};

pub const WORK_MIGRATED_EVENT: &str = "work.migrated";

/// Far above any plausible native backlog. A smaller limit would silently
/// strand the item past it, which is the one thing this module must not do.
const MIGRATION_LIMIT: i64 = 10_000;

/// The migration population: this project's open, un-retired native rows.
///
/// Terminal items are absent. Already-superseded rows are excluded by the shared
/// filter, so a resumed run picks up only what the failed run left. Rows whose
/// adoption link already names a forge object are excluded too, because the
/// upstream item already exists and a second issue would duplicate it.
pub fn open_native_items(
    view: &impl ReadView,
    project: &Project,
) -> Result<Vec<WorkItem>, VogtError> {
    let items = view.list_work_items(&WorkFilter {
        project_id: Some(project.id.clone()),
        exclude_terminal: true,
        limit: MIGRATION_LIMIT,
        ..WorkFilter::default()
    })?;
    let mut open = Vec::with_capacity(items.len());
    for item in items {
        if !already_upstream_coupled(view, &item)? {
            open.push(item);
        }
    }
    Ok(open)
}

/// Whether an adoption link already names a forge object for this row. A forge
/// object's subject key ends in `#<number>`; a source marker ends in `#L<line>`.
fn already_upstream_coupled(view: &impl ReadView, item: &WorkItem) -> Result<bool, VogtError> {
    Ok(view
        .work_links_for_subjects_by_item(&item.id)?
        .keys()
        .any(|subject| {
            subject.rsplit_once('#').is_some_and(|(_, tail)| {
                !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit())
            })
        }))
}

/// Refuse the whole link or publish before anything is sent. Migration creates
/// issues upstream, so it is governed by the same write-back policy gate as
/// `work.create`. Checking before the project links keeps "linked" and
/// "migrated" from drifting apart.
pub fn require_migratable(project: &Project, pending: &[WorkItem]) -> Result<(), VogtError> {
    if !pending.is_empty() && !permits(&project.write_back.to_string(), "create") {
        return Err(VogtError::UpstreamWriteRefused(format!(
            "cannot make {:?} upstream-truth: {} open native item(s) must migrate upstream, \
             and write-back policy {:?} does not permit 'create' — set the policy with \
             `forge writeback` first, so no open item is left behind silently",
            project.slug,
            pending.len(),
            project.write_back.to_string(),
        )));
    }
    Ok(())
}

/// Publish each pending item upstream and re-key it. The caller passes the
/// population enumerated before the link, so the policy gate and the migration
/// agree on what "pending" meant. Returns one entry per migrated item.
#[allow(clippy::too_many_arguments)]
pub fn migrate_open_native_items<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    project: &Project,
    items: &[WorkItem],
    provider: &ForgeClient,
    identity: &str,
    repo: &RepoRef,
    reason: &str,
    operation: &str,
) -> Result<Vec<Value>, VogtError> {
    let mut migrated: Vec<Value> = Vec::with_capacity(items.len());
    for item in items {
        let labels = (!item.labels.is_empty()).then(|| item.labels.clone());
        let sent =
            provider.create_issue(repo, &item.title, &migrated_body(item), labels.as_deref())?;
        let subject = match (&sent.outcome, &sent.subject_key) {
            (WriteBackOutcome::Succeeded, Some(subject)) => subject.clone(),
            _ => {
                let done = refs_of(&migrated);
                let remaining = items[migrated.len()..]
                    .iter()
                    .map(|entry| entry.reference.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(VogtError::UpstreamWriteFailed(format!(
                    "native-item migration stopped at {}: {}. Migrated upstream: {done}. \
                     Still native: {remaining} — {:?} is linked, those items remain readable, \
                     and re-running `forge link` resumes the migration",
                    item.reference,
                    sent.detail
                        .as_deref()
                        .unwrap_or("no detail from the provider"),
                    project.slug,
                )));
            }
        };
        commit_migration(
            ctx,
            project,
            item,
            &subject,
            identity,
            sent.source_url.as_deref(),
            reason,
            operation,
        )?;
        migrated.push(json!({
            "ref": item.reference,
            "subject_key": subject,
            "title": item.title,
            "source_url": sent.source_url,
        }));
    }
    Ok(migrated)
}

fn refs_of(migrated: &[Value]) -> String {
    let refs: Vec<&str> = migrated
        .iter()
        .filter_map(|entry| entry.get("ref").and_then(Value::as_str))
        .collect();
    if refs.is_empty() {
        "none".to_owned()
    } else {
        refs.join(", ")
    }
}

/// The issue body: the native prose, with its provenance named.
fn migrated_body(item: &WorkItem) -> String {
    let footer = format!("— migrated from Vogt as {}", item.reference);
    if item.body.is_empty() {
        footer
    } else {
        format!("{}\n\n{footer}", item.body)
    }
}

/// One item's local half: overlay in, native row retired, ledger written.
#[allow(clippy::too_many_arguments)]
fn commit_migration<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    project: &Project,
    item: &WorkItem,
    subject: &str,
    identity: &str,
    source_url: Option<&str>,
    reason: &str,
    operation: &str,
) -> Result<(), VogtError> {
    let now = crate::application::services::now_of(&ctx.clock);
    let record_id = crate::application::services::next_id(&ctx.id_factory, "wbk");
    let mut writing = write_of(ctx);
    audited_write(&mut writing, operation, reason, |txn, actor| {
        let existing = txn.work_overlay(subject)?;
        txn.upsert_work_overlay(&merged_overlay(existing, subject, project, now, item))?;
        txn.update_work_item(
            &item.id,
            &WorkItemUpdate {
                superseded_by: Some(subject.to_owned()),
                ..WorkItemUpdate::default()
            },
            now,
        )?;
        txn.insert_writeback(&WriteBackRecord {
            id: record_id.clone(),
            at: now,
            project_id: Some(project.id.clone()),
            work_item_id: Some(item.id.clone()),
            actor_id: actor.id.clone(),
            action: crate::core::WriteBackAction::Create,
            subject_key: Some(subject.to_owned()),
            policy: project.write_back.to_string(),
            outcome: crate::core::WriteBackOutcome::Succeeded,
            reason: reason.to_owned(),
            detail: Some(format!("as {identity}; migrated from {}", item.reference)),
            source_url: source_url.map(str::to_owned),
        })?;
        Ok(WriteOutcome::new(
            json!({
                "ref": item.reference,
                "subject_key": subject,
                "title": item.title,
                "source_url": source_url,
            }),
            "work_item",
            &item.id,
            json!({
                "ref": item.reference,
                "subject_key": subject,
                "title": item.title,
                "source_url": source_url,
            }),
            WORK_MIGRATED_EVENT,
            json!({"from": item.reference, "to": subject, "title": item.title}),
        ))
    })
    .map(|_| ())
}

/// The overlay row to store: the existing one, or a fresh one, plus the vogt-only
/// fields the native item carried. Mirrors Python's `_merged_overlay`.
fn merged_overlay(
    existing: Option<WorkOverlay>,
    subject: &str,
    project: &Project,
    now: crate::core::Moment,
    item: &WorkItem,
) -> WorkOverlay {
    let mut overlay = existing.unwrap_or_else(|| WorkOverlay {
        subject_key: subject.to_owned(),
        project_id: project.id.clone(),
        rank: None,
        workflow_state: None,
        priority: None,
        effort: None,
        assignee_actor_id: None,
        initiative_id: None,
        branches: Vec::new(),
        created_at: now,
        updated_at: now,
    });
    overlay.workflow_state = Some(item.state.clone());
    overlay.priority = Some(item.priority);
    overlay.effort = item.effort;
    overlay.assignee_actor_id = item.assignee_actor_id.clone();
    overlay.initiative_id = item.initiative_id.clone();
    overlay.updated_at = now;
    overlay
}
