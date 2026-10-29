//! Usage board readings (WI-2040). Ports `src/vogt/core/usage_board.py`.
//!
//! Text and numbers in, values out. This module opens no file, asks no clock
//! and stores nothing.

#![allow(dead_code)] // Domain vocabulary the bin re-exports; tests and later services use what the bin does not.
use std::collections::BTreeMap;

use regex::Regex;
use serde_json::Value;

use super::Moment;

/// `budget_duration` values LiteLLM accepts that mean "resets daily".
const DAILY: &[&str] = &["1d", "24h", "1440m", "86400s"];

#[derive(Debug, Clone, Default)]
pub struct LimitWindow {
    pub name: String,
    pub used_percent: Option<f64>,
    pub window_minutes: Option<i64>,
    pub resets_at: Option<Moment>,
    /// The provider's own words for the reset when it gave no timestamp.
    pub resets_text: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AccountReading {
    pub windows: Vec<LimitWindow>,
    pub limited_at: Option<Moment>,
    pub limit_reached: Option<String>,
    pub plan: Option<String>,
    pub tokens_5h: i64,
    pub observed_at: Option<Moment>,
}

#[derive(Debug, Clone)]
pub struct KeySpend {
    pub spend_usd: Option<f64>,
    pub max_budget_usd: Option<f64>,
    pub budget_duration: Option<String>,
    pub budget_reset_at: Option<Moment>,
    pub daily: bool,
    pub alert_usd: Option<f64>,
    pub alert: bool,
    pub alias: Option<String>,
    pub detail: Option<String>,
}

fn claude_epoch() -> Regex {
    Regex::new(r"limit reached\|(\d{9,11})").expect("pattern")
}
fn claude_resets() -> Regex {
    Regex::new(r#"(?i)\bresets\s+(?:at\s+)?([^\n"]{1,60})"#).expect("pattern")
}
fn claude_limit() -> Regex {
    Regex::new(r"(?i)(usage limit reached|limit reached|hit your limit)").expect("pattern")
}

/// Python's `datetime.fromisoformat` after replacing `Z` with `+00:00`, plus
/// epoch numbers. A naive value is read as UTC.
fn parse_time(value: &Value) -> Option<Moment> {
    match value {
        Value::Bool(_) | Value::Null => None,
        Value::Number(number) => {
            let seconds = number.as_f64()?;
            if !seconds.is_finite() {
                return None;
            }
            let whole = seconds.trunc() as i64;
            let nanos = ((seconds - whole as f64) * 1_000_000_000.0).round() as u32;
            chrono::DateTime::from_timestamp(whole, nanos).map(|_| Moment::from_unix(whole, nanos))
        }
        Value::String(text) if !text.is_empty() => {
            let normalized = text.replace('Z', "+00:00");
            let parsed = chrono::DateTime::parse_from_rfc3339(&normalized)
                .or_else(|_| {
                    let fmt = "%Y-%m-%dT%H:%M:%S%.6f";
                    chrono::NaiveDateTime::parse_from_str(&normalized, fmt)
                        .map(|naive| naive.and_utc().fixed_offset())
                })
                .or_else(|_| {
                    let fmt = "%Y-%m-%dT%H:%M:%S";
                    chrono::NaiveDateTime::parse_from_str(&normalized, fmt)
                        .map(|naive| naive.and_utc().fixed_offset())
                })
                .ok()?;
            Some(Moment::from_unix(
                parsed.timestamp(),
                parsed.timestamp_subsec_nanos(),
            ))
        }
        _ => None,
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(_) => None,
        Value::Number(number) => number.as_f64(),
        _ => None,
    }
}

fn lines(text: &str) -> Vec<serde_json::Map<String, Value>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(Value::Object(entry)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        out.push(entry);
    }
    out
}

/// The newest rate limits a Codex rollout transcript reported.
pub fn codex_reading(text: &str) -> AccountReading {
    let mut latest: Option<serde_json::Map<String, Value>> = None;
    let mut latest_at: Option<Moment> = None;
    for entry in lines(text) {
        let Some(Value::Object(payload)) = entry.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("token_count") {
            continue;
        }
        if let Some(Value::Object(limits)) = payload.get("rate_limits") {
            latest_at = entry.get("timestamp").and_then(parse_time);
            latest = Some(limits.clone());
        }
    }
    let Some(latest) = latest else {
        return AccountReading::default();
    };
    let mut windows = Vec::new();
    for name in ["primary", "secondary"] {
        let Some(Value::Object(raw)) = latest.get(name) else {
            continue;
        };
        let minutes = number(raw.get("window_minutes").unwrap_or(&Value::Null));
        let mut resets_at = raw.get("resets_at").and_then(parse_time);
        let seconds = number(raw.get("resets_in_seconds").unwrap_or(&Value::Null));
        if resets_at.is_none() {
            if let (Some(seconds), Some(at)) = (seconds, latest_at) {
                let whole = seconds.trunc() as i64;
                let nanos = ((seconds - whole as f64) * 1_000_000_000.0).round() as i64;
                let total = at.unix_seconds() + whole + at.nanos() as i64 / 1_000_000_000;
                let nanos = at.nanos() as i64 % 1_000_000_000 + nanos;
                let (carry, nanos) = if nanos >= 1_000_000_000 {
                    (1, nanos - 1_000_000_000)
                } else if nanos < 0 {
                    (-1, nanos + 1_000_000_000)
                } else {
                    (0, nanos)
                };
                resets_at = Some(Moment::from_unix(total + carry, nanos as u32));
            }
        }
        windows.push(LimitWindow {
            name: name.to_string(),
            used_percent: number(raw.get("used_percent").unwrap_or(&Value::Null)),
            window_minutes: minutes.map(|value| value as i64),
            resets_at,
            resets_text: None,
        });
    }
    let reached = latest
        .get("rate_limit_reached_type")
        .and_then(Value::as_str);
    let plan = latest.get("plan_type").and_then(Value::as_str);
    AccountReading {
        windows,
        limited_at: reached.filter(|text| !text.is_empty()).and(latest_at),
        limit_reached: reached.filter(|text| !text.is_empty()).map(str::to_string),
        plan: plan.filter(|text| !text.is_empty()).map(str::to_string),
        tokens_5h: 0,
        observed_at: latest_at,
    }
}

fn texts(entry: &serde_json::Map<String, Value>) -> Vec<String> {
    let Some(Value::Object(message)) = entry.get("message") else {
        return Vec::new();
    };
    match message.get("content") {
        Some(Value::String(content)) => vec![content.clone()],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| match block {
                Value::Object(block) => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// What one Claude account's transcripts say.
pub fn claude_reading(transcripts: &[String], now: Moment) -> AccountReading {
    let since = now.unix_seconds() - 5 * 3600;
    let mut by_message: BTreeMap<String, i64> = BTreeMap::new();
    let mut unnamed: i64 = 0;
    let mut answered_at: Option<Moment> = None;
    let mut hit_at: Option<Moment> = None;
    let mut hit_text: Option<String> = None;
    let mut newest: Option<Moment> = None;
    let limit = claude_limit();
    for text in transcripts {
        for entry in lines(text) {
            let at = entry.get("timestamp").and_then(parse_time);
            if let Some(at) = at {
                if newest
                    .map(|current| {
                        at.unix_seconds() > current.unix_seconds()
                            || (at.unix_seconds() == current.unix_seconds()
                                && at.nanos() > current.nanos())
                    })
                    .unwrap_or(true)
                {
                    newest = Some(at);
                }
            }
            if let Some(Value::Object(message)) = entry.get("message") {
                if let (Some(Value::Object(usage)), Some(at)) = (message.get("usage"), at) {
                    if answered_at
                        .map(|current| later(at, current))
                        .unwrap_or(true)
                    {
                        answered_at = Some(at);
                    }
                    if at.unix_seconds() >= since {
                        let mut count = 0i64;
                        for key in [
                            "input_tokens",
                            "output_tokens",
                            "cache_creation_input_tokens",
                        ] {
                            if let Some(Value::Number(value)) = usage.get(key) {
                                if let Some(whole) = value.as_i64() {
                                    count += whole;
                                }
                            }
                        }
                        match message.get("id").and_then(Value::as_str) {
                            Some(id) if !id.is_empty() => {
                                by_message.insert(id.to_string(), count);
                            }
                            _ => unnamed += count,
                        }
                    }
                }
            }
            let is_error = entry.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true);
            let rate = entry.get("error").and_then(Value::as_str) == Some("rate_limit");
            if !is_error && !rate {
                continue;
            }
            for said in texts(&entry) {
                if limit.is_match(&said)
                    && hit_at
                        .map(|current| at.is_some_and(|at| !earlier(at, current)))
                        .unwrap_or(true)
                {
                    hit_at = at;
                    hit_text = Some(said);
                }
            }
        }
    }
    let tokens = by_message.values().sum::<i64>() + unnamed;
    let lifted = match (hit_at, answered_at) {
        (Some(hit), Some(answered)) => later(answered, hit),
        _ => false,
    };
    let mut windows = Vec::new();
    if let Some(hit_text) = hit_text.clone() {
        let epoch = claude_epoch();
        let words = claude_resets();
        let resets_at = epoch
            .captures(&hit_text)
            .and_then(|caps| caps.get(1))
            .and_then(|hit| hit.as_str().parse::<i64>().ok())
            .map(|seconds| Moment::from_unix(seconds, 0));
        let resets_text = if resets_at.is_none() {
            words
                .captures(&hit_text)
                .and_then(|caps| caps.get(1))
                .map(|hit| hit.as_str().trim().to_string())
        } else {
            None
        };
        let still = match resets_at {
            None => true,
            Some(resets) => later(resets, now),
        };
        if !lifted && still {
            windows.push(LimitWindow {
                name: "limit".to_string(),
                resets_at,
                resets_text,
                ..LimitWindow::default()
            });
        }
    }
    AccountReading {
        windows,
        limited_at: hit_at,
        limit_reached: hit_text.map(|text| text.chars().take(160).collect()),
        plan: None,
        tokens_5h: tokens,
        observed_at: newest,
    }
}

fn later(left: Moment, right: Moment) -> bool {
    left.unix_seconds() > right.unix_seconds()
        || (left.unix_seconds() == right.unix_seconds() && left.nanos() > right.nanos())
}

fn earlier(left: Moment, right: Moment) -> bool {
    left.unix_seconds() < right.unix_seconds()
        || (left.unix_seconds() == right.unix_seconds() && left.nanos() < right.nanos())
}

/// A LiteLLM `/key/info` answer, with its alert decided.
pub fn key_spend(info: &Value, alert_usd: Option<f64>) -> KeySpend {
    let body = info
        .get("info")
        .filter(|value| value.is_object())
        .unwrap_or(info);
    let spend = body.get("spend").and_then(number);
    let duration = body
        .get("budget_duration")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let daily = duration
        .as_deref()
        .is_some_and(|value| DAILY.contains(&value));
    let over = alert_usd.is_some_and(|alert| spend.is_some_and(|spend| spend >= alert));
    let alert = daily && over;
    let detail = if daily {
        None
    } else {
        let head = match &duration {
            None => "spend is the key total: it has no one-day budget window, so a one day spend is not reported".to_string(),
            Some(duration) => format!("spend is for the {duration} budget window, not one day"),
        };
        let tail = if alert_usd.is_some() {
            "; no day alert is raised on it".to_string()
        } else {
            String::new()
        };
        Some(format!("{}{}", head, tail))
    };
    let alias = body
        .get("key_alias")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    KeySpend {
        spend_usd: spend,
        max_budget_usd: body.get("max_budget").and_then(number),
        budget_duration: duration,
        budget_reset_at: body.get("budget_reset_at").and_then(parse_time),
        daily,
        alert_usd,
        alert,
        alias,
        detail,
    }
}

/// Whether an account is limited now.
pub fn is_limited(reading: &AccountReading, now: Moment) -> bool {
    for window in &reading.windows {
        if window.resets_at.is_some_and(|resets| !later(resets, now)) {
            continue;
        }
        if window.used_percent.is_some_and(|used| used >= 100.0) {
            return true;
        }
        if window.name == "limit"
            && window
                .resets_at
                .map(|resets| later(resets, now))
                .unwrap_or(true)
        {
            return true;
        }
    }
    false
}
