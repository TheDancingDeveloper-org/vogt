//! Cache-aware compaction on a cold wake (WI-2037). Ports
//! `src/vogt/core/cold_wake.py`.
//!
//! A hibernated session woken after its provider's prompt-cache lifetime pays
//! the full input price to re-read its transcript. Past a size, a short
//! structured summary is cheaper to start from. This module decides whether a
//! wake compacts and renders the summary. It is pure: the caller supplies the
//! clock, the session's last reply and the work item's hand-off.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::{Comment, Moment};

/// The agent CLIs a compacted wake can start fresh under a new conversation id.
const COMPACTABLE_AGENTS: [&str; 3] = ["claude", "klaudia", "codex"];

/// The agent CLI's default provider, when the model name does not say.
fn agent_provider(agent: &str) -> Option<&'static str> {
    match agent {
        "claude" | "klaudia" => Some("anthropic"),
        "codex" => Some("openai"),
        _ => None,
    }
}

/// Model-name fragments that name a provider, checked in order.
const MODEL_PROVIDER: [(&str, &str); 11] = [
    ("deepseek", "deepseek"),
    ("claude", "anthropic"),
    ("opus", "anthropic"),
    ("sonnet", "anthropic"),
    ("haiku", "anthropic"),
    ("fable", "anthropic"),
    ("gemini", "google"),
    ("gpt", "openai"),
    ("grok", "xai"),
    ("glm", "zhipu"),
    ("qwen", "qwen"),
];

/// How much of the last reply and the hand-off body the summary carries.
const REPLY_CAP: usize = 6_000;
const HANDOFF_CAP: usize = 4_000;

/// Whether a wake compacts, and the facts the audit record keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub outcome: &'static str,
    pub provider: Option<String>,
    pub ttl_seconds: Option<i64>,
    pub idle_seconds: Option<i64>,
    pub context_tokens: Option<i64>,
    pub threshold_tokens: Option<i64>,
}

impl Decision {
    fn bare(outcome: &'static str) -> Self {
        Self {
            outcome,
            provider: None,
            ttl_seconds: None,
            idle_seconds: None,
            context_tokens: None,
            threshold_tokens: None,
        }
    }

    pub fn compact(&self) -> bool {
        self.outcome == "compact"
    }

    /// The audit record's `cold_wake` field, keys in Python's order.
    pub fn audit(&self) -> Value {
        json!({
            "outcome": self.outcome,
            "provider": self.provider,
            "ttl_seconds": self.ttl_seconds,
            "idle_seconds": self.idle_seconds,
            "context_tokens": self.context_tokens,
            "threshold_tokens": self.threshold_tokens,
        })
    }
}

/// The provider whose cache a session's conversation sits in.
pub fn provider_of(agent: Option<&str>, model: Option<&str>) -> Option<String> {
    let name = model.unwrap_or("").to_lowercase();
    for (fragment, provider) in MODEL_PROVIDER {
        if name.contains(fragment) {
            return Some(provider.to_string());
        }
    }
    agent_provider(agent.unwrap_or("")).map(str::to_string)
}

/// What `decide` reads about the wake.
pub struct Inputs<'a> {
    pub enabled: bool,
    pub opted_out: bool,
    pub agent: Option<&'a str>,
    pub model: Option<&'a str>,
    pub hibernated_at: Option<Moment>,
    pub now: Moment,
    pub context_tokens: Option<i64>,
    pub ttl_seconds: &'a BTreeMap<String, i64>,
    pub threshold_tokens: i64,
}

/// Whether waking this session should start fresh from a summary.
///
/// Compacts only when every condition holds: the feature is on, the session
/// has not opted out, its agent can start a fresh conversation, its provider's
/// cache lifetime is configured and has passed since it was hibernated, and
/// its last measured context is at or over the threshold.
pub fn decide(input: &Inputs<'_>) -> Decision {
    if !input.enabled {
        return Decision::bare("disabled");
    }
    if input.opted_out {
        return Decision::bare("opted-out");
    }
    if !input
        .agent
        .is_some_and(|agent| COMPACTABLE_AGENTS.contains(&agent))
    {
        return Decision::bare("agent");
    }
    let provider = provider_of(input.agent, input.model);
    let ttl = provider
        .as_ref()
        .and_then(|name| input.ttl_seconds.get(name))
        .copied();
    let Some(ttl) = ttl else {
        return Decision {
            provider,
            ..Decision::bare("provider")
        };
    };
    let idle = input
        .hibernated_at
        .map(|at| input.now.seconds_since(at) as i64);
    let facts = |outcome| Decision {
        outcome,
        provider: provider.clone(),
        ttl_seconds: Some(ttl),
        idle_seconds: idle,
        context_tokens: input.context_tokens,
        threshold_tokens: Some(input.threshold_tokens),
    };
    if idle.is_none_or(|idle| idle <= ttl) {
        return facts("warm");
    }
    let Some(tokens) = input.context_tokens else {
        return facts("unmeasured");
    };
    if tokens < input.threshold_tokens {
        return facts("small");
    }
    facts("compact")
}

