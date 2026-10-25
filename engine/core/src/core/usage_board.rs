//! Usage board readings. Ports `src/vogt/core/usage_board.py` (WI-2040 / WI-2144).
//!
//! Takes transcript text and `/key/info` JSON and returns values. Opens no
//! file, asks no clock, and stores nothing.

use std::collections::BTreeMap;

use regex::Regex;
use serde_json::Value;

use crate::core::Moment;

#[derive(Debug, Clone, PartialEq)]
pub struct LimitWindow {
    pub name: String,
    pub used_percent: Option<f64>,
    pub window_minutes: Option<i64>,
    pub resets_at: Option<Moment>,
    pub resets_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AccountReading {
    pub windows: Vec<LimitWindow>,
    pub limited_at: Option<Moment>,
    pub limit_reached: Option<String>,
    pub plan: Option<String>,
    pub tokens_5h: i64,
    pub observed_at: Option<Moment>,
}

#[derive(Debug, Clone, PartialEq)]
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

const DAILY: &[&str] = &["1d", "24h", "1440m", "86400s"];

fn claude_epoch() -> Regex {
    Regex::new(r"limit reached\|(\d{9,11})").expect("static pattern")
}
fn claude_resets() -> Regex {
    Regex::new(r#"(?i)\bresets\s+(?:at\s+)?([^\n"]{1,60})"#).expect("static pattern")
}
fn claude_limit() -> Regex {
    Regex::new(r"(?i)(usage limit reached|limit reached|hit your limit)").expect("static pattern")
}

/// A JSON number that is not a bool. Strings are not numbers here: Python's
/// `int | float` check rejects them.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn parse_time(value: &Value) -> Option<Moment> {
    match value {
        Value::Bool(_) => None,
        Value::Number(n) => {
            let raw = n.as_f64()?;
            if !raw.is_finite() {
                return None;
            }
            Some(Moment::from_unix(raw as i64, 0))
        }
        Value::String(text) if !text.is_empty() => {
            let normalised = text.replace('Z', "+00:00");
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&normalised) {
                return Some(Moment::from_unix(
                    dt.timestamp(),
                    dt.timestamp_subsec_nanos(),
                ));
            }
            let naive = chrono::NaiveDateTime::parse_from_str(&normalised, "%Y-%m-%dT%H:%M:%S%.f")
                .or_else(|_| {
                    chrono::NaiveDateTime::parse_from_str(&normalised, "%Y-%m-%dT%H:%M:%S")
                })
                .ok()?;
            Some(Moment::from_unix(naive.and_utc().timestamp(), 0))
        }
        _ => None,
    }
}

fn lines(text: &str) -> impl Iterator<Item = Value> + '_ {
    text.lines().filter_map(|line| {
        let line = line.trim();
        if !line.starts_with('{') {
            return None;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(Value::Object(_)) => serde_json::from_str(line).ok(),
            _ => None,
        }
    })
}

/// The newest rate limits a Codex rollout transcript reported.
pub fn codex_reading(text: &str) -> AccountReading {
    let mut latest: Option<Value> = None;
    let mut latest_at: Option<Moment> = None;
    for entry in lines(text) {
        let Some(payload) = entry.get("payload").filter(|v| v.is_object()) else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("token_count") {
            continue;
        }
        if let Some(limits) = payload.get("rate_limits").filter(|v| v.is_object()) {
            latest = Some(limits.clone());
            latest_at = entry.get("timestamp").and_then(parse_time);
        }
    }
    let Some(latest) = latest else {
        return AccountReading::default();
    };
    let mut windows = Vec::new();
    for name in ["primary", "secondary"] {
        let Some(raw) = latest.get(name).filter(|v| v.is_object()) else {
            continue;
        };
        let minutes = raw.get("window_minutes").and_then(number);
        let mut resets_at = raw.get("resets_at").and_then(parse_time);
        let seconds = raw.get("resets_in_seconds").and_then(number);
        if resets_at.is_none() {
            if let (Some(seconds), Some(at)) = (seconds, latest_at) {
                resets_at = Some(Moment::from_unix(
                    at.unix_seconds() + seconds as i64,
                    at.nanos(),
                ));
            }
        }
        windows.push(LimitWindow {
            name: name.to_string(),
            used_percent: raw.get("used_percent").and_then(number),
            window_minutes: minutes.map(|m| m as i64),
            resets_at,
            resets_text: None,
        });
    }
    let reached = latest
        .get("rate_limit_reached_type")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let plan = latest
        .get("plan_type")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    AccountReading {
        windows,
        limited_at: reached.as_ref().and(latest_at),
        limit_reached: reached,
        plan,
        tokens_5h: 0,
        observed_at: latest_at,
    }
}

