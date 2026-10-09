//! Contract checking: a value you read, not a barrier you pass.
//!
//! Ports `src/vogt/application/services/contracts.py`. Nothing here returns a
//! boolean another operation branches on, and nothing re-checks on a timer.
//! `contract evaluate` stores nothing; `contract check` records when asked.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

use crate::application::context::{write_of, AppContext, Built};
use crate::application::resolve;
use crate::application::writes::{audited_write, WriteOutcome};
use crate::config::VogtConfig;
use crate::core::{Clock, IdFactory, Moment, Project};
use crate::decisions::{
    contract_from_settings, default_scaffold, digest_of, evaluate, recommendations, Contract,
    ContractResult, ContractTree, CriterionResult, Recommendation, Scaffold,
};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, WriteTxn};
use crate::storage::observed_types::PendingObservation;

/// The evidence kind a recorded contract check lands under.
const KIND_CONTRACT: &str = "contract.check";

/// The collector name a recorded check sweeps under, matching Python's
/// `ContractCheckerCollector.name`.
const CONTRACT_COLLECTOR: &str = "contract-checker";

/// The event a recorded check appends.
const CONTRACT_CHECKED_EVENT: &str = "contract.checked";

/// What a project that never adopted the contract is told, and why that is not
/// a criticism.
pub const NOT_ADOPTED_DETAIL: &str =
    "this project has not adopted the contract, so there is nothing for it to \
     comply with — this is not a fault. `contract adopt` opts in.";

/// The contract this instance evaluates against. The one place the four
/// settings are read, so a call site cannot pick up three and miss the fourth.
pub fn configured_contract(config: &VogtConfig) -> Contract {
    let files: Vec<&str> = config
        .contract_required_files
        .iter()
        .map(String::as_str)
        .collect();
    let dirs: Vec<&str> = config
        .contract_required_dirs
        .iter()
        .map(String::as_str)
        .collect();
    let meta: Vec<&str> = config
        .contract_required_meta
        .iter()
        .map(String::as_str)
        .collect();
    contract_from_settings(&config.contract_version, &files, &dirs, &meta)
}

/// The top-level names this repository carries, or `None` when the question
/// could not be asked. `None` is not "it carries nothing": it lets the
/// contract fall back to reading the filesystem.
pub fn tracked_names(root: &Path) -> Option<BTreeSet<String>> {
    if !root.join(".git").exists() {
        return None;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let listing = String::from_utf8(output.stdout).ok()?;
    Some(
        listing
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .map(|entry| entry.split('/').next().unwrap_or(entry).to_string())
            .collect(),
    )
}

/// A filesystem the evaluator can ask about. Paths arrive already normalised.
struct Filesystem;

impl ContractTree for Filesystem {
    fn is_dir(&self, path: &str) -> bool {
        Path::new(path).is_dir()
    }

    fn is_file(&self, path: &str) -> bool {
        Path::new(path).is_file()
    }
}

/// Evaluate the contract against any path, storing nothing.
pub fn contract_evaluate_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let path = params
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest("contract.evaluate needs a path".to_string()))?;
    let config = match ctx {
        Built::SystemRandom(ctx) => &ctx.config,
        Built::SystemSequential(ctx) => &ctx.config,
        Built::StepRandom(ctx) => &ctx.config,
        Built::StepSequential(ctx) => &ctx.config,
    };
    Ok(contract_evaluate(config, path))
}

fn contract_evaluate(config: &VogtConfig, path: &str) -> Value {
    let root = Path::new(path);
    let result = evaluate(
        path,
        &configured_contract(config),
        &Filesystem,
        tracked_names(root).as_ref(),
        &[],
    );
    check_result(&result, None, false, None, None, None)
}

/// The advisory recommendations, in the shape the transports return.
pub fn advice(result: &ContractResult, name: &str, lifecycle_state: &str) -> Vec<Recommendation> {
    let scaffold = default_scaffold(name, "the project's owner", lifecycle_state);
    recommendations(result, Some(&scaffold))
}

/// The scaffold the advice above was built from, kept so a caller that wants
/// the written files has them without a second build.
pub fn scaffold_for(name: &str, lifecycle_state: &str) -> Scaffold {
    default_scaffold(name, "the project's owner", lifecycle_state)
}

