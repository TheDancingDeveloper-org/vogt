//! Ports `tests/test_dispatch.py` (WI-1080): the pure dispatch table and its
//! validation (WI-1323).

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::dispatch::{
    resolve, validate, DispatchSettings, BUDGET_RESTARTS_KEY, ESCALATE_KEY, MAX_WORKERS_KEY,
    PROFILES_KEY, PURPOSES, REVIEW_BY_OVERSIGHT_KEY,
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
        ("klaudia", "grok-4.7")
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
    assert_eq!(implement.template, "klaudia");
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
    assert_eq!(t.profiles["implement"].model, "grok-4.7");
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

#[test]
fn the_turn_budget_is_sized_by_the_model_it_runs() {
    let t = table(&[]);
    let implement = &t.profiles["implement"];
    assert_eq!(implement.turns_for(Some("haiku")), Some(200));
    assert_eq!(
        implement.turns_for(Some("claude-haiku-4-5-20251001")),
        Some(200)
    );
    assert_eq!(implement.turns_for(Some("sonnet-haikuish")), Some(120));
    assert_eq!(implement.turns_for(Some("grok-4.7")), Some(120));
    assert_eq!(implement.turns_for(None), Some(120));
}

#[test]
fn a_model_turn_override_merges_and_is_validated() {
    let t = table(&[(
        PROFILES_KEY,
        json!({"review": {"budget_turns_by_model": {"haiku": 30}}}),
    )]);
    assert_eq!(t.profiles["review"].turns_for(Some("haiku")), Some(30));
    assert_eq!(t.profiles["review"].turns_for(Some("opus")), Some(40));
    assert!(validate(
        PROFILES_KEY,
        &json!({"review": {"budget_turns_by_model": {"haiku": 0}}})
    )
    .is_err());
}

#[test]
fn the_budget_restart_cap_defaults_to_one_and_can_be_off() {
    assert_eq!(table(&[]).budget_restarts_per_item, 1);
    let off = table(&[(BUDGET_RESTARTS_KEY, json!({"value": 0}))]);
    assert_eq!(off.budget_restarts_per_item, 0);
    assert_eq!(
        validate(BUDGET_RESTARTS_KEY, &json!({"value": 0})).unwrap(),
        json!({"value": 0})
    );
}
