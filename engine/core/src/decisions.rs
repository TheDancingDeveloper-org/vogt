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
/// a digest computed here would not match one Python stored. Everything outside
/// 0x20..=0x7E becomes `\uXXXX`, with a surrogate pair above U+FFFF — DEL
/// (`\x7f`) included. Floats follow CPython's `repr`: the shortest round-trip
/// digit string, fixed notation for an exponent in `-4..=15`, otherwise
/// `d.ddde±XX` with an exponent of at least two digits, and `.0` on an integer.
pub fn canonical_json(payload: &Value) -> String {
    render_python_json(payload, false, true)
}

/// `json.dumps` as Python stores it: `", "` / `": "` separators and
/// `ensure_ascii=True`. Keys are sorted only where the Python call sorts them
/// (`sort_keys=True` for a workflow definition, not for an exclusions list).
pub fn python_json_dumps(payload: &Value, sort_keys: bool) -> String {
    render_python_json(payload, true, sort_keys)
}

fn render_python_json(value: &Value, spaced: bool, sort_keys: bool) -> String {
    let mut out = String::new();
    write_python_json(&mut out, value, spaced, sort_keys);
    out
}

fn write_python_json(out: &mut String, value: &Value, spaced: bool, sort_keys: bool) {
    let (item_gap, key_gap) = if spaced { (", ", ": ") } else { (",", ":") };
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
                    out.push_str(item_gap);
                }
                write_python_json(out, item, spaced, sort_keys);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            if sort_keys {
                keys.sort();
            }
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push_str(item_gap);
                }
                write_python_string(out, key);
                out.push_str(key_gap);
                write_python_json(out, &map[*key], spaced, sort_keys);
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
            ch if !('\u{0020}'..='\u{007e}').contains(&ch) => push_escaped(out, ch),
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
    // CPython's `repr`: shortest round-trip digits, fixed notation while the
    // exponent sits in -4..=15, otherwise scientific with a signed exponent of
    // at least two digits. `{:e}` is not that — it omits the '+' and the
    // zero-padding, and `{:.16}` is not shortest.
    let negative = value.is_sign_negative();
    let magnitude = value.abs();
    let raw = format!("{magnitude:e}");
    let (digits, exponent) = raw.split_once('e').expect("debug exponent form");
    let mut significant: String = digits.chars().filter(|ch| *ch != '.').collect();
    significant = significant.trim_end_matches('0').to_string();
    if significant.is_empty() {
        significant.push('0');
    }
    let exp: i32 = exponent.parse().expect("debug exponent");
    let mut rendered = if (-4..16).contains(&exp) {
        fixed_notation(&significant, exp)
    } else {
        let mantissa = if significant.len() == 1 {
            significant
        } else {
            format!("{}.{}", &significant[..1], &significant[1..])
        };
        format!("{mantissa}e{exp:+03}")
    };
    if negative {
        rendered.insert(0, '-');
    }
    rendered
}

/// `digits` is the shortest significant digit string; `exp` is its order.
fn fixed_notation(digits: &str, exp: i32) -> String {
    if exp < 0 {
        return format!("0.{}{digits}", "0".repeat((-exp - 1) as usize));
    }
    let point = exp as usize + 1;
    if point >= digits.len() {
        format!("{digits}{}.0", "0".repeat(point - digits.len()))
    } else {
        format!("{}.{}", &digits[..point], &digits[point..])
    }
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
            priority: item.priority.to_string(),
            updated_at: item.updated_at,
            trust_state: item.trust_state.to_string(),
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

    let age_days = (inputs.now.seconds_since(item.updated_at) / 86_400.0).max(0.0);
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
            trust_state: "verified".parse().unwrap(),
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
        // Pinned against CPython 3.12 json.dumps. `{:.16}` and Rust's `{:e}`
        // both disagree with these.
        let floats = [
            (1e-05, "1e-05"),
            (0.0001, "0.0001"),
            (1e16, "1e+16"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e22, "1e+22"),
            (123.0, "123.0"),
            (-0.0, "-0.0"),
            (5e-324, "5e-324"),
        ];
        for (value, expected) in floats {
            assert_eq!(
                canonical_json(&serde_json::json!(value)),
                expected,
                "{value}"
            );
        }
        assert_eq!(
            canonical_json(&serde_json::json!("\u{007f}")),
            r#""\u007f""#
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
            Moment::from_unix(now.unix_seconds() - 3650 * 86_400, 0),
        );
        assert!(
            score_item(&item("p0", now), &RankingInputs::at(now)).total
                > score_item(&ancient, &RankingInputs::at(now)).total
        );

        let stale = item("p2", Moment::from_unix(now.unix_seconds() - 30 * 86_400, 0));
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
            ..item("p1", Moment::from_unix(now.unix_seconds() - 5 * 86_400, 0))
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

// What a session overseer should look at first. Ports `core/oversight.py`.
//
// The order is decided from facts the engine reported, with the reason in
// words — never a bare rank — so the table says why a row is at the top.
// Pure: the clock is a parameter, and nothing here reads storage or the
// engine. Every reason string is copied from the Python, because the two
// compare them.

/// Lower first. `stalled` sits above `running` because a turn that has printed
/// nothing for a long time is worth a look before one that is busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    Approval,
    Blocked,
    Waiting,
    Stalled,
    Running,
    Idle,
    Hibernated,
    Exited,
    Unknown,
}

impl Attention {
    pub fn order(self) -> u8 {
        match self {
            Self::Approval => 0,
            Self::Blocked => 1,
            Self::Waiting => 2,
            Self::Stalled => 3,
            Self::Running => 4,
            Self::Idle => 5,
            Self::Hibernated => 6,
            Self::Exited => 7,
            Self::Unknown => 8,
        }
    }

    /// The classes a person, or a driver acting for one, must act on.
    pub fn needs_you(self) -> bool {
        matches!(self, Self::Approval | Self::Blocked | Self::Waiting)
    }
}

/// Startup gates an agent CLI stops at before any work, as words.
fn gate_words(kind: &str) -> Option<&'static str> {
    match kind {
        "folder-trust" => Some("folder trust"),
        "external-imports" => Some("external CLAUDE.md imports"),
        "read-outside-cwd" => Some("read outside the working directory"),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub attention: Attention,
    pub reason: String,
}

