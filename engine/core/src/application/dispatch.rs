//! Dispatch profiles: what a session started for a purpose runs with (WI-1323).
//! Ports `src/vogt/core/dispatch.py`.
//!
//! A start names *why* it is being made, and the profile for that purpose
//! supplies the template, model, effort and autopilot it would otherwise have
//! to restate. The code defaults are the fallback; a deployment's
//! `dispatch.profiles` preference overrides any field of any profile without
//! restating the rest.
//!
//! A profile never carries a role or a permission posture. Both are a
//! person's decision on each start, so a profile cannot raise an agent by
//! proxy.
//!
//! Pure: nothing here reads storage, the engine or the clock.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::core::py_repr;

/// The purposes a start may name, in the order the Python `Purpose` lists them.
pub const PURPOSES: [&str; 6] = [
    "implement",
    "implement-careful",
    "review",
    "mechanical",
    "investigate",
    "design",
];

/// The preference keys this module reads. Every other `dispatch.*` key is
/// refused on write (the write path is not ported yet).
pub const PROFILES_KEY: &str = "dispatch.profiles";
pub const MAX_WORKERS_KEY: &str = "dispatch.max_workers_per_overseer";
pub const ESCALATE_KEY: &str = "dispatch.escalate_after_failed_reviews";
pub const REVIEW_BY_OVERSIGHT_KEY: &str = "dispatch.review_by_oversight_only";
/// How many times the core restarts an item's worker from its hand-off after
/// a budget stop (WI-1833). `0` turns the restart off.
pub const BUDGET_RESTARTS_KEY: &str = "dispatch.budget_restarts_per_item";

pub const DEFAULT_MAX_WORKERS_PER_OVERSEER: i64 = 5;
pub const DEFAULT_ESCALATE_AFTER_FAILED_REVIEWS: i64 = 2;
pub const DEFAULT_REVIEW_BY_OVERSIGHT_ONLY: bool = true;
pub const DEFAULT_BUDGET_RESTARTS_PER_ITEM: i64 = 1;

/// The start defaults one purpose resolves to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Profile {
    pub template: String,
    pub model: String,
    pub effort: Option<String>,
    pub budget_turns: Option<i64>,
    pub budget_wall_clock_minutes: Option<i64>,
    pub worktree: bool,
    pub repo_map: bool,
    pub autopilot: bool,
    /// A hint carried into the brief: the session should read and report,
    /// not change the tree.
    pub read_only: bool,
}

/// One profile as a deployment overrides it: every field optional, and no
/// field it does not name.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_turns: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_wall_clock_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_map: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autopilot: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
}

/// The deployment's dispatch table with every default filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchSettings {
    pub profiles: BTreeMap<String, Profile>,
    pub max_workers_per_overseer: i64,
    pub escalate_after_failed_reviews: i64,
    pub review_by_oversight_only: bool,
    pub budget_restarts_per_item: i64,
}

impl DispatchSettings {
    /// The profile for a purpose. Every name in `PURPOSES` has one.
    pub fn profile(&self, purpose: &str) -> Option<&Profile> {
        self.profiles.get(purpose)
    }
}

/// The preference keys a write may name. Every other `dispatch.*` key is
/// refused, so a typo is said rather than stored and silently ignored.
pub const KEYS: [&str; 5] = [
    PROFILES_KEY,
    MAX_WORKERS_KEY,
    ESCALATE_KEY,
    REVIEW_BY_OVERSIGHT_KEY,
    BUDGET_RESTARTS_KEY,
];

/// The prefix every dispatch preference key carries (Python `KEY_PREFIX`).
pub const KEY_PREFIX: &str = "dispatch.";