fn texts(entry: &Value) -> Vec<String> {
    let Some(message) = entry.get("message").filter(|v| v.is_object()) else {
        return Vec::new();
    };
    match message.get("content") {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// What one Claude account's transcripts say.
pub fn claude_reading(tails: &[String], now: Moment) -> AccountReading {
    let since = now.unix_seconds() - 5 * 3600;
    let mut by_message: BTreeMap<String, i64> = BTreeMap::new();
    let mut unnamed = 0_i64;
    let mut answered_at: Option<Moment> = None;
    let mut hit_at: Option<Moment> = None;
    let mut hit_text: Option<String> = None;
    let mut newest: Option<Moment> = None;
    for text in tails {
        for entry in lines(text) {
            let at = entry.get("timestamp").and_then(parse_time);
            if let Some(at) = at {
                if newest.map(|seen| at > seen).unwrap_or(true) {
                    newest = Some(at);
                }
            }
            if let Some(usage) = entry
                .get("message")
                .filter(|v| v.is_object())
                .and_then(|message| message.get("usage"))
                .filter(|v| v.is_object())
            {
                if let Some(at) = at {
                    if answered_at.map(|seen| at > seen).unwrap_or(true) {
                        answered_at = Some(at);
                    }
                    if at.unix_seconds() >= since {
                        let count = integer_tokens(usage);
                        let message_id = entry
                            .get("message")
                            .and_then(|message| message.get("id"))
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty());
                        if let Some(id) = message_id {
                            by_message.insert(id.to_string(), count);
                        } else {
                            unnamed += count;
                        }
                    }
                }
            }
            let api_error = entry
                .get("isApiErrorMessage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let rate_limit = entry.get("error").and_then(Value::as_str) == Some("rate_limit");
            if !api_error && !rate_limit {
                continue;
            }
            for said in texts(&entry) {
                if !claude_limit().is_match(&said) {
                    continue;
                }
                let newer = match (hit_at, at) {
                    (None, _) => true,
                    (Some(_), Some(at)) if hit_at.is_some_and(|hit| at >= hit) => true,
                    _ => false,
                };
                if newer {
                    hit_at = at;
                    hit_text = Some(said);
                }
            }
        }
    }
    let tokens = by_message.values().sum::<i64>() + unnamed;
    let lifted = hit_at.is_some()
        && answered_at.is_some_and(|answered| hit_at.is_some_and(|hit| answered > hit));
    let mut windows = Vec::new();
    if let Some(text) = &hit_text {
        let epoch = claude_epoch()
            .captures(text)
            .and_then(|caps| caps.get(1))
            .and_then(|m| m.as_str().parse::<i64>().ok())
            .map(|epoch| Moment::from_unix(epoch, 0));
        let words = claude_resets()
            .captures(text)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().trim().to_string());
        let resets_text = if epoch.is_some() { None } else { words };
        let window = LimitWindow {
            name: "limit".to_string(),
            used_percent: None,
            window_minutes: None,
            resets_at: epoch,
            resets_text,
        };
        let still = !lifted && epoch.map(|resets| resets > now).unwrap_or(true);
        if still {
            windows.push(window);
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

fn integer_tokens(usage: &Value) -> i64 {
    [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
    ]
    .into_iter()
    .map(|key| match usage.get(key) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    })
    .sum()
}

/// A LiteLLM `/key/info` answer, with its alert decided.
pub fn key_spend(info: &Value, alert_usd: Option<f64>) -> KeySpend {
    let body = info
        .get("info")
        .filter(|inner| inner.is_object())
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
    let over = match (alert_usd, spend) {
        (Some(threshold), Some(spend)) => spend >= threshold,
        _ => false,
    };
    let alert = daily && over;
    let detail = if daily {
        None
    } else {
        let base = match &duration {
            None => "spend is the key's total: it has no one-day budget window, so a day's spend is not reported".to_string(),
            Some(duration) => format!("spend is for the key's {duration} budget window, not one day"),
        };
        Some(if alert_usd.is_some() {
            format!("{base}; no day alert is raised on it")
        } else {
            base
        })
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
        if window.resets_at.is_some_and(|resets| resets <= now) {
            continue;
        }
        if window.used_percent.is_some_and(|used| used >= 100.0) {
            return true;
        }
        if window.name == "limit" && window.resets_at.map(|resets| resets > now).unwrap_or(true) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(seconds: i64) -> Moment {
        Moment::from_unix(seconds, 0)
    }

    #[test]
    fn codex_newest_event_wins_and_unknown_reset_uses_seconds() {
        let text = r#"
{"timestamp":"2026-01-01T00:00:00Z","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":10,"window_minutes":300,"resets_at":1767225600},"plan_type":"old"}}}
{"timestamp":"2026-01-01T01:00:00+00:00","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":100,"window_minutes":300,"resets_in_seconds":60},"secondary":{"used_percent":"nope"},"rate_limit_reached_type":"primary","plan_type":"plus"}}}
"#;
        let reading = codex_reading(text);
        assert_eq!(reading.plan.as_deref(), Some("plus"));
        assert_eq!(reading.limit_reached.as_deref(), Some("primary"));
        assert_eq!(reading.windows.len(), 1);
        assert_eq!(reading.windows[0].used_percent, Some(100.0));
        assert_eq!(
            reading.windows[0].resets_at,
            Some(at(reading.observed_at.unwrap().unix_seconds() + 60))
        );
        assert!(is_limited(
            &reading,
            at(reading.observed_at.unwrap().unix_seconds())
        ));
    }

    #[test]
    fn claude_sums_each_message_once_and_lifts_after_a_later_answer() {
        let hit = r#"{"timestamp":"2026-01-01T00:00:00Z","isApiErrorMessage":true,"message":{"content":"Claude AI usage limit reached|1893456000"}}"#;
        let again = r#"{"timestamp":"2026-01-01T00:10:00Z","message":{"id":"m1","usage":{"input_tokens":3,"output_tokens":4,"cache_read_input_tokens":100,"cache_creation_input_tokens":1}}}"#;
        let repeat = r#"{"timestamp":"2026-01-01T00:11:00Z","message":{"id":"m1","usage":{"input_tokens":3,"output_tokens":4,"cache_creation_input_tokens":1}}}"#;
        let now = at(1_767_225_600);
        let reading = claude_reading(&[hit.into(), again.into(), repeat.into()], now);
        assert_eq!(reading.tokens_5h, 8);
        assert!(reading.windows.is_empty(), "a later answer lifts the limit");
        assert!(reading.limited_at.is_some());
        assert!(!is_limited(&reading, now));
    }

    #[test]
    fn key_spend_alerts_only_on_a_day_window() {
        let daily = key_spend(
            &json!({"info": {"spend": 4.5, "max_budget": 10, "budget_duration": "24h", "key_alias": "deep"}}),
            Some(4.0),
        );
        assert!(daily.daily && daily.alert);
        assert_eq!(daily.alias.as_deref(), Some("deep"));
        let total = key_spend(&json!({"spend": 99.0}), Some(1.0));
        assert!(!total.alert);
        assert!(total.detail.as_deref().unwrap().contains("no day alert"));
        let month = key_spend(&json!({"spend": 99.0, "budget_duration": "30d"}), Some(1.0));
        assert!(!month.daily);
        assert!(month.detail.as_deref().unwrap().contains("30d"));
    }
}