/// Where one session belongs in the oversight table, and why.
#[allow(clippy::too_many_arguments)]
pub fn classify(
    activity: Option<&str>,
    alive: Option<bool>,
    ready: Option<bool>,
    approval_question: Option<&str>,
    blocker: Option<&str>,
    approval_kind: Option<&str>,
    last_output_at: Option<crate::core::Moment>,
    now: crate::core::Moment,
    stall_after_secs: i64,
) -> Verdict {
    let verdict = |attention: Attention, reason: String| Verdict { attention, reason };
    if activity == Some("hibernated") {
        return verdict(
            Attention::Hibernated,
            "hibernated to free memory; wake it to continue".to_string(),
        );
    }
    if activity.is_none() || alive.is_none() {
        return verdict(
            Attention::Unknown,
            "the engine could not be asked".to_string(),
        );
    }
    if activity == Some("stopped") {
        return verdict(Attention::Exited, "stopped on request".to_string());
    }
    if alive == Some(false) {
        return verdict(
            Attention::Exited,
            format!("its process ended ({})", activity.unwrap_or_default()),
        );
    }
    if approval_question.is_some_and(|text| !text.is_empty())
        || activity == Some("awaiting-approval")
    {
        if let (Some(gate), Some(question)) = (
            approval_kind.and_then(gate_words),
            approval_question.filter(|text| !text.is_empty()),
        ) {
            return verdict(
                Attention::Approval,
                format!("stopped at a startup gate ({gate}): {question}"),
            );
        }
        return verdict(
            Attention::Approval,
            match approval_question.filter(|text| !text.is_empty()) {
                Some(question) => format!("asking for approval: {question}"),
                None => "showing a permission dialog".to_string(),
            },
        );
    }
    if let Some(blocker) = blocker.filter(|text| !text.is_empty()) {
        return verdict(
            Attention::Blocked,
            format!("blocked on a person: {blocker}"),
        );
    }
    if activity == Some("waiting-for-input") || (activity == Some("idle") && ready == Some(true)) {
        return verdict(
            Attention::Waiting,
            "at its prompt, waiting for the next instruction".to_string(),
        );
    }
    if activity == Some("running") {
        if let Some(last) = last_output_at {
            // Compared in f64 so a fraction is kept. Truncating to i64 would
            // call a turn stalled when its last output is half a second in the
            // future and the stall threshold is zero.
            let quiet = now.seconds_since(last);
            if quiet >= stall_after_secs as f64 {
                let minutes = (quiet / 60.0).floor() as i64;
                return verdict(
                    Attention::Stalled,
                    format!("running, but nothing printed for {minutes} min"),
                );
            }
        }
        return verdict(Attention::Running, "working".to_string());
    }
    verdict(
        Attention::Idle,
        "resting, not at a recognised prompt".to_string(),
    )
}

#[cfg(test)]
mod oversight_tests {
    use super::*;

    fn now() -> crate::core::Moment {
        crate::core::from_iso("2026-10-09T03:00:00Z").unwrap()
    }

    fn classify_running(quiet_for_secs: i64) -> Verdict {
        let moment = now();
        classify(
            Some("running"),
            Some(true),
            Some(false),
            None,
            None,
            None,
            Some(crate::core::Moment::from_unix(
                moment.unix_seconds() - quiet_for_secs,
                0,
            )),
            moment,
            600,
        )
    }

    #[test]
    fn each_session_lands_where_a_driver_should_look() {
        // tests/test_oversight.py, the parametrised table plus the reason
        // assertions that follow it.
        let moment = now();
        let ago = |minutes: i64| {
            Some(crate::core::Moment::from_unix(
                moment.unix_seconds() - minutes * 60,
                0,
            ))
        };
        type Row<'a> = (
            &'a str,
            Option<&'a str>,
            bool,
            bool,
            Option<&'a str>,
            Attention,
        );
        let cases: &[Row] = &[
            (
                "awaiting-approval",
                Some("Run rm?"),
                true,
                false,
                None,
                Attention::Approval,
            ),
            (
                "running",
                None,
                true,
                false,
                Some("needs a token"),
                Attention::Blocked,
            ),
            (
                "waiting-for-input",
                None,
                true,
                false,
                None,
                Attention::Waiting,
            ),
            ("idle", None, true, true, None, Attention::Waiting),
            ("idle", None, true, false, None, Attention::Idle),
            ("running", None, true, false, None, Attention::Running),
            (
                "hibernated",
                None,
                false,
                false,
                None,
                Attention::Hibernated,
            ),
            ("errored", None, false, false, None, Attention::Exited),
        ];
        for (activity, question, alive, ready, blocker, want) in cases {
            let found = classify(
                Some(activity),
                Some(*alive),
                Some(*ready),
                question.as_deref(),
                blocker.as_deref(),
                None,
                ago(0),
                moment,
                600,
            );
            assert_eq!(found.attention, *want, "{activity}");
        }
        let stalled = classify(
            Some("running"),
            Some(true),
            Some(false),
            None,
            None,
            None,
            ago(25),
            moment,
            600,
        );
        assert_eq!(stalled.attention, Attention::Stalled);
        assert_eq!(stalled.reason, "running, but nothing printed for 25 min");

        let dialog = classify(
            Some("awaiting-approval"),
            Some(true),
            Some(false),
            Some("Run rm?"),
            Some("x"),
            None,
            ago(0),
            moment,
            600,
        );
        assert_eq!(dialog.attention, Attention::Approval);
        assert!(dialog.reason.contains("Run rm?"));

        // An empty question is not a question. Python treats "" as false.
        let empty = classify(
            Some("running"),
            Some(true),
            Some(false),
            Some(""),
            None,
            None,
            ago(0),
            moment,
            600,
        );
        assert_eq!(empty.attention, Attention::Running);
    }

    #[test]
    fn the_order_puts_what_needs_a_person_first() {
        let ranked = [
            Attention::Idle,
            Attention::Approval,
            Attention::Running,
            Attention::Blocked,
            Attention::Unknown,
            Attention::Waiting,
            Attention::Stalled,
        ];
        let mut sorted = ranked.to_vec();
        sorted.sort_by_key(|attention| attention.order());
        assert_eq!(
            sorted,
            [
                Attention::Approval,
                Attention::Blocked,
                Attention::Waiting,
                Attention::Stalled,
                Attention::Running,
                Attention::Idle,
                Attention::Unknown,
            ]
        );
        assert!(Attention::Approval.needs_you());
        assert!(!Attention::Stalled.needs_you());
    }

    #[test]
    fn a_quiet_turn_is_stalled_and_a_gate_is_named() {
        assert_eq!(classify_running(600).attention, Attention::Stalled);
        assert_eq!(
            classify_running(600).reason,
            "running, but nothing printed for 10 min"
        );
        assert_eq!(classify_running(540).attention, Attention::Running);

        let gate = classify(
            Some("awaiting-approval"),
            Some(true),
            None,
            Some("trust this folder?"),
            None,
            Some("folder-trust"),
            None,
            now(),
            600,
        );
        assert_eq!(gate.attention, Attention::Approval);
        assert_eq!(
            gate.reason,
            "stopped at a startup gate (folder trust): trust this folder?"
        );
    }

    #[test]
    fn an_unasked_engine_is_unknown_and_a_dead_process_has_exited() {
        let unknown = classify(None, Some(true), None, None, None, None, None, now(), 600);
        assert_eq!(unknown.attention, Attention::Unknown);

        let ended = classify(
            Some("crashed"),
            Some(false),
            None,
            None,
            None,
            None,
            None,
            now(),
            600,
        );
        assert_eq!(ended.reason, "its process ended (crashed)");
    }
}

