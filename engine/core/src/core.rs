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

/// A fresh id from the process clock and randomness. This is what `init` uses
/// when `VOGT_TEST_IDS` is unset, so two plain inits never share an instance
/// id. The hook is the only path to a deterministic one.
pub fn fresh_id(prefix: &str) -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);
    let mut bytes = [0u8; 10];
    let _ = std::fs::File::open("/dev/urandom")
        .and_then(|mut source| std::io::Read::read_exact(&mut source, &mut bytes));
    let mut randomness = 0u128;
    for byte in bytes {
        randomness = (randomness << 8) | u128::from(byte);
    }
    new_id(prefix, millis, randomness)
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
        // Python's datetime resolves to microseconds, so a value below that
        // would render as ".000000", which fromisoformat never produced.
        Self {
            unix_seconds,
            nanos: nanos - nanos % 1000,
        }
    }

    pub fn unix_seconds(self) -> i64 {
        self.unix_seconds
    }

    pub fn nanos(self) -> u32 {
        self.nanos
    }

    pub fn seconds_since(self, earlier: Self) -> f64 {
        // ranking.py uses datetime.total_seconds(), which keeps the fraction.
        let seconds = (self.unix_seconds - earlier.unix_seconds) as f64;
        let nanos = self.nanos as f64 - earlier.nanos as f64;
        seconds + nanos / 1_000_000_000.0
    }

    pub fn to_iso(self) -> String {
        self.render("+00:00")
    }

    /// The form pydantic writes for `model_dump(mode="json")`: a `Z` suffix
    /// instead of `+00:00`. The storage form stays `to_iso`, because the two
    /// are not the same string and the database columns carry the latter.
    pub fn to_json(self) -> String {
        self.render("Z")
    }

    fn render(self, suffix: &str) -> String {
        let rendered = chrono::DateTime::from_timestamp(self.unix_seconds, self.nanos)
            .expect("a moment built here is in range")
            .format("%Y-%m-%dT%H:%M:%S%.6f")
            .to_string();
        let body = if self.nanos == 0 {
            rendered.replacen(".000000", "", 1)
        } else {
            rendered
        };
        format!("{body}{suffix}")
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
#[derive(Debug)]
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
#[derive(Debug)]
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
                std::fs::create_dir_all(parent)
                    .unwrap_or_else(|err| panic!("creating {}: {err}", parent.display()));
            }
            // A swallowed write hands the next process the same ids.
            let body = serde_json::to_string(&self.counts).expect("counts are strings and numbers");
            std::fs::write(path, body)
                .unwrap_or_else(|err| panic!("writing {}: {err}", path.display()));
        }
        format!("{prefix}_{issued:04}")
    }
}

pub const CLOCK_ENV: &str = "VOGT_TEST_CLOCK_START";
pub const IDS_ENV: &str = "VOGT_TEST_IDS";

/// Python's `repr` for a string. It prefers single quotes and switches to
/// double quotes when the value holds an apostrophe and no double quote, so
/// ` x ` reads `' x '` and `it's` reads `"it's"`.
fn py_repr(raw: &str) -> String {
    let quote = if raw.contains('\'') && !raw.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `VOGT_TEST_CLOCK_START`. Empty is unset. Anything that is not a timestamp is
/// an `InvalidRequest`, naming the value.
pub fn clock_from_env(value: Option<&str>) -> Result<Option<StepClock>, VogtError> {
    let Some(raw) = value.filter(|text| !text.trim().is_empty()) else {
        return Ok(None);
    };
    let start = from_iso(raw.trim()).map_err(|_| {
        VogtError::InvalidRequest(format!(
            "{CLOCK_ENV} must be an RFC3339 timestamp, not {}",
            py_repr(raw)
        ))
    })?;
    Ok(Some(StepClock::new(start)))
}

/// `VOGT_TEST_IDS`. Only `sequential` is a mode; anything else is refused.
pub fn ids_from_env(
    value: Option<&str>,
    path: Option<std::path::PathBuf>,
) -> Result<Option<SequentialIds>, VogtError> {
    let Some(raw) = value.filter(|text| !text.trim().is_empty()) else {
        return Ok(None);
    };
    if raw.trim() != "sequential" {
        return Err(VogtError::InvalidRequest(format!(
            "{IDS_ENV} must be 'sequential' when set, not {}",
            py_repr(raw)
        )));
    }
    SequentialIds::new(path)
        .map(Some)
        .map_err(VogtError::InvalidRequest)
}

/// The hook names that are set, for the one startup warning.
pub fn hooks_active(clock: Option<&str>, ids: Option<&str>) -> Vec<&'static str> {
    let mut active = Vec::new();
    if clock.is_some_and(|value| !value.trim().is_empty()) {
        active.push(CLOCK_ENV);
    }
    if ids.is_some_and(|value| !value.trim().is_empty()) {
        active.push(IDS_ENV);
    }
    active
}

