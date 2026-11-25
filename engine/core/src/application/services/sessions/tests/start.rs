use super::*;

#[test]
fn a_start_with_no_subject_and_no_scratch_project_names_the_setting() {
    let (_dir, built, _) = context("no-scratch", person(), None, |_| {}, vec![]);
    let error = super::start_session_op(&built, json!({"reason": "no subject"})).unwrap_err();
    let VogtError::InvalidRequest(message) = &error else {
        panic!("expected invalid_request, got {error:?}");
    };
    assert!(
        message.contains("session_scratch_project") && message.contains("scratch project"),
        "{message}"
    );
}

#[test]
fn a_model_no_registered_provider_serves_draws_a_warning() {
    let providers = br#"{"providers":[{"name":"p","models":["served-model"]}]}"#.to_vec();
    let (_dir, built, _) = context(
        "warn-model",
        person(),
        None,
        |_| {},
        vec![providers.clone(), providers.clone(), providers],
    );
    with_ctx!(&built, |ctx| {
        let warning = super::unserved_model_warning(ctx, Some("ghost-model"))
            .expect("an unserved model warns");
        assert!(warning.contains("ghost-model"), "{warning}");
        assert!(super::unserved_model_warning(ctx, Some("served-model")).is_none());
        assert!(super::unserved_model_warning(ctx, Some("opus")).is_none());
        Ok::<(), VogtError>(())
    })
    .unwrap();
}

fn provider_start(name: &str, params: serde_json::Value) -> Result<serde_json::Value, VogtError> {
    let providers = br#"{"providers":[{"name":"p","models":["served-model"]}]}"#.to_vec();
    let (dir, built, _) = context(
        name,
        person(),
        None,
        |_| {},
        vec![providers.clone(), providers],
    );
    let root = dir.join("widget");
    std::fs::create_dir_all(&root).unwrap();
    crate::application::projects::register_op(
        &built,
        json!({"name": "Widget", "root_path": root.to_string_lossy(), "reason": "test"}),
    )
    .unwrap();
    let mut params = params;
    params["project"] = json!("widget");
    params["reason"] = json!("probe");
    super::start_session_op(&built, params)
}

#[test]
fn an_unregistered_provider_refuses_the_start() {
    let error = provider_start(
        "prov-missing",
        json!({"provider": "nope", "model": "served-model"}),
    )
    .unwrap_err();
    assert!(
        matches!(&error, VogtError::InvalidRequest(m) if m.contains("no LLM provider named")),
        "{error:?}"
    );
}

#[test]
fn a_provider_that_does_not_serve_the_model_refuses_the_start() {
    let error =
        provider_start("prov-model", json!({"provider": "p", "model": "other"})).unwrap_err();
    assert!(
        matches!(&error, VogtError::InvalidRequest(m) if m.contains("does not serve")),
        "{error:?}"
    );
}

#[test]
fn a_provider_without_a_model_refuses_the_start() {
    let error = provider_start("prov-nomodel", json!({"provider": "p"})).unwrap_err();
    assert!(
        matches!(&error, VogtError::InvalidRequest(m) if m.contains("needs a model")),
        "{error:?}"
    );
}

#[test]
fn a_provider_that_serves_the_model_starts_without_a_warning() {
    let result =
        provider_start("prov-ok", json!({"provider": "p", "model": "served-model"})).unwrap();
    assert!(result.get("warning").is_none(), "{result}");
}

/// The warning rides the whole `session.start` result, not just the helper.
#[test]
fn a_start_with_an_unserved_model_carries_the_warning_in_its_result() {
    let providers = br#"{"providers":[{"name":"p","models":["served-model"]}]}"#.to_vec();
    let (dir, built, _) = context("warn-start", person(), None, |_| {}, vec![providers]);
    let root = dir.join("widget");
    std::fs::create_dir_all(&root).unwrap();
    crate::application::projects::register_op(
        &built,
        json!({"name": "Widget", "root_path": root.to_string_lossy(), "reason": "test"}),
    )
    .unwrap();
    let started = super::start_session_op(
        &built,
        json!({"project": "widget", "reason": "probe", "model": "ghost-model"}),
    );
    let result = started.expect("the start proceeds despite the warning");
    let warning = result["warning"].as_str().expect("a warning field");
    assert!(warning.contains("ghost-model"), "{warning}");
}

/// WI-1522: an overseer's task start carries a context ceiling and no turn
/// or clock limit; an explicit budget, even an empty one, wins.
#[test]
fn an_overseer_start_defaults_to_a_context_ceiling() {
    use super::effective_budget;
    use super::SessionBudget;
    use crate::application::services::sessions_test::start_params;
    use crate::core::AgentPolicy;
    let policy = AgentPolicy::default();
    let mut params = start_params(Some("vogt"), Some("sweep"), "test");
    params.template = Some("claude".to_string());
    params.role = "oversight".to_string();
    let budget = effective_budget(&params, &policy).unwrap();
    assert_eq!(budget.context_tokens, Some(200_000));
    assert_eq!((budget.turns, budget.wall_clock_minutes), (None, None));
    assert_eq!(budget.session_wall_clock_minutes, None);
    params.budget = Some(SessionBudget::default());
    assert_eq!(
        effective_budget(&params, &policy),
        Some(SessionBudget::default())
    );
    params.budget = None;
    params.role = "worker".to_string();
    assert_eq!(
        effective_budget(&params, &policy).unwrap().context_tokens,
        None
    );
}

// -- characterization of `start_session_tracked` (WI-4628) ---------------------
//
// Written against the unsplit function and unchanged by the phase split: they
// pin the branches the older tests left open (bypass refusal, an engine that
// refuses, `engine_started`, where the recorded cwd comes from, the cold-wake
// opt-out).

