//! Check a session's completion claim against git and the forge (WI-1835).
//! Ports `src/vogt/application/services/claims.py`.
//!
//! Reads the facts (the worktree's branch, HEAD, whether it is pushed, and how
//! many commits it gained since the session started, plus the observed pull
//! requests the claim names or that come from that branch) and hands them to
//! [`crate::core::claims`] to compare. Git is read with fixed `rev-parse`/
//! `rev-list`/`branch` commands that run no hooks. The forge is read from the
//! observed store, not called, so a check costs no forge request.
//!
//! A fact that cannot be read is listed under `unchecked`. It is never counted
//! as a mismatch, and never as a pass.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::application::context::AppContext;
use crate::application::models::{ClaimCheck, ClaimCheckClaim, ClaimPull};
use crate::application::services::sessions::SessionSummary;
use crate::core::claims::{self, ClaimFacts, PullFact};
use crate::core::{from_iso, py_repr, Clock, IdFactory, Moment};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Variables that would point git somewhere other than the worktree, or feed
/// it configuration from the caller's environment.
const GIT_ENV_DROPPED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_CONFIG_COUNT",
];

/// One read-only git question, or `None` when git cannot answer it.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["-c", "core.fsmonitor=", "-c", "core.hooksPath=/dev/null"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for name in GIT_ENV_DROPPED {
        command.env_remove(name);
    }
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    let mut child = command.spawn().ok()?;
    // Read on a thread, so a long answer cannot fill the pipe and wedge the
    // wait below.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > GIT_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return None,
        }
    };
    let bytes = reader.join().ok()?;
    if !status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).trim().to_string())
}

/// Commits on `head` made since `started` that only this branch has.
///
/// A commit any other remote branch already carries is someone else's work,
/// brought in by a rebase or pull onto a `main` that moved after the start,
/// and does not count. Only this branch's own remote ref is exempt, so this
/// session's pushed commits still count. A commit date is all git keeps, so
/// a commit this session rebased also counts: its committer date is new.
pub(crate) fn session_commits(
    root: &Path,
    head: &str,
    branch: Option<&str>,
    started: Moment,
) -> Option<i64> {
    let since = format!("--since={}", started.to_iso());
    let exclude;
    let mut args = vec!["rev-list", "--count", since.as_str(), head, "--not"];
    if let Some(branch) = branch.filter(|branch| !branch.is_empty() && *branch != "HEAD") {
        // Matched against the name under refs/remotes/, e.g. `origin/<branch>`.
        exclude = format!("--exclude=*/{branch}");
        args.push(exclude.as_str());
    }
    args.push("--remotes");
    let count = git(root, &args)?;
    digits(&count)
}

/// Python's `int(count) if count and count.isdigit() else None`.
fn digits(text: &str) -> Option<i64> {
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Python's `datetime.fromisoformat`, or `None` for anything else.
///
/// A naive timestamp reads as UTC here; Python would keep it naive and then
/// fail comparing it with the aware session start. Forge payloads carry an
/// offset, so the case does not arise from a sync.
fn parse(value: Option<&Value>) -> Option<Moment> {
    let text = value?.as_str().filter(|text| !text.is_empty())?;
    from_iso(&text.replace('Z', "+00:00")).ok()
}

/// Python's truthiness of a JSON payload value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(fields)) => !fields.is_empty(),
    }
}

/// Python's `str(payload.get("state") or "unknown")`.
fn state_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) if !text.is_empty() => text.clone(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(other) if truthy(Some(other)) => other.to_string(),
        _ => "unknown".to_string(),
    }
}

fn string_of(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_string)
}