/// A name is loopback only when it is literally `localhost`. Resolving it would
/// make the answer depend on DNS, and a test aid must not.
pub fn is_loopback(host: &str) -> bool {
    let candidate = host.trim().trim_matches(|ch| ch == '[' || ch == ']');
    if candidate.eq_ignore_ascii_case("localhost") {
        return true;
    }
    candidate
        .parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// A deterministic clock or id factory is only honoured on a loopback bind.
pub fn refuse_hooks_off_loopback(
    host: &str,
    clock: Option<&str>,
    ids: Option<&str>,
) -> Result<(), VogtError> {
    let active = hooks_active(clock, ids);
    if !active.is_empty() && !is_loopback(host) {
        return Err(VogtError::InvalidRequest(format!(
            "refusing to serve on {host}: {} selects a deterministic test clock or id \
             factory, which is only honoured on a loopback bind",
            active.join(", ")
        )));
    }
    Ok(())
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

/// Parse what CPython 3.12 `datetime.fromisoformat` accepts, as aware UTC.
///
/// The grammar is `_parse_isoformat_date` and `_parse_isoformat_time`: fixed
/// width fields, never variable ones. A missing offset is UTC. A comma is a
/// dot in the fraction, which is truncated to microseconds. Whitespace, a leap
/// second and an unpadded field are rejected, because Python rejects them.
pub fn from_iso(text: &str) -> Result<Moment, String> {
    let bytes = text.as_bytes();
    let (year, month, day, consumed) = parse_iso_date(bytes)?;
    let (hour, minute, second, micros, offset_seconds, offset_micros) = if consumed == bytes.len() {
        (0, 0, 0, 0, 0, 0)
    } else {
        parse_iso_time(&bytes[consumed..])?
    };
    let date = chrono::NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| format!("not a timestamp: {text}"))?;
    let time = chrono::NaiveTime::from_hms_micro_opt(hour, minute, second, micros)
        .ok_or_else(|| format!("not a timestamp: {text}"))?;
    let naive = chrono::NaiveDateTime::new(date, time);
    let utc = naive.and_utc()
        - chrono::Duration::seconds(offset_seconds)
        - chrono::Duration::microseconds(offset_micros);
    Ok(Moment::from_unix(
        utc.timestamp(),
        utc.timestamp_subsec_nanos(),
    ))
}

/// `YYYY-MM-DD`, `YYYYMMDD`, or an ISO week date `YYYY-Www-D`. Returns how many
/// bytes the date consumed, including its trailing separator.
fn parse_iso_date(bytes: &[u8]) -> Result<(i32, u32, u32, usize), String> {
    let year = fixed(bytes, 0, 4).ok_or_else(|| "not a timestamp".to_string())? as i32;
    // CPython's datetime starts at year 1 and ends at 9999.
    if !(1..=9999).contains(&year) {
        return Err("not a timestamp".to_string());
    }
    if bytes.len() >= 7 && bytes[4] == b'W' {
        let week = fixed(bytes, 5, 2).ok_or_else(|| "not a timestamp".to_string())?;
        let weekday = fixed(bytes, 7, 1).ok_or_else(|| "not a timestamp".to_string())?;
        if bytes.len() != 8 {
            return Err("not a timestamp".to_string());
        }
        let (year, month, day) = week_date(year, week, weekday)?;
        return Ok((year, month, day, 8));
    }
    if bytes.len() >= 8 && bytes[4].is_ascii_digit() {
        let month = fixed(bytes, 4, 2).ok_or_else(|| "not a timestamp".to_string())?;
        let day = fixed(bytes, 6, 2).ok_or_else(|| "not a timestamp".to_string())?;
        return Ok((year, month, day, 8));
    }
    if bytes.len() >= 10 && bytes[4] == b'-' && bytes[5] == b'W' {
        let week = fixed(bytes, 6, 2).ok_or_else(|| "not a timestamp".to_string())?;
        if bytes.len() < 10 || bytes[8] != b'-' {
            return Err("not a timestamp".to_string());
        }
        let weekday = fixed(bytes, 9, 1).ok_or_else(|| "not a timestamp".to_string())?;
        let (year, month, day) = week_date(year, week, weekday)?;
        return Ok((year, month, day, 10));
    }
    if bytes.len() >= 10 && bytes[4] == b'-' {
        let month = fixed(bytes, 5, 2).ok_or_else(|| "not a timestamp".to_string())?;
        if bytes.len() < 10 || bytes[7] != b'-' {
            return Err("not a timestamp".to_string());
        }
        let day = fixed(bytes, 8, 2).ok_or_else(|| "not a timestamp".to_string())?;
        return Ok((year, month, day, 10));
    }
    Err("not a timestamp".to_string())
}

/// The time half, starting at its separator. Any single byte separates the
/// date from the time, so `T`, `t` and `X` all work.
fn parse_iso_time(bytes: &[u8]) -> Result<(u32, u32, u32, u32, i64, i64), String> {
    if bytes.is_empty() {
        return Err("not a timestamp".to_string());
    }
    let rest = &bytes[1..];
    let (hour, minute, second, at) = parse_hms(rest)?;
    let (micros, at) = parse_fraction(rest, at)?;
    let offset = if at == rest.len() {
        (0, 0)
    } else {
        parse_offset(&rest[at..])?
    };
    Ok((hour, minute, second, micros, offset.0, offset.1))
}

fn parse_hms(bytes: &[u8]) -> Result<(u32, u32, u32, usize), String> {
    let hour = fixed(bytes, 0, 2).ok_or_else(|| "not a timestamp".to_string())?;
    if bytes.len() == 2 || starts_fraction_or_offset(bytes, 2) {
        return Ok((hour, 0, 0, 2));
    }
    if bytes.len() >= 4 && bytes[2].is_ascii_digit() {
        let minute = fixed(bytes, 2, 2).ok_or_else(|| "not a timestamp".to_string())?;
        if bytes.len() == 4 || starts_fraction_or_offset(bytes, 4) {
            return Ok((hour, minute, 0, 4));
        }
        let second = fixed(bytes, 4, 2).ok_or_else(|| "not a timestamp".to_string())?;
        return Ok((hour, minute, second, 6));
    }
    if bytes.len() >= 5 && bytes[2] == b':' {
        let minute = fixed(bytes, 3, 2).ok_or_else(|| "not a timestamp".to_string())?;
        if bytes.len() == 5 || starts_fraction_or_offset(bytes, 5) {
            return Ok((hour, minute, 0, 5));
        }
        if bytes.len() >= 8 && bytes[5] == b':' {
            let second = fixed(bytes, 6, 2).ok_or_else(|| "not a timestamp".to_string())?;
            return Ok((hour, minute, second, 8));
        }
    }
    Err("not a timestamp".to_string())
}

fn starts_fraction_or_offset(bytes: &[u8], at: usize) -> bool {
    matches!(bytes.get(at), Some(b'.' | b',' | b'+' | b'-' | b'Z' | b'z'))
}

fn parse_fraction(bytes: &[u8], at: usize) -> Result<(u32, usize), String> {
    if !matches!(bytes.get(at), Some(b'.' | b',')) {
        return Ok((0, at));
    }
    let mut digits = String::new();
    let mut index = at + 1;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        if digits.len() < 6 {
            digits.push(bytes[index] as char);
        }
        index += 1;
    }
    if digits.is_empty() {
        return Err("not a timestamp".to_string());
    }
    Ok((format!("{digits:0<6}").parse().unwrap_or(0), index))
}