// Rendering an initiative as a forge tracking issue. Ports
// `core/initiative_projection.py`. Vogt owns exactly the span between the two
// markers; everything a person writes outside it survives every re-render.

pub const MANAGED_START: &str = "<!-- vogt:initiative:start -->";
pub const MANAGED_END: &str = "<!-- vogt:initiative:end -->";

/// What the task list says when an initiative has no forge-numbered members yet.
pub const EMPTY_TASK_LIST: &str = "_No linked work items yet._";

pub fn marker_for(slug: &str) -> String {
    format!("<!-- vogt:initiative:{slug} -->")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLine {
    pub number: i64,
    pub title: String,
    /// True when the member is in a terminal workflow state.
    pub checked: bool,
}

impl TaskLine {
    pub fn from_state(number: i64, title: &str, state: &str) -> Self {
        Self {
            number,
            title: title.to_string(),
            checked: crate::core::TERMINAL_STATES.contains(&state),
        }
    }
}

pub fn render_task_line(line: &TaskLine) -> String {
    let mark = if line.checked { "x" } else { " " };
    format!("- [{mark}] #{} {}", line.number, line.title)
        .trim_end()
        .to_string()
}

pub fn render_task_list(lines: &[TaskLine]) -> String {
    if lines.is_empty() {
        return EMPTY_TASK_LIST.to_string();
    }
    lines
        .iter()
        .map(render_task_line)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_managed_region(
    slug: &str,
    body: &str,
    tasks: &[TaskLine],
    siblings: &[(&str, &str)],
) -> String {
    let mut parts = vec![MANAGED_START.to_string(), marker_for(slug), String::new()];
    let trimmed = body.trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
        parts.push(String::new());
    }
    parts.push("### Work items".to_string());
    parts.push(String::new());
    parts.push(render_task_list(tasks));
    if !siblings.is_empty() {
        parts.push(String::new());
        parts.push("### Tracked across other repositories".to_string());
        parts.push(String::new());
        for (sib_slug, url) in siblings {
            parts.push(format!("- {sib_slug}: {url}"));
        }
    }
    parts.push(String::new());
    parts.push(MANAGED_END.to_string());
    parts.join("\n")
}

/// Replace the managed region, preserving everything a person wrote outside it.
/// With no markers, the region is appended.
pub fn splice_managed_region(existing: Option<&str>, region: &str) -> String {
    let Some(existing) =
        existing.filter(|text| text.contains(MANAGED_START) && text.contains(MANAGED_END))
    else {
        let base = existing.unwrap_or("").trim_end();
        return if base.is_empty() {
            region.to_string()
        } else {
            format!("{base}\n\n{region}")
        };
    };
    let start = existing.find(MANAGED_START).unwrap_or(0);
    let end = existing.find(MANAGED_END).unwrap_or(existing.len()) + MANAGED_END.len();
    format!("{}{region}{}", &existing[..start], &existing[end..])
}

pub fn body_has_marker(body: Option<&str>, slug: &str) -> bool {
    body.is_some_and(|text| text.contains(&marker_for(slug)))
}

fn managed_span(body: &str) -> &str {
    let Some(start) = body.find(MANAGED_START) else {
        return "";
    };
    let Some(end) = body.find(MANAGED_END) else {
        return "";
    };
    &body[start + MANAGED_START.len()..end]
}

/// The `#<n> -> checked?` map inside the managed region only, so a checkbox a
/// person wrote in their own prose is never read as a member's state.
pub fn parse_checkbox_states(body: &str) -> Vec<(i64, bool)> {
    let span = managed_span(body);
    if span.is_empty() {
        return Vec::new();
    }
    span.lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("- [")?;
            let (mark, rest) = rest.split_once(']')?;
            let rest = rest.trim_start().strip_prefix('#')?;
            let number: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if number.is_empty() {
                return None;
            }
            // `\b` after the number: a digit run that continues into a word is
            // not a member reference.
            let boundary = rest.len() == number.len()
                || !rest[number.len()..]
                    .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
            boundary.then(|| (number.parse().unwrap_or(0), mark.eq_ignore_ascii_case("x")))
        })
        .collect()
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    #[test]
    fn a_region_round_trips_and_a_human_checkbox_is_ignored() {
        let tasks = vec![
            TaskLine::from_state(12, "Ship it", "done"),
            TaskLine::from_state(13, "Wait", "open"),
        ];
        let region = render_managed_region(
            "alpha",
            "The plan.",
            &tasks,
            &[("beta", "https://forge/beta")],
        );
        assert!(region.starts_with(MANAGED_START));
        assert!(region.contains(&marker_for("alpha")));
        assert!(region.contains("- [x] #12 Ship it"));
        assert!(region.contains("- [ ] #13 Wait"));
        assert!(region.contains("- beta: https://forge/beta"));

        let spliced = splice_managed_region(Some("A note.\n\n- [x] #99 mine\n"), &region);
        assert!(spliced.starts_with("A note."));
        assert_eq!(
            parse_checkbox_states(&spliced),
            vec![(12, true), (13, false)]
        );
        assert!(body_has_marker(Some(&spliced), "alpha"));
        assert!(!body_has_marker(Some(&spliced), "beta"));
    }

    #[test]
    fn an_empty_initiative_says_so_and_a_fresh_body_is_appended() {
        let region = render_managed_region("alpha", "  ", &[], &[]);
        assert!(region.contains(EMPTY_TASK_LIST));
        assert!(!region.contains("Tracked across"));
        assert_eq!(splice_managed_region(None, &region), region);
    }
}

// Which CI runs deserve attention on their own. Ports `core/ci_alerts.py`.
// Pure: observations in, verdicts out. Nothing here reads a store or a clock.

const PULL_REQUEST_EVENTS: &[&str] = &["pull_request", "pull_request_target", "merge_group"];
const GITHUB_MANAGED_EVENTS: &[&str] = &["dynamic"];
const GITHUB_MANAGED_PATH_PREFIX: &str = "dynamic/";
const FAILING: &[&str] = &[
    "failure",
    "timed_out",
    "startup_failure",
    "action_required",
    "error",
];
const PASSING: &[&str] = &["success", "neutral", "skipped"];

fn payload_str<'a>(payload: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
}

/// Whether a check observation is a run of a GitHub-managed workflow: its event
/// is `dynamic`, its workflow path sits under `dynamic/`, or — for an
/// observation stored before either was recorded — its name ends in a
/// Dependabot update number.
pub fn github_managed(payload: &serde_json::Value) -> bool {
    if payload_str(payload, "event").is_some_and(|event| GITHUB_MANAGED_EVENTS.contains(&event)) {
        return true;
    }
    if payload_str(payload, "workflow_path")
        .is_some_and(|path| path.starts_with(GITHUB_MANAGED_PATH_PREFIX))
    {
        return true;
    }
    if payload_str(payload, "event").is_some() {
        return false;
    }
    payload_str(payload, "check").is_some_and(dependabot_update_name)
}

