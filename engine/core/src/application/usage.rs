//! `usage.board`: every provider account's reported limits and every LiteLLM
//! key's spend, in one view (WI-2040). Ports
//! `src/vogt/application/services/usage.py` and `src/vogt/adapters/usage.py`.
//!
//! Read on request. Nothing is stored. A source that cannot be read says why
//! in its row's `detail`, so "unreadable" never reads as "unlimited".

use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use super::context::{AppContext, Built};
use super::services::now_of;
use crate::config::{UsageAccount, UsageKey};
use crate::core::usage_board::{self, AccountReading};
use crate::core::{Clock, IdFactory, Moment};
use crate::errors::VogtError;

const MAX_FILES: usize = 20;
const TAIL_BYTES: u64 = 512 * 1024;
const MAX_AGE_SECONDS: u64 = 7 * 24 * 3600;
const KEY_INFO_TIMEOUT: Duration = Duration::from_secs(10);

/// `(url, key) -> (status, body)`. Tests install one so no request leaves.
pub type KeyInfoFetch = fn(&str, &str) -> Result<(u16, Vec<u8>), String>;

fn live_key_info(url: &str, key: &str) -> Result<(u16, Vec<u8>), String> {
    let endpoint = format!("{}/key/info", url.trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new().timeout(KEY_INFO_TIMEOUT).build();
    let response = agent
        .get(&endpoint)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Accept", "application/json")
        .call();
    match response {
        Ok(response) => read_body(response.status(), response.into_reader()),
        Err(ureq::Error::Status(status, response)) => read_body(status, response.into_reader()),
        Err(other) => Err(other.to_string()),
    }
}

fn read_body(status: u16, mut reader: impl Read) -> Result<(u16, Vec<u8>), String> {
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .map_err(|err| err.to_string())?;
    Ok((status, body))
}

pub fn usage_board_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    match ctx {
        Built::SystemRandom(ctx) => board(ctx, params, live_key_info),
        Built::SystemSequential(ctx) => board(ctx, params, live_key_info),
        Built::StepRandom(ctx) => board(ctx, params, live_key_info),
        Built::StepSequential(ctx) => board(ctx, params, live_key_info),
    }
}

fn board<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: Value,
    fetch: KeyInfoFetch,
) -> Result<Value, VogtError> {
    // The board takes no filter. An unknown field is refused, as Params is.
    let _: crate::application::models::UsageBoardParams =
        serde_json::from_value(params).map_err(|err| VogtError::InvalidRequest(err.to_string()))?;
    let now = now_of(&ctx.clock);
    let accounts: Vec<Value> = ctx
        .config
        .usage_accounts
        .iter()
        .map(|account| account_view(account, now))
        .collect();
    let keys: Vec<Value> = ctx
        .config
        .usage_litellm_keys
        .iter()
        .map(|key| key_view(ctx, key, fetch))
        .collect();
    let alerts: Vec<Value> = keys
        .iter()
        .filter(|key| key.get("alert") == Some(&Value::Bool(true)))
        .filter_map(|key| {
            let spend = key.get("spend_usd")?.as_f64()?;
            let threshold = key.get("alert_usd")?.as_f64()?;
            let name = key.get("name")?.as_str()?;
            Some(json!({
                "key": name,
                "spend_usd": spend,
                "threshold_usd": threshold,
                "message": format!(
                    "{name} spent ${spend:.2} today, at or over its ${threshold:.2} day alert"
                ),
            }))
        })
        .collect();
    let mut out = Map::new();
    out.insert("accounts".to_string(), Value::Array(accounts.clone()));
    out.insert("keys".to_string(), Value::Array(keys));
    out.insert("alerts".to_string(), Value::Array(alerts));
    out.insert("generated_at".to_string(), Value::String(now.to_json()));
    let detail = if accounts.is_empty() && ctx.config.usage_litellm_keys.is_empty() {
        Some("usage_accounts and usage_litellm_keys are not configured, so no usage is read")
    } else {
        None
    };
    out.insert("detail".to_string(), json!(detail));
    Ok(Value::Object(out))
}

