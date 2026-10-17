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

use serde_json::Value;

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

pub const DEFAULT_MAX_WORKERS_PER_OVERSEER: i64 = 5;
pub const DEFAULT_ESCALATE_AFTER_FAILED_REVIEWS: i64 = 2;
pub const DEFAULT_REVIEW_BY_OVERSIGHT_ONLY: bool = true;

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
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileOverride {
    pub template: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub budget_turns: Option<i64>,
    pub budget_wall_clock_minutes: Option<i64>,
    pub worktree: Option<bool>,
    pub repo_map: Option<bool>,
    pub autopilot: Option<bool>,
    pub read_only: Option<bool>,
}

/// The deployment's dispatch table with every default filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchSettings {
    pub profiles: BTreeMap<String, Profile>,
    pub max_workers_per_overseer: i64,
    pub escalate_after_failed_reviews: i64,
    pub review_by_oversight_only: bool,
}

impl DispatchSettings {
    /// The profile for a purpose. Every name in `PURPOSES` has one.
    pub fn profile(&self, purpose: &str) -> Option<&Profile> {
        self.profiles.get(purpose)
    }
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

/// A scalar setting is stored as `{"value": b}` with `b` a boolean.
fn scalar_bool(stored: &BTreeMap<String, Value>, key: &str) -> Option<bool> {
    stored.get(key)?.get("value")?.as_bool()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