fn cap(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{}\n\n[truncated]", head.trim_end())
}

fn bullets(items: &[String]) -> String {
    items
        .iter()
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn field_text(fields: &Map<String, Value>, key: &str) -> Option<String> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn field_list(fields: &Map<String, Value>, key: &str) -> Vec<String> {
    fields
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// What `render_summary` is written from.
pub struct SummaryInputs<'a> {
    pub session_id: &'a str,
    pub work_item: Option<&'a str>,
    pub work_item_title: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub last_reply: Option<&'a str>,
    pub handoff: Option<&'a Comment>,
    pub decision: &'a Decision,
}

fn opt<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "None".to_string(), |v| v.to_string())
}

/// The structured summary a compacted wake starts from: markdown, in the order
/// a fresh agent needs it — task, state, decisions, files and branch, next
/// step. The engine appends the git facts and the original brief after it.
pub fn render_summary(input: &SummaryInputs<'_>) -> String {
    let empty = Map::new();
    let fields = input
        .handoff
        .and_then(|comment| comment.handoff.as_ref())
        .and_then(Value::as_object);
    let facts = fields.unwrap_or(&empty);
    let decision = input.decision;
    let mut out: Vec<String> = vec![
        "# Resumed from a summary (cold wake)".into(),
        String::new(),
        format!(
            "This session ({}) was hibernated for {}s, past its {} prompt-cache \
             lifetime ({}s), with about {} tokens of context. Rather than re-read that \
             transcript at full price, Vogt started a fresh conversation from this \
             summary. The earlier transcript is not in your context. Re-read files \
             before you change them; do not assume what they hold.",
            input.session_id,
            opt(decision.idle_seconds),
            opt(decision.provider.as_deref()),
            opt(decision.ttl_seconds),
            opt(decision.context_tokens),
        ),
        String::new(),
        "## Task".into(),
        String::new(),
    ];
    if let Some(work_item) = input.work_item.filter(|text| !text.is_empty()) {
        let title = input
            .work_item_title
            .filter(|text| !text.is_empty())
            .map(|title| format!(" — {title}"))
            .unwrap_or_default();
        out.push(format!("Work item {work_item}{title}."));
    }
    let reason = input.reason.filter(|text| !text.is_empty());
    if let Some(reason) = reason {
        out.push(format!("Session started for: {}", reason.trim()));
    }
    if input.work_item.is_none_or(str::is_empty) && reason.is_none() {
        out.push("(no task recorded; see the original brief below)".into());
    }
    out.extend([
        String::new(),
        "## State (your last reply before hibernation)".into(),
        String::new(),
    ]);
    match input.last_reply.filter(|text| !text.trim().is_empty()) {
        Some(reply) => out.push(cap(reply, REPLY_CAP)),
        None => out.push("(no reply recorded)".into()),
    }
    out.extend([
        String::new(),
        "## Decisions and hand-off".into(),
        String::new(),
    ]);
    if let Some(handoff) = input.handoff {
        out.push(format!(
            "Latest hand-off on the work item ({}):",
            handoff.created_at.to_iso()
        ));
        out.push(String::new());
        let body = handoff.body.trim();
        out.push(if body.is_empty() {
            "(no body)".into()
        } else {
            cap(body, HANDOFF_CAP)
        });
        let done = field_list(facts, "done");
        if !done.is_empty() {
            out.extend([String::new(), "Done:".into(), bullets(&done)]);
        }
        let blockers = field_list(facts, "blockers");
        if !blockers.is_empty() {
            out.extend([String::new(), "Blockers:".into(), bullets(&blockers)]);
        }
    } else {
        out.push("(no hand-off recorded on the work item)".into());
    }
    out.extend([String::new(), "## Files and branch".into(), String::new()]);
    let mut lines: Vec<String> = Vec::new();
    if let Some(branch) = field_text(facts, "branch") {
        lines.push(format!("branch: {branch}"));
    }
    if let Some(head) = field_text(facts, "head_sha") {
        lines.push(format!("head: {head}"));
    }
    if let Some(base) = field_text(facts, "base") {
        lines.push(format!("base: {base}"));
    }
    lines.extend(
        field_list(facts, "touch_points")
            .into_iter()
            .map(|path| format!("touches: {path}")),
    );
    out.push(if lines.is_empty() {
        "See the git facts of the session directory below.".into()
    } else {
        bullets(&lines)
    });
    out.extend([String::new(), "## Next step".into(), String::new()]);
    let remaining = field_list(facts, "remaining");
    out.push(if remaining.is_empty() {
        "Continue from the state above. If it is unclear, re-read the original brief \
         below and check the branch before acting."
            .into()
    } else {
        bullets(&remaining)
    });
    out.push(String::new());
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Moment {
        Moment::from_unix(seconds, 0)
    }

    fn ttl() -> BTreeMap<String, i64> {
        BTreeMap::from([("anthropic".to_string(), 300)])
    }

    fn inputs<'a>(ttl: &'a BTreeMap<String, i64>) -> Inputs<'a> {
        Inputs {
            enabled: true,
            opted_out: false,
            agent: Some("claude"),
            model: Some("sonnet"),
            hibernated_at: Some(at(1_000)),
            now: at(2_000),
            context_tokens: Some(180_000),
            ttl_seconds: ttl,
            threshold_tokens: 100_000,
        }
    }

    #[test]
    fn compacts_only_when_every_condition_holds() {
        let ttl = ttl();
        let decision = decide(&inputs(&ttl));
        assert!(decision.compact());
        assert_eq!(decision.idle_seconds, Some(1_000));
        assert_eq!(decision.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn each_gate_names_its_outcome() {
        let ttl = ttl();
        let off = Inputs {
            enabled: false,
            ..inputs(&ttl)
        };
        assert_eq!(decide(&off), Decision::bare("disabled"));
        let out = Inputs {
            opted_out: true,
            ..inputs(&ttl)
        };
        assert_eq!(decide(&out).outcome, "opted-out");
        let shell = Inputs {
            agent: Some("opencode"),
            ..inputs(&ttl)
        };
        assert_eq!(decide(&shell).outcome, "agent");
        let unknown = Inputs {
            model: Some("gpt-5"),
            ..inputs(&ttl)
        };
        let decision = decide(&unknown);
        assert_eq!(decision.outcome, "provider");
        assert_eq!(decision.provider.as_deref(), Some("openai"));
        let warm = Inputs {
            now: at(1_300),
            ..inputs(&ttl)
        };
        assert_eq!(decide(&warm).outcome, "warm");
        let unmeasured = Inputs {
            context_tokens: None,
            ..inputs(&ttl)
        };
        assert_eq!(decide(&unmeasured).outcome, "unmeasured");
        let small = Inputs {
            context_tokens: Some(99_999),
            ..inputs(&ttl)
        };
        assert_eq!(decide(&small).outcome, "small");
        let never_hibernated = Inputs {
            hibernated_at: None,
            ..inputs(&ttl)
        };
        assert_eq!(decide(&never_hibernated).outcome, "warm");
    }

    #[test]
    fn audit_keeps_pythons_key_order() {
        let ttl = ttl();
        let audit = decide(&inputs(&ttl)).audit();
        let keys: Vec<&str> = audit
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "outcome",
                "provider",
                "ttl_seconds",
                "idle_seconds",
                "context_tokens",
                "threshold_tokens"
            ]
        );
    }

    #[test]
    fn model_name_beats_the_agent_default() {
        assert_eq!(
            provider_of(Some("claude"), Some("deepseek-v4")).as_deref(),
            Some("deepseek")
        );
        assert_eq!(provider_of(Some("codex"), None).as_deref(), Some("openai"));
        assert_eq!(provider_of(Some("opencode"), None), None);
    }

    #[test]
    fn summary_without_a_hand_off_says_so() {
        let ttl = ttl();
        let decision = decide(&inputs(&ttl));
        let text = render_summary(&SummaryInputs {
            session_id: "ses_1",
            work_item: Some("WI-7"),
            work_item_title: Some("Fix it"),
            reason: Some(" do the thing "),
            last_reply: Some("  halfway  "),
            handoff: None,
            decision: &decision,
        });
        assert!(text.starts_with("# Resumed from a summary (cold wake)\n"));
        assert!(text.contains("hibernated for 1000s, past its anthropic prompt-cache"));
        assert!(text.contains("Work item WI-7 — Fix it.\nSession started for: do the thing"));
        assert!(text.contains("\nhalfway\n"));
        assert!(text.contains("(no hand-off recorded on the work item)"));
        assert!(text.contains("See the git facts of the session directory below."));
        assert!(text.ends_with("before acting.\n"));
    }

    #[test]
    fn a_long_reply_is_truncated() {
        let long = "x".repeat(REPLY_CAP + 10);
        let capped = cap(&long, REPLY_CAP);
        assert!(capped.ends_with("\n\n[truncated]"));
        assert_eq!(capped.chars().count(), REPLY_CAP + "\n\n[truncated]".len());
    }
}