fn account_view(account: &UsageAccount, now: Moment) -> Value {
    let tails = transcript_tails(&account.agent, &account.dir);
    let reading = if account.agent == "codex" {
        let mut reading = AccountReading::default();
        for tail in &tails {
            reading = usage_board::codex_reading(tail);
            if reading.observed_at.is_some() {
                break;
            }
        }
        reading
    } else {
        usage_board::claude_reading(&tails, now)
    };
    let detail = if tails.is_empty() {
        Some(format!(
            "no transcript under {} in the last week",
            account.dir.display()
        ))
    } else if reading.windows.is_empty() && reading.limited_at.is_none() {
        Some(
            if account.agent == "claude" {
                "no limit reported"
            } else {
                "no rollout reported rate limits"
            }
            .to_string(),
        )
    } else {
        None
    };
    let windows: Vec<Value> = reading
        .windows
        .iter()
        .map(|window| {
            let mut row = Map::new();
            row.insert("name".to_string(), json!(window.name));
            row.insert("used_percent".to_string(), json!(window.used_percent));
            row.insert("window_minutes".to_string(), json!(window.window_minutes));
            row.insert(
                "resets_at".to_string(),
                window
                    .resets_at
                    .map(|resets| Value::String(resets.to_json()))
                    .unwrap_or(Value::Null),
            );
            row.insert("resets_text".to_string(), json!(window.resets_text));
            Value::Object(row)
        })
        .collect();
    let mut row = Map::new();
    row.insert("name".to_string(), json!(account.name));
    row.insert("agent".to_string(), json!(account.agent));
    row.insert(
        "limited".to_string(),
        json!(usage_board::is_limited(&reading, now)),
    );
    row.insert("windows".to_string(), Value::Array(windows));
    row.insert(
        "limited_at".to_string(),
        reading
            .limited_at
            .map(|at| Value::String(at.to_json()))
            .unwrap_or(Value::Null),
    );
    row.insert("limit_reached".to_string(), json!(reading.limit_reached));
    row.insert("plan".to_string(), json!(reading.plan));
    row.insert("tokens_5h".to_string(), json!(reading.tokens_5h));
    row.insert(
        "observed_at".to_string(),
        reading
            .observed_at
            .map(|at| Value::String(at.to_json()))
            .unwrap_or(Value::Null),
    );
    row.insert("detail".to_string(), json!(detail));
    Value::Object(row)
}

fn key_view<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    key: &UsageKey,
    fetch: KeyInfoFetch,
) -> Value {
    let threshold = key
        .daily_alert_usd
        .as_deref()
        .or(ctx.config.usage_daily_spend_alert_usd.as_deref())
        .and_then(|text| text.parse::<f64>().ok());
    let bare = |detail: String| {
        let mut row = Map::new();
        row.insert("name".to_string(), json!(key.name));
        row.insert("alias".to_string(), Value::Null);
        row.insert("spend_usd".to_string(), Value::Null);
        row.insert("daily".to_string(), json!(false));
        row.insert("max_budget_usd".to_string(), Value::Null);
        row.insert("budget_duration".to_string(), Value::Null);
        row.insert("budget_reset_at".to_string(), Value::Null);
        row.insert("alert_usd".to_string(), json!(threshold));
        row.insert("alert".to_string(), json!(false));
        row.insert("detail".to_string(), json!(detail));
        Value::Object(row)
    };
    let Some(url) = ctx.config.usage_litellm_url.as_deref() else {
        return bare("usage_litellm_url is not set".to_string());
    };
    let secret = std::env::var(&key.key_env).unwrap_or_default();
    if secret.is_empty() {
        return bare(format!("{} is not set", key.key_env));
    }
    let (info, why) = key_info(url, &secret, fetch);
    let Some(info) = info else {
        return bare(why.unwrap_or_else(|| "LiteLLM unreachable".to_string()));
    };
    let spend = usage_board::key_spend(&info, threshold);
    let mut row = Map::new();
    row.insert("name".to_string(), json!(key.name));
    row.insert("alias".to_string(), json!(spend.alias));
    row.insert("spend_usd".to_string(), json!(spend.spend_usd));
    row.insert("daily".to_string(), json!(spend.daily));
    row.insert("max_budget_usd".to_string(), json!(spend.max_budget_usd));
    row.insert("budget_duration".to_string(), json!(spend.budget_duration));
    row.insert(
        "budget_reset_at".to_string(),
        spend
            .budget_reset_at
            .map(|resets| Value::String(resets.to_json()))
            .unwrap_or(Value::Null),
    );
    row.insert("alert_usd".to_string(), json!(spend.alert_usd));
    row.insert("alert".to_string(), json!(spend.alert));
    row.insert("detail".to_string(), json!(spend.detail));
    Value::Object(row)
}