/// The observed PRs the claim names, plus any from this branch or bound to the
/// work item.
fn pulls<C, I>(
    ctx: &AppContext<C, I>,
    project_id: Option<&str>,
    branch: Option<&str>,
    work_item: Option<&str>,
    named: &BTreeSet<i64>,
) -> Result<Vec<PullFact>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    if !ctx.observed.has_evidence_tables()? {
        return Ok(Vec::new());
    }
    let mut found: BTreeMap<i64, PullFact> = BTreeMap::new();
    let kinds = ["forge.pull_request".to_string()];
    for observation in ctx
        .observed
        .latest(&kinds, project_id, false, false, 1000)?
    {
        let payload = &observation.payload;
        // A JSON bool is not a number here, as Python's `isinstance(.., bool)`
        // guard keeps it out.
        let Some(number) = payload.get("number").and_then(Value::as_i64) else {
            continue;
        };
        let head_ref = payload.get("head_ref");
        let bound = payload
            .get("implements")
            .and_then(Value::as_array)
            .is_some_and(|edges| {
                edges.iter().any(|edge| {
                    edge.as_object().is_some_and(|edge| {
                        edge.get("subject").and_then(Value::as_str) == work_item
                    })
                })
            });
        let from_branch =
            branch.is_some_and(|branch| head_ref.and_then(Value::as_str) == Some(branch));
        if !(named.contains(&number) || from_branch || (work_item.is_some() && bound)) {
            continue;
        }
        let state = state_of(payload.get("state"));
        let merged_at = if state == "merged" {
            let merged = payload.get("merged_at");
            if truthy(merged) {
                parse(merged)
            } else {
                parse(payload.get("closed_at"))
            }
        } else {
            None
        };
        found.insert(
            number,
            PullFact {
                number,
                state,
                merged_at,
                merged_by: string_of(payload.get("merged_by")),
                checks: string_of(payload.get("checks")),
                head_ref: string_of(head_ref),
            },
        );
    }
    Ok(found.into_values().collect())
}

fn project_id<C, I>(ctx: &AppContext<C, I>, slug: Option<&str>) -> Result<Option<String>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let Some(slug) = slug else {
        return Ok(None);
    };
    Ok(ctx
        .declared
        .read()?
        .project_by_slug(slug)?
        .map(|project| project.id))
}

