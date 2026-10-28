use std::collections::BTreeSet;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::{Moment, Observation, WorkItem, WorkOverlay, DONE, TERMINAL_STATES, WONT_DO};

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

#[allow(dead_code)] // The backlog sorts with this; the ranking test asserts the order.
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

/// The finished state an upstream close lands in.
///
/// A close the forge says was not planned is `wont_do`, not `done`: the work
/// was declined, not delivered. Anything else that closed is `done`.
pub fn closed_state(observation: &Observation) -> &'static str {
    if observation
        .payload
        .get("state_reason")
        .and_then(Value::as_str)
        == Some("not_planned")
    {
        return WONT_DO;
    }
    DONE
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
        return closed_state(observation).to_string();
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
        Some(Value::Array(labels)) => labels.iter().map(py_str).collect(),
        _ => Vec::new(),
    }
}

/// Whether anything actually said what kind of work this is. `work_kind_of`
/// has to return something, and for an unlabelled issue that something is a
/// guess; recording the guess keeps the subject discoverable.
pub fn is_classified(observation: &Observation) -> bool {
    if observation.kind == "marker" || observation.kind == "forge.pull_request" {
        return true;
    }
    !labels_of(observation).is_empty()
}

/// A one-line description, whatever kind of subject this is.
pub fn title_of(observation: &Observation) -> String {
    let payload = &observation.payload;
    if observation.kind == "marker" {
        let text = py_str_of(payload.get("text")).trim().to_string();
        let path = py_or(payload.get("path"), "?");
        let line = py_or(payload.get("line"), "?");
        let tag = py_or(payload.get("tag"), "TODO");
        let marker = format!("{tag} {path}:{line}");
        return if text.is_empty() {
            marker
        } else {
            format!("{marker} — {text}")
        };
    }
    let title = py_str_of(payload.get("title")).trim().to_string();
    match (title.is_empty(), payload.get("number")) {
        (false, Some(number)) if !number.is_null() => format!("#{} {title}", py_str(number)),
        (false, _) => title,
        (true, _) => observation.subject_key.clone(),
    }
}

/// `str()` of a JSON value the way Python renders one out of a dict payload: a
/// string is its text, a number its digits, `true`/`false`/`null` become
/// `True`/`False`/`None`.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(flag) => if *flag { "True" } else { "False" }.to_string(),
        Value::Null => "None".to_string(),
        Value::Number(number) => crate::decisions::python_number(number),
        other => other.to_string(),
    }
}

/// [`py_str`] of a payload field. A missing key is `""`.
fn py_str_of(value: Option<&Value>) -> String {
    match value {
        Some(value) => py_str(value),
        None => String::new(),
    }
}

/// Python's truthiness of a JSON value: `null`, `false`, `0`, `0.0`, `""`, `[]`
/// and `{}` are falsy, and everything else is truthy.
fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::String(text) => !text.is_empty(),
        Value::Number(number) => number.as_f64().is_none_or(|magnitude| magnitude != 0.0),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
        Value::Bool(true) => true,
    }
}

/// `str(payload.get(key, fallback))`: a missing key takes the fallback, a
/// present `null` renders as Python's `None`.
fn py_or(value: Option<&Value>, fallback: &str) -> String {
    match value {
        None => fallback.to_string(),
        Some(value) => py_str(value),
    }
}

