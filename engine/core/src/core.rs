//! Domain types. Ports `src/vogt/core/`.
//!
//! This chunk is ids, clock, principal and the workflow machine. Entities are
//! the next chunk: workflow only needs the `WorkKind` and `LifecycleState`
//! literals, which are declared here so the machine does not wait on the
//! models. Nothing here reads storage or configuration. `new_id` and
//! `local_principal` take their clock, randomness and username as arguments.
//! The binary does not call this yet, so the public surface is allowed.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::errors::VogtError;

const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const TIME_CHARS: usize = 10;
const RANDOM_CHARS: usize = 16;

pub fn new_ulid(timestamp_ms: u64, randomness: u128) -> String {
    let mut out = encode(timestamp_ms, TIME_CHARS);
    // Randomness is 80 bits. Truncate so a wider integer cannot leak into the
    // time half.
    out.push_str(&encode(randomness & ((1_u128 << 80) - 1), RANDOM_CHARS));
    out
}

fn encode(value: impl Into<u128>, length: usize) -> String {
    let mut value = value.into();
    let mut chars = vec![0_u8; length];
    for slot in chars.iter_mut().rev() {
        *slot = ALPHABET[(value & 0x1F) as usize];
        value >>= 5;
    }
    String::from_utf8(chars).expect("alphabet is ascii")
}

/// `{prefix}_{26 chars}`. The caller supplies the millisecond clock and 80
/// bits of randomness; Python's `new_id` reads both from the process.
pub fn new_id(prefix: &str, timestamp_ms: u64, randomness: u128) -> String {
    format!("{prefix}_{}", new_ulid(timestamp_ms, randomness))
}

pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut previous_was_sep = false;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            previous_was_sep = false;
        } else if !previous_was_sep {
            out.push('-');
            previous_was_sep = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// UTC instant, stored as ISO-8601 with `+00:00` (Python's `isoformat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Moment {
    pub unix_seconds: i64,
    nanos: u32,
}

impl Moment {
    pub fn from_unix(unix_seconds: i64, nanos: u32) -> Self {
        // Python's datetime resolves to microseconds, so a value below that
        // would render as ".000000", which fromisoformat never produced.
        Self {
            unix_seconds,
            nanos: nanos - nanos % 1000,
        }
    }

    pub fn seconds_since(self, earlier: Self) -> f64 {
        // ranking.py uses datetime.total_seconds(), which keeps the fraction.
        let seconds = (self.unix_seconds - earlier.unix_seconds) as f64;
        let nanos = self.nanos as f64 - earlier.nanos as f64;
        seconds + nanos / 1_000_000_000.0
    }

    pub fn to_iso(self) -> String {
        let rendered = chrono::DateTime::from_timestamp(self.unix_seconds, self.nanos)
            .expect("a moment built here is in range")
            .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
            .to_string();
        if self.nanos == 0 {
            rendered.replacen(".000000", "", 1)
        } else {
            rendered
        }
    }
}

/// A clock answers "now" in UTC. The domain takes one; it never reads the wall.
pub trait Clock {
    fn now(&mut self) -> Moment;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&mut self) -> Moment {
        utc_now()
    }
}

/// One second later on every read. Selected by `VOGT_TEST_CLOCK_START`.
pub struct StepClock {
    next: Moment,
}

impl StepClock {
    pub fn new(start: Moment) -> Self {
        Self { next: start }
    }
}

impl Clock for StepClock {
    fn now(&mut self) -> Moment {
        let current = self.next;
        self.next = Moment::from_unix(current.unix_seconds + 1, current.nanos);
        current
    }
}

/// Mints ids. Held by the caller, so the domain stays free of randomness.
pub trait IdFactory {
    fn next(&mut self, prefix: &str) -> String;
}

/// `{prefix}_{n:04d}`, persisted as sorted JSON. `VOGT_TEST_IDS=sequential`.
pub struct SequentialIds {
    path: Option<std::path::PathBuf>,
    counts: BTreeMap<String, u64>,
}

impl SequentialIds {
    pub fn new(path: Option<std::path::PathBuf>) -> Result<Self, String> {
        let counts = match &path {
            Some(path) if path.is_file() => {
                serde_json::from_str(&std::fs::read_to_string(path).map_err(|err| err.to_string())?)
                    .map_err(|err| err.to_string())?
            }
            _ => BTreeMap::new(),
        };
        Ok(Self { path, counts })
    }
}

