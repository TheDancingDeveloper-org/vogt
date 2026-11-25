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