/// Validate a `dispatch.*` value before it is stored, and return its canonical
/// form (Python `dispatch.validate`). `{}` clears a key back to its default and
/// is always accepted. The messages name the key and the rule it broke; they
/// do not reproduce pydantic's wording.
pub fn validate(key: &str, value: &Value) -> Result<Value, String> {
    if !KEYS.contains(&key) {
        return Err(format!(
            "unknown dispatch setting {}; the keys are {}",
            py_repr(key),
            KEYS.join(", ")
        ));
    }
    let object = match value.as_object() {
        Some(object) => object,
        None => return Err(format!("invalid {key} value — expected an object")),
    };
    if object.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    if key == PROFILES_KEY {
        return validate_profiles(object);
    }
    validate_scalar(key, value)
}

fn validate_profiles(object: &Map<String, Value>) -> Result<Value, String> {
    let mut unknown: Vec<&String> = object
        .keys()
        .filter(|name| !PURPOSES.contains(&name.as_str()))
        .collect();
    unknown.sort();
    if !unknown.is_empty() {
        let names: Vec<String> = unknown.iter().map(|name| py_repr(name.as_str())).collect();
        return Err(format!(
            "unknown purpose {} in {PROFILES_KEY}; the purposes are {}",
            names.join(", "),
            PURPOSES.join(", ")
        ));
    }
    let mut out = Map::new();
    for (name, fields) in object {
        let parsed: ProfileOverride = serde_json::from_value(fields.clone()).map_err(|error| {
            format!(
                "invalid {PROFILES_KEY}.{name} — {}",
                profile_problem(&error)
            )
        })?;
        if let Some(problem) = profile_field_problem(&parsed) {
            return Err(format!("invalid {PROFILES_KEY}.{name} — {problem}"));
        }
        out.insert(
            name.clone(),
            serde_json::to_value(&parsed).expect("an override serialises"),
        );
    }
    Ok(Value::Object(out))
}

/// Serde's message names the bad field; pydantic's named the field too. Keep
/// serde's text, trimmed of its position suffix.
fn profile_problem(error: &serde_json::Error) -> String {
    let text = error.to_string();
    match text.rsplit_once(" at line ") {
        Some((head, _)) => head.to_string(),
        None => text,
    }
}

/// The rules serde's types cannot express: strings are non-empty, budgets
/// are positive.
fn profile_field_problem(over: &ProfileOverride) -> Option<String> {
    for (field, value) in [
        ("template", &over.template),
        ("model", &over.model),
        ("effort", &over.effort),
    ] {
        if value.as_deref().is_some_and(str::is_empty) {
            return Some(format!("{field}: must not be empty"));
        }
    }
    for (field, value) in [
        ("budget_turns", over.budget_turns),
        ("budget_wall_clock_minutes", over.budget_wall_clock_minutes),
    ] {
        if value.is_some_and(|number| number <= 0) {
            return Some(format!("{field}: must be greater than 0"));
        }
    }
    None
}

fn validate_scalar(key: &str, value: &Value) -> Result<Value, String> {
    let object = value.as_object().expect("checked by the caller");
    if key == BUDGET_RESTARTS_KEY {
        return validate_count(key, object, value);
    }
    let stored = object.get("value");
    let valid = object.len() == 1
        && match key {
            MAX_WORKERS_KEY | ESCALATE_KEY => stored
                .and_then(Value::as_i64)
                .is_some_and(|number| number > 0),
            _ => stored.and_then(Value::as_bool).is_some(),
        };
    if !valid {
        let shape = match key {
            MAX_WORKERS_KEY | ESCALATE_KEY => "a positive whole number under `value`",
            _ => "a boolean under `value`",
        };
        return Err(format!("invalid {key} value — expected {shape}"));
    }
    Ok(value.clone())
}

