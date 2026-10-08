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
    unix_seconds: i64,
    nanos: u32,
}

impl Moment {
    pub fn from_unix(unix_seconds: i64, nanos: u32) -> Self {
        Self {
            unix_seconds,
            nanos,
        }
    }

    pub fn to_iso(self) -> String {
        let (year, month, day, hour, minute, second) = civil(self.unix_seconds);
        if self.nanos == 0 {
            format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}+00:00")
        } else {
            let micros = self.nanos / 1000;
            format!(
                "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{micros:06}+00:00"
            )
        }
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

/// Parse `YYYY-MM-DDTHH:MM:SS` with an optional fraction and offset. A missing
/// offset is UTC, matching `from_iso`.
pub fn from_iso(text: &str) -> Result<Moment, String> {
    let (body, offset) = split_offset(text)?;
    let (date, time) = body
        .split_once('T')
        .ok_or_else(|| format!("not a timestamp: {text}"))?;
    let mut date_parts = date.split('-');
    let year: i64 = take_num(&mut date_parts)?;
    let month: i64 = take_num(&mut date_parts)?;
    let day: i64 = take_num(&mut date_parts)?;
    let (clock, fraction) = time
        .split_once('.')
        .map(|(c, f)| (c, Some(f)))
        .unwrap_or((time, None));
    let mut clock_parts = clock.split(':');
    let hour: i64 = take_num(&mut clock_parts)?;
    let minute: i64 = take_num(&mut clock_parts)?;
    let second: i64 = take_num(&mut clock_parts)?;
    let nanos = match fraction {
        Some(raw) => {
            let digits: String = raw.chars().take(9).collect();
            let padded = format!("{digits:0<9}");
            padded
                .parse::<u32>()
                .map_err(|_| format!("bad fraction in {text}"))?
        }
        None => 0,
    };
    let unix =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    Ok(Moment::from_unix(unix, nanos))
}

fn take_num<'a>(parts: &mut impl Iterator<Item = &'a str>) -> Result<i64, String> {
    parts
        .next()
        .ok_or_else(|| "short timestamp".to_string())?
        .parse()
        .map_err(|_| "bad number in timestamp".to_string())
}

fn split_offset(text: &str) -> Result<(&str, i64), String> {
    if let Some(body) = text.strip_suffix('Z') {
        return Ok((body, 0));
    }
    if let Some(at) = text.rfind(['+', '-']) {
        if at > 10 {
            let body = &text[..at];
            let sign: i64 = if text.as_bytes()[at] == b'+' { 1 } else { -1 };
            let offset = &text[at + 1..];
            let (hour, minute) = offset.split_once(':').unwrap_or((offset, "0"));
            let secs =
                hour.parse::<i64>().unwrap_or(0) * 3600 + minute.parse::<i64>().unwrap_or(0) * 60;
            return Ok((body, sign * secs));
        }
    }
    Ok((text, 0))
}

fn civil(mut secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let time = secs.rem_euclid(86_400) as u32;
    secs = secs.div_euclid(86_400);
    let z = secs + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (
        year,
        month as u32,
        day as u32,
        time / 3600,
        (time % 3600) / 60,
        time % 60,
    )
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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
        assert!(utc_now().unix_seconds > 0);
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
