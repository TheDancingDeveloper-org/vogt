use super::*;

#[test]
fn project_source_defaults_a_repository_to_git_main() {
    let now = Moment::from_unix(0, 0);
    let mut project = Project::new("p", "a5", "A5", "/work", now);
    project.repo_url = Some("https://github.com/octocat/Hello-World.git".to_string());
    project.default_branch = Some("master".to_string());

    let source = project_source(&project).expect("repository has a source");
    assert_eq!(source.reference, "master");
}

#[test]
fn project_source_uses_the_stored_default_branch_not_main() {
    let now = Moment::from_unix(0, 0);
    let mut project = Project::new("p", "a5", "A5", "/work", now);
    project.repo_url = Some("https://github.com/octocat/Hello-World.git".to_string());
    project.default_branch = Some("master".to_string());

    let (source, used_main) = project_source_with_fallback(&project);
    assert_eq!(source.expect("repository has a source").reference, "master");
    assert!(!used_main, "a recorded branch is not the main fallback");
}

#[test]
fn project_source_without_a_stored_branch_still_resolves_main_and_reports_it() {
    let now = Moment::from_unix(0, 0);
    let mut project = Project::new("p", "vogt", "Vogt", "/work", now);
    project.repo_url = Some("https://github.com/indexarr/vogt.git".to_string());

    let (source, fallback) = project_source_with_fallback(&project);
    assert_eq!(source.expect("repository has a source").reference, "main");
    assert!(fallback, "the main fallback must be reported, not silent");
}

#[test]
fn project_source_defaults_a_repository_to_git_main_when_unstated() {
    let now = Moment::from_unix(0, 0);
    let mut project = Project::new("p", "vogt", "Vogt", "/work", now);
    project.repo_url = Some("https://github.com/indexarr/vogt.git".to_string());

    let source = project_source(&project).expect("repository has a source");
    assert_eq!(source.kind(), vogt_engine_contract::source::SourceKind::Git);
    assert_eq!(source.reference, "main");
    assert_eq!(
        source.landing,
        vogt_engine_contract::source::LandingMode::PushBranch
    );
}

#[test]
fn project_source_preserves_a_graft_descriptor_from_landing_policy() {
    let now = Moment::from_unix(0, 0);
    let mut project = Project::new("p", "vogt", "Vogt", "/work", now);
    project.agent_policy.landing = Some(json!({
        "base_branch": "main",
        "worker_lands_by": "graft",
        "source": {
            "location": {"kind": "graft", "endpoint": "http://graft.test", "repo": "vogt"},
            "ref": "rust-2",
            "landing": "line"
        }
    }));

    let source = project_source(&project).expect("landing policy has a source");
    assert_eq!(
        source.kind(),
        vogt_engine_contract::source::SourceKind::Graft
    );
    assert_eq!(source.reference, "rust-2");
    assert_eq!(
        source.landing,
        vogt_engine_contract::source::LandingMode::Line
    );
}
