//! Contract checking: a value you read, not a barrier you pass.
//!
//! Ports `src/vogt/application/services/contracts.py`. Nothing here returns a
//! boolean another operation branches on, and nothing re-checks on a timer.
//! `contract evaluate` stores nothing; `contract check` records when asked.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{json, Value};

use crate::application::context::{write_of, AppContext, Built};
use crate::application::resolve;
use crate::application::writes::{audited_write, WriteOutcome};
use crate::config::VogtConfig;
use crate::core::{Clock, IdFactory, Project};
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

/// The event an adoption change appends, whether it opts in or back out.
const CONTRACT_ADOPTION_EVENT: &str = "contract.adoption_changed";

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

/// The current moment. A poisoned lock keeps the clock's last value rather than
/// panicking: one panicked holder should not take down every later request.
fn clock_now<C: Clock>(clock: &std::sync::Arc<std::sync::Mutex<C>>) -> crate::core::Moment {
    clock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .now()
}

/// The top-level names this repository carries, or `None` when the question
/// could not be asked. `None` is not "it carries nothing": it lets the
/// contract fall back to reading the filesystem.
pub fn tracked_names(root: &Path) -> Option<BTreeSet<String>> {
    if !root.join(".git").exists() {
        return None;
    }
    // Bounded, the way the git adapter runs git: a hung `ls-files` must not hang
    // the request. Python strips the listing before splitting it.
    let output = bounded_git(&["-C", &root.to_string_lossy(), "ls-files", "-z"])?;
    Some(
        output
            .trim()
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .map(|entry| entry.split('/').next().unwrap_or(entry).to_string())
            .collect(),
    )
}

/// Run git and return its stdout, or `None` when it cannot be asked — missing,
/// failing, or still running after the timeout. The timeout kills the process
/// group, so a helper git spawned does not keep the pipe open.
fn bounded_git(args: &[&str]) -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut command = Command::new("git");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            let _ = libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn().ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut stdout = String::new();
                stdout_of(&mut child, &mut stdout)?;
                return Some(stdout);
            }
            Ok(None) if started.elapsed() > crate::adapters::git::GIT_TIMEOUT => {
                #[cfg(unix)]
                unsafe {
                    libc::killpg(child.id() as i32, libc::SIGKILL);
                }
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
}

fn stdout_of(child: &mut std::process::Child, into: &mut String) -> Option<()> {
    use std::io::Read;
    child.stdout.take()?.read_to_string(into).ok().map(|_| ())
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
    let config = crate::with_ctx!(ctx, |ctx| &ctx.config);
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
    crate::with_ctx!(ctx, |ctx| contract_check(ctx, slug, reason))
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

    let now = clock_now(&ctx.clock);
    let root = Path::new(&project.root_path);
    let contract = configured_contract(&ctx.config);
    let tracked = tracked_names(root);
    // The evidence is evaluated without the exemptions. Python's collector
    // records what the contract says of the tree as it stands, and only the
    // answer the caller reads applies the exemptions. One evaluation for both
    // would drop an exempted criterion from the recorded failing list and
    // change the evidence digest.
    let evidence = evaluate(
        &project.root_path,
        &contract,
        &Filesystem,
        tracked.as_ref(),
        &[],
    );
    record_findings(&ctx.observed, &project, &evidence, &ctx.clock)?;

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

/// Opt a project into the contract, or back out of it.
///
/// Adoption is a declaration, never inferred from a passing check or a
/// scaffolded directory. Repeating the posture a project already has changes
/// nothing and says so. Declining clears the last verdict too: a project that
/// is no longer measured has nothing to have been found non-compliant about.
pub fn contract_adopt_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    set_adoption(ctx, &params, true)
}

pub fn contract_decline_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    set_adoption(ctx, &params, false)
}

fn set_adoption(ctx: &Built, params: &Value, adopted: bool) -> Result<Value, VogtError> {
    let operation = if adopted {
        "contract.adopt"
    } else {
        "contract.decline"
    };
    let slug = params
        .get("project")
        .and_then(Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest(format!("{operation} needs a project")))?;
    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest(format!("{operation} needs a reason")))?;
    crate::with_ctx!(ctx, |ctx| apply_adoption(
        ctx, operation, slug, reason, adopted
    ))
}