/// `Z`, `±HH`, `±HHMM`, `±HH:MM`, `±HH:MM:SS`, with an optional fraction on the
/// offset seconds. Returns whole seconds and leftover microseconds. A field
/// that is not two digits is an error, never a panic, and 24 hours or more is
/// rejected the way Python rejects it.
fn parse_offset(bytes: &[u8]) -> Result<(i64, i64), String> {
    if bytes == b"Z" || bytes == b"z" {
        return Ok((0, 0));
    }
    let bytes = bytes.strip_prefix(b" ").unwrap_or(bytes);
    if bytes.is_empty() || !matches!(bytes[0], b'+' | b'-') {
        return Err("not a timestamp".to_string());
    }
    let sign: i64 = if bytes[0] == b'+' { 1 } else { -1 };
    let body = &bytes[1..];
    let bad = || "not a timestamp".to_string();
    let (hour, minute, second, micros, consumed) =
        if body.len() >= 8 && body[2] == b':' && body[5] == b':' {
            (
                fixed(body, 0, 2).ok_or_else(bad)?,
                fixed(body, 3, 2).ok_or_else(bad)?,
                fixed(body, 6, 2).ok_or_else(bad)?,
                0,
                8,
            )
        } else if body.len() >= 5 && body[2] == b':' {
            (
                fixed(body, 0, 2).ok_or_else(bad)?,
                fixed(body, 3, 2).ok_or_else(bad)?,
                0,
                0,
                5,
            )
        } else if body.len() >= 4 && body[0].is_ascii_digit() && body[2].is_ascii_digit() {
            (
                fixed(body, 0, 2).ok_or_else(bad)?,
                fixed(body, 2, 2).ok_or_else(bad)?,
                0,
                0,
                4,
            )
        } else if body.len() >= 2 {
            (fixed(body, 0, 2).ok_or_else(bad)?, 0, 0, 0, 2)
        } else {
            return Err(bad());
        };
    let micros = if matches!(body.get(consumed), Some(b'.' | b',')) {
        let (fraction, end) = parse_fraction(body, consumed)?;
        if end != body.len() {
            return Err(bad());
        }
        fraction
    } else if consumed != body.len() {
        return Err(bad());
    } else {
        micros
    };
    let total = hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    if total >= 24 * 3600 {
        return Err(bad());
    }
    Ok((sign * total, sign * micros as i64))
}

/// ISO week date to a calendar date. Week 1 holds January 4th and starts Monday.
fn week_date(year: i32, week: u32, weekday: u32) -> Result<(i32, u32, u32), String> {
    if !(1..=53).contains(&week) || !(1..=7).contains(&weekday) {
        return Err("not a timestamp".to_string());
    }
    let jan4 = days_from_civil(year, 1, 4);
    let monday = jan4 - (jan4 + 3).rem_euclid(7);
    let days = monday + ((week - 1) * 7 + (weekday - 1)) as i64;
    let (y, m, d) = civil_date(days);
    if y != year {
        return Err("not a timestamp".to_string());
    }
    Ok((y, m, d))
}

/// Days since the Unix epoch for a civil date. Howard Hinnant's algorithm.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = if month <= 2 {
        year as i64 - 1
    } else {
        year as i64
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month as i64 + if month > 2 { -3 } else { 9 };
    let doy = (153 * m + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_date(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year as i32, month as u32, day as u32)
}

fn fixed(bytes: &[u8], at: usize, width: usize) -> Option<u32> {
    let slice = bytes.get(at..at + width)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(slice).ok()?.parse().ok()
}

impl serde::Serialize for Moment {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_json())
    }
}

impl<'de> serde::Deserialize<'de> for Moment {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <String as serde::Deserialize>::deserialize(deserializer)?;
        from_iso(&text).map_err(serde::de::Error::custom)
    }
}

pub const LOCAL_SCHEME: &str = "local";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

macro_rules! vocab {
    ($name:ident { $first:ident $(, $variant:ident)* $(,)? }) => {
        vocab!($name, serde, { $first $(, $variant)* });
    };
    ($name:ident, bare, { $first:ident $(, $variant:ident)* }) => {
        vocab!(@emit $name, , { $first $(, $variant)* });
    };
    ($name:ident, serde, { $first:ident $(, $variant:ident)* }) => {
        vocab!(@emit $name, #[derive(serde::Serialize, serde::Deserialize)] #[serde(rename_all = "snake_case")], { $first $(, $variant)* });
        vocab!(@text $name);
    };
    (@emit $name:ident, $(#[$serde:meta])* , { $first:ident $(, $variant:ident)* }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        $(#[$serde])*
        pub enum $name { $first, $($variant),* }

        impl $name {
            fn first() -> Self {
                Self::$first
            }
        }

        impl Default for $name {
            /// The first variant. The vocabularies whose pydantic default is
            /// something else name their own function instead of relying on
            /// this (`TrustState` defaults to `unverified`, `Priority` to `p2`).
            fn default() -> Self {
                Self::first()
            }
        }
    };
    (@text $name:ident) => {
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let text = serde_json::to_value(self).expect("an enum is a string");
                f.write_str(text.as_str().expect("an enum is a string"))
            }
        }

        /// The stored column is text. An unrecognised value is a data error,
        /// reported rather than mapped onto a default.
        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                serde_json::from_value(serde_json::Value::String(text.to_string()))
                    .map_err(|_| format!("unknown {}: {text:?}", stringify!($name)))
            }
        }

        impl rusqlite::types::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(self.to_string().into())
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                value.as_str()?.parse().map_err(|err: String| {
                    rusqlite::types::FromSqlError::Other(err.into())
                })
            }
        }
    };
}

vocab!(WorkKind {
    Feature,
    Bug,
    Chore,
    Question
});
vocab!(ProjectLifecycle {
    Incubating,
    Active,
    Maintenance,
    Archived
});
// `not_applicable` is what a reader is told, not a stored value, but the
// column type is the same literal so a row can carry it.
vocab!(ComplianceStatus {
    Compliant,
    NonCompliant,
    NotChecked,
    NotApplicable
});
vocab!(LinkState { Unlinked, Linked });
vocab!(WriteBack, bare, { Disabled, CommentOnly, Full });