/// Check what `summary`'s session claims (`text`) against git and the forge.
pub fn check_claim<C, I>(
    ctx: &AppContext<C, I>,
    summary: &SessionSummary,
    claim: ClaimCheckClaim,
    text: Option<&str>,
) -> Result<ClaimCheck, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let mut unchecked = Vec::new();
    let started = summary.started_at;
    if started.is_none() {
        unchecked.push("session start time unknown".to_string());
    }
    let mut branch = None;
    let mut head = None;
    let mut pushed = None;
    let mut new_commits = None;
    let root = Path::new(&summary.cwd);
    if summary.cwd.is_empty() || !root.is_dir() {
        unchecked.push(format!(
            "worktree {} is not readable here",
            py_repr(&summary.cwd)
        ));
    } else {
        head = git(root, &["rev-parse", "HEAD"]);
        branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]);
        match head.as_deref() {
            None => unchecked.push("worktree HEAD could not be read".to_string()),
            Some(sha) => {
                pushed = git(root, &["branch", "-r", "--contains", sha])
                    .map(|remotes| !remotes.is_empty());
                if let Some(started) = started {
                    new_commits = session_commits(root, sha, branch.as_deref(), started);
                }
            }
        }
    }
    let named = claims::mentioned_pulls(text);
    let project = project_id(ctx, summary.project.as_deref())?;
    let found = pulls(
        ctx,
        project.as_deref(),
        branch.as_deref().filter(|branch| *branch != "HEAD"),
        summary.work_item.as_deref(),
        &named,
    )?;
    let missing: Vec<i64> = named
        .iter()
        .copied()
        .filter(|number| !found.iter().any(|pull| pull.number == *number))
        .collect();
    if !missing.is_empty() {
        unchecked.push(format!(
            "no forge observation of {}",
            missing
                .iter()
                .map(|number| format!("PR #{number}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let facts = ClaimFacts {
        claim_text: text.map(str::to_string),
        session_started_at: started,
        branch: branch.clone(),
        head: head.clone(),
        pushed,
        new_commits,
        pulls: found,
    };
    let mismatches = claims::mismatches(&facts);
    Ok(ClaimCheck {
        claim,
        session: Some(summary.id.clone()),
        work_item: summary.work_item.clone(),
        session_started_at: started.map(Moment::to_json),
        branch,
        head,
        pushed,
        new_commits,
        pulls: facts
            .pulls
            .into_iter()
            .map(|pull| ClaimPull {
                number: pull.number,
                state: pull.state,
                merged_at: pull.merged_at.map(Moment::to_json),
                merged_by: pull.merged_by,
                checks: pull.checks,
                head_ref: pull.head_ref,
            })
            .collect(),
        mismatches,
        unchecked,
    })
}

/// The wire value of a claim, for the note's words.
fn claim_name(claim: ClaimCheckClaim) -> &'static str {
    match claim {
        ClaimCheckClaim::AutopilotDone => "autopilot-done",
        ClaimCheckClaim::Review => "review",
        ClaimCheckClaim::Done => "done",
    }
}

/// Python's `f"{value}"` for an optional value: `None` when absent.
fn or_none(value: Option<impl ToString>) -> String {
    value.map_or_else(|| "None".to_string(), |value| value.to_string())
}

pub fn comment_body(check: &ClaimCheck) -> String {
    let mut lines = vec![format!(
        "Claim check ({}) for session {}: mismatches",
        claim_name(check.claim),
        or_none(check.session.as_deref())
    )];
    lines.extend(
        check
            .mismatches
            .iter()
            .map(|mismatch| format!("- {mismatch}")),
    );
    let head = match check.head.as_deref() {
        Some(head) if !head.is_empty() => head.chars().take(12).collect::<String>(),
        _ => "?".to_string(),
    };
    lines.push(format!(
        "Facts: branch {}, head {head}, pushed {}, new commits {}.",
        or_none(check.branch.as_deref()),
        match check.pushed {
            Some(true) => "True",
            Some(false) => "False",
            None => "None",
        },
        or_none(check.new_commits),
    ));
    if !check.unchecked.is_empty() {
        lines.push(format!("Not checked: {}.", check.unchecked.join("; ")));
    }
    lines.join("\n")
}

/// Put a flagged check on its item as a note. Returns whether it did.
///
/// Nothing is recorded when the claim matched. A comment that cannot be
/// written does not undo or fail the operation that prompted it.
///
/// `once_key` makes the note idempotent, for a caller that sees the same
/// claim again and again (every sweep sees a done session until it moves
/// on). The key is written into the note, and a note already carrying it
/// among the item's recent comments is not written twice. Such a note is
/// only written on a native item, whose comments are local: on an upstream
/// item it would be a forge comment, posted from a read.
pub fn record_claim_check<C, I>(
    ctx: &AppContext<C, I>,
    check: &ClaimCheck,
    once_key: Option<&str>,
) -> Result<bool, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let Some(work_item) = check.work_item.as_deref() else {
        return Ok(false);
    };
    if check.mismatches.is_empty() {
        return Ok(false);
    }
    let mut body = comment_body(check);
    if let Some(once_key) = once_key {
        let marker = format!("Check key: {once_key}");
        let view = ctx.declared.read()?;
        let Some(item) = view.work_item_by_ref(work_item)? else {
            return Ok(false);
        };
        let recent = view.comments_for(&item.id, 50, true)?;
        drop(view);
        if recent.iter().any(|comment| comment.body.contains(&marker)) {
            return Ok(false);
        }
        body = format!("{body}\n{marker}");
    }
    let written = crate::application::services::work::comment_work(
        ctx,
        json!({
            "ref": work_item,
            "body": body,
            "reason": format!(
                "claim check ({}) found mismatches (WI-1835)",
                claim_name(check.claim)
            ),
        }),
    );
    Ok(written.is_ok())
}

#[cfg(test)]
#[path = "claims_test.rs"]
mod claims_test;