/// ` - Update #<digits>` at the end of a run name.
fn dependabot_update_name(name: &str) -> bool {
    let Some(marker) = name.rfind(" - Update #") else {
        return false;
    };
    let tail = &name[marker + " - Update #".len()..];
    !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneKind {
    Branch,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedRef {
    pub kind: LaneKind,
    pub lane: String,
    pub reference: String,
}

/// The lane a run belongs to, or `None` when it is not watched. A run with no
/// ref, one a pull request started, or one from a managed workflow is never
/// watched. A ref matching a branch pattern is a branch even if a tag pattern
/// would also match it.
pub fn watched_ref(
    branch: Option<&str>,
    event: Option<&str>,
    branches: &[&str],
    tags: &[&str],
) -> Option<WatchedRef> {
    let branch = branch.filter(|text| !text.is_empty())?;
    if event.is_some_and(|event| {
        PULL_REQUEST_EVENTS.contains(&event) || GITHUB_MANAGED_EVENTS.contains(&event)
    }) {
        return None;
    }
    for pattern in branches {
        if shell_match(branch, pattern) {
            return Some(WatchedRef {
                kind: LaneKind::Branch,
                lane: branch.to_string(),
                reference: branch.to_string(),
            });
        }
    }
    for pattern in tags {
        if shell_match(branch, pattern) {
            return Some(WatchedRef {
                kind: LaneKind::Tag,
                lane: (*pattern).to_string(),
                reference: branch.to_string(),
            });
        }
    }
    None
}

/// `fnmatch.fnmatchcase` for the patterns a watch list holds: a literal, or a
/// trailing `*` that matches the rest. Case sensitive, `*` and `?` elsewhere
/// are literal, which is what the shipped patterns need.
fn shell_match(text: &str, pattern: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) if !prefix.contains(['*', '?']) => text.starts_with(prefix),
        _ => text == pattern,
    }
}

fn ci_workflow_of(check: &crate::core::Observation) -> String {
    payload_str(&check.payload, "check")
        .unwrap_or("workflow")
        .to_string()
}

fn ci_revision_of(check: &crate::core::Observation) -> String {
    payload_str(&check.payload, "revision")
        .unwrap_or("")
        .to_string()
}

fn ci_conclusion_of(check: &crate::core::Observation) -> Option<String> {
    payload_str(&check.payload, "conclusion").map(str::to_string)
}