/// Evaluate a registered project's contract and record the result.
///
/// The result lands twice when the project has adopted the contract: as
/// evidence in the observed store with a subject key and a timestamp, and as a
/// projection on the project row. `recorded: true` is what distinguishes this
/// from `contract evaluate`. A project that never opted in is not measured and
/// nothing is recorded against it.
pub fn contract_check_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let slug = params
        .get("project")
        .and_then(Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest("contract.check needs a project".to_string()))?;
    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest("contract.check needs a reason".to_string()))?;
    match ctx {
        Built::SystemRandom(ctx) => contract_check(ctx, slug, reason),
        Built::SystemSequential(ctx) => contract_check(ctx, slug, reason),
        Built::StepRandom(ctx) => contract_check(ctx, slug, reason),
        Built::StepSequential(ctx) => contract_check(ctx, slug, reason),
    }
}

fn contract_check<C, I>(
    ctx: &AppContext<C, I>,
    project_slug: &str,
    reason: &str,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (project, exempt) = {
        let view = ctx.declared.read()?;
        let project = resolve::project(&view, project_slug)?;
        let exempt = exemptions(&view, &project)?;
        (project, exempt)
    };

    if project.contract_adopted_at.is_none() {
        return Ok(json!({
            "path": project.root_path,
            "project": project.slug,
            "contract_version": configured_contract(&ctx.config).version,
            "status": "not_applicable",
            "criteria": [],
            "failing": [],
            "inapplicable": [],
            "recommendations": [],
            "recorded": false,
            "checked_at": Value::Null,
            "detail": NOT_ADOPTED_DETAIL,
        }));
    }

    let now = ctx.clock.lock().expect("clock").now();
    let root = Path::new(&project.root_path);
    let contract = configured_contract(&ctx.config);
    let tracked = tracked_names(root);
    let exempt_refs: Vec<(&str, &str, &str)> = exempt
        .iter()
        .map(|(rule, target, reason)| (rule.as_str(), target.as_str(), reason.as_str()))
        .collect();
    let result = evaluate(
        &project.root_path,
        &contract,
        &Filesystem,
        tracked.as_ref(),
        &exempt_refs,
    );
    record_findings(&ctx.observed, &project, &result, now)?;
    let advice = advice(&result, &project.name, &project.lifecycle_state.to_string());

    // Everything the audit body needs, taken out of the generic context before
    // the closure is built. A closure that captures `ctx` itself does not
    // compile: the write's signature ties the closure to the clock and id
    // types, and capturing the context that holds them makes those lifetimes
    // look `'static`.
    let project_id = project.id.clone();
    let project_slug = project.slug.clone();
    let status = result.status.clone();
    let failing: Vec<String> = result.failing().iter().map(|c| c.target.clone()).collect();
    let recorded = check_result(
        &result,
        Some(&project.slug),
        true,
        Some(&now.to_json()),
        None,
        Some(&advice),
    );

    let mut write = write_of(ctx);
    record_check(
        &mut write,
        reason,
        &CheckRecord {
            now,
            project_id,
            project_slug,
            status,
            failing,
            recorded,
        },
    )
}

/// What a recorded check writes back onto the project, gathered so the audit
/// closure takes one argument rather than one per field.
struct CheckRecord {
    now: Moment,
    project_id: String,
    project_slug: String,
    status: String,
    failing: Vec<String>,
    recorded: Value,
}

/// The audited half of a check, split out so the closure is built against the
/// concrete store rather than the generic `DeclaredStore` bound. Against the
/// bound the compiler treats the closure as needing a `'static` clock and id
/// factory, and it does not compile.
fn record_check<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut crate::application::writes::WriteContext<
        '_,
        C,
        I,
        crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>,
    >,
    reason: &str,
    check: &CheckRecord,
) -> Result<Value, VogtError> {
    let project_id = check.project_id.clone();
    let project_slug = check.project_slug.clone();
    let status = check.status.clone();
    let failing = check.failing.clone();
    let now = check.now;
    let recorded = check.recorded.clone();
    audited_write(write, "contract.check", reason, |txn, _actor, _, _| {
        txn.update_project(
            &project_id,
            &crate::storage::interface::ProjectUpdate {
                compliance_status: Some(status.clone()),
                compliance_checked_at: Some(now),
                ..crate::storage::interface::ProjectUpdate::default()
            },
            now,
        )?;
        Ok(WriteOutcome {
            result: recorded,
            entity_kind: "project".to_string(),
            entity_id: project_id.clone(),
            payload: json!({ "compliance_status": status, "failing": failing }),
            event_kind: CONTRACT_CHECKED_EVENT.to_string(),
            summary: json!({
                "slug": project_slug,
                "status": status,
                "failing": failing.len(),
            }),
        })
    })
}