/// `budget_restarts_per_item` is pydantic's `_CountSetting`: one `value`
/// field, an integer greater than or equal to zero. The messages are the
/// ones Python's `DispatchError` carries.
fn validate_count(key: &str, object: &Map<String, Value>, value: &Value) -> Result<Value, String> {
    if let Some(extra) = object.keys().find(|name| name.as_str() != "value") {
        return Err(format!(
            "invalid {key} value — {extra}: Extra inputs are not permitted"
        ));
    }
    // Pydantic lax `int`. A value that fits in i64 is stored as that integer.
    // One that does not (`2**63`) is kept as JSON sent it, which is how Python
    // keeps an arbitrary-precision int. A float JSON cannot spell exactly
    // (`1e20`) is Python's "exceeded maximum size" message.
    let stored = match object.get("value") {
        Some(Value::Number(number)) => match number.as_i64() {
            Some(number) => Value::from(number),
            None if number.as_u64().is_some() => {
                // `2**63` and friends: Python keeps the integer. Store the
                // JSON number as it arrived.
                Value::Number(number.clone())
            }
            None if number.as_f64().is_some_and(|f| f.fract() == 0.0) => {
                let f = number.as_f64().unwrap();
                let exact = f as i64;
                if serde_json::Number::from(exact).as_f64() == Some(f) {
                    Value::from(exact)
                } else {
                    // JSON wrote this as 1e+20. Python cannot parse that as an
                    // integer.
                    return Err(format!(
                        "invalid {key} value — value: Unable to parse input string as an integer, exceeded maximum size"
                    ));
                }
            }
            None => {
                return Err(format!(
                    "invalid {key} value — value: Input should be a valid integer, got a number with a fractional part"
                ));
            }
        },
        Some(Value::Bool(flag)) => Value::from(i64::from(*flag)),
        Some(Value::String(text)) => match lax_int(text) {
            Some(number) => number,
            None => {
                return Err(format!(
                    "invalid {key} value — value: Input should be a valid integer, unable to parse string as an integer"
                ));
            }
        },
        _ => {
            return Err(format!(
                "invalid {key} value — value: Input should be a valid integer"
            ));
        }
    };
    if stored.as_i64().is_some_and(|number| number < 0) {
        return Err(format!(
            "invalid {key} value — value: Input should be greater than or equal to 0"
        ));
    }
    let _ = value;
    Ok(serde_json::json!({"value": stored}))
}

/// Pydantic's string-to-int: trim, allow `_` grouping and a trailing `.0`.
/// `Some(Number)` is the normalised integer. `None` is not an integer.
///
/// A leading or trailing underscore, or a doubled one, is rejected (`"_1"`,
/// `"1_"`, `"1__000"`). A value bigger than `i64` (`2**63`) is kept as an
/// exact JSON integer: `arbitrary_precision` makes `Number` parse the digits
/// instead of a float.
fn lax_int(text: &str) -> Option<Value> {
    let text = text.trim();
    // Pydantic's grouping: `_` may sit between digits (`1_000`, `0_1`). It
    // may not lead, trail, double, or sit next to the decimal point (`_1`,
    // `1_`, `1__000`, `1._0`).
    if text.starts_with('_')
        || text.ends_with('_')
        || text.contains("__")
        || text.contains("._")
        || text.contains("_.")
    {
        return None;
    }
    let text = text.replace('_', "");
    if let Some((whole, frac)) = text.split_once('.') {
        if whole.is_empty() || !frac.bytes().all(|byte| byte == b'0') {
            return None;
        }
        return integer_number(whole);
    }
    integer_number(&text)
}

/// `whole` is the digit text, optionally signed. `i64` stays a small JSON
/// number; anything larger is the exact digit string.
fn integer_number(whole: &str) -> Option<Value> {
    let unsigned = whole.strip_prefix('+').unwrap_or(whole);
    let digits = unsigned.strip_prefix('-').unwrap_or(unsigned);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if let Ok(number) = whole.parse::<i64>() {
        return Some(Value::from(number));
    }
    // `+` is not JSON. The rest is digits, so this is an exact integer.
    serde_json::from_str(unsigned).ok()
}