impl WriteBack {
    fn text(self) -> &'static str {
        match self {
            Self::Disabled => "none",
            Self::CommentOnly => "comment_only",
            Self::Full => "full",
        }
    }
}

impl std::fmt::Display for WriteBack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

impl std::str::FromStr for WriteBack {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "none" => Ok(Self::Disabled),
            "comment_only" => Ok(Self::CommentOnly),
            "full" => Ok(Self::Full),
            other => Err(format!("unknown WriteBack: {other:?}")),
        }
    }
}

impl rusqlite::types::ToSql for WriteBack {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.to_string().into())
    }
}

impl rusqlite::types::FromSql for WriteBack {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|err: String| rusqlite::types::FromSqlError::Other(err.into()))
    }
}

impl serde::Serialize for WriteBack {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::Disabled => "none",
            Self::CommentOnly => "comment_only",
            Self::Full => "full",
        })
    }
}

impl<'de> serde::Deserialize<'de> for WriteBack {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match <&str>::deserialize(deserializer)? {
            "none" => Ok(Self::Disabled),
            "comment_only" => Ok(Self::CommentOnly),
            "full" => Ok(Self::Full),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["none", "comment_only", "full"],
            )),
        }
    }
}

fn lifecycle_default() -> ProjectLifecycle {
    ProjectLifecycle::Active
}

fn compliance_default() -> ComplianceStatus {
    ComplianceStatus::NotChecked
}

fn write_back_default() -> WriteBack {
    WriteBack::Disabled
}

fn link_state_default() -> LinkState {
    LinkState::Unlinked
}

fn priority_default() -> Priority {
    Priority::P2
}
vocab!(Priority { P0, P1, P2, P3, P4 });
vocab!(Effort { Xs, S, M, L, Xl });
fn trust_state_default() -> TrustState {
    TrustState::Unverified
}

vocab!(Origin {
    Created,
    Adopted,
    Observed
});
vocab!(TrustState {
    Verified,
    Stale,
    Unverified,
    Disputed
});
vocab!(RelationKind {
    DependsOn,
    RelatesTo,
    DuplicateOf,
    ParentOf,
    ImplementedBy
});
vocab!(InitiativeState { Open, Closed });
vocab!(MatchKind { Exact, Pattern });
vocab!(LinkRelation {
    Completion,
    Reference
});
vocab!(TokenKind {
    Api,
    Session,
    Agent
});
vocab!(AuthOutcome { Allow, Deny });
vocab!(WriteBackAction {
    Create,
    Comment,
    Label,
    Close,
    Reopen
});
vocab!(WriteBackOutcome {
    Attempted,
    Succeeded,
    Failed,
    Skipped
});

/// Pydantic defaults for fields a caller may omit. `Default` on the enum
/// itself would default every vocabulary's first variant, including the ones
/// Python requires.
fn initiative_state_default() -> InitiativeState {
    InitiativeState::Open
}

fn link_relation_default() -> LinkRelation {
    LinkRelation::Completion
}

