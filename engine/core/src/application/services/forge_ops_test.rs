//! Tests for `forge.link`, `forge.publish` and `forge.import`, and the
//! native-item gate they share.
//!
//! The stand-in forge answers reads only, and it answers 404 to a repository
//! describe. So these tests cover the refusal paths, which run without a network,
//! and the link success path, which needs only a credential and no writes. The
//! publish and import success paths are not reachable through the stand-in.

use std::path::PathBuf;

use serde_json::json;

use crate::application::context::{build_context, Built};
use crate::application::services::{forge_import, forge_link, forge_publish, work};
use crate::config::VogtConfig;
use crate::core::{LinkState, Project, SequentialIds, StepClock};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

use super::services_test::{moment, opened, principal, unique};

/// A context whose instance holds a GitHub token file, with the stand-in forge
/// wired in, so a credential resolves and the forge answers reads.
fn with_token() -> Built {
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
    let transport = crate::application::forge_standin::StandInForge::one_of_each().transport();
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