/// When a run ran, for ordering: its own `updated_at`, then its run number,
/// then when Vogt observed it.
fn ci_ran_at(check: &crate::core::Observation) -> (String, i64, crate::core::Moment) {
    (
        payload_str(&check.payload, "updated_at")
            .unwrap_or("")
            .to_string(),
        check
            .payload
            .get("run_number")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        check.observed_at,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefFailure {
    pub workflow: String,
    pub lane: WatchedRef,
    pub conclusion: String,
}

/// Every watched lane whose newest decisive run failed. A run still going, a
/// cancelled run and a stale one say nothing, so they neither raise nor clear.
pub fn watched_failures(
    checks: &[crate::core::Observation],
    branches: &[&str],
    tags: &[&str],
) -> Vec<RefFailure> {
    let mut newest: std::collections::BTreeMap<(String, String, u8, String), (usize, String)> =
        std::collections::BTreeMap::new();
    let mut places: std::collections::BTreeMap<(String, String, u8, String), WatchedRef> =
        std::collections::BTreeMap::new();
    for (index, check) in checks.iter().enumerate() {
        let Some(conclusion) = ci_conclusion_of(check) else {
            continue;
        };
        if !FAILING.contains(&conclusion.as_str()) && !PASSING.contains(&conclusion.as_str()) {
            continue;
        }
        let Some(lane) = watched_ref(
            payload_str(&check.payload, "branch"),
            payload_str(&check.payload, "event"),
            branches,
            tags,
        ) else {
            continue;
        };
        if github_managed(&check.payload) {
            continue;
        }
        let key = (
            check.project_id.clone().unwrap_or_default(),
            ci_workflow_of(check),
            lane.kind as u8,
            lane.lane.clone(),
        );
        let replace = newest
            .get(&key)
            .is_none_or(|held| ci_ran_at(check) > ci_ran_at(&checks[held.0]));
        if replace {
            newest.insert(key.clone(), (index, conclusion));
            places.insert(key, lane);
        }
    }
    newest
        .into_iter()
        .filter(|(_, (_, conclusion))| FAILING.contains(&conclusion.as_str()))
        .map(|(key, (_, conclusion))| RefFailure {
            workflow: key.1.clone(),
            lane: places.remove(&key).expect("a lane was stored with the run"),
            conclusion,
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchState {
    Running,
    Passed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchRun {
    pub workflow: String,
    pub conclusion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCi {
    pub branch: String,
    pub revision: String,
    pub state: BranchState,
    pub runs: Vec<BranchRun>,
    pub failing: Vec<String>,
    pub concluded_at: Option<String>,
}

/// What CI says about `branch`'s newest observed revision, or `None` when no
/// run on the branch has been observed.
pub fn branch_ci(checks: &[crate::core::Observation], branch: &str) -> Option<BranchCi> {
    let on_branch: Vec<&crate::core::Observation> = checks
        .iter()
        .filter(|check| {
            payload_str(&check.payload, "branch") == Some(branch)
                && !ci_revision_of(check).is_empty()
        })
        .collect();
    if on_branch.is_empty() {
        return None;
    }
    let revision = ci_revision_of(
        on_branch
            .iter()
            .max_by_key(|check| ci_ran_at(check))
            .expect("non-empty"),
    );
    let mut latest: std::collections::BTreeMap<String, &crate::core::Observation> =
        std::collections::BTreeMap::new();
    for check in &on_branch {
        if ci_revision_of(check) != revision {
            continue;
        }
        let workflow = ci_workflow_of(check);
        let replace = latest
            .get(&workflow)
            .is_none_or(|held| ci_ran_at(check) > ci_ran_at(held));
        if replace {
            latest.insert(workflow, check);
        }
    }
    let runs: Vec<BranchRun> = latest
        .into_iter()
        .map(|(workflow, check)| BranchRun {
            workflow,
            conclusion: ci_conclusion_of(check),
        })
        .collect();
    let failing: Vec<String> = runs
        .iter()
        .filter(|run| {
            run.conclusion
                .as_deref()
                .is_some_and(|c| FAILING.contains(&c))
        })
        .map(|run| run.workflow.clone())
        .collect();
    let state = if runs.iter().any(|run| run.conclusion.is_none()) {
        BranchState::Running
    } else if !failing.is_empty() {
        BranchState::Failed
    } else if runs
        .iter()
        .all(|run| matches!(run.conclusion.as_deref(), Some("cancelled" | "stale")))
    {
        BranchState::Cancelled
    } else {
        BranchState::Passed
    };
    let concluded_at = if state == BranchState::Running {
        None
    } else {
        checks
            .iter()
            .filter(|check| {
                payload_str(&check.payload, "branch") == Some(branch)
                    && ci_revision_of(check) == revision
            })
            .filter_map(|check| payload_str(&check.payload, "updated_at"))
            .max()
            .map(str::to_string)
    };
    Some(BranchCi {
        branch: branch.to_string(),
        revision,
        state,
        runs,
        failing,
        concluded_at,
    })
}

#[cfg(test)]
mod ci_alert_tests {
    use super::*;

    fn check(
        branch: &str,
        event: &str,
        name: &str,
        conclusion: &str,
        updated: &str,
    ) -> crate::core::Observation {
        crate::core::Observation {
            id: String::new(),
            sweep_id: String::new(),
            collector: "ci".to_string(),
            kind: "check".to_string(),
            project_id: Some("p".to_string()),
            subject_key: String::new(),
            payload: serde_json::json!({"branch": branch, "event": event, "check": name, "conclusion": conclusion, "updated_at": updated, "revision": "abc"}),
            content_digest: String::new(),
            promoted: false,
            observed_at: crate::core::from_iso("2026-10-09T00:00:00Z").unwrap(),
        }
    }

    #[test]
    fn a_pull_request_and_a_dependabot_run_are_not_watched() {
        let branches = ["main", "master", "prod"];
        let tags = ["v*"];
        assert!(watched_ref(Some("main"), Some("pull_request"), &branches, &tags).is_none());
        assert!(watched_ref(Some("main"), Some("dynamic"), &branches, &tags).is_none());
        assert!(watched_ref(Some("wi-7"), Some("push"), &branches, &tags).is_none());
        assert!(watched_ref(None, Some("push"), &branches, &tags).is_none());
        let tag = watched_ref(Some("v0.7.2"), Some("push"), &branches, &tags).unwrap();
        assert_eq!(tag.kind, LaneKind::Tag);
        assert_eq!(tag.lane, "v*");
        assert!(github_managed(
            &serde_json::json!({"check": "npm in /web - Update #1606151494"})
        ));
        assert!(!github_managed(
            &serde_json::json!({"event": "push", "check": "build"})
        ));
    }

    #[test]
    fn a_later_success_clears_the_alert_and_a_cancelled_run_does_not() {
        let branches = ["main"];
        let tags = ["v*"];
        let failed = check("main", "push", "build", "failure", "2026-10-09T01:00:00Z");
        let cancelled = check("main", "push", "build", "cancelled", "2026-10-09T02:00:00Z");
        assert_eq!(
            watched_failures(&[failed.clone(), cancelled], &branches, &tags).len(),
            1
        );
        let fixed = check("main", "push", "build", "success", "2026-10-09T03:00:00Z");
        assert!(watched_failures(&[failed, fixed], &branches, &tags).is_empty());
    }

    #[test]
    fn a_bound_branch_reports_its_newest_revision() {
        let mut older = check(
            "feature",
            "push",
            "build",
            "failure",
            "2026-10-09T01:00:00Z",
        );
        older.payload["revision"] = "old".into();
        let head = check(
            "feature",
            "push",
            "build",
            "success",
            "2026-10-09T02:00:00Z",
        );
        let found = branch_ci(&[older, head], "feature").unwrap();
        assert_eq!(found.revision, "abc");
        assert_eq!(found.state, BranchState::Passed);
        assert!(branch_ci(&[], "feature").is_none());
    }
}

// Which agent, model and effort a session is running, and how we know.
// Ports `core/runtime.py`. Three sources, best first, and every answer says
// which it came from. None of them is a default the CLI might pick: an
// unknown is absent, not a guess.

const AGENTS: &[&str] = &["claude", "codex", "opencode", "klaudia"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRuntime {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// The agent CLI a command runs and the model and effort flags it carries.
/// A command that does not parse as a shell line is split on whitespace.
pub fn runtime_from_command(command: Option<&str>) -> CommandRuntime {
    let Some(command) = command.filter(|text| !text.is_empty()) else {
        return CommandRuntime {
            agent: None,
            model: None,
            effort: None,
        };
    };
    let words = split_command(command);
    let mut agent = None;
    let mut model = None;
    let mut effort = None;
    for (index, word) in words.iter().enumerate() {
        let name = word.rsplit('/').next().unwrap_or(word);
        if agent.is_none() && AGENTS.contains(&name) {
            agent = Some(name.to_string());
        }
        let following = words.get(index + 1).map(String::as_str);
        if matches!(word.as_str(), "--model" | "-m") {
            if let Some(value) = following {
                model = Some(value.to_string());
            }
        } else if let Some(value) = word.strip_prefix("--model=") {
            model = Some(value.to_string());
        } else if word == "--effort" {
            if let Some(value) = following {
                effort = Some(value.to_string());
            }
        } else if let Some(value) = word.strip_prefix("--effort=") {
            effort = Some(value.to_string());
        } else if word == "-c" {
            if let Some(value) = following.and_then(effort_override) {
                effort = Some(value);
            }
        }
    }
    CommandRuntime {
        agent,
        model,
        effort,
    }
}

/// `model_reasoning_effort=<value>`, with one layer of quotes stripped.
fn effort_override(value: &str) -> Option<String> {
    let rest = value.strip_prefix("model_reasoning_effort=")?;
    Some(rest.trim_matches(|c| c == '\'' || c == '"').to_string())
}

/// `shlex.split`, falling back to whitespace when the line is unbalanced.
fn split_command(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    let mut ok = true;
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (None, '"' | '\'') => quote = Some(ch),
            (Some(open), c) if c == open => quote = None,
            (None, '\\') => match chars.next() {
                Some(c) => current.push(c),
                None => ok = false,
            },
            (Some('"'), '\\') => match chars.peek() {
                Some(&c) if matches!(c, '"' | '\\' | '$' | '`' | '\n') => {
                    chars.next();
                    current.push(c);
                }
                _ => current.push('\\'),
            },
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            (_, c) => current.push(c),
        }
    }
    if quote.is_some() {
        ok = false;
    }
    if !current.is_empty() {
        words.push(current);
    }
    if ok {
        words
    } else {
        command.split_whitespace().map(str::to_string).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRuntime {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub model_basis: Option<&'static str>,
    pub effort: Option<String>,
    pub effort_basis: Option<&'static str>,
}

/// The best answer for each of model and effort, with its source. Transcript
/// beats the command line, which beats what the session was asked for.
pub fn resolve_runtime(
    command: Option<&str>,
    conversation_agent: Option<&str>,
    transcript_model: Option<&str>,
    transcript_effort: Option<&str>,
    asked_model: Option<&str>,
    asked_effort: Option<&str>,
) -> ResolvedRuntime {
    let flags = runtime_from_command(command);
    let pick =
        |candidates: &[(&Option<String>, &'static str)]| -> (Option<String>, Option<&'static str>) {
            for (value, basis) in candidates {
                if let Some(value) = value.as_deref().filter(|text| !text.is_empty()) {
                    return (Some(value.to_string()), Some(*basis));
                }
            }
            (None, None)
        };
    let transcript_model = transcript_model.map(str::to_string);
    let asked_model = asked_model.map(str::to_string);
    let transcript_effort = transcript_effort.map(str::to_string);
    let asked_effort = asked_effort.map(str::to_string);
    let (model, model_basis) = pick(&[
        (&transcript_model, "transcript"),
        (&flags.model, "command"),
        (&asked_model, "asked"),
    ]);
    let (effort, effort_basis) = pick(&[
        (&transcript_effort, "transcript"),
        (&flags.effort, "command"),
        (&asked_effort, "asked"),
    ]);
    ResolvedRuntime {
        agent: flags.agent.or_else(|| {
            conversation_agent
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        }),
        model,
        model_basis,
        effort,
        effort_basis,
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    #[test]
    fn a_command_names_its_agent_model_and_effort() {
        let found = runtime_from_command(Some("claude --model opus --effort high"));
        assert_eq!(found.agent.as_deref(), Some("claude"));
        assert_eq!(found.model.as_deref(), Some("opus"));
        assert_eq!(found.effort.as_deref(), Some("high"));

        let equals = runtime_from_command(Some(
            "/usr/bin/codex --model=gpt-5 -c model_reasoning_effort='xhigh'",
        ));
        assert_eq!(equals.agent.as_deref(), Some("codex"));
        assert_eq!(equals.model.as_deref(), Some("gpt-5"));
        assert_eq!(equals.effort.as_deref(), Some("xhigh"));

        assert!(runtime_from_command(None).agent.is_none());
        assert!(runtime_from_command(Some("")).model.is_none());
    }

    #[test]
    fn the_transcript_beats_the_command_which_beats_what_was_asked() {
        let found = resolve_runtime(
            Some("claude --model haiku --effort low"),
            None,
            Some("opus"),
            Some("high"),
            Some("sonnet"),
            Some("medium"),
        );
        assert_eq!(found.model.as_deref(), Some("opus"));
        assert_eq!(found.model_basis, Some("transcript"));
        assert_eq!(found.effort.as_deref(), Some("high"));
        assert_eq!(found.effort_basis, Some("transcript"));

        let asked = resolve_runtime(
            Some("echo hi"),
            Some("klaudia"),
            None,
            None,
            Some("sonnet"),
            None,
        );
        assert_eq!(asked.agent.as_deref(), Some("klaudia"));
        assert_eq!(asked.model.as_deref(), Some("sonnet"));
        assert_eq!(asked.model_basis, Some("asked"));
        assert_eq!(asked.effort, None);
    }
}

// What an agent did, reduced to something safe to keep. Ports
// `core/agent_activity.py`. Redaction runs before anything is kept, raw output
// is never stored, and the service tags and error flag are heuristics that say
// so. The patterns and their order are the Python's, because a difference is a
// credential kept or a useful row destroyed.

use std::sync::LazyLock;

pub const SUMMARY_LIMIT: usize = 300;
pub const EXCERPT_HEAD: usize = 200;
pub const EXCERPT_TAIL: usize = 200;
pub const SCAN_WINDOW: usize = 8_192;
pub const REDACTED: &str = "[REDACTED]";
pub const WITHHELD: &str = "[withheld: configuration or environment output]";

static PEM_BLOCK: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"(?s)-----BEGIN [A-Z0-9 ]+-----.*?(?:-----END [A-Z0-9 ]+-----|\z)")
        .expect("pattern")
});
static JWT_SHAPE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}")
        .expect("pattern")
});
static TOKEN_SHAPES: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        r"(?:",
        r"\bgh[pousr]_[A-Za-z0-9]{20,}",
        r"|\bgithub_pat_[A-Za-z0-9_]{20,}",
        r"|\bglpat-[A-Za-z0-9_-]{16,}",
        r"|\bsk-[A-Za-z0-9_-]{16,}",
        r"|\bxox[abposr]-[A-Za-z0-9-]{10,}",
        r"|\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        r"|\bAIza[0-9A-Za-z_-]{30,}",
        r"|\bya29\.[0-9A-Za-z_-]{20,}",
        r"|\bnpm_[A-Za-z0-9]{30,}",
        r"|\bhf_[A-Za-z0-9]{30,}",
        r"|\bst\.[A-Za-z0-9-]{8,}\.[A-Fa-f0-9]{16,}\.[A-Fa-f0-9]{16,}",
        r"|\b(?:pk|rk|sk)_(?:live|test)_[A-Za-z0-9]{16,}",
        r"|\bAGE-SECRET-KEY-1[0-9A-Z]{20,}",
        r")",
    ))
    .expect("pattern")
});
static AUTH_HEADER: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        "(?i)\\b((?:authorization|x-api-key|api-key|x-auth-token|private-token)\\s*[:=]\\s*(?:bearer\\s+|basic\\s+|token\\s+)?)[^\\s\"',;]+",
    )
    .expect("pattern")
});
static BEARER_TOKEN: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{8,}").expect("pattern")
});
static URL_USERINFO: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(://)[^/\s:@]+:[^/\s@]+@").expect("pattern"));

const SECRET_NAME: &str = concat!(
    r"[A-Za-z0-9_.-]*(?:token|secret|passw(?:or)?d|passwd|pwd|api[_-]?key|apikey",
    r"|private[_-]?key|access[_-]?key|client[_-]?secret|credential|auth[_-]?key",
    r"|session[_-]?key|signing[_-]?key|webhook[_-]?url|dsn)[A-Za-z0-9_.-]*"
);

static JSON_PAIR: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(&format!(
        r#"(?i)("{SECRET_NAME}"\s*:\s*)("(?:[^"\\\n]|\\.)*"|[^\s,}}\]]+)"#
    ))
    .expect("pattern")
});
static FLAG_VALUE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(&format!(
        r#"(?i)(--?{SECRET_NAME}[ =])("[^"\n]*"|'[^'\n]*'|[^\s"']+)"#
    ))
    .expect("pattern")
});
static ASSIGNMENT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(&format!(
        r#"(?i)\b({SECRET_NAME})(\s*[=:]\s*)("[^"\n]*"|'[^'\n]*'|[^\s"',;\}}&]+)"#
    ))
    .expect("pattern")
});
static LONG_HEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b[A-Fa-f0-9]{48,}\b").expect("pattern"));
static LONG_BLOB: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[A-Za-z0-9+_=-]{32,}").expect("pattern"));

