//! Decision-bearing core. Ports `ranking.py`, `digest.py`, `checks.py` and the
//! pure half of `observed.py`.
//!
//! Contract and drift stay for the next chunk: both are large and their pure
//! tests need fixtures this file does not have yet. Every explanation string
//! below is copied from the Python, because `why` compares them. The binary
//! does not call this yet.

#![allow(dead_code)]

use std::collections::BTreeSet;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::{Moment, Observation, WorkItem, WorkOverlay, DONE, TERMINAL_STATES};

/// `json.dumps(sort_keys=True, separators=(",", ":"), ensure_ascii=True)`.
///
/// serde_json writes non-ASCII raw and renders floats in a different form, so
/// a digest computed here would not match one Python stored. Non-ASCII becomes
/// `\uXXXX`, with a surrogate pair above U+FFFF. Floats use Python's
/// `repr`: six significant digits, switched to scientific notation the way
/// CPython does.
pub fn canonical_json(payload: &Value) -> String {
    let mut out = String::new();
    write_python_json(&mut out, payload);
    out
}

fn write_python_json(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&python_number(number)),
        Value::String(text) => write_python_string(out, text),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_python_json(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_python_string(out, key);
                out.push(':');
                write_python_json(out, &map[*key]);
            }
            out.push('}');
        }
    }
}

fn write_python_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 || (ch as u32) > 0x7F => push_escaped(out, ch),
            ch => out.push(ch),
        }
    }
    out.push('"');
}

fn push_escaped(out: &mut String, ch: char) {
    let code = ch as u32;
    if code <= 0xFFFF {
        out.push_str(&format!("\\u{code:04x}"));
    } else {
        let adjusted = code - 0x10000;
        let high = 0xD800 + (adjusted >> 10);
        let low = 0xDC00 + (adjusted & 0x3FF);
        out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
    }
}

fn python_number(number: &serde_json::Number) -> String {
    if let Some(value) = number.as_i64() {
        return value.to_string();
    }
    if let Some(value) = number.as_u64() {
        return value.to_string();
    }
    let Some(value) = number.as_f64() else {
        return "null".to_string();
    };
    if value.is_nan() || value.is_infinite() {
        return "null".to_string();
    }
    let repr = format!("{value:.16}");
    let trimmed = trim_float(&repr);
    let scientific = format!("{value:.16e}");
    // CPython picks the shorter form, breaking ties toward the plain one.
    if scientific.len() < trimmed.len() {
        scientific_python(&scientific)
    } else {
        trimmed
    }
}

fn trim_float(rendered: &str) -> String {
    if !rendered.contains('.') {
        return format!("{rendered}.0");
    }
    let trimmed = rendered.trim_end_matches('0');
    if trimmed.ends_with('.') {
        format!("{trimmed}0")
    } else {
        trimmed.to_string()
    }
}

fn scientific_python(rendered: &str) -> String {
    // `1.0000000000000000e-05` -> `1e-05`, keeping the sign and two-digit exponent.
    let (mantissa, exponent) = rendered.split_once('e').unwrap_or((rendered, "+00"));
    format!(
        "{}e{}",
        trim_float(mantissa).trim_end_matches(".0"),
        exponent
    )
}

pub fn digest_of(payload: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_json(payload).as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

pub const PASSING_CONCLUSIONS: &[&str] = &["success", "skipped"];

pub struct CheckRollup {
    pub revision: String,
    pub checks: Vec<Observation>,
    pub failing: Vec<String>,
    pub revisions_observed: usize,
    pub earlier_failures: usize,
}

impl CheckRollup {
    pub fn status(&self) -> &'static str {
        if self.failing.is_empty() {
            "passing"
        } else {
            "failing"
        }
    }
}

fn ran_at(check: &Observation) -> (String, Moment) {
    let stamp = match check.payload.get("updated_at") {
        Some(Value::String(stamp)) => stamp.clone(),
        _ => String::new(),
    };
    (stamp, check.observed_at)
}

fn revision_of(check: &Observation) -> String {
    match check.payload.get("revision") {
        Some(Value::String(revision)) => revision.clone(),
        _ => String::new(),
    }
}

