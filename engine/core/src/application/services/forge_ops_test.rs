//! Tests for `forge.link`, `forge.publish` and `forge.import`, and the
//! native-item gate they share.
//!
//! The refusal paths, the link success path, and the publish and import success
//! paths. The success paths run against the stand-in forge, which mints the
//! repository on create and describes `acme/widget`. The clone and the push are
//! the production functions aimed at a local tree and a local bare remote, since
//! the real seams carry `github.com` remotes a test cannot reach.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use crate::adapters::forge::registry::provider_for_host;
use crate::adapters::forge::ForgeProvider;
use crate::adapters::git::{
    inspect_publish_source, push_branch, CloneOutcome, CloneRequest, PushOutcome, PushRequest,
};
use crate::application::context::{build_context, Built};
use crate::application::forge_standin::StandInForge;
use crate::application::services::{
    forge_accounts, forge_import, forge_link, forge_publish, forge_repos, forge_writeback, work,
};
use crate::config::VogtConfig;
use crate::core::{LinkState, Project, SequentialIds, StepClock};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

use super::services_test::{moment, opened, principal, unique};

/// A context whose instance holds a GitHub token file, with the stand-in forge
/// wired in, so a credential resolves and the forge answers reads.
fn with_token() -> Built {
    with_token_on(&StandInForge::one_of_each())
}

