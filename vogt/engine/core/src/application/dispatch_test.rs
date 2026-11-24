//! Ports `tests/test_dispatch.py` (WI-1080): the pure dispatch table and its
//! validation (WI-1323).

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::dispatch::{
    check, escalation, resolve, validate, Decision, DispatchSettings, StartFacts, ENFORCE_KEY,
    ESCALATE_KEY, MAX_WORKERS_KEY, PROFILES_KEY, PURPOSES, REVIEW_BY_OVERSIGHT_KEY,
};

fn table(entries: &[(&str, Value)]) -> DispatchSettings {
    let stored: BTreeMap<String, Value> = entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    resolve(&stored)
}

#[test]
fn the_code_defaults_are_the_design_table() {
    let t = table(&[]);
    let implement = &t.profiles["implement"];
    assert_eq!(
        (implement.template.as_str(), implement.model.as_str()),
        ("claude", "sonnet")
    );
    assert_eq!(
        (implement.budget_turns, implement.budget_wall_clock_minutes),
        (Some(120), Some(90))
    );
    assert!(implement.worktree && implement.repo_map && implement.autopilot);
    let careful = &t.profiles["implement-careful"];
    assert_eq!(
        (
            careful.template.as_str(),
            careful.model.as_str(),
            careful.effort.as_deref()
        ),
        ("claude", "opus", Some("low"))
    );
    assert_eq!(careful.budget_turns, Some(150));
    let review = &t.profiles["review"];
    assert_eq!(
        (
            review.model.as_str(),
            review.effort.as_deref(),
            review.budget_turns
        ),
        ("opus", Some("low"), Some(40))
    );
    assert!(!review.autopilot && review.read_only);
    assert_eq!(t.profiles["mechanical"].model, "haiku");
    assert_eq!(t.profiles["mechanical"].budget_turns, Some(60));
    for name in ["investigate", "design"] {
        let p = &t.profiles[name];
        assert_eq!(
            (p.template.as_str(), p.model.as_str(), p.effort.as_deref()),
            ("claude", "opus", Some("low")),
            "{name}"
        );
        assert_eq!(p.budget_turns, Some(60), "{name}");
    }
    let mut names: Vec<&str> = t.profiles.keys().map(String::as_str).collect();
    let mut purposes = PURPOSES.to_vec();
    names.sort_unstable();
    purposes.sort_unstable();
    assert_eq!(names, purposes);
    assert_eq!(t.max_workers_per_overseer, 5);
    assert_eq!(t.escalate_after_failed_reviews, 2);
    assert!(t.review_by_oversight_only);
}

#[test]
fn an_override_changes_only_the_field_it_names() {
    let t = table(&[(PROFILES_KEY, json!({"implement": {"model": "x"}}))]);
    let implement = &t.profiles["implement"];
    assert_eq!(implement.model, "x");
    assert_eq!(implement.template, "claude");
    assert_eq!(implement.budget_turns, Some(120));
    assert_eq!(t.profiles["review"], table(&[]).profiles["review"]);
}

#[test]
fn the_scalar_settings_resolve_from_their_value() {
    let t = table(&[
        (MAX_WORKERS_KEY, json!({"value": 3})),
        (ESCALATE_KEY, json!({"value": 4})),
        (REVIEW_BY_OVERSIGHT_KEY, json!({"value": false})),
    ]);
    assert_eq!(t.max_workers_per_overseer, 3);
    assert_eq!(t.escalate_after_failed_reviews, 4);
    assert!(!t.review_by_oversight_only);
}

#[test]
fn a_stale_stored_value_falls_back_to_the_default() {
    let t = table(&[
        (PROFILES_KEY, json!({"implement": {"model": 7}, "gone": {}})),
        (MAX_WORKERS_KEY, json!({"value": "many"})),
    ]);
    assert_eq!(t.profiles["implement"].model, "sonnet");
    assert!(!t.profiles.contains_key("gone"));
    assert_eq!(t.max_workers_per_overseer, 5);
}

#[test]
fn a_malformed_value_is_refused() {
    let cases: [(&str, Value, &str); 8] = [
        (
            "dispatch.profiles",
            json!({"deploy": {"model": "x"}}),
            "unknown purpose",
        ),
        (
            "dispatch.profiles",
            json!({"implement": {"role": "oversight"}}),
            "role",
        ),
        (
            "dispatch.profiles",
            json!({"review": {"permission_mode": "bypass"}}),
            "permission",
        ),
        (
            "dispatch.profiles",
            json!({"review": {"budget_turns": 0}}),
            "budget_turns",
        ),
        (
            "dispatch.max_workers_per_overseer",
            json!({"value": 0}),
            "value",
        ),
        (
            "dispatch.max_workers_per_overseer",
            json!({"value": "five"}),
            "value",
        ),
        (
            "dispatch.review_by_oversight_only",
            json!({"value": "maybe"}),
            "value",
        ),
        (
            "dispatch.implement",
            json!({"model": "x"}),
            "unknown dispatch setting",
        ),
    ];
    for (key, value, said) in cases {
        let err = validate(key, &value).expect_err(key);
        assert!(err.contains(said), "{key} {value}: {err}");
    }
}