fn is_failing(check: &Observation) -> bool {
    match check.payload.get("conclusion") {
        None | Some(Value::Null) => false,
        Some(Value::String(conclusion)) => !PASSING_CONCLUSIONS.contains(&conclusion.as_str()),
        Some(_) => true,
    }
}

/// `None` means nobody has looked, which is not "nothing failed".
pub fn roll_up(checks: &[Observation]) -> Option<CheckRollup> {
    let dated: Vec<&Observation> = checks
        .iter()
        .filter(|check| !revision_of(check).is_empty())
        .collect();
    if dated.is_empty() {
        return None;
    }
    let newest = dated
        .iter()
        .max_by_key(|check| ran_at(check))
        .expect("dated is non-empty");
    let revision = revision_of(newest);
    let on_revision: Vec<Observation> = dated
        .iter()
        .filter(|check| revision_of(check) == revision)
        .map(|check| (*check).clone())
        .collect();
    let failing: BTreeSet<String> = on_revision
        .iter()
        .filter(|check| is_failing(check))
        .map(|check| match check.payload.get("check") {
            Some(Value::String(name)) => name.clone(),
            _ => "?".to_string(),
        })
        .collect();
    let revisions: BTreeSet<String> = dated.iter().map(|check| revision_of(check)).collect();
    let earlier = dated
        .iter()
        .filter(|check| revision_of(check) != revision && is_failing(check))
        .count();
    Some(CheckRollup {
        revision,
        checks: on_revision,
        failing: failing.into_iter().collect(),
        revisions_observed: revisions.len(),
        earlier_failures: earlier,
    })
}

pub const PRIORITY_POINTS: &[(&str, f64)] = &[
    ("p0", 100.0),
    ("p1", 55.0),
    ("p2", 25.0),
    ("p3", 8.0),
    ("p4", 0.0),
];
pub const STALENESS_POINTS_PER_DAY: f64 = 0.5;
pub const STALENESS_CAP_DAYS: f64 = 60.0;
pub const BLOCKING_FAN_OUT_POINTS: f64 = 8.0;
pub const INITIATIVE_WEIGHT_FACTOR: f64 = 0.25;
pub const OPEN_PR_POINTS: f64 = 12.0;
pub const BRANCH_ACTIVITY_POINTS: f64 = 8.0;
pub const BRANCH_ACTIVITY_WINDOW_DAYS: f64 = 14.0;
pub const TERMINAL_PENALTY: f64 = -1000.0;
pub const SCORE_PRECISION: i32 = 4;

fn priority_points(priority: &str) -> f64 {
    PRIORITY_POINTS
        .iter()
        .find(|(name, _)| *name == priority)
        .map(|(_, points)| *points)
        .unwrap_or(0.0)
}

