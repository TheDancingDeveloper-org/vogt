use super::*;

fn sweep_with(name: &str, params: serde_json::Value) -> serde_json::Value {
    let (dir, built, _seen) = context(name, person(), None, |_| {}, vec![sweep_payload()]);
    let result = super::sweep_sessions_op(&built, params).unwrap();
    let _ = std::fs::remove_dir_all(dir);
    result
}

#[test]
fn a_sweep_hides_excluded_roles_and_empty_screen_tails() {
    let result = sweep_with("sweep-default", json!({"exclude_roles": ["oversight"]}));
    let rows = result["rows"].as_array().unwrap();
    let ids: Vec<&str> = rows
        .iter()
        .map(|row| row["session"]["engine_session_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["w-1"], "{result}");
    assert!(rows[0].get("screen_tail").is_none(), "{}", rows[0]);
    assert_eq!(result["counts"]["total"], 1);
}

#[test]
fn a_sweep_hides_operator_sessions_by_default_and_none_when_told_to_keep_all() {
    // `operator` is the default; no engine role carries it yet, so the
    // default hides nothing today and both sessions stay.
    let result = sweep_with("sweep-operator", json!({}));
    assert_eq!(result["rows"].as_array().unwrap().len(), 2, "{result}");
    let result = sweep_with("sweep-all", json!({"exclude_roles": []}));
    assert_eq!(result["rows"].as_array().unwrap().len(), 2, "{result}");
}

#[test]
fn a_sweep_since_the_future_returns_no_rows() {
    let result = sweep_with("sweep-since", json!({"since": "2999-01-01T00:00:00Z"}));
    assert!(result["rows"].as_array().unwrap().is_empty(), "{result}");
}