/// As [`with_token`], on a stand-in the test keeps, so it can read back what the
/// forge was sent.
fn with_token_on(standin: &StandInForge) -> Built {
    let dir = std::env::temp_dir().join(format!(
        "vogt-forge-ops-{}-{}",
        std::process::id(),
        unique()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let token = dir.join("forge-token");
    std::fs::write(&token, "secret-token\n").unwrap();
    let mut clock = Some(StepClock::new(moment()));
    let mut ids = Some(SequentialIds::new(None).unwrap());
    crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
    let built = build_context(
        VogtConfig {
            data_dir: dir,
            github_token_file: Some(token),
            ..VogtConfig::default()
        },
        Some(principal()),
        clock,
        ids,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let Built::StepSequential(ctx) = built else {
        panic!("expected a step clock");
    };
    let transport = standin.transport();
    Built::StepSequential(ctx.with_forge_transport(move |url, headers, body, method| {
        transport(url, headers, body, method)
    }))
}

/// A fresh directory with no `.git`, so the publish source gate refuses it.
fn a_directory_that_is_not_a_repository() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vogt-not-a-repo-{}-{}",
        std::process::id(),
        unique()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn insert_project(built: &Built, slug: &str, repo_url: Option<&str>, root: &str, linked: bool) {
    let Built::StepSequential(ctx) = built else {
        panic!("expected a step clock");
    };
    let mut project = Project::new(&format!("prj_{slug}"), slug, slug, root, moment());
    project.repo_url = repo_url.map(str::to_owned);
    if linked {
        project.link_state = LinkState::Linked;
    }
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_project(&project).unwrap();
    txn.commit().unwrap();
}

fn project_link_state(built: &Built, slug: &str) -> String {
    let Built::StepSequential(ctx) = built else {
        panic!("expected a step clock");
    };
    let project = ctx
        .declared
        .read()
        .unwrap()
        .project_by_slug(slug)
        .unwrap()
        .unwrap();
    project.link_state.to_string()
}

#[test]
fn forge_link_refuses_an_unknown_project() {
    let built = opened();
    let refused = forge_link::link_op(&built, json!({"project": "nope", "reason": "link it"}));
    assert!(
        matches!(refused, Err(VogtError::NotFound(_))),
        "{refused:?}"
    );
}

#[test]
fn forge_link_refuses_a_host_it_cannot_read() {
    let built = opened();
    insert_project(
        &built,
        "stray",
        Some("https://no.such.host/acme/widget"),
        "/tmp",
        false,
    );
    match forge_link::link_op(&built, json!({"project": "stray", "reason": "link it"})) {
        Err(VogtError::LinkRefused(message)) => {
            assert!(message.starts_with("cannot link 'stray'"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(project_link_state(&built, "stray"), "unlinked");
}

#[test]
fn forge_link_without_a_credential_is_refused_and_names_the_fix() {
    let built = opened();
    insert_project(
        &built,
        "widget",
        Some("https://github.com/acme/widget"),
        "/tmp",
        false,
    );
    match forge_link::link_op(&built, json!({"project": "widget", "reason": "link it"})) {
        Err(VogtError::LinkRefused(message)) => {
            assert!(message.contains("no usable credential"), "{message}");
            assert!(message.contains("forge account link"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(project_link_state(&built, "widget"), "unlinked");
}

#[test]
fn forge_link_links_a_project_once_a_credential_resolves() {
    let built = with_token();
    insert_project(
        &built,
        "widget",
        Some("https://github.com/acme/widget"),
        "/tmp",
        false,
    );
    let linked = forge_link::link_op(
        &built,
        json!({"project": "widget", "reason": "make it upstream-truth"}),
    )
    .unwrap();
    assert_eq!(linked["project"]["link_state"], "linked", "{linked}");
    assert_eq!(linked["migrated"], json!([]));
    assert_eq!(project_link_state(&built, "widget"), "linked");

    // Idempotent: a second link re-validates and changes nothing.
    let again = forge_link::link_op(
        &built,
        json!({"project": "widget", "reason": "link it again"}),
    )
    .unwrap();
    assert_eq!(again["project"]["link_state"], "linked");
}

/// The policy gate runs before the link commits. An open native item and a
/// write-back policy that does not permit `create` refuse the whole link, so the
/// project is never left linked with an item no re-run could move.
#[test]
fn forge_link_refuses_open_native_items_the_policy_forbids_before_linking() {
    let built = with_token();
    insert_project(
        &built,
        "widget",
        Some("https://github.com/acme/widget"),
        "/tmp",
        false,
    );
    work::create_op(
        &built,
        json!({
            "kind": "chore",
            "title": "native item",
            "project": "widget",
            "local_only": true,
            "reason": "parity",
        }),
    )
    .unwrap();

    match forge_link::link_op(&built, json!({"project": "widget", "reason": "link it"})) {
        Err(VogtError::UpstreamWriteRefused(message)) => {
            assert!(message.contains("does not permit 'create'"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(project_link_state(&built, "widget"), "unlinked");
}

#[test]
fn forge_publish_refuses_a_project_that_is_already_linked() {
    let built = opened();
    insert_project(
        &built,
        "widget",
        Some("https://github.com/acme/widget"),
        "/tmp",
        true,
    );
    match forge_publish::publish_op(&built, json!({"project": "widget", "reason": "publish it"})) {
        Err(VogtError::PublishRefused(message)) => {
            assert!(message.contains("is already linked"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn forge_publish_never_overwrites_a_repository_a_project_already_names() {
    let built = opened();
    insert_project(
        &built,
        "widget",
        Some("https://github.com/acme/elsewhere"),
        "/tmp",
        false,
    );
    match forge_publish::publish_op(&built, json!({"project": "widget", "reason": "publish it"})) {
        Err(VogtError::PublishRefused(message)) => {
            assert!(message.contains("already names a repository"), "{message}");
            assert!(message.contains("forge link"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// The source gate runs before anything exists upstream, so a directory that is
/// not a repository is refused with no remote created and no push attempted.
#[test]
fn forge_publish_refuses_a_directory_that_is_not_a_repository_before_creating_anything() {
    let built = with_token();
    let root = a_directory_that_is_not_a_repository();
    insert_project(&built, "scratch", None, &root.display().to_string(), false);
    match forge_publish::publish_op(
        &built,
        json!({"project": "scratch", "reason": "publish it"}),
    ) {
        Err(VogtError::PublishSourceInvalid(message)) => {
            assert!(message.contains("is not a git repository"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(project_link_state(&built, "scratch"), "unlinked");
}

/// The read-only source gate, on its own: each refusal names its cause, and a
/// settled checkout reads its branch and revision.
#[test]
fn the_publish_gate_refuses_an_empty_repository() {
    let root = a_directory_that_is_not_a_repository();
    git(&["init", "-q", "-b", "main"], &root);
    match inspect_publish_source(&root) {
        Err(VogtError::PublishSourceInvalid(message)) => {
            assert!(message.contains("no commits"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn the_publish_gate_refuses_a_detached_head() {
    let (root, _) = a_publishable_checkout("detached");
    git(&["checkout", "-q", "--detach", "HEAD"], &root);
    match inspect_publish_source(&root) {
        Err(VogtError::PublishSourceInvalid(message)) => {
            assert!(message.contains("detached"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn the_publish_gate_reads_the_branch_and_revision() {
    let (root, revision) = a_publishable_checkout("clean");
    let source = inspect_publish_source(&root).unwrap();
    assert_eq!(source.branch, "main");
    assert_eq!(source.revision, revision);
}

#[test]
fn the_publish_gate_refuses_a_dirty_tree_by_name() {
    let (root, _) = a_publishable_checkout("dirty");
    std::fs::write(root.join("unsaved.txt"), "wip\n").unwrap();
    match inspect_publish_source(&root) {
        Err(VogtError::PublishWorkingTreeDirty(message)) => {
            assert!(message.contains("unsaved.txt"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// The gate passes, then the credential check refuses. Nothing is created.
#[test]
fn forge_publish_without_any_credential_is_refused_before_creating_anything() {
    let built = opened();
    let (root, _) = a_publishable_checkout("uncredentialed");
    insert_project(&built, "demo", None, &root.display().to_string(), false);
    match forge_publish::publish_op(&built, json!({"project": "demo", "reason": "publish it"})) {
        Err(VogtError::PublishRefused(message)) => {
            assert!(message.contains("no usable forge credential"), "{message}");
            assert!(message.contains("forge account link"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(project_link_state(&built, "demo"), "unlinked");
}

#[test]
fn forge_import_refuses_a_host_that_is_not_configured() {
    let built = opened();
    match forge_import::import_op(
        &built,
        json!({"owner": "acme", "name": "widget", "host": "no.such.host", "reason": "import it"}),
    ) {
        Err(VogtError::InvalidRequest(message)) => {
            assert!(
                message.contains("is not a configured forge host"),
                "{message}"
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn forge_import_refuses_when_no_credential_can_clone_the_repository() {
    let built = opened();
    match forge_import::import_op(
        &built,
        json!({"owner": "acme", "name": "widget", "reason": "import it"}),
    ) {
        Err(VogtError::InvalidRequest(message)) => {
            assert!(message.contains("no forge credential"), "{message}");
            assert!(message.contains("acme/widget"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

// -- the success paths: publish and import --------------------------------

/// What the thread-local seams saw during one test. The production clone and push
/// are plain functions, so they cannot capture a test's state; each test runs on
/// its own thread, so a thread-local is that test's alone.
#[derive(Default)]
struct Seams {
    bare_remote: Option<PathBuf>,
    pushes: Vec<(String, Option<String>)>,
    clones: Vec<(String, Option<String>)>,
}

thread_local! {
    static SEAMS: RefCell<Seams> = RefCell::new(Seams::default());
}

/// The clone seam: writes a one-file tree where a clone would land and answers the
/// revision and default branch a clone would report. No network.
fn clone_to_a_tree(request: &CloneRequest) -> Result<CloneOutcome, VogtError> {
    SEAMS.with(|seams| {
        seams
            .borrow_mut()
            .clones
            .push((request.remote.clone(), request.token.clone()));
    });
    std::fs::create_dir_all(&request.destination).unwrap();
    std::fs::write(request.destination.join("README.md"), "hi\n").unwrap();
    Ok(CloneOutcome {
        destination: request.destination.clone(),
        revision: Some("c".repeat(40)),
        default_branch: Some("main".to_owned()),
        reused: false,
    })
}

/// The push seam: the real plain `push_branch`, aimed at the bare remote the test
/// made. The production remote is `github.com`, which a test cannot reach.
fn push_to_bare_remote(request: &PushRequest) -> Result<PushOutcome, VogtError> {
    let bare = SEAMS.with(|seams| {
        let mut seams = seams.borrow_mut();
        seams
            .pushes
            .push((request.remote.clone(), request.token.clone()));
        seams.bare_remote.clone()
    });
    let bare = bare.expect("a test sets its bare remote before publishing");
    push_branch(&PushRequest {
        root: request.root.clone(),
        remote: bare.display().to_string(),
        branch: request.branch.clone(),
        token: None,
        helper_dir: None,
    })
}

/// The built context with the clone and push seams swapped in for the test.
fn with_clone_and_push_seams(built: Built) -> Built {
    let Built::StepSequential(mut ctx) = built else {
        panic!("expected a step clock");
    };
    ctx.cloner = clone_to_a_tree;
    ctx.pusher = push_to_bare_remote;
    Built::StepSequential(ctx)
}

fn git(args: &[&str], cwd: &Path) -> String {
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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A fresh checkout on `main` with one commit and a clean tree, and the revision
/// of that commit.
fn a_publishable_checkout(name: &str) -> (PathBuf, String) {
    let root = std::env::temp_dir()
        .join(format!(
            "vogt-forge-ops-{}-{}",
            std::process::id(),
            unique()
        ))
        .join(name);
    std::fs::create_dir_all(&root).unwrap();
    git(&["init", "-q", "-b", "main"], &root);
    std::fs::write(root.join("README.md"), "publish me\n").unwrap();
    git(&["add", "."], &root);
    git(
        &[
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "user.name=Forge Test",
            "commit",
            "-q",
            "-m",
            "seed",
        ],
        &root,
    );
    let revision = git(&["rev-parse", "HEAD"], &root);
    (root, revision)
}

/// An empty bare repository to push to, standing in for the new `github.com`
/// remote. Records it for the push seam.
fn a_bare_remote(name: &str) -> PathBuf {
    let bare = std::env::temp_dir()
        .join(format!(
            "vogt-forge-ops-{}-{}",
            std::process::id(),
            unique()
        ))
        .join(format!("{name}.git"));
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    git(
        &["init", "-q", "--bare", &bare.display().to_string()],
        bare.parent().unwrap(),
    );
    SEAMS.with(|seams| seams.borrow_mut().bare_remote = Some(bare.clone()));
    bare
}

#[test]
fn forge_publish_creates_the_repo_pushes_it_and_links_the_project() {
    let standin = StandInForge::one_of_each();
    let built = with_clone_and_push_seams(with_token_on(&standin));
    let (root, revision) = a_publishable_checkout("demo");
    let bare = a_bare_remote("demo");
    insert_project(&built, "demo", None, &root.display().to_string(), false);

    let published =
        forge_publish::publish_op(&built, json!({"project": "demo", "reason": "publish it"}))
            .unwrap();

    assert_eq!(
        published["repo"], "https://github.com/acme/demo",
        "{published}"
    );
    assert_eq!(published["branch"], "main");
    assert_eq!(published["revision"], revision);
    assert_eq!(published["migrated"], json!([]));
    assert_eq!(published["project"]["link_state"], "linked", "{published}");
    assert_eq!(
        published["project"]["repo_url"],
        "https://github.com/acme/demo"
    );
    assert_eq!(project_link_state(&built, "demo"), "linked");
    assert_eq!(git(&["rev-parse", "refs/heads/main"], &bare), revision);

    let creates = standin.creates();
    assert_eq!(creates.len(), 1, "{creates:?}");
    assert_eq!(
        creates[0]["name"], "demo",
        "the name defaults to the project slug"
    );
    assert_eq!(creates[0]["private"], true, "private by default");
    assert_eq!(creates[0]["auto_init"], false, "no generated first commit");

    let pushes = SEAMS.with(|seams| seams.borrow().pushes.clone());
    assert_eq!(
        pushes,
        vec![(
            "https://github.com/acme/demo.git".to_owned(),
            Some("secret-token".to_owned())
        )],
        "one push, to the plain clone URL, authenticated per invocation"
    );
}

#[test]
fn forge_publish_takes_the_name_and_visibility_the_caller_gives() {
    let standin = StandInForge::one_of_each();
    let built = with_clone_and_push_seams(with_token_on(&standin));
    let (root, _) = a_publishable_checkout("demo");
    let _bare = a_bare_remote("renamed");
    insert_project(&built, "demo", None, &root.display().to_string(), false);

    let published = forge_publish::publish_op(
        &built,
        json!({
            "project": "demo",
            "name": "renamed",
            "private": false,
            "description": "a demo",
            "reason": "publish it",
        }),
    )
    .unwrap();

    assert_eq!(
        published["repo"], "https://github.com/acme/renamed",
        "{published}"
    );
    assert_eq!(
        published["project"]["repo_url"],
        "https://github.com/acme/renamed"
    );
    let creates = standin.creates();
    assert_eq!(creates[0]["name"], "renamed");
    assert_eq!(creates[0]["private"], false);
    assert_eq!(creates[0]["description"], "a demo");
}

/// The picker pick: named, cloned under the file token, registered linked, and
/// its forge state read.
#[test]
fn forge_import_clones_registers_and_consolidates_under_the_file_token() {
    let standin = StandInForge::one_of_each();
    let built = with_clone_and_push_seams(with_token_on(&standin));

    let imported = forge_import::import_op(
        &built,
        json!({"owner": "acme", "name": "widget", "reason": "import it"}),
    )
    .unwrap();

    assert_eq!(imported["project"]["link_state"], "linked", "{imported}");
    assert_eq!(
        imported["project"]["repo_url"],
        "https://github.com/acme/widget"
    );
    assert_eq!(imported["cloned"], true);
    assert_eq!(imported["default_branch"], "main");
    assert_eq!(imported["revision"], "c".repeat(40));
    assert!(imported["consolidated"].is_object(), "{imported}");
    assert_eq!(project_link_state(&built, "widget"), "linked");

    let root = PathBuf::from(imported["root_path"].as_str().unwrap());
    assert!(
        root.join("README.md").is_file(),
        "the clone landed at {root:?}"
    );

    let clones = SEAMS.with(|seams| seams.borrow().clones.clone());
    assert_eq!(
        clones,
        vec![(
            "https://github.com/acme/widget.git".to_owned(),
            Some("secret-token".to_owned())
        )],
        "cloned under the file token, from the plain clone URL"
    );
    assert!(
        standin
            .requests()
            .iter()
            .any(|request| request.starts_with("GET ") && request.contains("/issues?")),
        "consolidation read the repository's issues: {:?}",
        standin.requests()
    );
}

/// The display-name override names the project and derives its slug; the
/// repository it clones is still the one the owner and name address.
#[test]
fn forge_import_takes_a_display_name_override() {
    let standin = StandInForge::one_of_each();
    let built = with_clone_and_push_seams(with_token_on(&standin));

    let imported = forge_import::import_op(
        &built,
        json!({
            "owner": "acme",
            "name": "widget",
            "display_name": "My Widget",
            "reason": "import it",
        }),
    )
    .unwrap();

    assert_eq!(imported["project"]["name"], "My Widget", "{imported}");
    assert_eq!(imported["project"]["slug"], "my-widget", "{imported}");
    assert_eq!(
        imported["project"]["repo_url"],
        "https://github.com/acme/widget"
    );
}

/// After an import the project is linked, so `forge.writeback`'s policy setter
/// operates on it.
#[test]
fn an_imported_project_takes_a_write_back_policy() {
    let standin = StandInForge::one_of_each();
    let built = with_clone_and_push_seams(with_token_on(&standin));
    let imported = forge_import::import_op(
        &built,
        json!({"owner": "acme", "name": "widget", "reason": "import it"}),
    )
    .unwrap();
    let slug = imported["project"]["slug"].as_str().unwrap().to_owned();

    let armed = forge_writeback::writeback_op(
        &built,
        json!({"project": slug, "policy": "full", "reason": "arm it"}),
    )
    .unwrap();

    assert_eq!(armed["project"]["write_back"], "full", "{armed}");
}

/// Linking a project with open native items opens each one upstream, in order,
/// and only then records the link. The policy permits the creates, so nothing is
/// refused.
#[test]
fn forge_link_migrates_open_native_items_upstream_before_it_links() {
    let standin = StandInForge::one_of_each();
    let built = with_token_on(&standin);
    insert_project(
        &built,
        "widget",
        Some("https://github.com/acme/widget"),
        "/tmp",
        false,
    );
    forge_writeback::writeback_op(
        &built,
        json!({"project": "widget", "policy": "full", "reason": "migrate it"}),
    )
    .unwrap();
    work::create_op(
        &built,
        json!({
            "kind": "chore",
            "title": "native item",
            "project": "widget",
            "local_only": true,
            "reason": "parity",
        }),
    )
    .unwrap();

    let linked =
        forge_link::link_op(&built, json!({"project": "widget", "reason": "link it"})).unwrap();

    assert_eq!(linked["project"]["link_state"], "linked", "{linked}");
    let migrated = linked["migrated"].as_array().expect("migrated is a list");
    assert_eq!(migrated.len(), 1, "{linked}");
    assert_eq!(migrated[0]["title"], "native item");
    assert_eq!(migrated[0]["subject_key"], "gh:acme/widget#8");
    assert_eq!(project_link_state(&built, "widget"), "linked");
    assert!(
        standin
            .requests()
            .iter()
            .any(|request| request == "POST https://api.github.com/repos/acme/widget/issues"),
        "the native item was created upstream: {:?}",
        standin.requests()
    );
}

/// A context on the stand-in with the GitHub file token, and optionally a
/// Forgejo host's file token and a Fernet key so accounts can be linked.
fn with_forge_config_on(standin: &StandInForge, forgejo: bool, account_key: bool) -> Built {
    let dir = std::env::temp_dir().join(format!(
        "vogt-forge-ops-{}-{}",
        std::process::id(),
        unique()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let github_token = dir.join("forge-token");
    std::fs::write(&github_token, "secret-token\n").unwrap();
    let mut config = VogtConfig {
        data_dir: dir.clone(),
        github_token_file: Some(github_token),
        ..VogtConfig::default()
    };
    if forgejo {
        let forgejo_token = dir.join("forgejo-token");
        std::fs::write(&forgejo_token, "frg_file_token\n").unwrap();
        config
            .forge_token_files
            .insert("forge.example.net".to_owned(), forgejo_token);
    }
    if account_key {
        let key = dir.join("account.key");
        std::fs::write(&key, "MDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDA=").unwrap();
        config.forge_account_key_file = Some(key);
    }
    let mut clock = Some(StepClock::new(moment()));
    let mut ids = Some(SequentialIds::new(None).unwrap());
    crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
    let built = build_context(
        config,
        Some(principal()),
        clock,
        ids,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let Built::StepSequential(ctx) = built else {
        panic!("expected a step clock");
    };
    let transport = standin.transport();
    with_clone_and_push_seams(Built::StepSequential(ctx.with_forge_transport(
        move |url, headers, body, method| transport(url, headers, body, method),
    )))
}

/// The publish conflict: a name the account already holds answers 422, which
/// is a typed refusal. Nothing is pushed and nothing local changes.
#[test]
fn forge_publish_answers_a_422_as_a_typed_refusal_and_pushes_nothing() {
    let standin = StandInForge::one_of_each().with_existing(&["demo"]);
    let built = with_forge_config_on(&standin, false, false);
    let (root, _) = a_publishable_checkout("demo");
    let _bare = a_bare_remote("demo");
    insert_project(&built, "demo", None, &root.display().to_string(), false);

    match forge_publish::publish_op(&built, json!({"project": "demo", "reason": "publish it"})) {
        Err(VogtError::RemoteRepoExists(message)) => {
            assert!(message.contains("never adopts"), "{message}");
        }
        other => panic!("expected RemoteRepoExists, got {other:?}"),
    }
    let pushes = SEAMS.with(|seams| seams.borrow().pushes.clone());
    assert!(
        pushes.is_empty(),
        "nothing was pushed at an existing remote: {pushes:?}"
    );
    assert_eq!(project_link_state(&built, "demo"), "unlinked");
}

/// The non-fast-forward refusal, on the real `git push`: a diverged remote
/// `main` is refused, the refusal is typed, and the remote's history stands.
#[test]
fn push_branch_refuses_a_non_fast_forward_and_keeps_the_remote_history() {
    let bare = a_bare_remote("diverged");
    let (theirs, _) = a_publishable_checkout("theirs");
    std::fs::write(theirs.join("theirs.txt"), "theirs\n").unwrap();
    git(&["add", "."], &theirs);
    git(
        &[
            "-c",
            "user.email=other@example.invalid",
            "-c",
            "user.name=Other",
            "commit",
            "-q",
            "-m",
            "theirs",
        ],
        &theirs,
    );
    push_branch(&PushRequest {
        root: theirs.clone(),
        remote: bare.display().to_string(),
        branch: "main".to_owned(),
        token: None,
        helper_dir: None,
    })
    .unwrap();

    let (ours, _) = a_publishable_checkout("ours");
    std::fs::write(ours.join("ours.txt"), "ours\n").unwrap();
    git(&["add", "."], &ours);
    git(
        &[
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "user.name=Forge Test",
            "commit",
            "-q",
            "-m",
            "diverge",
        ],
        &ours,
    );
    let before = git(&["rev-parse", "refs/heads/main"], &bare);

    let refused = push_branch(&PushRequest {
        root: ours,
        remote: bare.display().to_string(),
        branch: "main".to_owned(),
        token: None,
        helper_dir: None,
    });
    match refused {
        Err(VogtError::PublishNonFastForward(message)) => {
            assert!(message.contains("never forces"), "{message}");
        }
        other => panic!("expected PublishNonFastForward, got {other:?}"),
    }
    assert_eq!(git(&["rev-parse", "refs/heads/main"], &bare), before);
}

/// Write-through after publish: an issue written through the published
/// repository's provider lands upstream on the new repository and comes back
/// with its upstream ref. This is the provider write the link migration uses;
/// the `work.create` hook that calls it is not ported (WI-1068).
#[test]
fn a_published_repository_takes_writes_through_its_provider() {
    let standin = StandInForge::one_of_each();
    let built = with_forge_config_on(&standin, false, false);
    let (root, _) = a_publishable_checkout("demo");
    let _bare = a_bare_remote("demo");
    insert_project(&built, "demo", None, &root.display().to_string(), false);
    forge_publish::publish_op(&built, json!({"project": "demo", "reason": "publish it"})).unwrap();

    let Built::StepSequential(ctx) = &built else {
        panic!("expected a step clock");
    };
    let provider = provider_for_host("github.com", &ctx.config, ctx.forge_transport())
        .unwrap()
        .expect("github.com resolves with the file token");
    let repo = provider
        .parse(Some("https://github.com/acme/demo"))
        .expect("the published address parses");
    let sent = provider
        .create_issue(&repo, "First", "written through", None)
        .unwrap();

    assert_eq!(
        sent.subject_key.as_deref(),
        Some("gh:acme/demo#8"),
        "{sent:?}"
    );
    assert!(
        standin
            .requests()
            .iter()
            .any(|request| request == "POST https://api.github.com/repos/acme/demo/issues"),
        "the issue was created upstream: {:?}",
        standin.requests()
    );
}

/// The Forgejo picker lists the host-qualified repositories a configured
/// Forgejo host's file token can see.
#[test]
fn forgejo_picker_lists_host_qualified_repositories() {
    let standin = StandInForge::one_of_each();
    let built = with_forge_config_on(&standin, true, false);

    let listed = forge_repos::repos_op(&built, json!({"host": "forge.example.net"})).unwrap();

    let repos = listed["repos"].as_array().expect("repos is a list");
    assert_eq!(repos.len(), 1, "{listed}");
    assert_eq!(repos[0]["owner"], "acme");
    assert_eq!(repos[0]["name"], "fuelwatchx");
    assert_eq!(repos[0]["url"], "https://forge.example.net/acme/fuelwatchx");
}

/// Forgejo import clones the host-qualified URL under the host's file token,
/// registers the project linked, and consolidates it.
#[test]
fn forgejo_import_clones_the_host_qualified_url_under_the_file_token() {
    let standin = StandInForge::one_of_each();
    let built = with_forge_config_on(&standin, true, false);

    let imported = forge_import::import_op(
        &built,
        json!({"host": "forge.example.net", "owner": "acme", "name": "fuelwatchx", "reason": "import it"}),
    )
    .unwrap();

    assert_eq!(
        imported["project"]["repo_url"],
        "https://forge.example.net/acme/fuelwatchx"
    );
    assert_eq!(imported["project"]["link_state"], "linked", "{imported}");
    let clones = SEAMS.with(|seams| seams.borrow().clones.clone());
    assert_eq!(
        clones,
        vec![(
            "https://forge.example.net/acme/fuelwatchx.git".to_owned(),
            Some("frg_file_token".to_owned())
        )],
    );
}

/// The clone authenticates with the actor's linked PAT, not the instance file
/// token: the credential that could see the repository is the one that clones it.
#[test]
fn forge_import_clones_under_the_actors_credential() {
    let standin = StandInForge::one_of_each();
    let built = with_forge_config_on(&standin, false, true);
    forge_accounts::account_link_op(
        &built,
        json!({"token": "ghp_actor_pat", "reason": "link it"}),
    )
    .unwrap();

    forge_import::import_op(
        &built,
        json!({"owner": "octo-actor", "name": "rustnzb", "reason": "import it"}),
    )
    .unwrap();

    let clones = SEAMS.with(|seams| seams.borrow().clones.clone());
    assert_eq!(
        clones,
        vec![(
            "https://github.com/octo-actor/rustnzb.git".to_owned(),
            Some("ghp_actor_pat".to_owned())
        )],
        "the clone used the linked PAT, not secret-token"
    );
}