fn token_kind_default() -> TokenKind {
    TokenKind::Api
}

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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    pub id: String,
    pub kind: ActorKind,
    pub display_name: String,
    pub identity_ref: String,
    pub disabled: bool,
    pub created_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub root_path: String,
    pub repo_url: Option<String>,
    #[serde(default = "lifecycle_default")]
    pub lifecycle_state: ProjectLifecycle,
    pub current_version: Option<String>,
    pub contract_version: Option<String>,
    #[serde(default = "compliance_default")]
    pub compliance_status: ComplianceStatus,
    pub compliance_checked_at: Option<Moment>,
    pub contract_adopted_at: Option<Moment>,
    #[serde(default = "write_back_default")]
    pub write_back: WriteBack,
    #[serde(default = "link_state_default")]
    pub link_state: LinkState,
    #[serde(default)]
    pub exclusions: Vec<String>,
    #[serde(default = "trust_state_default")]
    pub trust_state: TrustState,
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
            lifecycle_state: ProjectLifecycle::Active,
            current_version: None,
            contract_version: None,
            compliance_status: ComplianceStatus::NotChecked,
            compliance_checked_at: None,
            contract_adopted_at: None,
            write_back: WriteBack::Disabled,
            link_state: LinkState::Unlinked,
            exclusions: Vec::new(),
            trust_state: TrustState::Unverified,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkItem {
    pub id: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub kind: WorkKind,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub state: String,
    #[serde(default = "priority_default")]
    pub priority: Priority,
    pub effort: Option<Effort>,
    pub project_id: Option<String>,
    pub project_slug: Option<String>,
    pub initiative_id: Option<String>,
    #[serde(default)]
    pub origin: Origin,
    #[serde(default = "trust_state_default")]
    pub trust_state: TrustState,
    pub assignee_actor_id: Option<String>,
    pub assignee_identity_ref: Option<String>,
    pub labels: Vec<String>,
    pub relations: Vec<Relation>,
    pub superseded_by: Option<String>,
    pub created_at: Moment,
    pub updated_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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

vocab!(GrantKind {
    Credential,
    Capability
});
vocab!(GrantUses { Once, Ttl });
vocab!(GrantState {
    Pending,
    Approved,
    Denied,
    Revoked
});

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionGrant {
    pub id: String,
    pub target_engine_session_id: String,
    pub kind: GrantKind,
    pub var: Option<String>,
    pub project_id: Option<String>,
    pub secret_name: Option<String>,
    pub capability: Option<String>,
    pub uses: GrantUses,
    pub ttl_seconds: i64,
    pub reason: String,
    pub requested_by: String,
    pub requested_at: Moment,
    pub state: GrantState,
    pub decided_by: Option<String>,
    pub decided_at: Option<Moment>,
    pub decision_reason: Option<String>,
    pub expires_at: Option<Moment>,
    pub revoked_by: Option<String>,
    pub revoked_at: Option<Moment>,
}

impl SessionGrant {
    /// `expired` for an approved grant past its expiry, else the stored state.
    /// `expired` is not itself a state, so it comes back as text.
    pub fn effective_state(&self, now: Moment) -> String {
        if self.state == GrantState::Approved {
            if let Some(expires) = self.expires_at {
                if expires <= now {
                    return "expired".to_string();
                }
            }
        }
        self.state.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub created_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Initiative {
    pub id: String,
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default = "initiative_state_default")]
    pub state: InitiativeState,
    #[serde(default)]
    pub weight: i64,
    pub created_at: Moment,
    pub updated_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relation {
    pub kind: RelationKind,
    pub related_id: String,
    pub related_ref: String,
    pub related_title: String,
    pub related_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comment {
    pub id: String,
    pub work_item_id: String,
    pub actor_id: String,
    pub actor_display_name: String,
    pub body: String,
    pub created_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suppression {
    pub id: String,
    pub match_kind: MatchKind,
    pub subject_key_or_pattern: String,
    pub scope_project_id: Option<String>,
    pub scope_project_slug: Option<String>,
    pub actor_id: String,
    pub actor_identity_ref: Option<String>,
    pub reason: String,
    pub created_at: Moment,
    pub revoked_at: Option<Moment>,
    pub revoked_reason: Option<String>,
}

impl Suppression {
    /// A suppression holds until someone revokes it.
    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractExemption {
    pub id: String,
    pub project_id: String,
    pub project_slug: Option<String>,
    pub rule: String,
    pub target: String,
    pub reason: String,
    pub declared_by: String,
    pub declared_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkLink {
    pub work_item_id: String,
    pub subject_key: String,
    pub origin_kind: String,
    pub source_url: Option<String>,
    #[serde(default = "link_relation_default")]
    pub relation: LinkRelation,
    pub created_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingSession {
    pub id: String,
    pub engine_session_id: String,
    pub project_id: String,
    pub work_item_id: Option<String>,
    pub actor_id: String,
    pub cwd: String,
    pub template: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub reason: String,
    pub started_at: Moment,
    pub stopped_at: Option<Moment>,
}

/// The hash only. A model that could round-trip the secret is one that leaks
/// it into logs and API responses.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Token {
    pub id: String,
    pub actor_id: String,
    pub actor_identity_ref: Option<String>,
    pub name: String,
    pub scopes: Vec<String>,
    #[serde(default = "token_kind_default")]
    pub kind: TokenKind,
    pub created_at: Moment,
    pub expires_at: Option<Moment>,
    pub last_used_at: Option<Moment>,
    pub revoked_at: Option<Moment>,
    pub revoked_reason: Option<String>,
}

impl Token {
    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// The hash is deliberately absent: the storage layer writes and reads it, so
/// no listing, result or audit payload can carry it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordCredential {
    pub actor_id: String,
    pub actor_identity_ref: Option<String>,
    pub username: String,
    pub scopes: Vec<String>,
    pub created_at: Moment,
    pub updated_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthDecision {
    pub id: String,
    pub at: Moment,
    pub decision: AuthOutcome,
    pub reason_code: String,
    pub operation: String,
    pub scope: Option<String>,
    pub actor_id: Option<String>,
    pub token_id: Option<String>,
    pub identity_ref: Option<String>,
    pub transport: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteBackRecord {
    pub id: String,
    pub at: Moment,
    pub project_id: Option<String>,
    pub work_item_id: Option<String>,
    pub actor_id: String,
    pub action: WriteBackAction,
    pub subject_key: Option<String>,
    pub policy: String,
    pub outcome: WriteBackOutcome,
    pub reason: String,
    pub detail: Option<String>,
    pub source_url: Option<String>,
}

/// No token field. The encrypted PAT lives in its own column and is read
/// through a dedicated accessor, so this entity can never carry it out.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeAccount {
    pub actor_id: String,
    pub host: String,
    pub login: String,
    pub scopes: String,
    pub created_at: Moment,
    pub updated_at: Moment,
}

vocab!(SweepOutcome {
    Running,
    Ok,
    Partial,
    Failed
});
vocab!(RefKind {
    Path,
    Git,
    Declared,
    Inherited
});
vocab!(DriftStatus {
    Open,
    Accepted,
    Rejected,
    Contested
});
vocab!(TriageState {
    Active,
    Archived,
    Snoozed
});

/// Append-only ledger row. Ports `AuditRecord` (`entities.py`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditRecord {
    pub id: String,
    pub txn_id: String,
    pub revision: i64,
    pub actor_id: String,
    pub actor_identity_ref: String,
    pub operation: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub reason: String,
    pub payload_digest: String,
    pub at: Moment,
}

/// Published change. Ports `Event`. `summary` is the JSON object the feed
/// carries; an absent actor or audit row stays `None`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub seq: i64,
    pub kind: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub actor_id: Option<String>,
    pub audit_id: Option<String>,
    pub summary: serde_json::Value,
    pub at: Moment,
}

/// A machine-raised question, resolved by a human or an agent. Ports
/// `DriftProposal`. `status` is the four-value literal, not a free string.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriftProposal {
    pub id: String,
    pub kind: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub project_id: Option<String>,
    pub project_slug: Option<String>,
    pub summary: String,
    pub evidence_observation_id: Option<String>,
    pub evidence_snapshot: serde_json::Value,
    pub proposed_change: serde_json::Value,
    pub status: DriftStatus,
    pub opened_at: Moment,
    pub superseded_at: Option<Moment>,
    pub superseded_detail: Option<String>,
    pub resolved_by_actor_id: Option<String>,
    pub resolved_by_identity_ref: Option<String>,
    pub resolved_at: Option<Moment>,
    pub resolution_reason: Option<String>,
}

/// The shared, audited decision attached to one Inbox occurrence. Ports
/// `InboxTriage`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxTriage {
    pub entry_key: String,
    pub state: TriageState,
    pub snooze_until: Option<Moment>,
    pub actor_id: String,
    pub actor_identity_ref: Option<String>,
    pub decided_at: Moment,
    pub occurrence_snapshot: serde_json::Value,
}

/// One per-actor setting. Ports `ActorPreference`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorPreference {
    pub actor_id: String,
    pub key: String,
    pub value: serde_json::Value,
    pub version: i64,
    pub updated_at: Moment,
}

/// One reference from a project to another. Ports `DepRef`. No lockfile is
/// parsed and no package version is resolved.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepRef {
    pub subject_key: String,
    pub from_project_id: String,
    pub from_project_slug: Option<String>,
    pub ref_kind: RefKind,
    pub raw_target: String,
    pub manifest: Option<String>,
    pub to_project_id: Option<String>,
    pub to_project_slug: Option<String>,
    pub observed_at: Moment,
}

/// A coverage record. Ports `Sweep`. `outcome` defaults to `running` until a
/// sweep finishes, which is what makes "absent" different from "not collected".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sweep {
    pub id: String,
    pub collector: String,
    pub scope: Vec<String>,
    pub started_at: Moment,
    pub finished_at: Option<Moment>,
    pub outcome: SweepOutcome,
    pub stats: BTreeMap<String, i64>,
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn a_missing_trust_state_reads_as_unverified() {
        // Python's default is unverified. The first variant is verified, so a
        // blanket Default would mark an imported item as checked.
        let item: WorkItem = serde_json::from_str(
            r#"{"id":"wrk_1","ref":"WI-1","kind":"bug","title":"t","state":"open","labels":[],"relations":[],"created_at":"2026-01-01T00:00:00+00:00","updated_at":"2026-01-01T00:00:00+00:00"}"#,
        )
        .unwrap();
        assert_eq!(item.trust_state, TrustState::Unverified);
        assert_eq!(item.origin, Origin::Created);
    }

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
            assert_eq!(
                from_iso(text)
                    .map(|m| m.to_iso())
                    .unwrap_or_else(|e| format!("ERR {e}")),
                expected,
                "{text}"
            );
        }
        // The rows the chrono formats got wrong. None is a rejection.
        let rejected = [
            "2026-08-12T05:00:00+5:30",
            "2026-08-12T05:00:00+05:3",
            "2026-08-12T05:00:00+24:00",
            "2026-08-12T05:00:00-24:00",
            "2026-8-1T5:0:0",
            "2026-13-40T25:61:61",
            "2026-08-12T23:59:60",
            " 2026-01-02T03:04:05Z",
            "2026-01-02T03:04:05Z ",
            "2026-08-12T24:00:00",
        ];
        for text in rejected {
            assert!(from_iso(text).is_err(), "{text}");
        }
        let more = [
            ("2026-01-02T03:04:05+0530", "2026-01-01T21:34:05+00:00"),
            ("2026-01-02T03:04:05+05", "2026-01-01T22:04:05+00:00"),
            ("2026-01-02T05:00+05:00", "2026-01-02T00:00:00+00:00"),
            ("2026-01-02T05", "2026-01-02T05:00:00+00:00"),
            ("2026-01-02T050000", "2026-01-02T05:00:00+00:00"),
            ("2026-W33-3", "2026-08-12T00:00:00+00:00"),
            ("2026-08-12X05:00:00", "2026-08-12T05:00:00+00:00"),
            (
                "2026-08-12T05:00:00.123",
                "2026-08-12T05:00:00.123000+00:00",
            ),
            (
                "2026-08-12T05:00:00.1234567",
                "2026-08-12T05:00:00.123456+00:00",
            ),
            ("20260812", "2026-08-12T00:00:00+00:00"),
            ("2026-08-12T05:00:00-05:30", "2026-08-12T10:30:00+00:00"),
            ("2026-08-12T05:00:00.0Z", "2026-08-12T05:00:00+00:00"),
            (
                "2026-08-12T05:00:00+05:30:15.5",
                "2026-08-11T23:29:44.500000+00:00",
            ),
            ("2026-08-12T05:00:00 +05:00", "2026-08-12T00:00:00+00:00"),
            ("2026W331", "2026-08-10T00:00:00+00:00"),
            ("2026-08-12T0500", "2026-08-12T05:00:00+00:00"),
        ];
        for (text, expected) in more {
            assert_eq!(
                from_iso(text).map(|m| m.to_iso()).ok().as_deref(),
                Some(expected),
                "{text}"
            );
        }
        let later = from_iso("2026-01-02T00:00:01.500000Z").unwrap();
        let earlier = from_iso("2026-01-02T00:00:00Z").unwrap();
        assert!((later.seconds_since(earlier) - 1.5).abs() < 1e-9);
        assert!(utc_now().unix_seconds() > 0);
    }

    #[test]
    fn a_mangled_timestamp_is_an_error_never_a_panic() {
        // Every truncation and every single-byte corruption of a value the
        // table accepts. `20261x12` used to panic in the basic-date branch.
        let accepted = [
            "2026-01-02T03:04:05Z",
            "2026-01-02T03:04:05+00:00",
            "2026-01-02",
            "20260102T030405",
            "2026-01-02T03:04:05+05:30:15",
            "2026W331",
            "2026-08-12T0500",
            "2026-08-12T05:00:00 +05:00",
            "2026-08-12T05:00:00+05:30:15.5",
        ];
        for text in accepted {
            let bytes = text.as_bytes();
            for end in 0..=bytes.len() {
                let _ = from_iso(&text[..end]);
            }
            for index in 0..bytes.len() {
                for replacement in *b"x-:+ ." {
                    let mut mangled = bytes.to_vec();
                    mangled[index] = replacement;
                    let _ = from_iso(&String::from_utf8(mangled).unwrap());
                }
            }
        }
        assert!(from_iso("20261x12").is_err());
        assert!(from_iso("0000-01-01").is_err());
        assert!(from_iso("10000-01-01").is_err());
    }

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
    fn the_hook_selection_matches_the_python_messages() {
        assert!(clock_from_env(Some("  ")).unwrap().is_none());
        let bad = clock_from_env(Some("tomorrow")).unwrap_err().to_string();
        assert!(
            bad.contains("VOGT_TEST_CLOCK_START must be an RFC3339 timestamp, not 'tomorrow'"),
            "{bad}"
        );
        let mut clock = clock_from_env(Some("2026-01-02T03:04:05Z"))
            .unwrap()
            .unwrap();
        assert_eq!(clock.now().to_iso(), "2026-01-02T03:04:05+00:00");

        let bad = ids_from_env(Some("random"), None).unwrap_err().to_string();
        assert!(
            bad.contains("VOGT_TEST_IDS must be 'sequential' when set, not 'random'"),
            "{bad}"
        );

        assert!(is_loopback("localhost") && is_loopback("[::1]") && is_loopback("127.0.0.1"));
        assert!(!is_loopback("localhost.example") && !is_loopback("0.0.0.0"));
        assert!(refuse_hooks_off_loopback("127.0.0.1", Some("x"), None).is_ok());
        let refused = refuse_hooks_off_loopback("0.0.0.0", Some("x"), Some("sequential"))
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("refusing to serve on 0.0.0.0"),
            "{refused}"
        );
        assert!(
            refused.contains("VOGT_TEST_CLOCK_START, VOGT_TEST_IDS"),
            "{refused}"
        );
    }

    #[test]
    fn a_suppression_is_active_until_it_is_revoked() {
        assert_eq!(
            serde_json::to_string(&MatchKind::Pattern).unwrap(),
            "\"pattern\""
        );
        let mut suppression = Suppression {
            id: "sup_0001".to_string(),
            match_kind: MatchKind::Exact,
            subject_key_or_pattern: "git:main".to_string(),
            scope_project_id: None,
            scope_project_slug: None,
            actor_id: "act_0001".to_string(),
            actor_identity_ref: None,
            reason: "noise".to_string(),
            created_at: from_iso("2026-01-02T03:04:05+00:00").unwrap(),
            revoked_at: None,
            revoked_reason: None,
        };
        assert!(suppression.active());
        suppression.revoked_at = Some(suppression.created_at);
        assert!(!suppression.active());
    }

    #[test]
    fn the_vocabulary_serialises_as_pythons_literals() {
        assert_eq!(
            serde_json::to_string(&WorkKind::Feature).unwrap(),
            "\"feature\""
        );
        assert_eq!(serde_json::to_string(&Priority::P2).unwrap(), "\"p2\"");
        assert_eq!(serde_json::to_string(&Effort::Xl).unwrap(), "\"xl\"");
        assert_eq!(
            serde_json::to_string(&Origin::Observed).unwrap(),
            "\"observed\""
        );
        assert_eq!(
            serde_json::to_string(&TrustState::Unverified).unwrap(),
            "\"unverified\""
        );
        assert_eq!(
            serde_json::to_string(&RelationKind::ImplementedBy).unwrap(),
            "\"implemented_by\""
        );
        assert_eq!(
            serde_json::to_string(&InitiativeState::Closed).unwrap(),
            "\"closed\""
        );
        assert!(serde_json::from_str::<RelationKind>("\"invented\"").is_err());
        assert_eq!(
            serde_json::to_string(&LinkRelation::Completion).unwrap(),
            "\"completion\""
        );
        assert_eq!(
            serde_json::to_string(&TokenKind::Agent).unwrap(),
            "\"agent\""
        );
        assert_eq!(
            serde_json::to_string(&AuthOutcome::Deny).unwrap(),
            "\"deny\""
        );
        assert_eq!(
            serde_json::to_string(&WriteBackAction::Reopen).unwrap(),
            "\"reopen\""
        );
        assert_eq!(
            serde_json::to_string(&WriteBackOutcome::Skipped).unwrap(),
            "\"skipped\""
        );
    }

    /// A field pydantic defaults must decode when the payload omits it.
    #[test]
    fn omitted_fields_take_the_pydantic_default() {
        let initiative: Initiative = serde_json::from_str(
            r#"{"id":"i","slug":"s","title":"t","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(initiative.body, "");
        assert_eq!(initiative.state, InitiativeState::Open);
        assert_eq!(initiative.weight, 0);

        let link: WorkLink = serde_json::from_str(
            r#"{"work_item_id":"w","subject_key":"k","origin_kind":"git","created_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(link.relation, LinkRelation::Completion);

        let token: Token = serde_json::from_str(
            r#"{"id":"t","actor_id":"a","name":"n","scopes":[],"created_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(token.kind, TokenKind::Api);
    }

    /// pydantic's `extra="forbid"`: a key the model does not declare is an error.
    #[test]
    fn an_unknown_field_is_refused() {
        let err = serde_json::from_str::<Label>(
            r#"{"id":"l","name":"bug","color":null,"created_at":"2026-01-01T00:00:00Z","extra":1}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn the_json_names_match_python() {
        // ActorKind is snake_case and WorkItem's handle is "ref", as pydantic
        // emits them. A moment's JSON form is the Z suffix pydantic writes for
        // model_dump(mode="json"); the +00:00 form is what the columns store.
        assert_eq!(
            serde_json::to_string(&ActorKind::Human).unwrap(),
            "\"human\""
        );
        let moment = from_iso("2026-01-02T03:04:05.123456+00:00").unwrap();
        assert_eq!(moment.to_iso(), "2026-01-02T03:04:05.123456+00:00");
        assert_eq!(
            serde_json::to_string(&moment).unwrap(),
            "\"2026-01-02T03:04:05.123456Z\""
        );
        let whole = from_iso("2026-01-02T03:04:05+00:00").unwrap();
        assert_eq!(
            serde_json::to_string(&whole).unwrap(),
            "\"2026-01-02T03:04:05Z\""
        );
        let item = WorkItem {
            id: "wrk_0001".to_string(),
            reference: "WI-1".to_string(),
            kind: "bug".parse().unwrap(),
            title: "a title".to_string(),
            body: String::new(),
            state: "open".to_string(),
            priority: "p2".parse().unwrap(),
            effort: None,
            project_id: None,
            project_slug: None,
            initiative_id: None,
            origin: "created".parse().unwrap(),
            trust_state: "unverified".parse().unwrap(),
            assignee_actor_id: None,
            assignee_identity_ref: None,
            labels: Vec::new(),
            relations: Vec::new(),
            superseded_by: None,
            created_at: from_iso("2026-01-02T03:04:05Z").unwrap(),
            updated_at: from_iso("2026-01-02T03:04:05Z").unwrap(),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["ref"], "WI-1");
        assert!(json.get("reference").is_none());
        assert_eq!(json["created_at"], "2026-01-02T03:04:05Z");
        let back: WorkItem = serde_json::from_value(json).unwrap();
        assert_eq!(back, item);
    }

    fn a_principal_needs_an_identity() {
        assert!(Principal::new("  ", ActorKind::Human, "nobody").is_err());
        let local = local_principal("sprooty");
        assert_eq!(local.identity_ref, "local:sprooty");
        assert_eq!(local.kind, ActorKind::Human);
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
    fn entities_carry_the_python_defaults() {
        let now = Moment::from_unix(1_700_000_000, 0);
        let project = Project::new("prj_1", "vogt", "Vogt", "/src", now);
        assert_eq!(project.lifecycle_state, ProjectLifecycle::Active);
        assert_eq!(project.compliance_status, ComplianceStatus::NotChecked);
        assert_eq!(project.link_state, LinkState::Unlinked);
        assert_eq!(project.write_back.to_string(), "none");
        assert!(require_text("  ").is_err());
        assert_eq!(require_text("  because  ").unwrap(), "because");
        assert!(!DECLARABLE_RELATION_KINDS.contains(&"implemented_by"));

        let grant = SessionGrant {
            id: "g".into(),
            target_engine_session_id: "eng".into(),
            kind: GrantKind::Credential,
            var: None,
            project_id: None,
            secret_name: None,
            capability: None,
            uses: GrantUses::Once,
            ttl_seconds: 60,
            reason: "push".into(),
            requested_by: "act".into(),
            requested_at: now,
            state: GrantState::Approved,
            decided_by: None,
            decided_at: None,
            decision_reason: None,
            expires_at: Some(now),
            revoked_by: None,
            revoked_at: None,
        };
        assert_eq!(grant.effective_state(now), "expired");
        assert_eq!(
            grant.effective_state(Moment::from_unix(1_699_999_999, 0)),
            "approved"
        );
        let pending = SessionGrant {
            state: GrantState::Pending,
            expires_at: Some(now),
            ..grant
        };
        assert_eq!(
            pending.effective_state(Moment::from_unix(1_800_000_000, 0)),
            "pending"
        );
    }

    #[test]
    fn the_seven_ported_entities_round_trip_python_json() {
        // Pinned against pydantic model_dump(mode="json") for the same values.
        let cases: &[(&str, &str)] = &[
            (
                "AuditRecord",
                r#"{"actor_id":"act_1","actor_identity_ref":"local:a","at":"2024-01-02T00:00:00Z","entity_id":"prj_1","entity_kind":"project","id":"aud_1","operation":"op","payload_digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","reason":"because","revision":3,"txn_id":"txn_1"}"#,
            ),
            (
                "Event",
                r#"{"actor_id":null,"at":"2024-01-02T00:00:00Z","audit_id":null,"entity_id":"prj_1","entity_kind":"project","kind":"created","seq":1,"summary":{"verb":"created"}}"#,
            ),
            (
                "DriftProposal",
                r#"{"evidence_observation_id":null,"evidence_snapshot":{},"id":"dft_1","kind":"status","opened_at":"2024-01-02T00:00:00Z","project_id":null,"project_slug":null,"proposed_change":{},"resolution_reason":null,"resolved_at":null,"resolved_by_actor_id":null,"resolved_by_identity_ref":null,"status":"open","subject_id":"prj_1","subject_kind":"project","summary":"s","superseded_at":null,"superseded_detail":null}"#,
            ),
            (
                "InboxTriage",
                r#"{"actor_id":"act_1","actor_identity_ref":null,"decided_at":"2024-01-02T00:00:00Z","entry_key":"k","occurrence_snapshot":{},"snooze_until":null,"state":"active"}"#,
            ),
            (
                "ActorPreference",
                r#"{"actor_id":"act_1","key":"theme","updated_at":"2024-01-02T00:00:00Z","value":{"mode":"dark"},"version":1}"#,
            ),
            (
                "DepRef",
                r#"{"from_project_id":"prj_1","from_project_slug":null,"manifest":null,"observed_at":"2024-01-02T00:00:00Z","raw_target":"../x","ref_kind":"path","subject_key":"sub","to_project_id":null,"to_project_slug":null}"#,
            ),
            (
                "Sweep",
                r#"{"collector":"git","detail":null,"finished_at":null,"id":"swp_1","outcome":"running","scope":["prj_1"],"started_at":"2024-01-02T00:00:00Z","stats":{}}"#,
            ),
        ];
        for (name, json) in cases {
            let value: serde_json::Value = serde_json::from_str(json).unwrap();
            let back = match *name {
                "AuditRecord" => serde_json::to_value(
                    serde_json::from_value::<AuditRecord>(value.clone()).unwrap(),
                )
                .unwrap(),
                "Event" => {
                    serde_json::to_value(serde_json::from_value::<Event>(value.clone()).unwrap())
                        .unwrap()
                }
                "DriftProposal" => serde_json::to_value(
                    serde_json::from_value::<DriftProposal>(value.clone()).unwrap(),
                )
                .unwrap(),
                "InboxTriage" => serde_json::to_value(
                    serde_json::from_value::<InboxTriage>(value.clone()).unwrap(),
                )
                .unwrap(),
                "ActorPreference" => serde_json::to_value(
                    serde_json::from_value::<ActorPreference>(value.clone()).unwrap(),
                )
                .unwrap(),
                "DepRef" => {
                    serde_json::to_value(serde_json::from_value::<DepRef>(value.clone()).unwrap())
                        .unwrap()
                }
                "Sweep" => {
                    serde_json::to_value(serde_json::from_value::<Sweep>(value.clone()).unwrap())
                        .unwrap()
                }
                _ => unreachable!(),
            };
            assert_eq!(back, value, "{name}");
        }
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
