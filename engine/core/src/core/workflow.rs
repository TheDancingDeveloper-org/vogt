//! The workflow machine and project lifecycle. Ports the workflow of
//! `src/vogt/core/`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::errors::VogtError;

pub type LifecycleState = &'static str;

pub const OPEN: &str = "open";
pub const IN_PROGRESS: &str = "in_progress";
pub const REVIEW: &str = "review";
pub const DONE: &str = "done";
pub const BLOCKED: &str = "blocked";
pub const WONT_DO: &str = "wont_do";

pub const TERMINAL_STATES: &[&str] = &[DONE, WONT_DO];

pub const DEFAULT_INITIAL_STATE: &str = OPEN;

pub fn default_transitions() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        (OPEN, vec![IN_PROGRESS, BLOCKED, WONT_DO]),
        (IN_PROGRESS, vec![REVIEW, BLOCKED, OPEN, WONT_DO]),
        (REVIEW, vec![DONE, IN_PROGRESS, BLOCKED, WONT_DO]),
        (BLOCKED, vec![OPEN, IN_PROGRESS, WONT_DO]),
        (DONE, vec![OPEN]),
        (WONT_DO, vec![OPEN]),
    ]
}

/// `TransitionRejected`. The message is `"{rule}: {text}"`, status 409.
pub fn transition_rejected(rule: &str, text: &str) -> VogtError {
    VogtError::TransitionRejected {
        rule: rule.to_string(),
        message: format!("{rule}: {text}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Workflow {
    pub kind: String,
    pub initial_state: String,
    /// Insertion order is the state order, matching Python's dict.
    pub transitions: Vec<(String, Vec<String>)>,
}

impl Workflow {
    pub fn states(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for (source, targets) in &self.transitions {
            if !seen.contains(source) {
                seen.push(source.clone());
            }
            for target in targets {
                if !seen.contains(target) {
                    seen.push(target.clone());
                }
            }
        }
        seen
    }

    pub fn allowed_from(&self, state: &str) -> &[String] {
        self.transitions
            .iter()
            .find(|(source, _)| source == state)
            .map(|(_, targets)| targets.as_slice())
            .unwrap_or(&[])
    }

    pub fn check(&self, from_state: &str, to_state: &str) -> Result<(), VogtError> {
        if from_state == to_state {
            return Err(transition_rejected(
                "transition.no_op",
                &format!("item is already in state '{to_state}'"),
            ));
        }
        let states = self.states();
        if !states.iter().any(|state| state == to_state) {
            let mut names: Vec<&str> = states.iter().map(String::as_str).collect();
            names.sort_unstable();
            return Err(transition_rejected(
                "transition.unknown_state",
                &format!(
                    "{} has no state '{to_state}' (states: {})",
                    self.kind,
                    names.join(", ")
                ),
            ));
        }
        let allowed = self.allowed_from(from_state);
        if !allowed.iter().any(|target| target == to_state) {
            let hint = match self.shortest_path(from_state, to_state) {
                Some(path) => format!(
                    "; shortest path: {} — pass walk=true to take it one audited hop at a time",
                    path.join(" -> ")
                ),
                None => String::new(),
            };
            let allowed_text = if allowed.is_empty() {
                "nothing".to_string()
            } else {
                allowed.join(", ")
            };
            return Err(transition_rejected(
                "transition.not_allowed",
                &format!(
                    "{} has no {from_state} -> {to_state} edge (allowed from {from_state}: {allowed_text}){hint}",
                    self.kind
                ),
            ));
        }
        Ok(())
    }

    /// The stored form: `{"initial_state", "transitions"}` with keys sorted,
    /// matching `json.dumps(..., sort_keys=True)` — spaced separators and
    /// non-ASCII escaped as `\uXXXX`, the same bytes Python writes.
    pub fn to_definition_json(&self) -> String {
        let transitions: BTreeMap<&str, &Vec<String>> = self
            .transitions
            .iter()
            .map(|(source, targets)| (source.as_str(), targets))
            .collect();
        crate::decisions::python_json_dumps(
            &serde_json::json!({
                "initial_state": self.initial_state,
                "transitions": transitions,
            }),
            true,
        )
    }

    /// Inverse of `to_definition_json`. A definition without a transitions
    /// object is refused, matching `Workflow.from_definition`.
    pub fn from_definition_json(kind: &str, text: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|err| err.to_string())?;
        let raw = value
            .get("transitions")
            .and_then(|item| item.as_object())
            .ok_or_else(|| format!("workflow definition for {kind} has no transitions map"))?;
        // serde_json is built with preserve_order, so iterating the map gives
        // the keys in the order they were written, escapes already decoded.
        let transitions = raw
            .iter()
            .filter_map(|(source, targets)| {
                targets.as_array().map(|list| {
                    (
                        source.clone(),
                        list.iter()
                            .map(|item| match item {
                                serde_json::Value::String(text) => text.clone(),
                                other => other.to_string(),
                            })
                            .collect(),
                    )
                })
            })
            .collect();
        let initial = value
            .get("initial_state")
            .and_then(|item| item.as_str())
            .unwrap_or(DEFAULT_INITIAL_STATE);
        Ok(Self {
            kind: kind.to_string(),
            initial_state: initial.to_string(),
            transitions,
        })
    }

    pub fn shortest_path(&self, from_state: &str, to_state: &str) -> Option<Vec<String>> {
        if from_state == to_state {
            return Some(vec![from_state.to_string()]);
        }
        let mut previous: BTreeMap<String, String> = BTreeMap::new();
        let mut frontier = VecDeque::from([from_state.to_string()]);
        let mut seen = BTreeSet::from([from_state.to_string()]);
        while !frontier.is_empty() {
            let width = frontier.len();
            for _ in 0..width {
                let state = frontier.pop_front().expect("width counted");
                if state != from_state && TERMINAL_STATES.contains(&state.as_str()) {
                    continue;
                }
                for target in self.allowed_from(&state) {
                    if !seen.insert(target.clone()) {
                        continue;
                    }
                    previous.insert(target.clone(), state.clone());
                    if target == to_state {
                        let mut path = vec![target.clone()];
                        while path.last().map(String::as_str) != Some(from_state) {
                            let prior = previous
                                .get(path.last().expect("path grows"))
                                .expect("linked");
                            path.push(prior.clone());
                        }
                        path.reverse();
                        return Some(path);
                    }
                    frontier.push_back(target.clone());
                }
            }
        }
        None
    }
}

pub fn default_workflow(kind: &str) -> Workflow {
    Workflow {
        kind: kind.to_string(),
        initial_state: DEFAULT_INITIAL_STATE.to_string(),
        transitions: default_transitions()
            .into_iter()
            .map(|(source, targets)| {
                (
                    source.to_string(),
                    targets.into_iter().map(str::to_string).collect(),
                )
            })
            .collect(),
    }
}

impl Workflow {
    /// The persisted shape: the initial state and the transition map, in the
    /// map's insertion order. Ports `Workflow.to_definition`.
    pub fn to_definition(&self) -> serde_json::Value {
        serde_json::json!({
            "initial_state": self.initial_state,
            "transitions": serde_json::Map::from_iter(self.transitions.iter().map(
                |(source, targets)| (source.clone(), serde_json::json!(targets)),
            )),
        })
    }

    /// A stored definition back into a machine. A missing `transitions` map is
    /// an error; a source whose targets are not a list is skipped; a missing
    /// initial state is `open`. Ports `Workflow.from_definition`.
    pub fn from_definition(kind: &str, definition: &serde_json::Value) -> Result<Self, String> {
        let raw = definition
            .get("transitions")
            .and_then(serde_json::Value::as_object);
        let Some(raw) = raw else {
            return Err(format!(
                "workflow definition for {kind} has no transitions map"
            ));
        };
        let transitions = raw
            .iter()
            .filter_map(|(source, targets)| {
                targets.as_array().map(|targets| {
                    (
                        source.clone(),
                        targets
                            .iter()
                            .map(|target| target.as_str().unwrap_or_default().to_string())
                            .collect(),
                    )
                })
            })
            .collect();
        let initial = definition
            .get("initial_state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(DEFAULT_INITIAL_STATE);
        Ok(Self {
            kind: kind.to_string(),
            initial_state: initial.to_string(),
            transitions,
        })
    }
}

pub fn check_completion_allowed(
    to_state: &str,
    blockers: &[(&str, &str)],
) -> Result<(), VogtError> {
    if to_state != DONE || blockers.is_empty() {
        return Ok(());
    }
    let listed = blockers
        .iter()
        .map(|(reference, state)| format!("{reference} ({state})"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(transition_rejected(
        "transition.blocked_by_dependency",
        &format!(
            "cannot complete while {} depends_on target(s) are unfinished: {listed}",
            blockers.len()
        ),
    ))
}

pub fn lifecycle_transitions() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([
        ("incubating", vec!["active", "archived"]),
        ("active", vec!["maintenance", "archived", "incubating"]),
        ("maintenance", vec!["active", "archived"]),
        ("archived", vec!["active", "maintenance"]),
    ])
}

pub fn check_lifecycle_transition(from_state: &str, to_state: &str) -> Result<(), VogtError> {
    if from_state == to_state {
        return Err(transition_rejected(
            "lifecycle.no_op",
            &format!("project is already '{to_state}'"),
        ));
    }
    let table = lifecycle_transitions();
    let allowed = table.get(from_state).map(Vec::as_slice).unwrap_or(&[]);
    if !allowed.contains(&to_state) {
        let text = if allowed.is_empty() {
            "nothing".to_string()
        } else {
            allowed.join(", ")
        };
        return Err(transition_rejected(
            "lifecycle.not_allowed",
            &format!("no {from_state} -> {to_state} edge (allowed from {from_state}: {text})"),
        ));
    }
    Ok(())
}

/// Declared-store shapes. Ports `src/vogt/core/entities.py`.
///
/// Closed structs stand in for pydantic's `extra="forbid"`. Timestamps are the
/// `Moment` above. Maps are `serde_json::Value` because the Python side stores
/// `dict[str, object]`. No model carries a secret: `Token` has no hash and
/// `ForgeAccount` has no token, matching the Python models.
pub const DECLARABLE_RELATION_KINDS: &[&str] =
    &["depends_on", "relates_to", "duplicate_of", "parent_of"];

pub fn require_text(value: &str) -> Result<String, String> {
    let stripped = value.trim();
    if stripped.is_empty() {
        Err("must not be blank".to_string())
    } else {
        Ok(stripped.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_ascii_state_survives_its_own_definition() {
        let workflow = Workflow {
            kind: "bug".parse().unwrap(),
            initial_state: "open".into(),
            transitions: vec![
                ("open".into(), vec!["été".into()]),
                ("été".into(), vec!["open".into()]),
            ],
        };
        let reloaded =
            Workflow::from_definition_json("bug", &workflow.to_definition_json()).unwrap();
        assert_eq!(reloaded, workflow);
    }

    #[test]
    fn a_definition_round_trips_and_a_mapless_one_is_refused() {
        let workflow = default_workflow("bug");
        let stored = workflow.to_definition();
        let restored = Workflow::from_definition("bug", &stored).unwrap();
        assert_eq!(restored, workflow);

        let skipped = serde_json::json!({"transitions": {"open": ["done"], "bad": "nope"}});
        let partial = Workflow::from_definition("bug", &skipped).unwrap();
        assert_eq!(
            partial.transitions,
            vec![("open".to_string(), vec!["done".to_string()])]
        );
        assert_eq!(partial.initial_state, "open");

        let err = Workflow::from_definition("bug", &serde_json::json!({})).unwrap_err();
        assert!(err.contains("has no transitions map"), "{err}");
    }

    #[test]
    fn the_default_machine_rejects_open_to_done_with_a_path() {
        let workflow = default_workflow("bug");
        assert_eq!(workflow.initial_state, "open");
        assert_eq!(
            workflow.states(),
            [
                "open",
                "in_progress",
                "blocked",
                "wont_do",
                "review",
                "done",
            ]
        );
        let err = workflow.check("open", "done").unwrap_err();
        assert_eq!(err.code(), "transition_rejected");
        assert_eq!(err.http_status(), 409);
        let message = err.message();
        assert!(message.contains("transition.not_allowed"), "{message}");
        assert!(message.contains("open -> done"), "{message}");
        assert!(message.contains("in_progress"), "{message}");
        assert!(workflow
            .check("open", "nope")
            .unwrap_err()
            .message()
            .contains("unknown_state"));
        assert!(workflow
            .check("open", "open")
            .unwrap_err()
            .message()
            .contains("no_op"));
        assert!(workflow.check("open", "in_progress").is_ok());
    }

    #[test]
    fn completion_names_the_blockers() {
        let err = check_completion_allowed("done", &[("WI-2", "open")]).unwrap_err();
        assert!(
            err.message().contains("transition.blocked_by_dependency"),
            "{}",
            err.message()
        );
        assert!(err.message().contains("WI-2 (open)"), "{}", err.message());
        assert!(check_completion_allowed("done", &[]).is_ok());
        assert!(check_completion_allowed("wont_do", &[("WI-2", "open")]).is_ok());
    }

    #[test]
    fn lifecycle_edges() {
        assert!(check_lifecycle_transition("incubating", "active").is_ok());
        let err = check_lifecycle_transition("incubating", "maintenance").unwrap_err();
        assert!(
            err.message().contains("lifecycle.not_allowed"),
            "{}",
            err.message()
        );
        assert!(check_lifecycle_transition("archived", "active").is_ok());
    }
}