/// The criteria somebody declared unmeetable for this project, as
/// `(rule, target, reason)` triples the evaluator understands.
fn exemptions(
    view: &impl crate::storage::interface::ReadView,
    project: &Project,
) -> Result<Vec<(String, String, String)>, VogtError> {
    Ok(view
        .contract_exemptions(&project.id)?
        .into_iter()
        .map(|exemption| (exemption.rule, exemption.target, exemption.reason))
        .collect())
}

/// Land the check as evidence, the way every other observation lands: one sweep,
/// one finding carrying the result just computed, then the latest view rebuilt.
/// A store without evidence tables records nothing and says so by omission
/// rather than by failing the check.
fn record_findings<O: ObservedStore>(
    observed: &O,
    project: &Project,
    result: &ContractResult,
    now: Moment,
) -> Result<(), VogtError> {
    if !observed.has_evidence_tables()? {
        return Ok(());
    }
    let payload = json!({
        "contract_version": result.contract_version,
        "status": result.status,
        "failing": result.failing().iter().map(|c| json!({
            "rule": c.rule, "target": c.target, "detail": c.detail,
        })).collect::<Vec<_>>(),
        "evaluated": result.criteria.iter().map(|c| json!({
            "rule": c.rule, "target": c.target, "satisfied": c.satisfied, "tracked": c.tracked,
        })).collect::<Vec<_>>(),
    });
    let finding = PendingObservation {
        kind: KIND_CONTRACT.to_string(),
        subject_key: format!("contract:{}", project.slug),
        payload: payload.clone(),
        content_digest: digest_of(&payload),
        project_id: Some(project.id.clone()),
        source_url: None,
        promoted: false,
    };
    let sweep = observed.begin_sweep(CONTRACT_COLLECTOR, std::slice::from_ref(&project.id), now)?;
    let stats = observed.append(&sweep.id, &[finding], now)?;
    let mut recorded = BTreeMap::new();
    recorded.insert("projects".to_string(), 1);
    recorded.insert("new".to_string(), stats.new);
    recorded.insert("unchanged".to_string(), stats.unchanged);
    observed.finish_sweep(
        &sweep.id,
        crate::core::SweepOutcome::Ok,
        &recorded,
        now,
        None,
    )?;
    observed.rebuild_latest()?;
    Ok(())
}

fn check_result(
    result: &ContractResult,
    project: Option<&str>,
    recorded: bool,
    checked_at: Option<&str>,
    detail: Option<&str>,
    advice: Option<&[Recommendation]>,
) -> Value {
    let failing = result.failing();
    let inapplicable = result.inapplicable();
    let advice = advice
        .map(|items| items.to_vec())
        .unwrap_or_else(|| recommendations(result, Some(&scaffold_for("this project", "active"))));
    json!({
        "path": result.path,
        "project": project,
        "contract_version": result.contract_version,
        "status": result.status,
        "criteria": result.criteria.iter().map(criterion_view).collect::<Vec<_>>(),
        "failing": failing.iter().map(|c| criterion_view(c)).collect::<Vec<_>>(),
        "inapplicable": inapplicable.iter().map(|c| criterion_view(c)).collect::<Vec<_>>(),
        "recommendations": advice.iter().map(recommendation_view).collect::<Vec<_>>(),
        "recorded": recorded,
        "checked_at": checked_at,
        "detail": detail,
    })
}

fn criterion_view(criterion: &CriterionResult) -> Value {
    json!({
        "rule": criterion.rule,
        "target": criterion.target,
        "satisfied": criterion.satisfied,
        "detail": criterion.detail,
        "applicable": criterion.applicable,
        "tracked": criterion.tracked,
    })
}

fn recommendation_view(one: &Recommendation) -> Value {
    json!({
        "rule": one.rule,
        "target": one.target,
        "remedy": one.remedy,
        "instruction": one.instruction,
    })
}