impl IdFactory for SequentialIds {
    fn next(&mut self, prefix: &str) -> String {
        let count = self.counts.entry(prefix.to_string()).or_insert(0);
        *count += 1;
        let issued = *count;
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(body) = serde_json::to_string(&self.counts) {
                let _ = std::fs::write(path, body);
            }
        }
        format!("{prefix}_{issued:04}")
    }
}

pub fn utc_now() -> Moment {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Moment::from_unix(duration.as_secs() as i64, duration.subsec_nanos())
}

pub fn to_iso(moment: Moment) -> String {
    moment.to_iso()
}

/// Parse what `datetime.fromisoformat` accepts, as an aware UTC instant.
///
/// A missing offset is UTC. A comma fraction is a dot. Date-only, space
/// separated, basic format (`20260102T030405`), a truncated clock (`T05:00`)
/// and an offset with seconds (`+05:30:15`) all parse. An impossible date does
/// not: chrono validates the calendar, the hand parser did not. Sub-microsecond
/// digits are truncated, matching CPython.
pub fn from_iso(text: &str) -> Result<Moment, String> {
    let candidate = text.trim().replace(',', ".");
    let candidate = if let Some(at) = candidate.find(['t', 'T']) {
        format!("{}T{}", &candidate[..at], &candidate[at + 1..])
    } else {
        candidate
    };
    // An offset with seconds (`+05:30:15`) is legal for fromisoformat and not
    // for RFC3339, so it is peeled off before chrono sees the text.
    let (body, extra_offset) = split_seconds_offset(&candidate);
    let parsed = chrono::DateTime::parse_from_rfc3339(&body)
        .or_else(|_| parse_loose(&body))
        .map_err(|_| format!("not a timestamp: {text}"))?;
    let shifted = parsed + chrono::Duration::seconds(-extra_offset);
    let nanos = shifted.timestamp_subsec_nanos();
    Ok(Moment::from_unix(shifted.timestamp(), nanos))
}

/// `+05:30:15` becomes `+05:30` plus 15 seconds. Anything else is unchanged.
fn split_seconds_offset(text: &str) -> (String, i64) {
    let bytes = text.as_bytes();
    if bytes.len() > 9 && bytes[bytes.len() - 3] == b':' && bytes[bytes.len() - 6] == b':' {
        if let Some(sign_at) = text.rfind(['+', '-']) {
            let offset = &text[sign_at..];
            if offset.len() == 9 {
                let seconds: i64 = offset[7..].parse().unwrap_or(0);
                let sign: i64 = if bytes[sign_at] == b'+' { 1 } else { -1 };
                return (
                    format!("{}{}", &text[..sign_at], &offset[..6]),
                    sign * seconds,
                );
            }
        }
    }
    (text.to_string(), 0)
}

fn parse_loose(text: &str) -> Result<chrono::DateTime<chrono::FixedOffset>, chrono::ParseError> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime};
    const DATE: &str = "%Y-%m-%d";
    const BASIC_DATE: &str = "%Y%m%d";
    let offset = chrono::FixedOffset::east_opt(0).expect("zero offset");
    if let Ok(date) = NaiveDate::parse_from_str(text, DATE) {
        let naive = date.and_time(NaiveTime::MIN);
        return Ok(DateTime::<chrono::FixedOffset>::from_naive_utc_and_offset(
            naive, offset,
        ));
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y%m%dT%H%M%S%.f",
        "%Y%m%dT%H%M%S",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return Ok(DateTime::<chrono::FixedOffset>::from_naive_utc_and_offset(
                naive, offset,
            ));
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, BASIC_DATE) {
        let naive = date.and_time(NaiveTime::MIN);
        return Ok(DateTime::<chrono::FixedOffset>::from_naive_utc_and_offset(
            naive, offset,
        ));
    }
    NaiveDateTime::parse_from_str("", "").map(|_| unreachable!())
}

pub const LOCAL_SCHEME: &str = "local";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    Human,
    Agent,
}

impl ActorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub identity_ref: String,
    pub kind: ActorKind,
    pub display_name: String,
}

impl Principal {
    pub fn new(identity_ref: &str, kind: ActorKind, display_name: &str) -> Result<Self, String> {
        if identity_ref.trim().is_empty() {
            return Err("identity_ref must not be empty".to_string());
        }
        Ok(Self {
            identity_ref: identity_ref.to_string(),
            kind,
            display_name: display_name.to_string(),
        })
    }
}