/// The code defaults (design C10).
fn default_profiles() -> BTreeMap<String, Profile> {
    let profile = |template: &str, model: &str| Profile {
        template: template.to_string(),
        model: model.to_string(),
        effort: None,
        budget_turns: None,
        budget_wall_clock_minutes: None,
        worktree: false,
        repo_map: false,
        autopilot: true,
        read_only: false,
    };
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "implement".to_string(),
        Profile {
            budget_turns: Some(120),
            budget_wall_clock_minutes: Some(90),
            worktree: true,
            repo_map: true,
            ..profile("klaudia", "grok-4.7")
        },
    );
    profiles.insert(
        "implement-careful".to_string(),
        Profile {
            effort: Some("low".to_string()),
            budget_turns: Some(150),
            ..profile("claude", "opus")
        },
    );
    profiles.insert(
        "review".to_string(),
        Profile {
            effort: Some("low".to_string()),
            budget_turns: Some(40),
            autopilot: false,
            read_only: true,
            ..profile("claude", "opus")
        },
    );
    profiles.insert(
        "mechanical".to_string(),
        Profile {
            budget_turns: Some(60),
            ..profile("claude", "haiku")
        },
    );
    profiles.insert(
        "investigate".to_string(),
        Profile {
            effort: Some("low".to_string()),
            budget_turns: Some(60),
            ..profile("claude", "opus")
        },
    );
    profiles.insert(
        "design".to_string(),
        Profile {
            effort: Some("low".to_string()),
            budget_turns: Some(60),
            ..profile("claude", "opus")
        },
    );
    profiles
}

/// The dispatch table from the stored `dispatch.*` values over the defaults.
///
/// `stored` maps a key to its value; a missing or empty key is its default. A
/// stored value that no longer parses (written by an older build) is ignored
/// rather than failing every start.
pub fn resolve(stored: &BTreeMap<String, Value>) -> DispatchSettings {
    let mut profiles = default_profiles();
    if let Some(overrides) = stored.get(PROFILES_KEY).and_then(Value::as_object) {
        for (name, fields) in overrides {
            let Some(profile) = profiles.get_mut(name) else {
                continue;
            };
            let Ok(over) = serde_json::from_value::<ProfileOverride>(fields.clone()) else {
                continue;
            };
            merge(profile, over);
        }
    }
    DispatchSettings {
        profiles,
        max_workers_per_overseer: scalar_int(stored, MAX_WORKERS_KEY)
            .unwrap_or(DEFAULT_MAX_WORKERS_PER_OVERSEER),
        escalate_after_failed_reviews: scalar_int(stored, ESCALATE_KEY)
            .unwrap_or(DEFAULT_ESCALATE_AFTER_FAILED_REVIEWS),
        review_by_oversight_only: scalar_bool(stored, REVIEW_BY_OVERSIGHT_KEY)
            .unwrap_or(DEFAULT_REVIEW_BY_OVERSIGHT_ONLY),
        budget_restarts_per_item: scalar_count(stored, BUDGET_RESTARTS_KEY)
            .unwrap_or(DEFAULT_BUDGET_RESTARTS_PER_ITEM),
    }
}

/// Lay the fields an override names over a profile; the rest stand.
fn merge(profile: &mut Profile, over: ProfileOverride) {
    if let Some(template) = over.template {
        profile.template = template;
    }
    if let Some(model) = over.model {
        profile.model = model;
    }
    if let Some(effort) = over.effort {
        profile.effort = Some(effort);
    }
    if let Some(turns) = over.budget_turns {
        profile.budget_turns = Some(turns);
    }
    if let Some(minutes) = over.budget_wall_clock_minutes {
        profile.budget_wall_clock_minutes = Some(minutes);
    }
    if let Some(worktree) = over.worktree {
        profile.worktree = worktree;
    }
    if let Some(repo_map) = over.repo_map {
        profile.repo_map = repo_map;
    }
    if let Some(autopilot) = over.autopilot {
        profile.autopilot = autopilot;
    }
    if let Some(read_only) = over.read_only {
        profile.read_only = read_only;
    }
}

/// A scalar setting is stored as `{"value": n}` with `n` a positive integer.
fn scalar_int(stored: &BTreeMap<String, Value>, key: &str) -> Option<i64> {
    stored
        .get(key)?
        .get("value")?
        .as_i64()
        .filter(|value| *value > 0)
}