/// Replace every credential-shaped substring. Whole blocks first, then named
/// shapes, then pairs whose name marks the value, then the shape-only fallback.
pub fn activity_redact(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = PEM_BLOCK.replace_all(text, "[REDACTED:pem]").into_owned();
    out = JWT_SHAPE.replace_all(&out, "[REDACTED:jwt]").into_owned();
    out = TOKEN_SHAPES
        .replace_all(&out, "[REDACTED:token]")
        .into_owned();
    out = AUTH_HEADER
        .replace_all(&out, format!("$1{REDACTED}"))
        .into_owned();
    out = BEARER_TOKEN
        .replace_all(&out, format!("$1{REDACTED}"))
        .into_owned();
    out = URL_USERINFO.replace_all(&out, "$1[REDACTED]@").into_owned();
    out = JSON_PAIR
        .replace_all(&out, format!("$1\"{REDACTED}\""))
        .into_owned();
    out = FLAG_VALUE
        .replace_all(&out, format!("$1{REDACTED}"))
        .into_owned();
    out = ASSIGNMENT
        .replace_all(&out, format!("$1$2{REDACTED}"))
        .into_owned();
    out = LONG_HEX.replace_all(&out, REDACTED).into_owned();
    LONG_BLOB
        .replace_all(&out, |caps: &regex::Captures<'_>| {
            activity_blob(caps.get(0).expect("match").as_str())
        })
        .into_owned()
}

