//! Ports `tests/test_claim_check.py`'s cases that stand on git and the
//! service: a rebase's commits are not the session's, a session's move to
//! review is checked and noted, a person's move is not a claim, and a flagged
//! DONE check is noted once per key. The sweep's `done` row case waits on the
//! sweep classifying `done` rows (WI-1518); its noting half is the once-key
//! case here.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use super::{check_claim, comment_body, record_claim_check, session_commits};
use crate::application::context::{build_context, write_of, Built};
use crate::application::models::{ClaimCheck, ClaimCheckClaim};
use crate::application::services::sessions::{list_sessions, ListSessionsParams};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{
    from_iso, ActorKind, CodingSession, LinkState, Moment, Origin, Principal, Priority, Project,
    StepClock, SweepOutcome, TrustState, WorkItem, WorkKind,
};
use crate::errors::VogtError;
use crate::registry::default_registry;
use crate::storage::interface::{ObservedStore, WriteTxn};
use crate::storage::observed_types::PendingObservation;

const WHY: &str = "claim check test";
const ENGINE_ID: &str = "00000000-0000-4000-8000-000000000001";

fn started() -> Moment {
    from_iso("2026-10-18T09:00:00+00:00").unwrap()
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn temp(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vogt-claims-{label}-{}-{}",
        std::process::id(),
        unique()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// git with a fixed identity and dates, as the Python fixture runs it.
fn git_as(root: &Path, who: &str, date: &str, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env_clear()
        .env("GIT_AUTHOR_NAME", who)
        .env("GIT_AUTHOR_EMAIL", format!("{who}@example.com"))
        .env("GIT_COMMITTER_NAME", who)
        .env("GIT_COMMITTER_EMAIL", format!("{who}@example.com"))
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("HOME", root)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git(root: &Path, args: &[&str]) -> String {
    git_as(root, "t", "2026-10-17T09:00:00Z", args)
}

/// A checkout on `wi-1707-x` whose only commit predates the session.
fn repo(base: &Path) -> PathBuf {
    let root = base.join("proj");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "wi-1707-x"]);
    std::fs::write(root.join("f"), "x").unwrap();
    git(&root, &["add", "f"]);
    git(&root, &["commit", "-q", "-m", "old work"]);
    root
}

fn context(dir: &Path, principal: Principal) -> Built {
    let config = crate::config::VogtConfig {
        data_dir: dir.to_path_buf(),
        ..crate::config::VogtConfig::default()
    };
    build_context(
        config,
        Some(principal),
        Some(StepClock::new(started())),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap()
}

fn person() -> Principal {
    Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()
}

fn as_session() -> Principal {
    Principal::new(
        &format!("agent:engine:{ENGINE_ID}"),
        ActorKind::Agent,
        "Session",
    )
    .unwrap()
}

/// A project, a native item, a session bound to it in `checkout`, and an
/// observed PR #193 from the session's branch that merged the day before the
/// session started. Returns the data dir.
fn wired(base: &Path, checkout: &Path) -> PathBuf {
    let dir = base.join("data");
    std::fs::create_dir_all(&dir).unwrap();
    let mut clock = None;
    let mut ids = None;
    crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
    let built = context(&dir, person());
    let cwd = checkout.display().to_string();
    crate::with_ctx!(&built, |ctx| {
        let mut write = write_of(ctx);
        audited_write(&mut write, "seed", WHY, |txn, actor| {
            let mut project = Project::new("prj_proj", "proj", "Proj", &cwd, started());
            project.link_state = LinkState::Unlinked;
            txn.insert_project(&project)?;
            txn.insert_work_item(&WorkItem {
                id: "wrk_1".to_string(),
                reference: "WI-1".to_string(),
                kind: WorkKind::Feature,
                title: "the header button".to_string(),
                body: String::new(),
                state: "open".to_string(),
                priority: Priority::P2,
                effort: None,
                project_id: Some("prj_proj".to_string()),
                project_slug: Some("proj".to_string()),
                initiative_id: None,
                origin: Origin::Created,
                trust_state: TrustState::Unverified,
                assignee_actor_id: None,
                assignee_identity_ref: None,
                labels: Vec::new(),
                relations: Vec::new(),
                superseded_by: None,
                created_at: started(),
                updated_at: started(),
            })?;
            txn.insert_session(&CodingSession {
                id: "ses_0001".to_string(),
                engine_session_id: ENGINE_ID.to_string(),
                project_id: "prj_proj".to_string(),
                work_item_id: Some("wrk_1".to_string()),
                actor_id: actor.id.clone(),
                cwd: cwd.clone(),
                template: Some("claude".to_string()),
                model: None,
                effort: None,
                reason: WHY.to_string(),
                purpose: None,
                started_by_session: None,
                started_by_kind: None,
                started_at: started(),
                stopped_at: None,
            })?;
            Ok(WriteOutcome {
                result: json!({}),
                entity_kind: "session".to_string(),
                entity_id: "ses_0001".to_string(),
                payload: json!({}),
                event_kind: "session.started".to_string(),
                summary: json!({}),
            })
        })
        .unwrap();
        let sweep = ctx
            .observed
            .begin_sweep("test", &["prj_proj".to_string()], started())
            .unwrap();
        ctx.observed
            .append(
                &sweep.id,
                &[PendingObservation {
                    kind: "forge.pull_request".to_string(),
                    subject_key: "gh:acme/app#193".to_string(),
                    payload: json!({
                        "number": 193,
                        "state": "merged",
                        "head_ref": "wi-1707-x",
                        "checks": "success",
                        "implements": [],
                        "closed_at": started().plus_days(-1).to_iso(),
                    }),
                    content_digest: "pr-193".to_string(),
                    project_id: Some("prj_proj".to_string()),
                    source_url: None,
                    promoted: false,
                }],
                started(),
            )
            .unwrap();
        ctx.observed
            .finish_sweep(&sweep.id, SweepOutcome::Ok, &[], started(), None)
            .unwrap();
        ctx.observed.rebuild_latest().unwrap();
    });
    dir
}

fn run(built: &Built, op: &str, params: Value) -> Result<Value, VogtError> {
    default_registry()
        .get(op)
        .expect("the operation is registered")
        .run(Some(built), params)
}

fn comments(built: &Built) -> Vec<String> {
    let item = run(built, "work.get", json!({"ref": "WI-1"})).unwrap();
    item["comments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|comment| comment["body"].as_str().unwrap().to_string())
        .collect()
}

fn summary_of(built: &Built) -> crate::application::services::sessions::SessionSummary {
    crate::with_ctx!(built, |ctx| list_sessions(
        ctx,
        &ListSessionsParams {
            project: None,
            work_item: Some("WI-1".to_string()),
            include_stopped: false,
            limit: 50,
            offset: 0,
            order: "started".to_string(),
        },
    )
    .unwrap()
    .sessions
    .remove(0))
}

#[test]
fn others_commits_brought_in_by_a_rebase_are_not_the_sessions() {
    // A worker that pulled a moved main after it started and did nothing
    // still made no commits of its own.
    let base = temp("rebase");
    let repo = repo(&base);
    let origin = base.join("origin.git");
    git(&base, &["init", "-q", "--bare", origin.to_str().unwrap()]);
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo, &["push", "-q", "origin", "wi-1707-x:main"]);
    let other = base.join("other");
    git(
        &base,
        &[
            "clone",
            "-q",
            "-b",
            "main",
            origin.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    std::fs::write(other.join("g"), "y").unwrap();
    git(&other, &["add", "g"]);
    git_as(
        &other,
        "o",
        "2026-10-18T10:00:00Z",
        &["commit", "-q", "-m", "someone else"],
    );
    git(&other, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    git(&repo, &["merge", "-q", "--ff-only", "origin/main"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    assert_eq!(
        session_commits(&repo, &head, Some("wi-1707-x"), started()),
        Some(0)
    );

    std::fs::write(repo.join("h"), "z").unwrap();
    git(&repo, &["add", "h"]);
    git_as(
        &repo,
        "t",
        "2026-10-18T11:00:00Z",
        &["commit", "-q", "-m", "mine"],
    );
    git(&repo, &["push", "-q", "origin", "wi-1707-x"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(
        session_commits(&repo, &head, Some("wi-1707-x"), started()),
        Some(1)
    );
}

#[test]
fn a_sessions_move_to_review_is_checked_and_noted_on_the_item() {
    let base = temp("review");
    let checkout = repo(&base);
    let dir = wired(&base, &checkout);
    let session = context(&dir, as_session());

    let result = run(
        &session,
        "work.transition",
        json!({
            "ref": "WI-1",
            "to_state": "review",
            "walk": true,
            "reason": "PR #193 was opened and merged",
        }),
    )
    .unwrap();

    let check = &result["claim_check"];
    assert_eq!(check["claim"], "review");
    assert_eq!(check["session"], "ses_0001");
    assert_eq!(check["work_item"], "WI-1");
    assert_eq!(check["branch"], "wi-1707-x");
    assert_eq!(check["new_commits"], 0);
    assert_eq!(check["pushed"], false);
    assert_eq!(check["session_started_at"], "2026-10-18T09:00:00Z");
    assert_eq!(
        check["pulls"],
        json!([{
            "number": 193,
            "state": "merged",
            "merged_at": "2026-10-17T09:00:00Z",
            "merged_by": null,
            "checks": "success",
            "head_ref": "wi-1707-x",
        }])
    );
    assert_eq!(
        check["mismatches"],
        json!([
            "claimed PR #193; it merged 2026-10-17T09:00:00+00:00, before this session started (2026-10-18T09:00:00+00:00)",
            "no new commits from this session on wi-1707-x",
        ])
    );
    assert_eq!(check["unchecked"], json!([]));
    let notes = comments(&context(&dir, person()));
    assert!(
        notes
            .iter()
            .any(|body| body.starts_with("Claim check (review) for session ses_0001: mismatches")),
        "{notes:?}"
    );
}

#[test]
fn a_persons_move_is_not_a_claim() {
    let base = temp("person");
    let checkout = repo(&base);
    let dir = wired(&base, &checkout);

    let result = run(
        &context(&dir, person()),
        "work.transition",
        json!({"ref": "WI-1", "to_state": "review", "walk": true, "reason": WHY}),
    )
    .unwrap();

    assert!(result.as_object().unwrap().contains_key("claim_check"));
    assert_eq!(result["claim_check"], Value::Null);
}

#[test]
fn a_flagged_done_check_is_noted_once_per_key() {
    // The sweep's half of the WI-1707 case: the DONE claim is noted on the
    // item once, however often it is swept.
    let base = temp("once");
    let checkout = repo(&base);
    let dir = wired(&base, &checkout);
    let built = context(&dir, person());
    let summary = summary_of(&built);
    let text = "PR #193 was opened and merged.\nAUTOPILOT: DONE";
    let key = format!("{}@{}", summary.id, started().to_iso());
    let check: ClaimCheck = crate::with_ctx!(&built, |ctx| {
        let check = check_claim(ctx, &summary, ClaimCheckClaim::AutopilotDone, Some(text)).unwrap();
        assert!(record_claim_check(ctx, &check, Some(&key)).unwrap());
        assert!(!record_claim_check(ctx, &check, Some(&key)).unwrap());
        check
    });
    assert_eq!(check.new_commits, Some(0));
    assert!(check
        .mismatches
        .iter()
        .any(|line| line.contains("before this session started")));
    let notes: Vec<String> = comments(&built)
        .into_iter()
        .filter(|body| body.contains("Claim check (autopilot-done)"))
        .collect();
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].ends_with("\nCheck key: ses_0001@2026-10-18T09:00:00+00:00"),
        "{}",
        notes[0]
    );
}

#[test]
fn a_matched_claim_or_an_unbound_one_records_nothing() {
    let base = temp("nothing");
    let checkout = repo(&base);
    let dir = wired(&base, &checkout);
    let built = context(&dir, person());
    let mut check = ClaimCheck {
        claim: ClaimCheckClaim::Done,
        session: Some("ses_0001".to_string()),
        work_item: Some("WI-1".to_string()),
        session_started_at: None,
        branch: None,
        head: None,
        pushed: None,
        new_commits: None,
        pulls: Vec::new(),
        mismatches: Vec::new(),
        unchecked: Vec::new(),
    };
    crate::with_ctx!(&built, |ctx| {
        assert!(!record_claim_check(ctx, &check, None).unwrap());
        check.mismatches = vec!["no new commits from this session".to_string()];
        check.work_item = None;
        assert!(!record_claim_check(ctx, &check, None).unwrap());
    });
    assert!(comments(&built).is_empty());
}

#[test]
fn an_unreadable_worktree_is_unchecked_not_a_mismatch() {
    let base = temp("unreadable");
    let checkout = base.join("gone");
    let dir = wired(&base, &checkout);
    let built = context(&dir, person());
    let mut summary = summary_of(&built);
    summary.started_at = None;
    crate::with_ctx!(&built, |ctx| {
        let check = check_claim(ctx, &summary, ClaimCheckClaim::Done, Some("see PR #7")).unwrap();
        assert!(check.mismatches.is_empty());
        assert_eq!(
            check.unchecked,
            vec![
                "session start time unknown".to_string(),
                format!("worktree '{}' is not readable here", checkout.display()),
                "no forge observation of PR #7".to_string(),
            ]
        );
    });
}

#[test]
fn the_note_names_the_mismatches_and_the_facts() {
    let check = ClaimCheck {
        claim: ClaimCheckClaim::Review,
        session: Some("ses_1".to_string()),
        work_item: Some("WI-1".to_string()),
        session_started_at: None,
        branch: Some("wi-1-x".to_string()),
        head: Some("0123456789abcdef".to_string()),
        pushed: Some(false),
        new_commits: None,
        pulls: Vec::new(),
        mismatches: vec!["branch head 0123456789ab is not pushed".to_string()],
        unchecked: vec!["a".to_string(), "b".to_string()],
    };
    assert_eq!(
        comment_body(&check),
        "Claim check (review) for session ses_1: mismatches\n\
         - branch head 0123456789ab is not pushed\n\
         Facts: branch wi-1-x, head 0123456789ab, pushed False, new commits None.\n\
         Not checked: a; b."
    );
    let bare = ClaimCheck {
        branch: None,
        head: None,
        pushed: None,
        new_commits: Some(0),
        unchecked: Vec::new(),
        ..check
    };
    assert!(
        comment_body(&bare).ends_with("Facts: branch None, head ?, pushed None, new commits 0.")
    );
}