/// A count is stored as `{"value": n}` with `n` zero or more. Zero is a real
/// setting (`budget_restarts_per_item` turns restarts off), not "unset".
fn scalar_count(stored: &BTreeMap<String, Value>, key: &str) -> Option<i64> {
    stored
        .get(key)?
        .get("value")?
        .as_i64()
        .filter(|value| *value >= 0)
}

/// A scalar setting is stored as `{"value": b}` with `b` a boolean.
fn scalar_bool(stored: &BTreeMap<String, Value>, key: &str) -> Option<bool> {
    stored.get(key)?.get("value")?.as_bool()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_valid_profile_override_is_stored_in_canonical_form() {
        let stored = validate(PROFILES_KEY, &json!({"review": {"model": "sonnet"}})).unwrap();
        assert_eq!(stored, json!({"review": {"model": "sonnet"}}));
    }

    #[test]
    fn an_empty_value_clears_a_key_and_is_accepted() {
        assert_eq!(validate(PROFILES_KEY, &json!({})).unwrap(), json!({}));
        assert_eq!(validate(MAX_WORKERS_KEY, &json!({})).unwrap(), json!({}));
    }

    #[test]
    fn an_unknown_key_or_purpose_is_refused_and_named() {
        let key = validate("dispatch.nope", &json!({"value": 1})).unwrap_err();
        assert!(
            key.starts_with("unknown dispatch setting 'dispatch.nope'"),
            "{key}"
        );
        let purpose = validate(PROFILES_KEY, &json!({"ship": {"model": "x"}})).unwrap_err();
        assert!(
            purpose.contains("unknown purpose 'ship' in dispatch.profiles"),
            "{purpose}"
        );
    }

    #[test]
    fn a_field_the_profile_cannot_hold_is_refused() {
        let unknown =
            validate(PROFILES_KEY, &json!({"review": {"role": "oversight"}})).unwrap_err();
        assert!(
            unknown.starts_with("invalid dispatch.profiles.review"),
            "{unknown}"
        );
        let empty = validate(PROFILES_KEY, &json!({"review": {"model": ""}})).unwrap_err();
        assert!(empty.contains("model: must not be empty"), "{empty}");
        let zero = validate(PROFILES_KEY, &json!({"review": {"budget_turns": 0}})).unwrap_err();
        assert!(
            zero.contains("budget_turns: must be greater than 0"),
            "{zero}"
        );
    }

    #[test]
    fn a_scalar_must_be_its_own_shape() {
        assert!(validate(MAX_WORKERS_KEY, &json!({"value": 0})).is_err());
        assert!(validate(MAX_WORKERS_KEY, &json!({"value": 3, "extra": 1})).is_err());
        assert!(validate(REVIEW_BY_OVERSIGHT_KEY, &json!({"value": "yes"})).is_err());
        assert_eq!(
            validate(REVIEW_BY_OVERSIGHT_KEY, &json!({"value": false})).unwrap(),
            json!({"value": false})
        );
    }

    #[test]
    fn every_purpose_has_a_code_default_profile() {
        let settings = resolve(&BTreeMap::new());
        for purpose in PURPOSES {
            assert!(settings.profile(purpose).is_some(), "{purpose}");
        }
        assert_eq!(settings.profile("implement").unwrap().model, "grok-4.7");
        assert!(settings.profile("review").unwrap().read_only);
        assert!(!settings.profile("review").unwrap().autopilot);
        assert_eq!(settings.max_workers_per_overseer, 5);
        assert!(settings.review_by_oversight_only);
    }

    #[test]
    fn an_override_names_only_the_fields_it_changes() {
        let mut stored = BTreeMap::new();
        stored.insert(
            PROFILES_KEY.to_string(),
            json!({"implement": {"model": "sonnet"}}),
        );
        let implement = resolve(&stored).profiles.remove("implement").unwrap();
        assert_eq!(implement.model, "sonnet");
        assert_eq!(implement.template, "klaudia");
        assert_eq!(implement.budget_turns, Some(120));
        assert!(implement.worktree);
    }

    #[test]
    fn a_stored_value_that_no_longer_parses_is_ignored() {
        let mut stored = BTreeMap::new();
        stored.insert(
            PROFILES_KEY.to_string(),
            json!({"review": {"model": "sonnet", "bogus": 1}, "nonsense": {}}),
        );
        stored.insert(MAX_WORKERS_KEY.to_string(), json!({"value": 0}));
        stored.insert(ESCALATE_KEY.to_string(), json!({"value": 7}));
        let settings = resolve(&stored);
        assert_eq!(settings.profile("review").unwrap().model, "opus");
        assert!(!settings.profiles.contains_key("nonsense"));
        assert_eq!(settings.max_workers_per_overseer, 5);
        assert_eq!(settings.escalate_after_failed_reviews, 7);
    }

    #[test]
    fn a_scalar_setting_overrides_its_default() {
        let mut stored = BTreeMap::new();
        stored.insert(REVIEW_BY_OVERSIGHT_KEY.to_string(), json!({"value": false}));
        assert!(!resolve(&stored).review_by_oversight_only);
    }

    #[test]
    fn the_budget_restart_cap_defaults_to_one_and_can_be_off() {
        assert_eq!(
            resolve(&BTreeMap::new()).budget_restarts_per_item,
            DEFAULT_BUDGET_RESTARTS_PER_ITEM
        );
        let mut stored = BTreeMap::new();
        stored.insert(BUDGET_RESTARTS_KEY.to_string(), json!({"value": 0}));
        assert_eq!(resolve(&stored).budget_restarts_per_item, 0);
        assert_eq!(
            validate(BUDGET_RESTARTS_KEY, &json!({"value": 0})).unwrap(),
            json!({"value": 0})
        );
        assert!(validate(BUDGET_RESTARTS_KEY, &json!({"value": -1})).is_err());
        assert!(validate(BUDGET_RESTARTS_KEY, &json!({"value": 1, "extra": true})).is_err());
        // Pydantic lax int: the stored value is the normalised integer.
        assert_eq!(
            validate(BUDGET_RESTARTS_KEY, &json!({"value": "3"})).unwrap(),
            json!({"value": 3})
        );
        assert_eq!(
            validate(BUDGET_RESTARTS_KEY, &json!({"value": true})).unwrap(),
            json!({"value": 1})
        );
        assert_eq!(
            validate(BUDGET_RESTARTS_KEY, &json!({"value": 1.0})).unwrap(),
            json!({"value": 1})
        );
        assert_eq!(
            validate(BUDGET_RESTARTS_KEY, &json!({"value": 1.5})).unwrap_err(),
            "invalid dispatch.budget_restarts_per_item value — value: Input should be a valid integer, got a number with a fractional part"
        );
        assert_eq!(
            validate(BUDGET_RESTARTS_KEY, &json!({"value": "1.0"})).unwrap(),
            json!({"value": 1})
        );
        assert_eq!(
            validate(BUDGET_RESTARTS_KEY, &json!({"value": "1_000"})).unwrap(),
            json!({"value": 1000})
        );
        assert_eq!(
            validate(BUDGET_RESTARTS_KEY, &json!({"value": 1e20})).unwrap_err(),
            "invalid dispatch.budget_restarts_per_item value — value: Unable to parse input string as an integer, exceeded maximum size"
        );
        let two_pow_63 = serde_json::json!({"value": 9223372036854775808u64});
        assert_eq!(
            validate(BUDGET_RESTARTS_KEY, &two_pow_63).unwrap(),
            two_pow_63
        );
        // Leading and trailing underscores are not integers. A big integer
        // stays the digits, not a float.
        assert!(validate(BUDGET_RESTARTS_KEY, &json!({"value": "_1"})).is_err());
        assert!(validate(BUDGET_RESTARTS_KEY, &json!({"value": "1_"})).is_err());
        let big = validate(
            BUDGET_RESTARTS_KEY,
            &json!({"value": "9223372036854775808"}),
        )
        .unwrap();
        assert_eq!(big["value"].to_string(), "9223372036854775808");
    }
}