/// A long mixed-case alphanumeric with a digit is random by construction. A
/// commit id, a path segment or a word is not, and is kept.
fn activity_blob(text: &str) -> String {
    let mixed = text.chars().any(|c| c.is_ascii_digit())
        && text.chars().any(|c| c.is_ascii_uppercase())
        && text.chars().any(|c| c.is_ascii_lowercase());
    if mixed {
        REDACTED.to_string()
    } else {
        text.to_string()
    }
}

static DUMP_COMMAND: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        r"(?i)\.config\.environment|\bconfig\.environment\b|\bprintenv\b",
        r"|(?:^|[;&|(]\s*|\bsudo\s+)env\s*(?:$|[;&|)])|\bdeclare\s+-x\b|\bexport\s+-p\b",
        r"|\bkubectl\s+config\s+view|kube/?config\b|\bk3s\.yaml\b",
        r"|\bcat\s+[^\s|;]*\.env\b|/proc/[^\s]*/environ\b",
        r"|\binfisical\s+(?:secrets|export|run)\b|\bsecrets?\s+(?:get|export|list|show)\b",
        r"|\bGetStack\b|\bGetVariable\b|\bListVariables\b|\bvault\s+(?:kv\s+)?read\b",
    ))
    .expect("pattern")
});
static KUBECONFIG_SHAPE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(concat!(
        r"(?is)client-(?:key|certificate)-data\s*:|certificate-authority-data\s*:",
        r"|\bkind\s*:\s*Config\b[\s\S]*\busers\s*:",
    ))
    .expect("pattern")
});
static ENV_LINE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?m)^\s*(?:export\s+)?[A-Z][A-Z0-9_]{2,}=\S").expect("pattern")
});

pub fn dumps_secrets(command: &str) -> bool {
    DUMP_COMMAND.is_match(command)
}

pub fn looks_like_dump(output: &str) -> bool {
    if KUBECONFIG_SHAPE.is_match(output).unwrap_or(false) {
        return true;
    }
    ENV_LINE
        .find_iter(output)
        .filter(|found| found.start() < SCAN_WINDOW)
        .count()
        >= 3
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cut(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(limit - 1).collect::<String>())
    }
}

pub fn excerpt(output: &str) -> String {
    if output.len() <= 2 * SCAN_WINDOW {
        let cleaned = one_line(&activity_redact(output));
        if cleaned.chars().count() <= EXCERPT_HEAD + EXCERPT_TAIL {
            return cleaned;
        }
        let head: String = cleaned.chars().take(EXCERPT_HEAD).collect();
        let tail: String = cleaned
            .chars()
            .rev()
            .take(EXCERPT_TAIL)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return format!("{head} … {tail}");
    }
    let head: String = one_line(&activity_redact(&output[..SCAN_WINDOW]))
        .chars()
        .take(EXCERPT_HEAD)
        .collect();
    let tail_src = one_line(&activity_redact(&output[output.len() - SCAN_WINDOW..]));
    let tail: String = tail_src
        .chars()
        .rev()
        .take(EXCERPT_TAIL)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head} … {tail}")
}

const SUMMARY_FIELDS: &[&str] = &[
    "command",
    "cmd",
    "file_path",
    "path",
    "notebook_path",
    "pattern",
    "query",
    "url",
    "description",
    "prompt",
    "skill",
];

