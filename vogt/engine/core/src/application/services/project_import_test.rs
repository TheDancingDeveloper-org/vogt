//! Tests for `project.import` and the cloner under it. Ports `tests/test_import.py`.
//!
//! The operation runs through an injected cloner that records what it was asked
//! for and writes a one-file tree, so it runs offline. The cloner itself runs
//! against a real local `git`, for the parts that are about git's behaviour
//! rather than Vogt's. The Python cases that spy on `subprocess.run` (the argv of
//! every git call, a `which` that finds nothing, a binary that cannot execute)
//! have no seam here: `run_git` spawns its fixed argument lists internally.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use crate::adapters::git::{clone_repository, CloneOutcome, CloneRequest};
use crate::application::context::{build_context, Built};
use crate::application::forge_standin::StandInForge;
use crate::application::projects;
use crate::application::services::history;
use crate::config::VogtConfig;
use crate::core::{ActorKind, Principal, SequentialIds, StepClock};
use crate::errors::VogtError;

use super::services_test::{moment, principal, unique};

const WHY: &str = "onboarding the estate";
const REPO: &str = "acme/widget";

thread_local! {
    static CLONES: RefCell<Vec<CloneRequestSeen>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Debug)]
struct CloneRequestSeen {
    remote: String,
    destination: PathBuf,
}

/// A clone that records what it was asked for and writes a tree.
fn clone_to_a_tree(request: &CloneRequest) -> Result<CloneOutcome, VogtError> {
    CLONES.with(|clones| {
        clones.borrow_mut().push(CloneRequestSeen {
            remote: request.remote.clone(),
            destination: request.destination.clone(),
        });
    });
    std::fs::create_dir_all(&request.destination).unwrap();
    std::fs::write(request.destination.join("README.md"), "hi\n").unwrap();
    Ok(CloneOutcome {
        destination: request.destination.clone(),
        revision: Some("a".repeat(40)),
        default_branch: Some("main".to_owned()),
        reused: false,
    })
}