fn trust_penalty(state: &str) -> f64 {
    match state {
        "verified" => 0.0,
        "stale" => -2.0,
        "disputed" => -8.0,
        _ => -3.0,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Contribution {
    pub input: String,
    pub detail: String,
    pub value: f64,
    pub weight: f64,
    pub contribution: f64,
}

impl Contribution {
    fn of(input: &str, detail: String, value: f64, weight: f64, contribution: f64) -> Self {
        Self {
            input: input.to_string(),
            detail,
            value: round_to(value, SCORE_PRECISION),
            weight,
            contribution: round_to(contribution, SCORE_PRECISION),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub subject_id: String,
    pub reference: String,
    pub total: f64,
    pub contributions: Vec<Contribution>,
}

pub struct RankingInputs {
    pub now: Moment,
    pub blocking_fan_out: i64,
    pub initiative_weight: i64,
    pub is_terminal: bool,
    pub open_pr: bool,
    pub branch_activity_seconds: Option<i64>,
}

impl RankingInputs {
    pub fn at(now: Moment) -> Self {
        Self {
            now,
            blocking_fan_out: 0,
            initiative_weight: 0,
            is_terminal: false,
            open_pr: false,
            branch_activity_seconds: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Rankable {
    pub id: String,
    pub reference: String,
    pub priority: String,
    pub updated_at: Moment,
    pub trust_state: String,
    pub state: String,
    pub has_initiative: bool,
}

impl Rankable {
    pub fn from_work_item(item: &WorkItem) -> Self {
        Self {
            id: item.id.clone(),
            reference: item.reference.clone(),
            priority: item.priority.clone(),
            updated_at: item.updated_at,
            trust_state: item.trust_state.clone(),
            state: item.state.clone(),
            has_initiative: item.initiative_id.is_some(),
        }
    }
}

fn round_to(value: f64, places: i32) -> f64 {
    let factor = 10_f64.powi(places);
    (value * factor).round() / factor
}

pub fn score_item(item: &Rankable, inputs: &RankingInputs) -> Score {
    let mut contributions = Vec::new();
    let priority = priority_points(&item.priority);
    contributions.push(Contribution::of(
        "priority",
        item.priority.clone(),
        1.0,
        priority,
        priority,
    ));

    let age_days = ((inputs.now.seconds_since(item.updated_at)) as f64 / 86_400.0).max(0.0);
    let capped = age_days.min(STALENESS_CAP_DAYS);
    let staleness = capped * STALENESS_POINTS_PER_DAY;
    let mut detail = format!("{age_days:.1} days since last change");
    if age_days > capped {
        detail.push_str(&format!(", capped at {STALENESS_CAP_DAYS}"));
    }
    contributions.push(Contribution::of(
        "staleness",
        detail,
        round_to(capped, 3),
        STALENESS_POINTS_PER_DAY,
        staleness,
    ));

    let fan_out = inputs.blocking_fan_out as f64 * BLOCKING_FAN_OUT_POINTS;
    contributions.push(Contribution::of(
        "blocking_fan_out",
        format!("{} item(s) depend on this one", inputs.blocking_fan_out),
        inputs.blocking_fan_out as f64,
        BLOCKING_FAN_OUT_POINTS,
        fan_out,
    ));

    let initiative = inputs.initiative_weight as f64 * INITIATIVE_WEIGHT_FACTOR;
    contributions.push(Contribution::of(
        "initiative_weight",
        if item.has_initiative {
            format!("initiative weight {}", inputs.initiative_weight)
        } else {
            "no initiative".to_string()
        },
        inputs.initiative_weight as f64,
        INITIATIVE_WEIGHT_FACTOR,
        initiative,
    ));

    let trust = trust_penalty(&item.trust_state);
    contributions.push(Contribution::of(
        "trust_penalty",
        format!("trust state {}", item.trust_state),
        1.0,
        trust,
        trust,
    ));

    let open_pr = if inputs.open_pr { OPEN_PR_POINTS } else { 0.0 };
    contributions.push(Contribution::of(
        "open_pr",
        if inputs.open_pr {
            "an open pull request implements this"
        } else {
            "no open pull request"
        }
        .to_string(),
        if inputs.open_pr { 1.0 } else { 0.0 },
        OPEN_PR_POINTS,
        open_pr,
    ));

    let fraction = branch_activity_fraction(inputs.branch_activity_seconds);
    contributions.push(Contribution::of(
        "branch_activity",
        match inputs.branch_activity_seconds {
            Some(_) => branch_activity_detail(inputs.branch_activity_seconds),
            None => "no branch observed".to_string(),
        },
        round_to(fraction, 3),
        BRANCH_ACTIVITY_POINTS,
        fraction * BRANCH_ACTIVITY_POINTS,
    ));

    if inputs.is_terminal {
        contributions.push(Contribution::of(
            "terminal_state",
            format!(
                "state {} is terminal; excluded from ranked views",
                item.state
            ),
            1.0,
            TERMINAL_PENALTY,
            TERMINAL_PENALTY,
        ));
    }

    let total = round_to(
        contributions.iter().map(|entry| entry.contribution).sum(),
        SCORE_PRECISION,
    );
    Score {
        subject_id: item.id.clone(),
        reference: item.reference.clone(),
        total,
        contributions,
    }
}

fn branch_activity_fraction(seconds: Option<i64>) -> f64 {
    let Some(seconds) = seconds else { return 0.0 };
    let days = (seconds as f64 / 86_400.0).max(0.0);
    (1.0 - days / BRANCH_ACTIVITY_WINDOW_DAYS).max(0.0)
}

fn branch_activity_detail(seconds: Option<i64>) -> String {
    let Some(seconds) = seconds else {
        return "no branch observed".to_string();
    };
    let days = (seconds as f64 / 86_400.0).max(0.0);
    if days >= BRANCH_ACTIVITY_WINDOW_DAYS {
        format!("branch last active {days:.1}d ago, outside the activity window")
    } else {
        format!("branch active {days:.1}d ago")
    }
}

pub fn rank(mut scores: Vec<Score>) -> Vec<Score> {
    scores.sort_by(|left, right| {
        right
            .total
            .partial_cmp(&left.total)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.reference.cmp(&right.reference))
    });
    scores
}

pub const PENDING_INPUTS: &[(&str, &str)] = &[(
    "ci_red_boost",
    "arrives with the CI check observations it reads",
)];

pub const WORKLIKE_KINDS: &[&str] = &["forge.issue", "forge.pull_request", "marker"];
pub const BUG_LABELS: &[&str] = &["bug", "defect", "regression", "crash"];
pub const BUG_TAGS: &[&str] = &["FIXME", "HACK", "XXX"];

pub fn work_kind_of(observation: &Observation) -> &'static str {
    if observation.kind == "marker" {
        let tag = observation
            .payload
            .get("tag")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_uppercase();
        return if BUG_TAGS.contains(&tag.as_str()) {
            "bug"
        } else {
            "chore"
        };
    }
    if observation.kind == "forge.pull_request" {
        return "chore";
    }
    let labels = labels_of(observation);
    if labels
        .iter()
        .any(|label| BUG_LABELS.contains(&label.to_lowercase().as_str()))
    {
        "bug"
    } else {
        "feature"
    }
}

pub fn lifecycle_of(observation: &Observation) -> &'static str {
    if observation.kind == "marker" {
        return "open";
    }
    let Some(state) = observation.payload.get("state").and_then(Value::as_str) else {
        return "unknown";
    };
    match state.trim().to_lowercase().as_str() {
        "open" => "open",
        "closed" | "merged" => "closed",
        _ => "unknown",
    }
}

pub fn upstream_state(
    observation: &Observation,
    overlay: Option<&WorkOverlay>,
    initial_state: &str,
) -> String {
    if lifecycle_of(observation) == "closed" {
        if let Some(overlay) = overlay {
            if let Some(state) = &overlay.workflow_state {
                if TERMINAL_STATES.contains(&state.as_str()) {
                    return state.clone();
                }
            }
        }
        return DONE.to_string();
    }
    if let Some(overlay) = overlay {
        if let Some(state) = &overlay.workflow_state {
            if !state.is_empty() {
                return state.clone();
            }
        }
    }
    initial_state.to_string()
}

pub fn is_worklike(observation: &Observation) -> bool {
    if !WORKLIKE_KINDS.contains(&observation.kind.as_str()) {
        return false;
    }
    if observation.kind == "marker" {
        return observation.promoted;
    }
    true
}

pub fn priority_of(observation: &Observation) -> &'static str {
    for label in labels_of(observation) {
        let candidate = label.trim().to_lowercase();
        if ["p0", "p1", "p2", "p3", "p4"].contains(&candidate.as_str()) {
            return match candidate.as_str() {
                "p0" => "p0",
                "p1" => "p1",
                "p2" => "p2",
                "p3" => "p3",
                _ => "p4",
            };
        }
    }
    if observation.kind == "marker" {
        "p3"
    } else {
        "p2"
    }
}

fn labels_of(observation: &Observation) -> Vec<String> {
    match observation.payload.get("labels") {
        Some(Value::Array(labels)) => labels
            .iter()
            .map(|label| label.to_string().trim_matches('"').to_string())
            .collect(),
        _ => Vec::new(),
    }
}

pub const DEFAULT_CONTRACT_VERSION: &str = "v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    pub version: String,
    pub required_files: Vec<String>,
    pub required_dirs: Vec<String>,
    pub required_meta: Vec<String>,
}

impl Contract {
    pub fn stock() -> Self {
        Self {
            version: DEFAULT_CONTRACT_VERSION.to_string(),
            required_files: vec!["AGENTS.md".into(), "README.md".into(), "LICENSE".into()],
            required_dirs: vec!["docs".into(), "design".into(), "src".into()],
            required_meta: vec!["name".into(), "lifecycle_state".into(), "owner".into()],
        }
    }
}

pub fn rules_digest(contract: &Contract) -> String {
    let material = [
        &contract.required_files,
        &contract.required_dirs,
        &contract.required_meta,
    ]
    .iter()
    .map(|part| {
        let mut sorted: Vec<&str> = part.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        sorted.join(",")
    })
    .collect::<Vec<_>>()
    .join("|");
    let mut hasher = Sha256::new();
    hasher.update(material.as_bytes());
    format!("{:x}", hasher.finalize())[..6].to_string()
}

/// A contract whose rules differ from stock while still carrying the default
/// version gets `v1+<digest>`. A named version is kept as given.
pub fn contract_from_settings(
    version: &str,
    required_files: &[&str],
    required_dirs: &[&str],
    required_meta: &[&str],
) -> Contract {
    let contract = Contract {
        version: version.to_string(),
        required_files: required_files.iter().map(|s| s.to_string()).collect(),
        required_dirs: required_dirs.iter().map(|s| s.to_string()).collect(),
        required_meta: required_meta.iter().map(|s| s.to_string()).collect(),
    };
    let stock = Contract::stock();
    let stock_rules = contract.required_files == stock.required_files
        && contract.required_dirs == stock.required_dirs
        && contract.required_meta == stock.required_meta;
    if stock_rules || version != DEFAULT_CONTRACT_VERSION {
        contract
    } else {
        Contract {
            version: format!("{version}+{}", rules_digest(&contract)),
            ..contract
        }
    }
}

pub const VERSION_MISMATCH: &str = "version_mismatch";
pub const AUTO_ACCEPTABLE_KINDS: &[&str] = &[VERSION_MISMATCH, "forge_state_mismatch"];

pub const HUMAN_GATED_REASON: &[(&str, &str)] = &[
    ("unresolved_dependency", "accepting this asserts the target is not a project; usually it is a project nobody has registered yet"),
    ("vanished_upstream", "accepting this asserts an upstream object is gone for good; a repo transfer or a permissions change looks identical from here"),
    ("ci_red_vs_healthy", "a red build is a fact about the build, not a decision about the project's lifecycle state — somebody has to say which is wrong"),
    ("update_automation_gap", "turning on a security toggle is a change to the repository's settings, not to Vogt's data; accepting only records the judgement"),
    ("broken_path_dependency", "the target is inside this project and is not there; only somebody who knows whether it moved or was deleted can say what to do"),
    ("referenced_issue_state_mismatch", "the reference was read out of the item's own text rather than adopted as a link, and only somebody who knows which register is right can say whether to close the issue or reopen the item"),
    ("initiative_checkbox_drift", "a checkbox was ticked upstream and the member's workflow state was not; only somebody who knows which is right can say whether to move the item or let the next re-render restore the box"),
    ("initiative_tracking_close", "the initiative is closed here; whether its tracking issue should be closed upstream is a person's call — Vogt proposes it, never writes it"),
];

pub fn normalise_version(value: &str) -> String {
    let stripped = value.trim();
    stripped.trim_start_matches(['v', 'V']).to_string()
}

pub fn auto_acceptable(kind: &str) -> bool {
    AUTO_ACCEPTABLE_KINDS.contains(&kind)
}

/// Subject keys a text names, as `gh:owner/repo#number`. Bare `#44` is not a
/// reference: it is the least decidable form, and WI-16's title uses it for a
/// pull request.
pub fn issue_references(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for (owner, repo, number) in find_references(text) {
        let repo = repo.strip_suffix(".git").unwrap_or(repo);
        let key = format!("gh:{owner}/{repo}#{number}");
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

fn find_references(text: &str) -> Vec<(&str, &str, &str)> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if let Some((end, parts)) = match_url(&text[index..]) {
            found.push(parts);
            index += end;
            continue;
        }
        if let Some((end, parts)) = match_qualified(&text[index..]) {
            found.push(parts);
            index += end;
            continue;
        }
        index += text[index..].chars().next().unwrap().len_utf8();
    }
    found
}

fn match_url(text: &str) -> Option<(usize, (&str, &str, &str))> {
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest.strip_prefix("github.com/")?;
    let (owner, rest) = take_name(rest)?;
    let rest = rest.strip_prefix('/')?;
    let (repo, rest) = take_name(rest)?;
    let rest = rest.strip_prefix("/issues/")?;
    let (number, rest) = take_digits(rest)?;
    if owner.is_empty() || repo.is_empty() || number.is_empty() {
        return None;
    }
    let consumed = text.len() - rest.len();
    Some((consumed, (owner, repo, number)))
}

fn match_qualified(text: &str) -> Option<(usize, (&str, &str, &str))> {
    if !text.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return None;
    }
    let (owner, rest) = take_name(text)?;
    let rest = rest.strip_prefix('/')?;
    let (repo, rest) = take_name(rest)?;
    let rest = rest.strip_prefix('#')?;
    let (number, rest) = take_digits(rest)?;
    if number.is_empty() {
        return None;
    }
    let boundary = rest.chars().next();
    if boundary.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((text.len() - rest.len(), (owner, repo, number)))
}

fn take_name(text: &str) -> Option<(&str, &str)> {
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
            end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    (end > 0).then(|| (&text[..end], &text[end..]))
}

fn take_digits(text: &str) -> Option<(&str, &str)> {
    let end = text
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .map(char::len_utf8)
        .sum();
    (end > 0).then(|| (&text[..end], &text[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Moment;

    fn moment(seconds: i64) -> Moment {
        Moment::from_unix(seconds, 0)
    }

    fn item(priority: &str, updated: Moment) -> Rankable {
        Rankable {
            id: "wrk_1".into(),
            reference: "WI-1".into(),
            priority: priority.into(),
            updated_at: updated,
            trust_state: "verified".into(),
            state: "open".into(),
            has_initiative: false,
        }
    }

    #[test]
    fn digests_ignore_key_order_and_notice_content() {
        let left = serde_json::json!({"b": 1, "a": 2});
        let right = serde_json::json!({"a": 2, "b": 1});
        assert_eq!(canonical_json(&left), r#"{"a":2,"b":1}"#);
        assert_eq!(digest_of(&left), digest_of(&right));
        // Pinned against Python json.dumps(ensure_ascii=True): the em dash and
        // the accented e must be \u escapes or the digest diverges.
        let titled = serde_json::json!({"title": "fix — crash é"});
        assert_eq!(
            canonical_json(&titled),
            r#"{"title":"fix \u2014 crash \u00e9"}"#
        );
        assert_eq!(
            digest_of(&titled),
            "sha256:14a825749356e438d4550bc8fee0c6c45cbd801d78ccf96a5432571a2c8790b4"
        );
        assert_ne!(
            digest_of(&left),
            digest_of(&serde_json::json!({"a": 2, "b": 3}))
        );
        assert!(digest_of(&left).starts_with("sha256:"));
    }

    fn check(revision: &str, name: &str, conclusion: Option<&str>, ran_at: &str) -> Observation {
        let mut payload =
            serde_json::json!({"revision": revision, "check": name, "updated_at": ran_at});
        if let Some(conclusion) = conclusion {
            payload["conclusion"] = Value::String(conclusion.to_string());
        }
        Observation {
            id: format!("obs_{revision}_{name}"),
            sweep_id: "swp_1".into(),
            collector: "forge-checks".into(),
            kind: "ci.check".into(),
            project_id: Some("prj_1".into()),
            subject_key: format!("ci:{revision}:{name}"),
            payload,
            content_digest: "sha256:x".into(),
            promoted: false,
            observed_at: moment(1_700_000_000),
        }
    }

    #[test]
    fn a_green_head_reads_green_however_red_its_history() {
        let rollup = roll_up(&[
            check(
                "0291aff",
                "build image",
                Some("failure"),
                "2026-08-09T22:41:46Z",
            ),
            check(
                "004f5f3",
                "build image",
                Some("failure"),
                "2026-08-11T03:51:48Z",
            ),
            check(
                "9fe53d8",
                "Runner policy",
                Some("success"),
                "2026-08-13T09:36:23Z",
            ),
            check(
                "9fe53d8",
                "build and deploy",
                Some("success"),
                "2026-08-13T09:36:25Z",
            ),
            check(
                "9fe53d8",
                "build image",
                Some("success"),
                "2026-08-13T09:36:27Z",
            ),
        ])
        .unwrap();
        assert_eq!(rollup.status(), "passing");
        assert_eq!(rollup.revision, "9fe53d8");
        assert_eq!(rollup.checks.len(), 3);
        assert!(rollup.failing.is_empty());
        assert_eq!(rollup.earlier_failures, 2);
        assert_eq!(rollup.revisions_observed, 3);
    }

    #[test]
    fn the_newest_run_wins_and_a_rerun_counts_once() {
        let rollup = roll_up(&[
            check(
                "6875f6d",
                "build and deploy",
                Some("success"),
                "2026-08-16T04:50:53Z",
            ),
            check(
                "6a55741",
                "build and deploy",
                Some("success"),
                "2026-08-16T04:56:39Z",
            ),
            check(
                "ca6a2ca",
                "build and deploy",
                Some("success"),
                "2026-08-16T04:55:22Z",
            ),
        ])
        .unwrap();
        assert_eq!(rollup.revision, "6a55741");

        let failing = roll_up(&[
            check("aaa1111", "build", Some("failure"), "2026-08-16T10:00:00Z"),
            check("aaa1111", "build", Some("failure"), "2026-08-16T11:00:00Z"),
        ])
        .unwrap();
        assert_eq!(failing.failing, vec!["build".to_string()]);
        assert!(roll_up(&[]).is_none());
        assert!(roll_up(&[check("", "build", Some("failure"), "2026-08-16T04:00:00Z")]).is_none());
        assert_eq!(
            roll_up(&[check("aaa1111", "ci", None, "2026-08-16T10:00:00Z")])
                .unwrap()
                .status(),
            "passing"
        );
    }

    #[test]
    fn priority_dominates_and_staleness_is_capped() {
        let now = moment(1_781_000_000);
        let high = score_item(&item("p0", now), &RankingInputs::at(now));
        let low = score_item(&item("p4", now), &RankingInputs::at(now));
        assert_eq!(high.total - low.total, 100.0);

        let ancient = item(
            "p3",
            Moment::from_unix(
                now.seconds_since(Moment::from_unix(0, 0)) - 3650 * 86_400,
                0,
            ),
        );
        assert!(
            score_item(&item("p0", now), &RankingInputs::at(now)).total
                > score_item(&ancient, &RankingInputs::at(now)).total
        );

        let stale = item(
            "p2",
            Moment::from_unix(now.seconds_since(Moment::from_unix(0, 0)) - 30 * 86_400, 0),
        );
        assert!(
            score_item(&stale, &RankingInputs::at(now)).total
                > score_item(&item("p2", now), &RankingInputs::at(now)).total
        );
    }

    #[test]
    fn contributions_sum_and_order_is_total() {
        let now = moment(1_781_000_000);
        let mut inputs = RankingInputs::at(now);
        inputs.blocking_fan_out = 2;
        inputs.initiative_weight = 40;
        let subject = Rankable {
            has_initiative: true,
            ..item(
                "p1",
                Moment::from_unix(now.seconds_since(Moment::from_unix(0, 0)) - 5 * 86_400, 0),
            )
        };
        let score = score_item(&subject, &inputs);
        let sum: f64 = score
            .contributions
            .iter()
            .map(|entry| entry.contribution)
            .sum();
        assert!((score.total - sum).abs() < 1e-9, "{} vs {sum}", score.total);

        let mut first = item("p2", now);
        first.id = "a".into();
        first.reference = "WI-2".into();
        let mut second = item("p2", now);
        second.reference = "WI-1".into();
        let scores = vec![
            score_item(&first, &RankingInputs::at(now)),
            score_item(&second, &RankingInputs::at(now)),
        ];
        let ordered = rank(scores.clone());
        let names: Vec<&str> = ordered
            .iter()
            .map(|score| score.reference.as_str())
            .collect();
        assert_eq!(names, vec!["WI-1", "WI-2"]);
        let reversed = rank(scores.into_iter().rev().collect());
        let names: Vec<&str> = reversed
            .iter()
            .map(|score| score.reference.as_str())
            .collect();
        assert_eq!(names, vec!["WI-1", "WI-2"]);

        let mut terminal = RankingInputs::at(now);
        terminal.is_terminal = true;
        let finished = Rankable {
            state: "done".into(),
            ..item("p2", now)
        };
        let sunk = score_item(&finished, &terminal);
        assert!(sunk.total < 0.0);
        assert!(sunk
            .contributions
            .iter()
            .any(|entry| entry.input == "terminal_state"));
        assert_eq!(PENDING_INPUTS[0].0, "ci_red_boost");
    }

    #[test]
    fn a_contract_names_itself_honestly() {
        let stock = contract_from_settings(
            "v1",
            &["AGENTS.md", "README.md", "LICENSE"],
            &["docs", "design", "src"],
            &["name", "lifecycle_state", "owner"],
        );
        assert_eq!(stock.version, "v1");

        let edited = contract_from_settings("v1", &["README.md"], &["src"], &[]);
        assert!(edited.version.starts_with("v1+"), "{}", edited.version);
        assert_ne!(edited.version, "v1");

        let named = contract_from_settings("acme-2026.1", &["README.md"], &["src"], &[]);
        assert_eq!(named.version, "acme-2026.1");

        let one = contract_from_settings("v1", &["b.md", "a.md"], &["src"], &[]);
        let other = contract_from_settings("v1", &["a.md", "b.md"], &["src"], &[]);
        assert_eq!(one.version, other.version);
        let different = contract_from_settings("v1", &["c.md"], &["src"], &[]);
        assert_ne!(one.version, different.version);
    }

    #[test]
    fn a_leading_v_is_not_a_different_version() {
        for (declared, observed) in [
            ("1.4.0", "v1.4.0"),
            ("v1.4.0", "1.4.0"),
            ("V1.4.0", "1.4.0"),
        ] {
            assert_eq!(
                normalise_version(declared),
                normalise_version(observed),
                "{declared}"
            );
        }
        assert_ne!(normalise_version("1.4.0"), normalise_version("1.5.0"));
    }

    #[test]
    fn a_qualified_reference_names_an_issue() {
        let cases = [
            (
                "GitHub: https://github.com/TheDancingDeveloper-org/vogt/issues/44",
                vec!["gh:TheDancingDeveloper-org/vogt#44"],
            ),
            (
                "see TheDancingDeveloper-org/vogt#44 for context",
                vec!["gh:TheDancingDeveloper-org/vogt#44"],
            ),
            ("http://www.github.com/o/r/issues/7", vec!["gh:o/r#7"]),
            (
                "https://github.com/o/r/issues/7 and again o/r#7",
                vec!["gh:o/r#7"],
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(issue_references(text), expected, "{text}");
        }
        for text in [
            "Regression from #43 (WI-2): dep-refs emits a ref_kind storage rejects",
            "fix in PR https://github.com/TheDancingDeveloper-org/vogt/pull/45",
            "issue 44 is the one",
            "#44",
        ] {
            assert!(issue_references(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn forge_state_is_auto_acceptable_in_both_directions() {
        assert!(auto_acceptable("version_mismatch"));
        assert!(auto_acceptable("forge_state_mismatch"));
        assert!(!auto_acceptable("vanished_upstream"));
        assert!(!auto_acceptable("referenced_issue_state_mismatch"));
        assert_eq!(HUMAN_GATED_REASON.len(), 8);
    }
    #[test]
    fn observed_guesses_match_the_python() {
        let base = Observation {
            id: "o".into(),
            sweep_id: "s".into(),
            collector: "c".into(),
            kind: "forge.issue".into(),
            project_id: None,
            subject_key: "gh:acme/app#1".into(),
            payload: serde_json::json!({"labels": ["Bug"], "state": "OPEN", "title": "Crash", "number": 7}),
            content_digest: "sha256:x".into(),
            promoted: false,
            observed_at: moment(0),
        };
        assert_eq!(work_kind_of(&base), "bug");
        assert_eq!(lifecycle_of(&base), "open");
        assert_eq!(priority_of(&base), "p2");
        assert!(is_worklike(&base));

        let marker = Observation {
            kind: "marker".into(),
            promoted: false,
            payload: serde_json::json!({"tag": "fixme"}),
            ..base.clone()
        };
        assert_eq!(work_kind_of(&marker), "bug");
        assert!(!is_worklike(&marker));
        assert!(is_worklike(&Observation {
            promoted: true,
            ..marker
        }));

        let merged = Observation {
            kind: "forge.pull_request".into(),
            payload: serde_json::json!({"state": "merged"}),
            ..base
        };
        assert_eq!(lifecycle_of(&merged), "closed");
        assert_eq!(upstream_state(&merged, None, "open"), "done");
    }
}