/// One redacted line saying what a call did.
pub fn summarize_input(call_input: &serde_json::Value) -> String {
    let text = match call_input {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Object(map) => {
            let picked: Vec<&str> = SUMMARY_FIELDS
                .iter()
                .filter_map(|key| {
                    map.get(*key)
                        .and_then(serde_json::Value::as_str)
                        .filter(|text| !text.is_empty())
                })
                .collect();
            if picked.is_empty() {
                serde_json::to_string(call_input).unwrap_or_default()
            } else {
                picked.into_iter().take(2).collect::<Vec<_>>().join(" ")
            }
        }
        serde_json::Value::Array(parts) => parts
            .iter()
            .map(|part| match part {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(" "),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    };
    let window: String = text.chars().take(4 * SCAN_WINDOW).collect();
    cut(&one_line(&activity_redact(&window)), SUMMARY_LIMIT)
}

/// The default service heuristics. An operator's table extends and overrides it.
pub const DEFAULT_SERVICES: &[(&str, &str)] = &[
    (
        "github",
        r"\bgh\s+(?:pr|run|api|workflow|release|repo|issue|secret|variable|auth)\b|api\.github\.com|github\.com/|mcp__github",
    ),
    (
        "git",
        r"\bgit\s+(?:push|pull|fetch|clone|rebase|merge|commit|checkout|switch)\b",
    ),
    (
        "docker",
        r"\bdocker(?:\s+compose|-compose)?\s+\w+|\bghcr\.io\b|\bpodman\b",
    ),
    ("kubernetes", r"\bkubectl\b|\bhelm\s|\bk3s\b|\bkubeconfig\b"),
    (
        "komodo",
        r"\bkomodo\b|\bDeployStack\b|\bGetStack\b|\bWriteStackFile\b",
    ),
    ("infisical", r"\binfisical\b"),
    ("cloudflare", r"\bcloudflare|\bwrangler\b|\bcloudflared\b"),
    ("caddy", r"\bcaddy\b|\bCaddyfile\b"),
    ("tailscale", r"\btailscale\b|\btailnet\b"),
    ("dns", r"\bnslookup\b|\bdig\s+\S|\bresolvectl\b"),
    ("forgejo", r"\bforgejo\b|\bgitea\b"),
    ("woodpecker", r"\bwoodpecker\b"),
    ("firebase", r"\bfirebase\b|\bfcm\b"),
    (
        "play",
        r"\bandroidpublisher\b|\bfastlane\b|\bgoogle play\b|\bplay console\b",
    ),
    ("ssh", r#"(?:^|[\s;&|("'])(?:ssh|scp|rsync)\s"#),
    (
        "http",
        r#"(?:^|[\s;&|("'])(?:curl|wget|http)\s|\bWebFetch\b"#,
    ),
    ("vogt", r"\bmcp__vogt__|\bvogt\s+\w+"),
    ("cadastre", r"\bmcp__cadastre__"),
];

pub struct ServiceMatcher {
    patterns: Vec<(String, regex::Regex)>,
}

impl ServiceMatcher {
    pub fn build(overrides: &[(&str, &str)]) -> Self {
        let mut merged: Vec<(String, String)> = DEFAULT_SERVICES
            .iter()
            .map(|(name, pattern)| ((*name).to_string(), (*pattern).to_string()))
            .collect();
        for (name, pattern) in overrides {
            if pattern.is_empty() {
                merged.retain(|(existing, _)| existing != name);
            } else if let Some(slot) = merged.iter_mut().find(|(existing, _)| existing == name) {
                slot.1 = (*pattern).to_string();
            } else {
                merged.push(((*name).to_string(), (*pattern).to_string()));
            }
        }
        merged.sort_by(|left, right| left.0.cmp(&right.0));
        Self {
            patterns: merged
                .into_iter()
                .map(|(name, pattern)| {
                    (
                        name,
                        regex::RegexBuilder::new(&pattern)
                            .case_insensitive(true)
                            .build()
                            .expect("service pattern"),
                    )
                })
                .collect(),
        }
    }

    pub fn tags(&self, tool: &str, call_input: &serde_json::Value) -> Vec<String> {
        let text = match call_input {
            serde_json::Value::String(text) => text.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        let window: String = text.chars().take(4 * SCAN_WINDOW).collect();
        let haystack = format!("{tool} {window}");
        self.patterns
            .iter()
            .filter(|(_, pattern)| pattern.is_match(&haystack))
            .map(|(name, _)| name.clone())
            .collect()
    }
}

const SHELL_TOOLS: &[&str] = &[
    "Bash",
    "BashOutput",
    "exec",
    "exec_command",
    "shell",
    "write_stdin",
];
const FAILURE_WINDOW: usize = 1_000;

static EXIT_STATUS: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        r"(?im)^\s*(?:Script failed\b|Process exited with code [1-9]",
        r"|Exit code:?\s*[1-9]|exit status [1-9]|Command failed with exit code [1-9])",
    ))
    .expect("pattern")
});
static FAILURE_LINE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        r"(?im)^\s*(?:error\b|fatal:|ERROR\b|Error:|panic:",
        r"|Traceback \(most recent call last\)",
        r"|.*\bcommand not found\b|.*\bpermission denied\b|.*\bNo such file or directory\b",
        r"|.*\b(?:401 Unauthorized|403 Forbidden|HTTP (?:401|403|404|500|502|503|504))\b",
        r"|.*\bconnection refused\b|.*\b(?:timed out|deadline exceeded)\b|<tool_use_error>)",
    ))
    .expect("pattern")
});

pub fn is_error(output: &str, flagged: Option<bool>, tool: &str) -> bool {
    if flagged == Some(true) {
        return true;
    }
    let window: String = output.chars().take(FAILURE_WINDOW).collect();
    if EXIT_STATUS.is_match(&window) {
        return true;
    }
    if !SHELL_TOOLS.contains(&tool) {
        return false;
    }
    FAILURE_LINE.is_match(&window)
}

pub fn result_excerpt(output: &str, error: bool, withheld: bool) -> Option<String> {
    if !error {
        return None;
    }
    if withheld || looks_like_dump(output) {
        return Some(WITHHELD.to_string());
    }
    Some(excerpt(output))
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    /// Secrets assembled from pieces, the way the Python test builds them, so
    /// this file never holds a string a scanner would read as a credential.
    fn github_classic() -> String {
        format!("gh{}{}", "p_", "Zq3".repeat(12))
    }

    fn password() -> String {
        format!("{}{}{}", "Tr0ub4dor", "&3-", "horse-battery")
    }

    fn hex_key() -> String {
        "9f".repeat(32)
    }

    #[test]
    fn a_named_secret_and_a_token_shape_are_removed_and_a_commit_id_survives() {
        for (text, secret) in [
            (
                format!("export GH_TOKEN={}", github_classic()),
                github_classic(),
            ),
            (
                format!("DB_PASSWORD='{}' ./migrate", password()),
                password(),
            ),
            (format!("echo {} > key.bin", hex_key()), hex_key()),
        ] {
            let redacted = activity_redact(&text);
            assert!(!redacted.contains(&secret), "{redacted}");
            assert!(redacted.contains("REDACTED"));
        }
        let sha = "4a4cf5a0".repeat(5);
        let kept = format!("git show {sha} -- src/vogt/storage");
        assert_eq!(activity_redact(&kept), kept);
    }

    #[test]
    fn a_private_key_block_goes_whole_even_when_cut_off() {
        let body = format!(
            "-----BEGIN {}-----\n{}{}\n-----END {}-----",
            "OPENSSH PRIVATE KEY",
            "b3BlbnNzaC1rZXktdjEAAAAA",
            "BG5vbmUAAAAEbm9uZQ",
            "OPENSSH PRIVATE KEY"
        );
        let redacted = activity_redact(&format!("cat key\n{body}\ndone"));
        assert!(!redacted.contains("PRIVATE KEY"));
        assert!(!redacted.contains("b3BlbnNzaC1rZXkt"));
        assert!(redacted.contains("[REDACTED:pem]"));
        assert!(redacted.starts_with("cat key"));
        assert!(redacted.ends_with("done"));

        let cut = &body[..body.find("-----END").expect("end")];
        assert!(activity_redact(&format!("prefix {cut}")).contains("[REDACTED:pem]"));
    }

    #[test]
    fn a_dump_is_withheld_and_only_a_failure_keeps_an_excerpt() {
        assert!(dumps_secrets("printenv | sort"));
        assert!(dumps_secrets("kubectl config view --raw"));
        assert!(dumps_secrets("cat deploy/.env"));
        assert!(!dumps_secrets("git status --short"));
        assert_eq!(
            result_excerpt("anything", true, true).as_deref(),
            Some(WITHHELD)
        );
        let env = "HOME=/root\nPATH=/usr/bin\nSHELL=/bin/sh\n";
        assert!(looks_like_dump(env));
        assert!(!looks_like_dump("error: build failed\nsee log"));
        assert!(result_excerpt("all good", false, false).is_none());
        assert!(is_error("fatal: not a git repository", Some(false), "Bash"));
        assert!(!is_error(
            "Script completed\nWall time 1.0 seconds",
            None,
            ""
        ));
        let tags = ServiceMatcher::build(&[])
            .tags("Bash", &serde_json::json!({"command": "gh pr view 1"}));
        assert!(tags.contains(&"github".to_string()), "{tags:?}");
    }
}