fn clones() -> Vec<CloneRequestSeen> {
    CLONES.with(|clones| clones.borrow().clone())
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vogt-import-{label}-{}-{}",
        std::process::id(),
        unique()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An initialised instance whose import root is `<tmp>/estate`, acting as
/// `who`, with the recording cloner. With `forge`, a GitHub token file and the
/// stand-in forge are wired in, so the import consolidates.
fn importing_as(who: Principal, forge: Option<&StandInForge>) -> (Built, PathBuf) {
    importing_hiding(who, forge, None)
}

/// As [`importing_as`], with the forge answering 404 for `hidden` (`owner/name`).
fn importing_hiding(
    who: Principal,
    forge: Option<&StandInForge>,
    hidden: Option<&'static str>,
) -> (Built, PathBuf) {
    CLONES.with(|clones| clones.borrow_mut().clear());
    let tmp = scratch("ctx");
    let data_dir = tmp.join("data");
    let root = tmp.join("estate");
    let mut clock = Some(StepClock::new(moment()));
    let mut ids = Some(SequentialIds::new(None).unwrap());
    crate::application::instance::init(&data_dir, &mut clock, &mut ids).unwrap();
    let github_token_file = forge.map(|_| {
        let token = tmp.join("forge-token");
        std::fs::write(&token, "secret-token\n").unwrap();
        token
    });
    let built = build_context(
        VogtConfig {
            data_dir,
            import_root: Some(root.clone()),
            github_token_file,
            ..VogtConfig::default()
        },
        Some(who),
        clock,
        ids,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let Built::StepSequential(mut ctx) = built else {
        panic!("expected a step clock");
    };
    ctx.cloner = clone_to_a_tree;
    let ctx = match forge {
        None => ctx,
        Some(standin) => {
            let transport = standin.transport();
            ctx.with_forge_transport(move |url, headers, body, method| {
                if hidden.is_some_and(|repo| url.ends_with(&format!("/repos/{repo}"))) {
                    return (404, Vec::new(), std::collections::BTreeMap::new());
                }
                transport(url, headers, body, method)
            })
        }
    };
    (Built::StepSequential(ctx), root)
}

fn importing() -> (Built, PathBuf) {
    importing_as(principal(), None)
}

fn import(built: &Built, params: Value) -> Result<Value, VogtError> {
    projects::import_op(built, params)
}

fn slugs(built: &Built) -> Vec<String> {
    let listed = projects::list_op(built, json!({})).unwrap();
    listed["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|project| project["slug"].as_str().unwrap().to_owned())
        .collect()
}

// -- the operation ------------------------------------------------------------

/// A token-authenticated caller may only name paths within the import root; a
/// loopback caller keeps the unrestricted form, because it already owns the
/// filesystem.
#[test]
fn remote_callers_cannot_name_paths_outside_the_import_root() {
    let remote = Principal::new("agent:remote", ActorKind::Agent, "remote").unwrap();
    let (remote_ctx, root) = importing_as(remote, None);
    let outside = root.parent().unwrap().join("etc").join("loot");
    let outside = outside.to_str().unwrap();

    for refused in [
        projects::register_op(
            &remote_ctx,
            json!({"name": "loot", "root_path": outside, "reason": WHY}),
        ),
        projects::create_op(
            &remote_ctx,
            json!({"name": "loot", "root_path": outside, "reason": WHY}),
        ),
        import(
            &remote_ctx,
            json!({"repo": REPO, "root_path": outside, "consolidate": false, "reason": WHY}),
        ),
    ] {
        match refused {
            Err(VogtError::InvalidRequest(message)) => {
                assert!(message.contains("import root"), "{message}");
            }
            other => panic!("a remote path outside the root was accepted: {other:?}"),
        }
    }
    assert!(clones().is_empty(), "a refused import cloned");

    let within = root.join("sub").join("here");
    let contained = projects::register_op(
        &remote_ctx,
        json!({"name": "contained", "root_path": within.to_str().unwrap(), "reason": WHY}),
    )
    .unwrap();
    assert_eq!(contained["project"]["root_path"], within.to_str().unwrap());

    let (local_ctx, _) = importing();
    let registered = projects::register_op(
        &local_ctx,
        json!({"name": "anywhere", "root_path": outside, "reason": WHY}),
    )
    .unwrap();
    assert_eq!(registered["project"]["root_path"], outside);
}

/// The demo, minus the forge: named, cloned, registered, on disk.
#[test]
fn import_clones_registers_and_lands_at_the_default_root() {
    let (built, root) = importing();
    let result = import(
        &built,
        json!({"repo": REPO, "consolidate": false, "reason": WHY}),
    )
    .unwrap();

    assert_eq!(result["project"]["slug"], "widget");
    let landed = root.join("widget");
    assert_eq!(result["root_path"], landed.to_str().unwrap());
    assert!(landed.is_dir());
    assert_eq!(result["revision"], "a".repeat(40));
    assert_eq!(result["cloned"], true);

    assert_eq!(slugs(&built), vec!["widget".to_owned()]);
    // The remote is stored so collectors can find it, and stored without a
    // credential in it.
    let listed = projects::list_op(&built, json!({})).unwrap();
    let repo_url = listed["projects"][0]["repo_url"].as_str().unwrap();
    assert_eq!(repo_url, "https://github.com/acme/widget");
    assert!(!repo_url.contains('@'));
}

#[test]
fn an_explicit_root_path_wins_over_the_import_root() {
    let (built, root) = importing();
    let elsewhere = root.parent().unwrap().join("somewhere-else");
    let result = import(
        &built,
        json!({
            "repo": REPO,
            "root_path": elsewhere.to_str().unwrap(),
            "consolidate": false,
            "reason": WHY,
        }),
    )
    .unwrap();
    assert_eq!(result["root_path"], elsewhere.to_str().unwrap());
    assert_eq!(clones()[0].destination, elsewhere);
}

/// "Registered" and "imported from GitHub" are different answers; collapsing
/// them would lose the only record that a clone happened.
#[test]
fn the_import_is_audited_as_an_import() {
    let (built, _) = importing();
    import(
        &built,
        json!({"repo": REPO, "consolidate": false, "reason": WHY}),
    )
    .unwrap();

    let audit = history::audit_list_op(&built, json!({})).unwrap();
    let records = audit["records"].as_array().unwrap();
    let operations: Vec<&str> = records
        .iter()
        .map(|record| record["operation"].as_str().unwrap())
        .collect();
    assert!(operations.contains(&"project.import"), "{operations:?}");
    assert!(!operations.contains(&"project.register"), "{operations:?}");
    for record in records {
        if record["operation"] == "project.import" {
            assert_eq!(record["reason"], WHY);
        }
    }

    let events = history::events_list_op(&built, json!({})).unwrap();
    assert!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "project.imported"),
        "{events}"
    );
}

/// Cloning a repository only to fail on a name is a minute wasted.
#[test]
fn a_duplicate_slug_is_refused_before_anything_is_cloned() {
    let (built, _) = importing();
    let params = json!({"repo": REPO, "consolidate": false, "reason": WHY});
    import(&built, params.clone()).unwrap();
    assert_eq!(clones().len(), 1);

    let error = import(&built, params).unwrap_err();
    assert!(matches!(error, VogtError::Conflict(_)), "{error:?}");
    assert_eq!(
        clones().len(),
        1,
        "the second import cloned before checking"
    );
}

#[test]
fn the_ways_a_person_names_a_repository() {
    for reference in [
        "owner/name",
        "https://github.com/owner/name",
        "https://github.com/owner/name.git",
        "git@github.com:owner/name.git",
        "  owner/name  ",
    ] {
        let (built, _) = importing();
        let result = import(
            &built,
            json!({"repo": reference, "consolidate": false, "reason": WHY}),
        )
        .unwrap_or_else(|error| panic!("{reference:?}: {error:?}"));
        assert_eq!(result["project"]["slug"], "name", "{reference:?}");
        assert_eq!(
            clones()[0].remote,
            "https://github.com/owner/name.git",
            "{reference:?}"
        );
    }
}

/// A reference this instance cannot address is a request to go looking.
/// Nothing here looks, so it is an error rather than a search.
#[test]
fn something_that_is_not_a_repository_is_refused() {
    for reference in ["", "   ", "just-a-name", "https://example.com"] {
        let (built, _) = importing();
        let error = import(
            &built,
            json!({"repo": reference, "consolidate": false, "reason": WHY}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::InvalidRequest(_)),
            "{reference:?}: {error:?}"
        );
        assert!(clones().is_empty(), "{reference:?} was cloned");
    }
}

#[test]
fn a_reason_is_required() {
    let (built, _) = importing();
    let error = import(&built, json!({"repo": REPO, "consolidate": false})).unwrap_err();
    assert!(matches!(error, VogtError::InvalidRequest(_)), "{error:?}");
    assert!(clones().is_empty());
}

// -- with the forge configured ------------------------------------------------

/// The import demo. Import reads history; it never writes it. Every request the
/// adapter sent is recorded, and any method other than GET fails the test.
#[test]
fn import_consolidates_and_changes_nothing_upstream() {
    let standin = StandInForge::one_of_each();
    let (built, _) = importing_as(principal(), Some(&standin));
    let result = import(&built, json!({"repo": REPO, "reason": WHY})).unwrap();

    assert!(result["consolidated"].is_object(), "{result}");
    assert_eq!(result["project"]["link_state"], "linked");
    let mutations: Vec<String> = standin
        .requests()
        .into_iter()
        .filter(|request| !request.starts_with("GET "))
        .collect();
    assert!(
        mutations.is_empty(),
        "import mutated upstream: {mutations:?}"
    );
    assert!(
        standin
            .requests()
            .iter()
            .any(|request| request.contains("/issues?")),
        "consolidation read no issues: {:?}",
        standin.requests()
    );
}

/// 404 is "no such repository, to us", checked before anything clones.
#[test]
fn a_repository_the_token_cannot_see_is_not_imported() {
    let standin = StandInForge::one_of_each();
    let (built, _) = importing_hiding(principal(), Some(&standin), Some("acme/ghost"));
    let error = import(&built, json!({"repo": "acme/ghost", "reason": WHY})).unwrap_err();
    assert!(matches!(error, VogtError::NotFound(_)), "{error:?}");
    assert!(clones().is_empty(), "an invisible repository was cloned");
    assert!(slugs(&built).is_empty());
}

/// The core never requires the optional adapter. A public repository clones
/// without a token; what is missing is the consolidation, and the result says
/// so rather than pretending the repository had no issues.
#[test]
fn without_the_adapter_the_import_still_works_and_says_so() {
    let (built, _) = importing();
    let result = import(&built, json!({"repo": REPO, "reason": WHY})).unwrap();
    assert!(result["consolidated"].is_null(), "{result}");
    assert!(
        result["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("not collected"),
        "{result}"
    );
    assert_eq!(result["project"]["link_state"], "unlinked");
}

// -- the cloner itself, against a real git ------------------------------------

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit(cwd: &Path, message: &str) {
    git(
        cwd,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-m",
            message,
        ],
    );
}

/// A real repository on disk, so `clone_repository` faces a real git; and a
/// scratch directory beside it for destinations.
fn origin() -> (PathBuf, PathBuf) {
    let tmp = scratch("git");
    let source = tmp.join("origin");
    std::fs::create_dir_all(&source).unwrap();
    git(&source, &["init", "--initial-branch", "main"]);
    std::fs::write(source.join("README.md"), "origin\n").unwrap();
    git(&source, &["add", "README.md"]);
    commit(&source, "first");
    (source, tmp)
}

fn clone(remote: &Path, destination: &Path) -> Result<CloneOutcome, VogtError> {
    clone_repository(&CloneRequest {
        remote: remote.display().to_string(),
        destination: destination.to_path_buf(),
        token: None,
        helper_dir: None,
    })
}

#[test]
fn clone_puts_a_working_tree_where_it_was_told() {
    let (origin, tmp) = origin();
    let destination = tmp.join("cloned").join("here");
    let outcome = clone(&origin, &destination).unwrap();

    assert_eq!(
        std::fs::read_to_string(destination.join("README.md")).unwrap(),
        "origin\n"
    );
    assert_eq!(outcome.revision.as_deref().map(str::len), Some(40));
    assert_eq!(outcome.default_branch.as_deref(), Some("main"));
    assert!(!outcome.reused);
}

/// Right after a clone the tree is clean and local HEAD equals origin HEAD, so
/// the parity gate passes and the re-import registers what is there.
#[test]
fn a_second_import_of_a_clean_checkout_at_parity_reuses_it() {
    let (origin, tmp) = origin();
    let destination = tmp.join("cloned");
    let first = clone(&origin, &destination).unwrap();
    let second = clone(&origin, &destination).unwrap();

    assert!(second.reused);
    assert_eq!(second.revision, first.revision);
}

/// An untracked local file is uncommitted work. The receipt names the condition
/// and the tree is left untouched: Vogt runs no stash on the user's behalf.
#[test]
fn reimporting_over_a_dirty_working_tree_is_refused() {
    let (origin, tmp) = origin();
    let destination = tmp.join("cloned");
    clone(&origin, &destination).unwrap();
    std::fs::write(destination.join("local-work.txt"), "mine\n").unwrap();

    match clone(&origin, &destination) {
        Err(VogtError::ImportWorkingTreeDirty(message)) => {
            assert!(message.contains("uncommitted changes"), "{message}");
        }
        other => panic!("a dirty tree was reimported: {other:?}"),
    }
    assert!(
        destination.join("local-work.txt").exists(),
        "a refused import touched the tree"
    );
}

/// A local commit origin does not have makes the two HEADs differ. The tree is
/// clean, so it is the divergence that fails; Vogt runs no merge, rebase or pull.
#[test]
fn reimporting_over_a_diverged_default_branch_is_refused() {
    let (origin, tmp) = origin();
    let destination = tmp.join("cloned");
    clone(&origin, &destination).unwrap();
    std::fs::write(destination.join("local.txt"), "committed locally\n").unwrap();
    git(&destination, &["add", "local.txt"]);
    commit(&destination, "ahead");

    match clone(&origin, &destination) {
        Err(VogtError::ImportBranchDiverged(message)) => {
            assert!(message.contains("diverged from origin HEAD"), "{message}");
        }
        other => panic!("a diverged branch was reimported: {other:?}"),
    }
}

#[test]
fn a_destination_holding_a_different_repository_is_refused() {
    let (origin, tmp) = origin();
    let destination = tmp.join("cloned");
    clone(&origin, &destination).unwrap();

    let error = clone_repository(&CloneRequest {
        remote: "https://github.com/someone/else.git".to_owned(),
        destination: destination.clone(),
        token: None,
        helper_dir: None,
    })
    .unwrap_err();
    assert!(matches!(error, VogtError::Conflict(_)), "{error:?}");
    assert!(
        destination.join("README.md").exists(),
        "a refused import touched the tree"
    );
}

/// "git answered, and the answer is that there is no origin" is a real answer,
/// and it still means the destination is not this remote's clone.
#[test]
fn a_checkout_with_no_origin_is_still_a_conflict() {
    let (origin, tmp) = origin();
    let destination = tmp.join("cloned");
    clone(&origin, &destination).unwrap();
    git(&destination, &["remote", "remove", "origin"]);

    match clone(&origin, &destination) {
        Err(VogtError::Conflict(message)) => {
            assert!(message.contains("unknown remote"), "{message}");
        }
        other => panic!("a checkout with no origin was reused: {other:?}"),
    }
}

/// The single most destructive thing an import could do, prevented.
#[test]
fn a_destination_holding_unrelated_files_is_refused() {
    let destination = scratch("unrelated");
    std::fs::write(destination.join("important.txt"), "do not lose me\n").unwrap();

    let error = clone_repository(&CloneRequest {
        remote: "https://github.com/o/n.git".to_owned(),
        destination: destination.clone(),
        token: None,
        helper_dir: None,
    })
    .unwrap_err();
    assert!(matches!(error, VogtError::Conflict(_)), "{error:?}");
    assert!(destination.join("important.txt").exists());
}

/// A token in the clone's own configuration is a token in a file that outlives
/// the import. The helper that carried it is gone once the clone returns.
#[test]
fn the_token_never_reaches_the_clone_config_and_the_helper_does_not_outlive_it() {
    let (origin, tmp) = origin();
    let destination = tmp.join("cloned");
    let helper_dir = tmp.join("data").join("tmp");
    clone_repository(&CloneRequest {
        remote: origin.display().to_string(),
        destination: destination.clone(),
        token: Some("ghp_supersecret".to_owned()),
        helper_dir: Some(helper_dir.clone()),
    })
    .unwrap();

    let config = std::fs::read_to_string(destination.join(".git").join("config")).unwrap();
    assert!(
        !config.contains("ghp_supersecret"),
        "the token was written into the clone's configuration"
    );
    let left: Vec<_> = std::fs::read_dir(&helper_dir)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "the askpass helper survived: {left:?}");
}

/// An error message is a place secrets escape to logs, and git echoes the
/// remote back in most failures.
#[test]
fn a_failure_message_does_not_repeat_a_credential() {
    let tmp = scratch("redact");
    let error = clone_repository(&CloneRequest {
        remote: "https://user:hunter2@example.invalid/o/n.git".to_owned(),
        destination: tmp.join("nope"),
        token: None,
        helper_dir: None,
    })
    .unwrap_err();
    assert!(!format!("{error:?}").contains("hunter2"), "{error:?}");
    assert!(!error.to_string().contains("hunter2"), "{error}");
}

/// Empty is not occupied; refusing it would fail every second attempt.
#[test]
fn an_empty_destination_is_cloned_into() {
    let (origin, tmp) = origin();
    let destination = tmp.join("empty");
    std::fs::create_dir_all(&destination).unwrap();

    let outcome = clone(&origin, &destination).unwrap();
    assert!(!outcome.reused);
    assert!(destination.join("README.md").exists());
}