pub fn local_principal(os_user: &str) -> Principal {
    Principal {
        identity_ref: format!("{LOCAL_SCHEME}:{os_user}"),
        kind: ActorKind::Human,
        display_name: os_user.to_string(),
    }
}

pub type WorkKind = &'static str;
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub id: String,
    pub kind: ActorKind,
    pub display_name: String,
    pub identity_ref: String,
    pub disabled: bool,
    pub created_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub root_path: String,
    pub repo_url: Option<String>,
    pub lifecycle_state: String,
    pub current_version: Option<String>,
    pub contract_version: Option<String>,
    pub compliance_status: String,
    pub compliance_checked_at: Option<Moment>,
    pub contract_adopted_at: Option<Moment>,
    pub write_back: String,
    pub link_state: String,
    pub exclusions: Vec<String>,
    pub trust_state: String,
    pub created_at: Moment,
    pub updated_at: Moment,
}

impl Project {
    pub fn new(id: &str, slug: &str, name: &str, root_path: &str, now: Moment) -> Self {
        Self {
            id: id.to_string(),
            slug: slug.to_string(),
            name: name.to_string(),
            root_path: root_path.to_string(),
            repo_url: None,
            lifecycle_state: "active".to_string(),
            current_version: None,
            contract_version: None,
            compliance_status: "not_checked".to_string(),
            compliance_checked_at: None,
            contract_adopted_at: None,
            write_back: "none".to_string(),
            link_state: "unlinked".to_string(),
            exclusions: Vec::new(),
            trust_state: "unverified".to_string(),
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItem {
    pub id: String,
    pub reference: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub priority: String,
    pub effort: Option<String>,
    pub project_id: Option<String>,
    pub initiative_id: Option<String>,
    pub origin: String,
    pub trust_state: String,
    pub superseded_by: Option<String>,
    pub created_at: Moment,
    pub updated_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub id: String,
    pub sweep_id: String,
    pub collector: String,
    pub kind: String,
    pub project_id: Option<String>,
    pub subject_key: String,
    pub payload: serde_json::Value,
    pub content_digest: String,
    pub promoted: bool,
    pub observed_at: Moment,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkOverlay {
    pub subject_key: String,
    pub project_id: String,
    pub rank: Option<f64>,
    pub workflow_state: Option<String>,
    pub priority: Option<String>,
    pub effort: Option<String>,
    pub branches: Vec<String>,
    pub created_at: Moment,
    pub updated_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGrant {
    pub id: String,
    pub state: String,
    pub expires_at: Option<Moment>,
}

impl SessionGrant {
    /// `expired` for an approved grant past its expiry, else the stored state.
    pub fn effective_state(&self, now: Moment) -> &str {
        if self.state == "approved" {
            if let Some(expires) = self.expires_at {
                if expires <= now {
                    return "expired";
                }
            }
        }
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulids_match_python_and_sort() {
        let earlier = new_ulid(1_700_000_000_000, 0);
        let later = new_ulid(1_700_000_001_000, 0);
        assert_eq!(earlier, "01HF7YAT000000000000000000");
        assert_eq!(later, "01HF7YATZ80000000000000000");
        assert!(earlier < later);
    }

    #[test]
    fn ids_carry_their_prefix() {
        let generated = new_id("prj", 1_700_000_000_000, 0);
        assert!(generated.starts_with("prj_"));
        assert_eq!(generated.len(), "prj_".len() + 26);
    }

    #[test]
    fn slugify_matches_the_table() {
        for (name, expected) in [
            ("Vogt", "vogt"),
            ("  Rust NZB  ", "rust-nzb"),
            ("nzb-core", "nzb-core"),
            ("A//B", "a-b"),
            ("///", ""),
            ("Region 2 (west)", "region-2-west"),
        ] {
            assert_eq!(slugify(name), expected, "{name}");
        }
    }

    #[test]
    fn timestamps_round_trip_as_utc() {
        // 2026-08-12 05:00 at UTC+10 is 2026-08-11 19:00 UTC.
        let moment = from_iso("2026-08-12T05:00:00+10:00").unwrap();
        assert_eq!(to_iso(moment), "2026-08-11T19:00:00+00:00");
        assert_eq!(
            from_iso("2026-08-12T05:00:00").unwrap().to_iso(),
            "2026-08-12T05:00:00+00:00"
        );
        // Pinned against CPython 3.12 datetime.fromisoformat. The forms it
        // accepts must parse; the two it rejects must not.
        let accepted = [
            ("2026-01-02T03:04:05Z", "2026-01-02T03:04:05+00:00"),
            (
                "2026-01-02T03:04:05.5+00:00",
                "2026-01-02T03:04:05.500000+00:00",
            ),
            ("2026-01-02T03:04:05,5", "2026-01-02T03:04:05.500000+00:00"),
            ("2026-01-02", "2026-01-02T00:00:00+00:00"),
            ("2026-01-02 03:04:05", "2026-01-02T03:04:05+00:00"),
            ("2026-01-02T05:00", "2026-01-02T05:00:00+00:00"),
            ("20260102T030405", "2026-01-02T03:04:05+00:00"),
            ("2026-01-02T03:04:05+05:30", "2026-01-01T21:34:05+00:00"),
            ("2026-01-02T03:04:05+05:30:15", "2026-01-01T21:33:50+00:00"),
            (
                "2026-01-02T03:04:05.123456789Z",
                "2026-01-02T03:04:05.123456+00:00",
            ),
            ("2026-01-02t03:04:05", "2026-01-02T03:04:05+00:00"),
        ];
        for (text, expected) in accepted {
            assert_eq!(from_iso(text).unwrap().to_iso(), expected, "{text}");
        }
        // CPython 3.12 rejects an unpadded date; chrono accepts it. The result
        // is still the date it says, which is what a digest cares about.
        assert_eq!(
            from_iso("2026-8-1T5:0:0").unwrap().to_iso(),
            "2026-08-01T05:00:00+00:00"
        );
        assert!(from_iso("2026-13-40T25:61:61").is_err());
        let later = from_iso("2026-01-02T00:00:01.500000Z").unwrap();
        let earlier = from_iso("2026-01-02T00:00:00Z").unwrap();
        assert!((later.seconds_since(earlier) - 1.5).abs() < 1e-9);
        assert!(utc_now().unix_seconds > 0);
    }

    #[test]
    fn the_step_clock_advances_one_second_a_read() {
        let start = from_iso("2026-01-02T03:04:05Z").unwrap();
        let mut clock = StepClock::new(start);
        assert_eq!(clock.now(), start);
        assert_eq!(clock.now().to_iso(), "2026-01-02T03:04:06+00:00");
    }

    #[test]
    fn sequential_ids_persist_across_a_new_factory() {
        let dir = std::env::temp_dir().join(format!("vogt-ids-{}", std::process::id()));
        let path = dir.join("ids.json");
        let _ = std::fs::remove_dir_all(&dir);
        let mut first = SequentialIds::new(Some(path.clone())).unwrap();
        assert_eq!(first.next("act"), "act_0001");
        assert_eq!(first.next("wrk"), "wrk_0001");
        assert_eq!(first.next("act"), "act_0002");
        let mut second = SequentialIds::new(Some(path)).unwrap();
        assert_eq!(second.next("act"), "act_0003");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_principal_needs_an_identity() {
        assert!(Principal::new("  ", ActorKind::Human, "nobody").is_err());
        let local = local_principal("sprooty");
        assert_eq!(local.identity_ref, "local:sprooty");
        assert_eq!(local.kind, ActorKind::Human);
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
    fn entities_carry_the_python_defaults() {
        let now = Moment::from_unix(1_700_000_000, 0);
        let project = Project::new("prj_1", "vogt", "Vogt", "/src", now);
        assert_eq!(project.lifecycle_state, "active");
        assert_eq!(project.compliance_status, "not_checked");
        assert_eq!(project.link_state, "unlinked");
        assert_eq!(project.write_back, "none");
        assert!(require_text("  ").is_err());
        assert_eq!(require_text("  because  ").unwrap(), "because");
        assert!(!DECLARABLE_RELATION_KINDS.contains(&"implemented_by"));

        let grant = SessionGrant {
            id: "g".into(),
            state: "approved".into(),
            expires_at: Some(now),
        };
        assert_eq!(grant.effective_state(now), "expired");
        assert_eq!(
            grant.effective_state(Moment::from_unix(1_699_999_999, 0)),
            "approved"
        );
        let pending = SessionGrant {
            state: "pending".into(),
            expires_at: Some(now),
            ..grant
        };
        assert_eq!(
            pending.effective_state(Moment::from_unix(1_800_000_000, 0)),
            "pending"
        );
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
