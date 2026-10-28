//! Clock, timestamps and the deterministic test hooks. Ports the time helpers
//! of `src/vogt/core/`.

use std::collections::BTreeMap;

use crate::errors::VogtError;

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

    /// `now + timedelta(days=n)`. A day is 86400 seconds, as Python's timedelta
    /// counts it, with no calendar arithmetic.
    pub fn plus_days(self, days: i64) -> Self {
        Self {
            unix_seconds: self.unix_seconds + days * 86_400,
            nanos: self.nanos,
        }
    }

    /// Always `+00:00`. A moment is UTC, so there is no other offset to print.
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
    /// Whether a read moves the clock. A step clock does, one second at a time,
    /// and a wall clock does not. A caller that needs the next instant without
    /// spending this one asks here instead of reading.
    fn steps(&self) -> bool {
        false
    }
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&mut self) -> Moment {
        utc_now()
    }
}

/// One second later on every read. Selected by `VOGT_TEST_CLOCK_START`.
#[derive(Debug, Clone)]
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

    fn steps(&self) -> bool {
        true
    }
}

/// Mints ids. Held by the caller, so the domain stays free of randomness.
pub trait IdFactory {
    fn next(&mut self, prefix: &str) -> String;
}

/// `{prefix}_{n:04d}`, persisted as sorted JSON. `VOGT_TEST_IDS=sequential`.
#[derive(Debug, Clone)]
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
/// ` x ` reads `' x '` and `it's` reads `"it's"`. A character `str.isprintable`
/// rejects — anything but a graphic or U+0020 — is `\xNN`, `\uNNNN` or
/// `\UNNNNNNNN`.
pub fn py_repr(raw: &str) -> String {
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
            c if !py_printable(c) => push_py_escape(&mut out, c),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable`: a graphic character or a space, and the only space is
/// U+0020. C0/C1 controls, format characters, line and paragraph separators,
/// private-use and unassigned code points are all rejected.
pub(crate) fn py_printable(ch: char) -> bool {
    if ch.is_control() || (ch.is_whitespace() && ch != ' ') {
        return false;
    }
    use unicode_general_category::GeneralCategory;
    !matches!(
        unicode_general_category::get_general_category(ch),
        GeneralCategory::Unassigned
            | GeneralCategory::PrivateUse
            | GeneralCategory::Surrogate
            | GeneralCategory::Format
            | GeneralCategory::Control
    )
}

fn push_py_escape(out: &mut String, ch: char) {
    let cp = u32::from(ch);
    if cp < 0x100 {
        out.push_str(&format!("\\x{cp:02x}"));
    } else if cp < 0x10000 {
        out.push_str(&format!("\\u{cp:04x}"));
    } else {
        out.push_str(&format!("\\U{cp:08x}"));
    }
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

/// A timestamp as pydantic's `mode="json"` renders a UTC datetime: the same as
/// `to_iso`, except the offset is `Z` rather than `+00:00`. Answers use this;
/// storage keeps `to_iso`.
pub fn to_json_timestamp(moment: Moment) -> String {
    moment.to_iso().replace("+00:00", "Z")
}

/// A timestamp parsed the way CPython 3.12 `datetime.fromisoformat` parses it,
/// before it is folded into UTC: the wall clock as written, its fraction in
/// microseconds, and the offset in seconds as written. `None` is a naive value.
///
/// The offset keeps its seconds here; pydantic's JSON form prints only
/// `±HH:MM`, truncating toward zero, and that is the renderer's job.
pub struct ParsedTimestamp {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub micros: u32,
    pub offset_seconds: Option<i64>,
}

/// Parse what CPython 3.12 `datetime.fromisoformat` accepts, keeping the offset
/// instead of folding it into UTC. A missing offset is `None`. A trailing `Z`
/// is accepted only as uppercase, because Python rewrites only `"Z"`.
pub fn parse_timestamp(text: &str) -> Result<ParsedTimestamp, String> {
    let bytes = text.as_bytes();
    let (year, month, day, consumed) = parse_iso_date(bytes)?;
    let (hour, minute, second, micros, offset) = if consumed == bytes.len() {
        (0, 0, 0, 0, None)
    } else {
        let (hour, minute, second, micros, seconds, _, offset_len) =
            parse_iso_time(&bytes[consumed..])?;
        // `offset_len` counts the separator. Nothing after it means no offset.
        let offset = (offset_len > 1).then_some(seconds);
        (hour, minute, second, micros, offset)
    };
    // Reject a date or time the calendar does not have, as Python does.
    chrono::NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| format!("not a timestamp: {text}"))?;
    chrono::NaiveTime::from_hms_micro_opt(hour, minute, second, micros)
        .ok_or_else(|| format!("not a timestamp: {text}"))?;
    Ok(ParsedTimestamp {
        year,
        month,
        day,
        hour,
        minute,
        second,
        micros,
        offset_seconds: offset,
    })
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
        let (hour, minute, second, micros, seconds, micros_part, _) =
            parse_iso_time(&bytes[consumed..])?;
        (hour, minute, second, micros, seconds, micros_part)
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
/// date from the time, so `T`, `t` and `X` all work. The tuple is the clock,
/// its microseconds, the offset in seconds and microseconds, and how many
/// bytes of offset were present (zero when there was none).
fn parse_iso_time(bytes: &[u8]) -> Result<IsoTime, String> {
    if bytes.is_empty() {
        return Err("not a timestamp".to_string());
    }
    let rest = &bytes[1..];
    let (hour, minute, second, at) = parse_hms(rest)?;
    let (micros, at) = parse_fraction(rest, at)?;
    let (offset, offset_len) = if at == rest.len() {
        ((0, 0), 0)
    } else {
        (parse_offset(&rest[at..])?, rest.len() - at)
    };
    Ok((hour, minute, second, micros, offset.0, offset.1, offset_len))
}

type IsoTime = (u32, u32, u32, u32, i64, i64, usize);

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