#[test]
fn each_key_is_validated_by_its_own_shape() {
    assert_eq!(
        validate("dispatch.max_workers_per_overseer", &json!({"value": 5})).unwrap(),
        json!({"value": 5})
    );
    assert!(validate(
        "dispatch.escalate_after_failed_reviews",
        &json!({"value": 2})
    )
    .is_ok());
    assert!(validate("dispatch.review_by_oversight_only", &json!({"value": true})).is_ok());
    assert_eq!(
        validate(
            "dispatch.profiles",
            &json!({"implement": {"model": "grok-4.8"}})
        )
        .unwrap(),
        json!({"implement": {"model": "grok-4.8"}})
    );
    assert_eq!(
        validate("dispatch.profiles", &json!({})).unwrap(),
        json!({})
    );
}

// -- check (WI-1329) ----------------------------------------------------------

fn facts<'a>(
    purpose: Option<&'a str>,
    review: bool,
    model: Option<&'a str>,
    caller_role: &'a str,
    live_workers: i64,
) -> StartFacts<'a> {
    StartFacts {
        purpose,
        review,
        model,
        caller_role: Some(caller_role),
        live_workers,
    }
}

fn rules(decision: &Decision) -> Vec<&'static str> {
    decision.violations.iter().map(|v| v.rule.name()).collect()
}

#[test]
fn enforcement_is_off_by_default() {
    assert!(!table(&[]).enforce);
    assert!(table(&[(ENFORCE_KEY, json!({"value": true}))]).enforce);
    assert!(validate(ENFORCE_KEY, &json!({"value": true})).is_ok());
    assert!(validate(ENFORCE_KEY, &json!({"value": 3})).is_err());
}

#[test]
fn a_worker_starting_a_review_breaks_review_by_oversight_only() {
    let settings = table(&[]);
    let judge = |role| rules(&check(&facts(None, true, None, role, 0), &settings));
    assert_eq!(judge("worker"), ["review_by_oversight_only"]);
    assert!(judge("oversight").is_empty());
    assert!(judge("person").is_empty());
    let off = table(&[(REVIEW_BY_OVERSIGHT_KEY, json!({"value": false}))]);
    assert!(rules(&check(&facts(None, true, None, "worker", 0), &off)).is_empty());
}

#[test]
fn a_model_other_than_the_profiles_is_named() {
    let settings = table(&[]);
    for purpose in [
        "implement",
        "implement-careful",
        "mechanical",
        "investigate",
    ] {
        let decision = check(
            &facts(Some(purpose), false, Some("something-else"), "oversight", 0),
            &settings,
        );
        assert_eq!(rules(&decision), ["profile_model"], "{purpose}");
        assert!(decision.violations[0].message.contains("something-else"));
        let model = settings.profiles[purpose].model.clone();
        let ok = check(
            &facts(Some(purpose), false, Some(&model), "oversight", 0),
            &settings,
        );
        assert!(rules(&ok).is_empty(), "{purpose}");
    }
}

#[test]
fn the_model_rule_follows_a_profile_override() {
    let settings = table(&[(PROFILES_KEY, json!({"implement": {"model": "grok-4.8"}}))]);
    let judge = |model| {
        rules(&check(
            &facts(Some("implement"), false, Some(model), "oversight", 0),
            &settings,
        ))
    };
    assert!(judge("grok-4.8").is_empty());
    assert_eq!(judge("grok-4.7"), ["profile_model"]);
}

#[test]
fn no_purpose_is_judged_as_implement() {
    let settings = table(&[]);
    let decision = check(
        &facts(None, false, Some("something-else"), "oversight", 0),
        &settings,
    );
    assert_eq!(rules(&decision), ["profile_model"]);
    assert!(decision.violations[0].message.contains("purpose=implement"));
    let model = settings.profiles["implement"].model.clone();
    assert!(rules(&check(
        &facts(None, false, Some(&model), "oversight", 0),
        &settings
    ))
    .is_empty());
}

#[test]
fn a_review_is_not_held_to_the_profile_model() {
    let decision = check(
        &facts(Some("review"), true, Some("other"), "oversight", 0),
        &table(&[]),
    );
    assert!(rules(&decision).is_empty());
}

#[test]
fn the_worker_cap_refuses_the_sixth() {
    let settings = table(&[]);
    let judge = |live, role| check(&facts(None, false, None, role, live), &settings);
    assert!(rules(&judge(4, "oversight")).is_empty());
    let over = judge(5, "oversight");
    assert!(over.violations[0]
        .message
        .starts_with("max_workers_per_overseer=5 reached"));
    assert!(rules(&judge(9, "person")).is_empty());
    let unknown = StartFacts {
        caller_role: None,
        ..facts(None, false, None, "x", 9)
    };
    assert!(rules(&check(&unknown, &settings)).is_empty());
}

#[test]
fn failed_reviews_escalate_implement_and_a_start_with_no_purpose() {
    let settings = table(&[]);
    assert_eq!(escalation(Some("implement"), 1, &settings), None);
    assert_eq!(
        escalation(Some("implement"), 2, &settings),
        Some("implement-careful")
    );
    assert_eq!(escalation(Some("mechanical"), 5, &settings), None);
    assert_eq!(escalation(None, 2, &settings), Some("implement-careful"));
    assert_eq!(escalation(None, 1, &settings), None);
}