fn apply_adoption<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    operation: &str,
    slug: &str,
    reason: &str,
    adopted: bool,
) -> Result<Value, VogtError> {
    let project = resolve::project(&ctx.declared.read()?, slug)?;
    let now = clock_now(&ctx.clock);
    let already = project.contract_adopted_at.is_some() == adopted;
    let adopted_at = if adopted {
        Some(if already {
            project.contract_adopted_at.unwrap_or(now)
        } else {
            now
        })
    } else {
        None
    };
    let detail = if already {
        format!("{} already had that posture; nothing changed", project.slug)
    } else if adopted {
        "the contract applies to this project from now on; `contract check` evaluates it"
            .to_string()
    } else {
        NOT_ADOPTED_DETAIL.to_string()
    };
    let result = json!({
        "project": project.slug,
        "adopted": adopted,
        "adopted_at": adopted_at.map(|moment| moment.to_json()),
        "status": if adopted { "not_checked" } else { "not_applicable" },
        "detail": detail,
    });
    let mut write = write_of(ctx);
    record_adoption(
        &mut write,
        operation,
        reason,
        &AdoptionRecord {
            now,
            project_id: project.id,
            project_slug: project.slug,
            adopted,
            already,
            result,
        },
    )
}

struct AdoptionRecord {
    now: crate::core::Moment,
    project_id: String,
    project_slug: String,
    adopted: bool,
    already: bool,
    result: Value,
}

fn record_adoption<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut crate::application::writes::WriteContext<
        '_,
        C,
        I,
        crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>,
    >,
    operation: &str,
    reason: &str,
    adoption: &AdoptionRecord,
) -> Result<Value, VogtError> {
    let project_id = adoption.project_id.clone();
    let project_slug = adoption.project_slug.clone();
    let adopted = adoption.adopted;
    let already = adoption.already;
    let now = adoption.now;
    let result = adoption.result.clone();
    audited_write(write, operation, reason, |txn, _actor| {
        if !already {
            txn.update_project(
                &project_id,
                &crate::storage::interface::ProjectUpdate {
                    contract_adopted_at: if adopted { Some(now) } else { None },
                    clear_contract_adopted_at: !adopted,
                    // Declining resets the verdict; adopting leaves it for the
                    // next check rather than inventing one.
                    compliance_status: if adopted {
                        None
                    } else {
                        Some("not_checked".to_string())
                    },
                    ..crate::storage::interface::ProjectUpdate::default()
                },
                now,
            )?;
        }
        Ok(WriteOutcome {
            result,
            entity_kind: "project".to_string(),
            entity_id: project_id.clone(),
            payload: json!({ "contract_adopted": adopted }),
            event_kind: CONTRACT_ADOPTION_EVENT.to_string(),
            summary: json!({ "slug": project_slug, "adopted": adopted }),
        })
    })
}

/// What a recorded check writes back onto the project, gathered so the audit
/// closure takes one argument rather than one per field.
struct CheckRecord {
    now: crate::core::Moment,
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
    audited_write(write, "contract.check", reason, |txn, _actor| {
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
fn record_findings<O: ObservedStore, C: Clock>(
    observed: &O,
    project: &Project,
    result: &ContractResult,
    clock: &std::sync::Arc<std::sync::Mutex<C>>,
) -> Result<(), VogtError> {
    if !observed.has_evidence_tables()? {
        return Ok(());
    }
    let now = clock_now(clock);
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
    // Ordered, matching Python's json.dumps(stats): projects, new, unchanged.
    let recorded = [
        ("projects", 1i64),
        ("new", stats.new),
        ("unchanged", stats.unchanged),
    ];
    observed.finish_sweep(
        &sweep.id,
        crate::core::SweepOutcome::Ok,
        &recorded,
        // Python reads the clock again for the finish, so a step clock stamps
        // finished_at one second after started_at.
        clock_now(clock),
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