fn key_info(url: &str, key: &str, fetch: KeyInfoFetch) -> (Option<Value>, Option<String>) {
    let (status, body) = match fetch(url, key) {
        Ok(answer) => answer,
        Err(err) => return (None, Some(format!("LiteLLM unreachable: {err}"))),
    };
    if status >= 400 {
        return (None, Some(format!("LiteLLM /key/info answered {status}")));
    }
    match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(_)) => {}
        Ok(_) => {
            return (
                None,
                Some("LiteLLM /key/info did not answer a JSON object".to_string()),
            )
        }
        Err(_) => {
            return (
                None,
                Some("LiteLLM /key/info did not answer JSON".to_string()),
            )
        }
    }
    (serde_json::from_slice(&body).ok(), None)
}

/// The tails of the account's newest transcripts, newest first.
fn transcript_tails(agent: &str, directory: &Path) -> Vec<String> {
    let root = directory.join(if agent == "claude" {
        "projects"
    } else {
        "sessions"
    });
    if !root.is_dir() {
        return Vec::new();
    }
    let cutoff = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs().saturating_sub(MAX_AGE_SECONDS))
        .unwrap_or(0);
    let mut found = Vec::new();
    fn walk(dir: &Path, cutoff: u64, found: &mut Vec<(f64, std::path::PathBuf)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, cutoff, found);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let Ok(age) = modified.duration_since(UNIX_EPOCH) else {
                continue;
            };
            if age.as_secs_f64() >= cutoff as f64 {
                found.push((age.as_secs_f64(), path));
            }
        }
    }
    walk(&root, cutoff, &mut found);
    found.sort_by(|left, right| right.0.total_cmp(&left.0));
    let mut tails = Vec::new();
    for (_, path) in found.into_iter().take(MAX_FILES) {
        let Ok(mut handle) = std::fs::File::open(&path) else {
            continue;
        };
        let Ok(meta) = handle.metadata() else {
            continue;
        };
        let size = meta.len();
        if size > TAIL_BYTES {
            use std::io::Seek;
            if handle
                .seek(std::io::SeekFrom::Start(size - TAIL_BYTES))
                .is_err()
            {
                continue;
            }
        }
        let mut bytes = Vec::new();
        if handle.read_to_end(&mut bytes).is_err() {
            continue;
        }
        tails.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    tails
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::build_context;
    use crate::config::VogtConfig;
    use crate::core::{Moment, SequentialIds, StepClock};
    use std::sync::{Arc, Mutex};

    fn now() -> Moment {
        Moment::from_unix(
            chrono::DateTime::parse_from_rfc3339("2026-10-20T12:00:00Z")
                .unwrap()
                .timestamp(),
            0,
        )
    }

    fn codex_line(at: &str, limits: Value) -> String {
        json!({
            "timestamp": at,
            "type": "event_msg",
            "payload": {"type": "token_count", "info": null, "rate_limits": limits},
        })
        .to_string()
    }

    #[test]
    fn codex_reading_takes_the_newest_rate_limits() {
        let limits = json!({
            "primary": {"used_percent": 42.0, "window_minutes": 300, "resets_at": 1792504800},
            "secondary": {"used_percent": 100.0, "window_minutes": 10080, "resets_at": 1792836000},
            "plan_type": "pro",
            "rate_limit_reached_type": "secondary",
        });
        let text = format!(
            "{}\n{}\nnot json",
            codex_line(
                "2026-10-20T11:55:00+00:00",
                json!({"primary": {"used_percent": 1.0, "window_minutes": 300}})
            ),
            codex_line("2026-10-20T12:00:00+00:00", limits),
        );
        let reading = usage_board::codex_reading(&text);
        assert_eq!(
            reading
                .windows
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>(),
            ["primary", "secondary"]
        );
        assert_eq!(reading.windows[0].used_percent, Some(42.0));
        assert_eq!(
            reading.windows[0].resets_at.unwrap().unix_seconds(),
            1792504800
        );
        assert_eq!(reading.plan.as_deref(), Some("pro"));
        assert_eq!(reading.limit_reached.as_deref(), Some("secondary"));
        assert!(usage_board::is_limited(&reading, now()));
    }

    #[test]
    fn claude_limit_and_tokens() {
        let reset = 1_792_504_800i64; // 2026-10-20T14:00:00Z, two hours after now()
        let said = format!("Claude AI usage limit reached|{reset}");
        let text = [
            json!({"timestamp": "2026-10-20T11:00:00+00:00", "message": {"usage": {"input_tokens": 100, "output_tokens": 50}}}).to_string(),
            json!({"timestamp": "2026-10-20T03:00:00+00:00", "message": {"usage": {"input_tokens": 999}}}).to_string(),
            json!({"timestamp": "2026-10-20T12:00:00+00:00", "isApiErrorMessage": true, "message": {"content": [{"text": said}]}}).to_string(),
        ]
        .join("\n");
        let reading = usage_board::claude_reading(&[text], now());
        assert_eq!(reading.tokens_5h, 150);
        assert_eq!(reading.windows[0].resets_at.unwrap().unix_seconds(), reset);
        assert!(usage_board::is_limited(&reading, now()));
    }

    #[test]
    fn worded_reset_lifts_after_a_later_answer() {
        let said = "You've hit your limit · resets 3pm (Europe/London)";
        let hit = json!({"timestamp": "2026-10-20T10:00:00+00:00", "isApiErrorMessage": true, "message": {"content": [{"text": said}]}}).to_string();
        let still = usage_board::claude_reading(std::slice::from_ref(&hit), now());
        assert_eq!(
            still.windows[0].resets_text.as_deref(),
            Some("3pm (Europe/London)")
        );
        assert!(usage_board::is_limited(&still, now()));
        let answered = json!({"timestamp": "2026-10-20T11:55:00+00:00", "message": {"id": "msg_after", "usage": {"output_tokens": 3}}}).to_string();
        let lifted = usage_board::claude_reading(&[format!("{hit}\n{answered}")], now());
        assert!(lifted.windows.is_empty());
        assert!(!usage_board::is_limited(&lifted, now()));
    }

    #[test]
    fn daily_key_alerts_and_a_month_window_does_not() {
        let daily = usage_board::key_spend(
            &json!({"info": {"spend": 31.5, "max_budget": 50, "budget_duration": "1d", "budget_reset_at": "2026-10-21T00:00:00Z", "key_alias": "dlab"}}),
            Some(30.0),
        );
        assert!(daily.daily && daily.alert && daily.detail.is_none());
        assert_eq!(daily.alias.as_deref(), Some("dlab"));
        let month = usage_board::key_spend(
            &json!({"info": {"spend": 80.0, "budget_duration": "30d"}}),
            Some(30.0),
        );
        assert!(!month.alert);
        assert!(month.detail.as_deref().unwrap().contains("no day alert"));
    }

    #[test]
    fn an_empty_board_says_nothing_is_configured() {
        let dir = std::env::temp_dir().join(format!("usage-board-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = VogtConfig {
            data_dir: dir,
            ..VogtConfig::default()
        };
        let ctx = build_context(
            config,
            Some(crate::core::local_principal("usage-test")),
            Some(StepClock::new(now())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = ctx else {
            panic!("step")
        };
        let result = board(&ctx, json!({}), live_key_info).unwrap();
        assert_eq!(
            result["detail"],
            "usage_accounts and usage_litellm_keys are not configured, so no usage is read"
        );
        assert!(result["accounts"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_missing_key_env_says_it_is_unset_and_never_prints_a_secret() {
        let dir = std::env::temp_dir().join(format!("usage-board-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = VogtConfig {
            data_dir: dir,
            usage_litellm_url: Some("http://litellm.test".to_string()),
            usage_litellm_keys: vec![UsageKey {
                name: "missing".to_string(),
                key_env: "VOGT_TEST_UNSET_USAGE_KEY_2665".to_string(),
                daily_alert_usd: None,
            }],
            usage_daily_spend_alert_usd: Some("25".to_string()),
            ..VogtConfig::default()
        };
        let ctx = build_context(
            config,
            Some(crate::core::local_principal("usage-test")),
            Some(StepClock::new(now())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = ctx else {
            panic!("step")
        };
        fn boom(_: &str, _: &str) -> Result<(u16, Vec<u8>), String> {
            panic!("no request")
        }
        let result = board(&ctx, json!({}), boom).unwrap();
        assert_eq!(
            result["keys"][0]["detail"],
            "VOGT_TEST_UNSET_USAGE_KEY_2665 is not set"
        );
        assert_eq!(result["keys"][0]["alert_usd"], 25.0);
        let _ = Arc::new(Mutex::new(()));
    }
}