/// `str()` of a JSON value: a string is its text, `true`/`false`/`null` become
/// `True`/`False`/`None`, and a list or dict uses Python's quoting — single
/// quotes, switching to double quotes when the text holds a `'` and no `"`.
pub(super) fn py_display(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(flag) => if *flag { "True" } else { "False" }.to_string(),
        Value::Null => "None".to_string(),
        Value::Number(number) => py_number(number),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(py_literal).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(key, item)| format!("{}: {}", py_quote(key), py_literal(item)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

fn py_literal(value: &Value) -> String {
    match value {
        Value::String(text) => py_quote(text),
        other => py_display(other),
    }
}

fn py_quote(text: &str) -> String {
    crate::core::py_repr(text)
}

/// `str` of a number. serde prints `0.00000001` where Python prints `1e-07`,
/// and pads a positive exponent with `+`.
fn py_number(number: &serde_json::Number) -> String {
    // An integer stays digits however large it is. Only a float takes an exponent.
    let Some(value) = number.as_f64().filter(|_| number.is_f64()) else {
        return number.to_string();
    };
    if value.is_finite() && value.abs() != 0.0 {
        let magnitude = value.abs().log10();
        if !(-4.0..16.0).contains(&magnitude) {
            // The shortest form, which round-trips the way Python's repr does,
            // then the exponent padded to two digits with an explicit sign.
            let rendered = format!("{value:e}");
            if let Some((mantissa, exponent)) = rendered.split_once('e') {
                let sign = if exponent.starts_with('-') { "-" } else { "+" };
                let digits = exponent.trim_start_matches(['+', '-']);
                return format!("{mantissa}e{sign}{digits:0>2}");
            }
        }
    }
    number.to_string()
}

/// The work-item subject keys a PR observation says it implements. Empty for
/// anything that is not a PR, or a PR that named no work — never a guess.
pub fn implemented_targets(observation: &Observation) -> std::collections::BTreeSet<String> {
    let mut targets = std::collections::BTreeSet::new();
    if observation.kind != "forge.pull_request" {
        return targets;
    }
    let Some(edges) = observation
        .payload
        .get("implements")
        .and_then(Value::as_array)
    else {
        return targets;
    };
    for edge in edges {
        // Python's `str(edge["subject"])` for any truthy value. Truthiness is the
        // value's, not the rendered text's: `0` is falsy, while a subject whose
        // text is literally `"None"` is real and must be kept.
        if let Some(value) = edge.get("subject").filter(|value| py_truthy(value)) {
            targets.insert(py_str(value));
        }
    }
    targets
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
    hex::encode(hasher.finalize().as_slice())[..6].to_string()
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

pub const COMPLIANT: &str = "compliant";
pub const NON_COMPLIANT: &str = "non_compliant";
pub const NOT_CHECKED: &str = "not_checked";
/// The one answer never stored on a project row: this project never adopted the
/// contract, so there is nothing to comply with, and no view may present it as a fault.
pub const NOT_APPLICABLE: &str = "not_applicable";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionResult {
    pub rule: String,
    pub target: String,
    pub satisfied: bool,
    pub detail: String,
    pub applicable: bool,
    pub tracked: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractResult {
    pub contract_version: String,
    pub path: String,
    pub status: String,
    pub criteria: Vec<CriterionResult>,
}

impl ContractResult {
    /// The criteria that failed and were not declared inapplicable.
    pub fn failing(&self) -> Vec<&CriterionResult> {
        self.criteria
            .iter()
            .filter(|criterion| !criterion.satisfied && criterion.applicable)
            .collect()
    }

    pub fn inapplicable(&self) -> Vec<&CriterionResult> {
        self.criteria
            .iter()
            .filter(|criterion| !criterion.applicable)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scaffold {
    pub directories: Vec<String>,
    pub files: Vec<ScaffoldFile>,
}

/// The minimum a new project needs to pass the stock contract. No licence text
/// is invented: picking one is the owner's decision.
pub fn default_scaffold(name: &str, owner: &str, lifecycle_state: &str) -> Scaffold {
    Scaffold {
        directories: Contract::stock().required_dirs,
        files: vec![
            ScaffoldFile {
                path: "README.md".into(),
                content: format!(
                    "# {name}\n\nOne line on what this is and who it is for.\n\n## Status\n\n`{lifecycle_state}`.\n"
                ),
            },
            ScaffoldFile {
                path: "AGENTS.md".into(),
                content: format!(
                    "# {name} — Agent Guidance\n\n## Where things live\n\n- `docs/` — first-class documentation.\n- `design/` — diagrams, mockups, exploratory notes (may be messy).\n- `src/` — implementation.\n\n## Ground rules\n\n- Record decisions where the next reader will look for them.\n"
                ),
            },
            ScaffoldFile {
                path: "LICENSE".into(),
                content: format!(
                    "Copyright (c) {owner}\n\nTODO: choose a licence and replace this file with its full text.\n"
                ),
            },
        ],
    }
}

/// What the evaluator may know about a path. The caller resolves `~` and
/// symlinks; this stays a pure read of the answers.
pub trait ContractTree {
    fn is_dir(&self, path: &str) -> bool;
    fn is_file(&self, path: &str) -> bool;
}

/// Check a folder against a contract. Advisory: the result is a value to read
/// and it blocks nothing. `tracked` is the set of top-level names the
/// repository carries, from a caller that was allowed to ask git, and `None`
/// where nobody could ask. A criterion met on disk but absent from that set
/// fails, because a clone would not have it. `inapplicable` maps
/// `(rule, target)` to the reason it cannot apply here.
pub fn evaluate(
    path: &str,
    contract: &Contract,
    tree: &dyn ContractTree,
    tracked: Option<&std::collections::BTreeSet<String>>,
    inapplicable: &[(&str, &str, &str)],
) -> ContractResult {
    let path = normalise_path(path);
    let path = path.as_str();
    if !tree.is_dir(path) {
        return ContractResult {
            contract_version: contract.version.clone(),
            path: path.to_string(),
            status: NOT_CHECKED.to_string(),
            criteria: vec![CriterionResult {
                rule: "path.exists".into(),
                target: path.to_string(),
                satisfied: false,
                detail: "the path does not exist or is not a directory, so no criterion below was evaluated — this is 'not checked', not 'does not comply'".into(),
                applicable: true,
                tracked: None,
            }],
        };
    }
    let mut criteria = vec![CriterionResult {
        rule: "path.exists".into(),
        target: path.to_string(),
        satisfied: true,
        detail: "directory exists".into(),
        applicable: true,
        tracked: None,
    }];
    for name in &contract.required_files {
        criteria.push(exempted(
            criterion(
                "required_file",
                name,
                tree.is_file(&child(path, name)),
                tracked,
            ),
            inapplicable,
        ));
    }
    for name in &contract.required_dirs {
        criteria.push(exempted(
            criterion(
                "required_dir",
                name,
                tree.is_dir(&child(path, name)),
                tracked,
            ),
            inapplicable,
        ));
    }
    let failing = criteria
        .iter()
        .any(|item| !item.satisfied && item.applicable);
    ContractResult {
        contract_version: contract.version.clone(),
        path: path.to_string(),
        status: if failing { NON_COMPLIANT } else { COMPLIANT }.to_string(),
        criteria,
    }
}

fn child(root: &str, name: &str) -> String {
    if name.starts_with('/') {
        return name.to_string();
    }
    if root.ends_with('/') {
        format!("{root}{name}")
    } else {
        format!("{root}/{name}")
    }
}

/// `str(Path(path).expanduser())`: `~` is the home directory, a trailing slash
/// and a redundant `.` or doubled separator collapse, and an empty path is `.`.
/// `..` is kept, because this is not a resolve.
fn normalise_path(path: &str) -> String {
    let expanded = if path == "~" || path.starts_with("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        if path == "~" {
            home
        } else {
            format!("{home}{}", &path[1..])
        }
    } else {
        path.to_string()
    };
    if expanded.is_empty() {
        return ".".to_string();
    }
    let absolute = expanded.starts_with('/');
    let parts: Vec<&str> = expanded
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".into()
    } else {
        joined
    }
}

fn criterion(
    rule: &str,
    name: &str,
    on_disk: bool,
    tracked: Option<&std::collections::BTreeSet<String>>,
) -> CriterionResult {
    let suffix = if rule == "required_dir" { "/" } else { "" };
    let (satisfied, detail, carried) = if !on_disk {
        (
            false,
            format!("{name}{suffix} is missing"),
            tracked.map(|_| false),
        )
    } else if tracked.is_none() {
        (true, "present".to_string(), None)
    } else if tracked.is_some_and(|names| names.contains(name)) {
        (true, "present".to_string(), Some(true))
    } else {
        (
            false,
            format!(
                "{name}{suffix} is present in the working tree but not tracked, so no clone of this repository has it"
            ),
            Some(false),
        )
    };
    CriterionResult {
        rule: rule.to_string(),
        target: name.to_string(),
        satisfied,
        detail,
        applicable: true,
        tracked: carried,
    }
}

/// A declaration does not make the criterion satisfied — the file is still
/// absent. It makes it not-counted, and only when it would otherwise fail.
fn exempted(mut result: CriterionResult, inapplicable: &[(&str, &str, &str)]) -> CriterionResult {
    let reason = inapplicable
        .iter()
        .rev()
        .find_map(|(rule, target, reason)| {
            (*rule == result.rule && *target == result.target).then_some(*reason)
        });
    if let Some(reason) = reason.filter(|_| !result.satisfied) {
        result.applicable = false;
        result.detail = format!("declared inapplicable to this project: {reason}");
    }
    result
}

pub const JUDGEMENT_TARGETS: &[(&str, &str)] = &[
    ("LICENSE", "choose a licence for this project and write its full text to LICENSE. Vogt deliberately does not pick one: a scaffold that silently writes MIT makes that decision for the owner."),
    ("design", "decide what this project's design record is — diagrams, mockups, exploratory notes — and put it in design/. An empty directory satisfies the letter of the criterion and none of its point, and git cannot carry one anyway."),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    pub rule: String,
    pub target: String,
    pub remedy: String,
    pub instruction: String,
}

/// What would close each failing criterion. Advisory: nothing here applies
/// anything, and a criterion declared inapplicable has nothing to recommend.
pub fn recommendations(
    result: &ContractResult,
    scaffold: Option<&Scaffold>,
) -> Vec<Recommendation> {
    let written: std::collections::BTreeSet<&str> = scaffold
        .map(|scaffold| {
            scaffold
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect()
        })
        .unwrap_or_default();
    let directories: std::collections::BTreeSet<&str> = scaffold
        .map(|scaffold| scaffold.directories.iter().map(String::as_str).collect())
        .unwrap_or_default();
    result
        .failing()
        .into_iter()
        .map(|criterion| {
            let (remedy, instruction) = if let Some((_, text)) = JUDGEMENT_TARGETS
                .iter()
                .find(|(target, _)| *target == criterion.target)
            {
                ("judgement", (*text).to_string())
            } else if written.contains(criterion.target.as_str())
                || directories.contains(criterion.target.as_str())
            {
                let slash = if criterion.rule == "required_dir" { "/" } else { "" };
                (
                    "scaffold",
                    format!(
                        "`project scaffold` writes {}{slash} into this project without overwriting anything that is already there.",
                        criterion.target
                    ),
                )
            } else if criterion.tracked == Some(false) {
                (
                    "judgement",
                    format!(
                        "{} is in the working tree but no clone carries it. Commit it, or decide it does not belong in this repository.",
                        criterion.target
                    ),
                )
            } else {
                (
                    "judgement",
                    format!(
                        "decide what {} should contain for this project and add it. Nothing here can write it for you.",
                        criterion.target
                    ),
                )
            };
            Recommendation {
                rule: criterion.rule.clone(),
                target: criterion.target.clone(),
                remedy: remedy.to_string(),
                instruction,
            }
        })
        .collect()
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
    let stripped = value.trim_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{1f}')
    });
    stripped.trim_start_matches(['v', 'V']).to_string()
}

pub fn auto_acceptable(kind: &str) -> bool {
    AUTO_ACCEPTABLE_KINDS.contains(&kind)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceSnapshot {
    pub subject_key: String,
    pub content_digest: String,
    pub observed_at: crate::core::Moment,
    pub collector: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftFinding {
    pub kind: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub summary: String,
    pub proposed_change: serde_json::Value,
    pub project_id: Option<String>,
    pub evidence_observation_id: Option<String>,
    pub evidence: Option<EvidenceSnapshot>,
}

#[allow(clippy::too_many_arguments)]
fn finding(
    kind: &str,
    subject_kind: &str,
    subject_id: &str,
    project_id: Option<&str>,
    summary: String,
    proposed_change: serde_json::Value,
    evidence: Option<EvidenceSnapshot>,
    evidence_observation_id: Option<&str>,
) -> DriftFinding {
    DriftFinding {
        kind: kind.to_string(),
        subject_kind: subject_kind.to_string(),
        subject_id: subject_id.to_string(),
        summary,
        proposed_change,
        project_id: project_id.map(str::to_string),
        evidence_observation_id: evidence_observation_id.map(str::to_string),
        evidence,
    }
}

/// Declared version against the newest release actually seen. A leading `v` is
/// not a difference.
#[allow(clippy::too_many_arguments)]
pub fn version_mismatch(
    project_id: &str,
    project_slug: &str,
    declared: Option<&str>,
    observed: &str,
    evidence: EvidenceSnapshot,
    evidence_observation_id: Option<&str>,
) -> Option<DriftFinding> {
    if declared.is_some_and(|declared| normalise_version(declared) == normalise_version(observed)) {
        return None;
    }
    let stated = declared
        .map(crate::core::py_repr)
        .unwrap_or_else(|| "nothing".into());
    Some(finding(
        VERSION_MISMATCH,
        "project",
        project_id,
        Some(project_id),
        format!(
            "{project_slug} declares {stated} but the newest observed release is {}",
            crate::core::py_repr(observed)
        ),
        serde_json::json!({
            "entity": "project", "field": "current_version",
            "from": declared, "to": observed,
        }),
        Some(evidence),
        evidence_observation_id,
    ))
}

fn where_in(manifest: Option<&str>) -> String {
    manifest
        .filter(|name| !name.is_empty())
        .map(|name| format!(" in {name}"))
        .unwrap_or_default()
}

/// A path reference inside the project's own tree that resolves to nothing.
#[allow(clippy::too_many_arguments)]
pub fn broken_path_dependency(
    subject_key: &str,
    project_id: &str,
    project_slug: &str,
    raw_target: &str,
    manifest: Option<&str>,
    evidence: EvidenceSnapshot,
    evidence_observation_id: Option<&str>,
) -> DriftFinding {
    finding(
        "broken_path_dependency",
        "dependency",
        subject_key,
        Some(project_id),
        format!(
            "{project_slug} references {}{}, which is inside this project and does not exist",
            crate::core::py_repr(raw_target),
            where_in(manifest)
        ),
        serde_json::json!({
            "action": "fix_manifest_or_restore_path",
            "raw_target": raw_target, "manifest": manifest,
        }),
        Some(evidence),
        evidence_observation_id,
    )
}

/// An internal-looking reference whose target is not a registered project.
#[allow(clippy::too_many_arguments)]
pub fn unresolved_dependency(
    subject_key: &str,
    project_id: &str,
    project_slug: &str,
    raw_target: &str,
    manifest: Option<&str>,
    evidence: EvidenceSnapshot,
    evidence_observation_id: Option<&str>,
) -> DriftFinding {
    finding(
        "unresolved_dependency",
        "dependency",
        subject_key,
        Some(project_id),
        format!(
            "{project_slug} references {}{}, which is not a registered project",
            crate::core::py_repr(raw_target),
            where_in(manifest)
        ),
        serde_json::json!({
            "action": "register_or_ignore", "raw_target": raw_target, "manifest": manifest,
        }),
        Some(evidence),
        evidence_observation_id,
    )
}

fn last_observed_open(
    subject_key: &str,
    work_ref: &str,
    declared_state: &str,
    evidence: &EvidenceSnapshot,
) -> String {
    format!(
        "{subject_key} was open when last observed ({}), but {work_ref} is {} here — the incremental sync reads all states, so this is an observed reopen, not a close it failed to see",
        evidence.observed_at.to_iso(),
        crate::core::py_repr(declared_state)
    )
}

/// A linked issue's state disagrees with the item's.
///
/// A not-planned close proposes `wont_do`, the same finished state the
/// upstream mapping gives it, so accepting does not turn a declined item into
/// a delivered one.
#[allow(clippy::too_many_arguments)]
pub fn forge_state_mismatch(
    work_item_id: &str,
    work_ref: &str,
    declared_state: &str,
    upstream_state: &str,
    subject_key: &str,
    project_id: Option<&str>,
    evidence: EvidenceSnapshot,
    evidence_observation_id: Option<&str>,
    upstream_reason: Option<&str>,
) -> DriftFinding {
    let closed_to = if upstream_reason == Some("not_planned") {
        WONT_DO
    } else {
        DONE
    };
    let summary = if upstream_state == "closed" {
        format!(
            "{subject_key} is closed upstream, but {work_ref} is {} here",
            crate::core::py_repr(declared_state)
        )
    } else {
        last_observed_open(subject_key, work_ref, declared_state, &evidence)
    };
    finding(
        "forge_state_mismatch",
        "work_item",
        work_item_id,
        project_id,
        summary,
        serde_json::json!({
            "entity": "work_item", "field": "state",
            "from": declared_state,
            "to": if upstream_state == "closed" { closed_to } else { "open" },
            "work_ref": work_ref,
        }),
        Some(evidence),
        evidence_observation_id,
    )
}

/// A tracking-issue checkbox disagrees with the member's state. One proposal
/// per member, so two boxes out of step stay two questions.
#[allow(clippy::too_many_arguments)]
pub fn initiative_checkbox_drift(
    initiative_id: &str,
    initiative_slug: &str,
    project_id: Option<&str>,
    subject_key: &str,
    number: i64,
    work_ref: &str,
    upstream_checked: bool,
    expected_checked: bool,
    evidence: EvidenceSnapshot,
    evidence_observation_id: Option<&str>,
) -> DriftFinding {
    let ticked = if upstream_checked {
        "ticked"
    } else {
        "unticked"
    };
    let ought = if expected_checked {
        "done/terminal"
    } else {
        "still open"
    };
    finding(
        "initiative_checkbox_drift",
        "initiative",
        &format!("{initiative_id}:{number}"),
        project_id,
        format!(
            "initiative {}: {work_ref} (#{number}) is {ticked} on the tracking issue but is {ought} here",
            crate::core::py_repr(initiative_slug)
        ),
        serde_json::json!({
            "entity": "work_item", "initiative": initiative_slug, "work_ref": work_ref,
            "subject_key": subject_key, "number": number,
            "upstream_checked": upstream_checked, "expected_checked": expected_checked,
        }),
        Some(evidence),
        evidence_observation_id,
    )
}

/// A linked forge object is absent within a sweep that provably covered it.
pub fn vanished_upstream(
    work_item_id: &str,
    work_ref: &str,
    subject_key: &str,
    project_id: Option<&str>,
    swept_at: crate::core::Moment,
) -> DriftFinding {
    finding(
        "vanished_upstream",
        "work_item",
        work_item_id,
        project_id,
        format!(
            "{work_ref} links {subject_key}, which a completed sweep at {} did not find",
            swept_at.to_iso(),
        ),
        serde_json::json!({
            "entity": "work_link", "action": "review",
            "subject_key": subject_key, "work_ref": work_ref,
        }),
        Some(EvidenceSnapshot {
            subject_key: subject_key.to_string(),
            content_digest: String::new(),
            observed_at: swept_at,
            collector: "forge-issues".into(),
            payload: serde_json::json!({"absent_in_completed_sweep": true}),
        }),
        None,
    )
}

/// CI is red on the default branch while the project claims to be fine.
#[allow(clippy::too_many_arguments)]
pub fn ci_red_vs_healthy(
    project_id: &str,
    project_slug: &str,
    lifecycle_state: &str,
    failing: &[&str],
    revision: &str,
    evidence: EvidenceSnapshot,
    evidence_observation_id: Option<&str>,
) -> DriftFinding {
    let mut failing: Vec<&str> = failing.to_vec();
    failing.sort_unstable();
    let head: String = revision.chars().take(12).collect();
    finding(
        "ci_red_vs_healthy",
        "project",
        project_id,
        Some(project_id),
        format!(
            "{project_slug} is {} but {} check(s) failed on {head}: {}",
            crate::core::py_repr(lifecycle_state),
            failing.len(),
            failing.join(", ")
        ),
        serde_json::json!({
            "entity": "project", "action": "review",
            "failing": failing, "revision": revision,
        }),
        Some(evidence),
        evidence_observation_id,
    )
}

/// A work item and the issue its own text names disagree. It proposes nothing
/// and applies nothing.
#[allow(clippy::too_many_arguments)]
pub fn referenced_issue_state_mismatch(
    work_item_id: &str,
    work_ref: &str,
    declared_state: &str,
    upstream_state: &str,
    subject_key: &str,
    project_id: Option<&str>,
    evidence: EvidenceSnapshot,
    evidence_observation_id: Option<&str>,
) -> DriftFinding {
    let finished_here = if matches!(declared_state, "done" | "wont_do") {
        "finished"
    } else {
        "open"
    };
    finding(
        "referenced_issue_state_mismatch",
        "work_item",
        work_item_id,
        project_id,
        format!(
            "{work_ref} references {subject_key}, which was {upstream_state} when last observed ({}), while {work_ref} is {} — {finished_here} here, {upstream_state} there",
            evidence.observed_at.to_iso(),
            crate::core::py_repr(declared_state)
        ),
        serde_json::json!({
            "entity": "work_item", "action": "review", "subject_key": subject_key,
            "work_ref": work_ref, "declared_state": declared_state,
            "upstream_state": upstream_state,
        }),
        Some(evidence),
        evidence_observation_id,
    )
}

/// One of the three automation facts is off, named so the reader knows which.
#[allow(clippy::too_many_arguments)]
pub fn update_automation_gap(
    project_id: &str,
    project_slug: &str,
    missing: &[&str],
    evidence: EvidenceSnapshot,
    evidence_observation_id: Option<&str>,
) -> DriftFinding {
    let mut missing: Vec<&str> = missing.to_vec();
    missing.sort_unstable();
    finding(
        "update_automation_gap",
        "project",
        project_id,
        Some(project_id),
        format!(
            "{project_slug} is missing {} of the three update automation facts: {}",
            missing.len(),
            missing.join(", ")
        ),
        serde_json::json!({
            "entity": "repository_settings", "action": "enable", "missing": missing,
        }),
        Some(evidence),
        evidence_observation_id,
    )
}

/// Subject keys a text names, as `gh:owner/repo#number`. Bare `#44` is not a
/// reference: it is the least decidable form, and WI-16's title uses it for a
/// pull request.
pub fn issue_references(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let push = |keys: &mut Vec<String>, owner: &str, repo: &str, number: &str| {
        let repo = repo.strip_suffix(".git").unwrap_or(repo);
        let key = format!("gh:{owner}/{repo}#{number}");
        if !keys.contains(&key) {
            keys.push(key);
        }
    };
    for (_, parts) in scan(text, true) {
        push(&mut keys, parts.0, parts.1, parts.2);
    }
    for (_, parts) in scan(text, false) {
        push(&mut keys, parts.0, parts.1, parts.2);
    }
    keys
}

fn scan(text: &str, urls: bool) -> Vec<(usize, (&str, &str, &str))> {
    let mut found = Vec::new();
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        let hit = if urls {
            match_url(rest)
        } else if index == 0 || !py_word(text[..index].chars().next_back().unwrap()) {
            match_qualified(rest)
        } else {
            None
        };
        if let Some((end, parts)) = hit {
            found.push((index, parts));
            index += end;
            continue;
        }
        index += rest.chars().next().unwrap().len_utf8();
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
    if boundary.is_some_and(py_word) {
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

/// Python's `\w`: a letter or number, or `_`.
fn py_word(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
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
        assert_eq!(crate::decisions::canonical_json(&left), r#"{"a":2,"b":1}"#);
        assert_eq!(
            crate::decisions::digest_of(&left),
            crate::decisions::digest_of(&right)
        );
        // Pinned against Python json.dumps(ensure_ascii=True): the em dash and
        // the accented e must be \u escapes or the digest diverges.
        let titled = serde_json::json!({"title": "fix — crash é"});
        assert_eq!(
            crate::decisions::canonical_json(&titled),
            r#"{"title":"fix \u2014 crash \u00e9"}"#
        );
        assert_eq!(
            crate::decisions::digest_of(&titled),
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
                crate::decisions::canonical_json(&serde_json::json!(value)),
                expected,
                "{value}"
            );
        }
        assert_eq!(
            crate::decisions::canonical_json(&serde_json::json!("\u{007f}")),
            r#""\u007f""#
        );
        assert_ne!(
            crate::decisions::digest_of(&left),
            crate::decisions::digest_of(&serde_json::json!({"a": 2, "b": 3}))
        );
        assert!(crate::decisions::digest_of(&left).starts_with("sha256:"));
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
            source_url: None,
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
            source_url: None,
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
            ..marker.clone()
        }));

        let merged = Observation {
            kind: "forge.pull_request".into(),
            payload: serde_json::json!({"state": "merged"}),
            ..base.clone()
        };
        assert_eq!(lifecycle_of(&merged), "closed");
        assert_eq!(upstream_state(&merged, None, "open"), "done");

        assert!(is_classified(&base));
        assert!(!is_classified(&Observation {
            payload: serde_json::json!({}),
            ..base.clone()
        }));
        assert!(is_classified(&marker));
        assert!(is_classified(&merged));

        let titled = Observation {
            payload: serde_json::json!({"title": " Crash ", "number": 7}),
            ..base.clone()
        };
        assert_eq!(title_of(&titled), "#7 Crash");
        assert_eq!(
            title_of(&Observation {
                payload: serde_json::json!({"number": 7}),
                ..base.clone()
            }),
            "gh:acme/app#1"
        );
        let noted = Observation {
            payload: serde_json::json!({"text": " fix me ", "path": "a.rs", "line": 12, "tag": "FIXME"}),
            ..marker.clone()
        };
        assert_eq!(title_of(&noted), "FIXME a.rs:12 — fix me");
        assert_eq!(
            title_of(&Observation {
                payload: serde_json::json!({"line": null}),
                ..marker
            }),
            "TODO ?:None"
        );

        let linked = Observation {
            payload: serde_json::json!({"implements": [
                {"subject": "wi:1"}, {"subject": ""}, "nope", {"other": 1}, {"subject": "wi:2"}
            ]}),
            ..merged
        };
        assert_eq!(
            implemented_targets(&linked),
            ["wi:1", "wi:2"].into_iter().map(str::to_string).collect()
        );
        assert!(implemented_targets(&base).is_empty());
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    struct Tree {
        dirs: Vec<String>,
        files: Vec<String>,
    }

    impl ContractTree for Tree {
        fn is_dir(&self, path: &str) -> bool {
            self.dirs.iter().any(|dir| dir == path)
        }
        fn is_file(&self, path: &str) -> bool {
            self.files.iter().any(|file| file == path)
        }
    }

    #[test]
    fn a_missing_root_is_not_checked_rather_than_non_compliant() {
        let tree = Tree {
            dirs: vec![],
            files: vec![],
        };
        let result = evaluate("/work/app", &Contract::stock(), &tree, None, &[]);
        assert_eq!(result.status, NOT_CHECKED);
        assert_eq!(result.criteria.len(), 1);
        assert_eq!(result.criteria[0].rule, "path.exists");
        let collapsed = evaluate("/work//app/", &Contract::stock(), &tree, None, &[]);
        assert_eq!(collapsed.path, "/work/app");
        let empty = evaluate("", &Contract::stock(), &tree, None, &[]);
        assert_eq!(empty.path, ".");
    }

    #[test]
    fn a_file_on_disk_but_untracked_fails_and_an_exemption_is_not_counted() {
        let tree = Tree {
            dirs: vec![
                "/work/app".into(),
                "/work/app/docs".into(),
                "/work/app/src".into(),
            ],
            files: vec![
                "/work/app/AGENTS.md".into(),
                "/work/app/README.md".into(),
                "/work/app/LICENSE".into(),
            ],
        };
        let tracked = ["AGENTS.md", "README.md", "LICENSE", "docs", "src"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let result = evaluate("/work/app", &Contract::stock(), &tree, Some(&tracked), &[]);
        assert_eq!(result.status, NON_COMPLIANT);
        let design = result
            .criteria
            .iter()
            .find(|item| item.target == "design")
            .expect("design");
        assert_eq!(design.detail, "design/ is missing");

        let exempted = evaluate(
            "/work/app",
            &Contract::stock(),
            &tree,
            Some(&tracked),
            &[(
                "required_dir",
                "design",
                "a cargo workspace has no design record",
            )],
        );
        assert_eq!(exempted.status, COMPLIANT);
        assert_eq!(exempted.inapplicable().len(), 1);
        assert!(exempted.inapplicable()[0]
            .detail
            .contains("cargo workspace"));

        let untracked = ["AGENTS.md", "README.md", "docs", "design", "src"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let loose = evaluate(
            "/work/app",
            &Contract::stock(),
            &tree,
            Some(&untracked),
            &[],
        );
        let licence = loose
            .criteria
            .iter()
            .find(|item| item.target == "LICENSE")
            .expect("licence");
        assert!(!licence.satisfied);
        assert_eq!(licence.tracked, Some(false));
    }

    #[test]
    fn a_scaffold_remedy_is_named_and_a_licence_is_a_judgement() {
        let scaffold = default_scaffold("vogt", "Ada", "active");
        assert!(scaffold
            .files
            .iter()
            .any(|file| file.path == "LICENSE" && file.content.contains("Copyright (c) Ada")));
        let tree = Tree {
            dirs: vec!["/work/app".into()],
            files: vec![],
        };
        let result = evaluate("/work/app", &Contract::stock(), &tree, None, &[]);
        let advice = recommendations(&result, Some(&scaffold));
        let licence = advice
            .iter()
            .find(|item| item.target == "LICENSE")
            .expect("licence");
        assert_eq!(licence.remedy, "judgement");
        let readme = advice
            .iter()
            .find(|item| item.target == "README.md")
            .expect("readme");
        assert_eq!(readme.remedy, "scaffold");
        assert!(readme
            .instruction
            .contains("`project scaffold` writes README.md "));
    }
}

#[cfg(test)]
mod r49 {
    #[test]
    fn kubeconfig_and_marked_token() {
        assert!(crate::decisions::looks_like_dump(
            "kind: Config\nclusters:\n- x\nusers:\n"
        ));
        assert!(crate::decisions::looks_like_dump("kind:Config xyz users:"));
        let key = format!("\t\u{345}sk-{}{}", "ab".repeat(9), "\u{216b}");
        assert!(
            !crate::decisions::activity_redact(&key).contains("sk-"),
            "{}",
            crate::decisions::activity_redact(&key)
        );
        let split = crate::decisions::split_command("--effort\\\n\u{1c}\rclaude");
        assert_eq!(split, vec!["--effort\n\u{1c}", "claude"]);
        // Under case-insensitive matching a combining mark that folds to a
        // letter must stay a boundary, or the credential after it survives.
        for leaked in [
            "\u{345}password=hunter2",
            "\u{345}Authorization: s3cr3tvalue",
            "\u{345}Bearer abcdefghijkl",
        ] {
            assert!(
                !crate::decisions::activity_redact(leaked).contains("hunter2"),
                "{leaked}"
            );
            assert!(
                !crate::decisions::activity_redact(leaked).contains("s3cr3tvalue"),
                "{leaked}"
            );
            assert!(
                !crate::decisions::activity_redact(leaked).contains("abcdefghijkl"),
                "{leaked}"
            );
        }
        assert!(crate::decisions::dumps_secrets("\u{345}printenv"));
        assert!(crate::decisions::looks_like_dump(
            "\u{345}kind: Config\nusers:"
        ));
    }
}

#[cfg(test)]
mod drift_tests {
    use super::*;

    fn evidence() -> EvidenceSnapshot {
        EvidenceSnapshot {
            subject_key: "gh:acme/app#1".into(),
            content_digest: "d".into(),
            observed_at: crate::core::Moment::from_unix(1_760_000_000, 0),
            collector: "forge-issues".into(),
            payload: serde_json::json!({}),
        }
    }

    #[test]
    fn a_leading_v_is_not_a_version_difference() {
        assert!(version_mismatch("p", "app", Some("v1.4"), "1.4", evidence(), None).is_none());
        let found = version_mismatch("p", "app", None, "1.5", evidence(), None).unwrap();
        assert!(found.summary.contains("declares nothing"));
        assert_eq!(found.proposed_change["to"], "1.5");
        assert!(auto_acceptable(&found.kind));
        assert!(!auto_acceptable("unresolved_dependency"));
    }

    #[test]
    fn a_bare_number_is_not_an_issue_reference() {
        let text =
            "see Regression from #43 and https://github.com/acme/app/issues/44 plus acme/app#7";
        assert_eq!(
            issue_references(text),
            vec!["gh:acme/app#44".to_string(), "gh:acme/app#7".to_string()]
        );
        assert_eq!(
            issue_references("https://github.com/acme/app.git/issues/2"),
            vec!["gh:acme/app#2".to_string()]
        );
        assert!(issue_references("_a/b#3").is_empty());
        assert_eq!(
            issue_references("a/b#1 https://github.com/c/d/issues/2"),
            vec!["gh:c/d#2".to_string(), "gh:a/b#1".to_string()]
        );
        assert!(version_mismatch("p", "app", Some("1.4\u{1f}"), "1.4", evidence(), None).is_none());
        let broken =
            broken_path_dependency("dep", "p", "app", "../missing", Some(""), evidence(), None);
        assert!(!broken.summary.contains(" in ,"));
    }

    #[test]
    fn a_closed_upstream_proposes_done_and_an_open_one_says_so() {
        let closed = forge_state_mismatch(
            "wi",
            "WI-1",
            "open",
            "closed",
            "gh:acme/app#1",
            None,
            evidence(),
            None,
            Some("completed"),
        );
        assert_eq!(closed.proposed_change["to"], "done");
        let open = forge_state_mismatch(
            "wi",
            "WI-1",
            "done",
            "open",
            "gh:acme/app#1",
            None,
            evidence(),
            None,
            None,
        );
        assert!(open.summary.contains("observed reopen"));
        assert_eq!(open.proposed_change["to"], "open");
    }

    #[test]
    fn a_not_planned_close_proposes_wont_do() {
        // WI-1752: accepting the drift must not turn declined work into done.
        let declined = forge_state_mismatch(
            "wi",
            "WI-1",
            "open",
            "closed",
            "gh:acme/app#1",
            None,
            evidence(),
            None,
            Some("not_planned"),
        );
        assert_eq!(declined.proposed_change["to"], "wont_do");
    }

    #[test]
    fn an_upstream_close_finishes_done_or_wont_do_by_its_reason() {
        let completed = observed(
            "forge.issue",
            serde_json::json!({"state": "closed", "state_reason": "completed"}),
        );
        assert_eq!(closed_state(&completed), "done");
        assert_eq!(upstream_state(&completed, None, "open"), "done");
        let unsaid = observed("forge.issue", serde_json::json!({"state": "closed"}));
        assert_eq!(upstream_state(&unsaid, None, "open"), "done");
        let declined = observed(
            "forge.issue",
            serde_json::json!({"state": "closed", "state_reason": "not_planned"}),
        );
        assert_eq!(closed_state(&declined), "wont_do");
        assert_eq!(upstream_state(&declined, None, "open"), "wont_do");
    }

    fn observed(kind: &str, payload: serde_json::Value) -> Observation {
        Observation {
            id: "obs_1".into(),
            sweep_id: "swp_1".into(),
            collector: "test".into(),
            kind: kind.into(),
            project_id: None,
            subject_key: "subj".into(),
            payload,
            content_digest: String::new(),
            source_url: None,
            promoted: false,
            observed_at: Moment::from_unix(0, 0),
        }
    }

    #[test]
    fn a_pull_request_implements_a_subject_that_is_not_text() {
        // Python's `str(edge["subject"])`: a numeric subject is the subject
        // `"12"`, and a boolean renders capitalised the way `str(True)` does.
        let pull = observed(
            "forge.pull_request",
            serde_json::json!({"implements": [
                {"subject": 12}, {"subject": true}, {"subject": false}, {"subject": null}
            ]}),
        );
        let targets = implemented_targets(&pull);
        assert!(targets.contains("12"));
        assert!(targets.contains("True"));
        assert!(!targets.contains("False"));
        assert!(!targets.contains("None"));

        // Falsy subjects are skipped, including a zero and an empty collection,
        // but a subject whose text is literally "None" is real.
        let edges = observed(
            "forge.pull_request",
            serde_json::json!({"implements": [
                {"subject": 0}, {"subject": 0.0}, {"subject": ""},
                {"subject": []}, {"subject": {}}, {"subject": "None"}, {"subject": "False"}
            ]}),
        );
        let kept = implemented_targets(&edges);
        assert_eq!(
            kept.into_iter().collect::<Vec<_>>(),
            vec!["False".to_string(), "None".to_string()]
        );
    }

    #[test]
    fn a_marker_title_reads_a_non_text_path_and_line() {
        let marker = observed(
            "marker",
            serde_json::json!({"tag": "TODO", "path": 7, "line": 3}),
        );
        assert_eq!(title_of(&marker), "TODO 7:3");
    }
}