mod tracked {
    use crate::application::services::sessions_test::{
        insert_project, opened, script_with, start_params,
    };
    use crate::storage::interface::{DeclaredStore, ReadView};

    use super::*;

    const ENGINE_ROW: &str = r#"{"id":"eng-t","name":"beta","cwd":"/resumed/dir"}"#;

    fn recorded_cwd(built: &crate::application::context::Built) -> String {
        with_ctx!(built, |ctx| {
            let view = ctx.declared.read().unwrap();
            view.session_by_engine_id("eng-t").unwrap().unwrap().cwd
        })
    }

    #[test]
    fn an_agent_cannot_grant_bypass_and_the_engine_is_not_asked() {
        let (_dir, built, seen) =
            context("bypass", agent("agent:engine:eng-w"), None, |_| {}, vec![]);
        let error = super::start_session_op(
            &built,
            json!({"reason": "go", "project": "beta", "permission_mode": "bypass"}),
        )
        .unwrap_err();
        assert!(
            matches!(&error, VogtError::BypassRefused(m) if m.contains("agent:engine:eng-w")),
            "{error:?}"
        );
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn an_engine_that_refuses_leaves_no_record_and_engine_started_unset() {
        let script = script_with(vec![(500, r#"{"error":"boom"}"#.to_string())]);
        let built = opened(Some(std::sync::Arc::clone(&script)));
        insert_project(&built, "beta");
        let mut engine_started = false;
        let outcome = with_ctx!(&built, |ctx| {
            crate::application::services::sessions::start_session_tracked(
                ctx,
                &start_params(Some("beta"), None, "because"),
                &mut engine_started,
            )
        });
        assert!(outcome.is_err(), "{outcome:?}");
        assert!(!engine_started);
        let events = with_ctx!(&built, |ctx| {
            ctx.declared
                .read()
                .unwrap()
                .list_events(0, 100, None)
                .unwrap()
        });
        assert!(events.iter().all(|event| event.kind != "session.started"));
        assert!(!script
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.starts_with("DELETE ")));
    }

    #[test]
    fn a_started_session_sets_engine_started_and_a_plain_start_keeps_the_project_cwd() {
        let script = script_with(vec![(200, ENGINE_ROW.to_string())]);
        let built = opened(Some(script));
        insert_project(&built, "beta");
        let mut engine_started = false;
        with_ctx!(&built, |ctx| {
            crate::application::services::sessions::start_session_tracked(
                ctx,
                &start_params(Some("beta"), None, "because"),
                &mut engine_started,
            )
        })
        .unwrap();
        assert!(engine_started);
        assert_eq!(recorded_cwd(&built), "/work/tree");
    }

    #[test]
    fn a_resume_or_a_worktree_records_the_directory_the_engine_reports() {
        for (resume, worktree) in [(Some("raw-conversation"), None), (None, Some(true))] {
            let script = script_with(vec![(200, ENGINE_ROW.to_string())]);
            let built = opened(Some(script));
            insert_project(&built, "beta");
            let mut params = start_params(Some("beta"), None, "because");
            params.resume = resume.map(str::to_string);
            params.worktree = worktree;
            with_ctx!(&built, |ctx| {
                crate::application::services::sessions::start_session(ctx, &params)
            })
            .unwrap();
            assert_eq!(recorded_cwd(&built), "/resumed/dir");
        }
    }

    #[test]
    fn the_cold_wake_opt_out_is_recorded_with_the_session() {
        for (asked, expected) in [(true, true), (false, false)] {
            let script = script_with(vec![(200, ENGINE_ROW.to_string())]);
            let built = opened(Some(script));
            insert_project(&built, "beta");
            let mut params = start_params(Some("beta"), None, "because");
            params.cold_wake_opt_out = asked;
            let result = with_ctx!(&built, |ctx| {
                crate::application::services::sessions::start_session(ctx, &params)
            })
            .unwrap();
            let id = result.session.id.clone();
            let stored = with_ctx!(&built, |ctx| {
                ctx.declared
                    .read()
                    .unwrap()
                    .session_cold_wake_opt_out(&id)
                    .unwrap()
            });
            assert_eq!(stored, expected);
        }
    }

    fn resolved(purpose: Option<&str>, review_of: Option<&str>) -> (StartSessionParams, bool) {
        let built = opened(None);
        let mut params = start_params(Some("beta"), None, "because");
        params.purpose = purpose.map(str::to_string);
        params.review_of = review_of.map(str::to_string);
        let (filled, read_only, _) = with_ctx!(&built, |ctx| {
            crate::application::services::sessions::resolve_purpose(ctx, &params)
        })
        .unwrap();
        (filled, read_only)
    }

    #[test]
    fn a_review_of_with_no_purpose_runs_as_review_with_the_review_profile() {
        let (filled, read_only) = resolved(None, Some("WI-1"));
        assert_eq!(filled.purpose.as_deref(), Some("review"));
        assert!(read_only, "the review profile is read-only");
        assert_eq!(
            filled.autopilot,
            Some(false),
            "the review profile fill applies"
        );
        assert!(filled.template.is_some() && filled.model.is_some());
    }

    #[test]
    fn a_review_of_with_an_explicit_purpose_keeps_that_purpose() {
        let (filled, read_only) = resolved(Some("implement"), Some("WI-1"));
        assert_eq!(filled.purpose.as_deref(), Some("implement"));
        assert!(!read_only);
    }

    #[test]
    fn a_start_with_no_review_of_and_no_purpose_is_unchanged() {
        let (filled, read_only) = resolved(None, None);
        assert_eq!(filled.purpose, None);
        assert!(!read_only);
        assert_eq!(filled.template, None);
    }
}
