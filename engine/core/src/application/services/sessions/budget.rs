use super::*;

/// The limits a start carries (Python `SessionBudgetParams`). Each field is one
/// limit; a field left out is not set.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionBudget {
    pub turns: Option<i64>,
    pub context_tokens: Option<i64>,
    pub wall_clock_minutes: Option<i64>,
    pub session_wall_clock_minutes: Option<i64>,
}

/// The engine's `budget` object: the fields that are set, and `{}` for a
/// budget that names no limit, which the engine reads as "no limits".
pub(super) fn budget_wire(budget: &SessionBudget) -> Value {
    let mut wire = serde_json::Map::new();
    for (key, value) in [
        ("turns", budget.turns),
        ("context_tokens", budget.context_tokens),
        ("wall_clock_minutes", budget.wall_clock_minutes),
        (
            "session_wall_clock_minutes",
            budget.session_wall_clock_minutes,
        ),
    ] {
        if let Some(value) = value {
            wire.insert(key.to_string(), json!(value));
        }
    }
    Value::Object(wire)
}

/// Python's `_budget_params`: the engine's budget object as a summary carries
/// it. Only positive limits survive (zero is not a limit), and a row that
/// yields none is no budget rather than an empty one (WI-2026).
pub(crate) fn budget_of_engine(
    raw: Option<&serde_json::Map<String, Value>>,
) -> Option<SessionBudget> {
    let raw = raw?;
    let limit = |key: &str| {
        raw.get(key)
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
    };
    let budget = SessionBudget {
        turns: limit("turns"),
        context_tokens: limit("context_tokens"),
        wall_clock_minutes: limit("wall_clock_minutes"),
        session_wall_clock_minutes: limit("session_wall_clock_minutes"),
    };
    (budget != SessionBudget::default()).then_some(budget)
}

/// Default overseer context ceiling (`ENGINE_OVERSIGHT_CONTEXT_TOKENS`).
const DEFAULT_OVERSIGHT_CONTEXT_TOKENS: i64 = 200_000;

fn oversight_context_tokens() -> i64 {
    std::env::var("ENGINE_OVERSIGHT_CONTEXT_TOKENS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_OVERSIGHT_CONTEXT_TOKENS)
}

pub(super) fn effective_budget(
    params: &StartSessionParams,
    policy: &crate::core::AgentPolicy,
) -> Option<SessionBudget> {
    if let Some(budget) = &params.budget {
        return Some(budget.clone());
    }
    let has_task = params
        .task
        .as_deref()
        .is_some_and(|task| !task.trim().is_empty());
    // An overseer re-reads its whole history each sweep: bound its context so
    // the engine compacts it (WI-1522). No turn or clock limit.
    if params.template.is_some() && has_task && params.role == "oversight" {
        return Some(SessionBudget {
            turns: None,
            context_tokens: Some(oversight_context_tokens()),
            wall_clock_minutes: None,
            session_wall_clock_minutes: None,
        });
    }
    if params.template.is_some() && has_task && params.role == "worker" {
        return Some(SessionBudget {
            turns: Some(120),
            context_tokens: None,
            wall_clock_minutes: Some(90),
            session_wall_clock_minutes: policy.session_wall_clock_minutes,
        });
    }
    None
}
