//! `usage.board`: every provider account's reported limits and every LiteLLM
//! key's spend, in one view (WI-2040 / WI-2144).
//!
//! Read on request. A source that cannot be read says why in its row's
//! `detail`, so "unreadable" never reads as "unlimited".

use serde_json::{json, Value};

use super::context::{AppContext, Built};
use super::services::now_of;
use crate::adapters::usage::{key_info, transcript_tails, HttpKeyInfo, KeyInfoFetch};
use crate::config::{UsageAccount, UsageKey, VogtConfig};
use crate::core::usage_board::{self, AccountReading};
use crate::core::Moment;
use crate::errors::VogtError;

/// What each configured account and key reports now.
pub fn usage_board_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    match ctx {
        Built::SystemRandom(ctx) => board(ctx, params),
        Built::SystemSequential(ctx) => board(ctx, params),
        Built::StepRandom(ctx) => board(ctx, params),
        Built::StepSequential(ctx) => board(ctx, params),
    }
}

fn board<C: crate::core::Clock, I: crate::core::IdFactory>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let _: UsageBoardParams = serde_json::from_value(or_object(params))
        .map_err(|err| VogtError::InvalidRequest(err.to_string()))?;
    let now = now_of(&ctx.clock);
    let unix = u64::try_from(now.unix_seconds()).unwrap_or(0);
    let accounts: Vec<Value> = ctx
        .config
        .usage_accounts
        .iter()
        .map(|account| account_view(account, now, unix))
        .collect();
    let keys: Vec<Value> = ctx
        .config
        .usage_litellm_keys
        .iter()
        .map(|key| key_view(&ctx.config, key, &HttpKeyInfo))
        .collect();
    let alerts: Vec<Value> = keys
        .iter()
        .filter(|key| key.get("alert").and_then(Value::as_bool) == Some(true))
        .filter_map(|key| {
            let spend = key.get("spend_usd").and_then(Value::as_f64)?;
            let threshold = key.get("alert_usd").and_then(Value::as_f64)?;
            let name = key.get("name").and_then(Value::as_str)?;
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
    let detail = if accounts.is_empty() && keys.is_empty() {
        json!("usage_accounts and usage_litellm_keys are not configured, so no usage is read")
    } else {
        Value::Null
    };
    Ok(json!({
        "accounts": accounts,
        "keys": keys,
        "alerts": alerts,
        "generated_at": now.to_json(),
        "detail": detail,
    }))
}

#[derive(serde::Deserialize)]
struct UsageBoardParams {}

fn or_object(value: Value) -> Value {
    if value.is_null() {
        json!({})
    } else {
        value
    }
}

fn dollars(cents: Option<i64>) -> Option<f64> {
    cents.map(|value| value as f64 / 100.0)
}

fn account_view(account: &UsageAccount, now: Moment, unix: u64) -> Value {
    let tails = transcript_tails(&account.agent, &account.dir, unix);
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
    json!({
        "name": account.name,
        "agent": account.agent,
        "limited": usage_board::is_limited(&reading, now),
        "windows": reading.windows.iter().map(|window| json!({
            "name": window.name,
            "used_percent": window.used_percent,
            "window_minutes": window.window_minutes,
            "resets_at": window.resets_at.map(Moment::to_json),
            "resets_text": window.resets_text,
        })).collect::<Vec<_>>(),
        "limited_at": reading.limited_at.map(Moment::to_json),
        "limit_reached": reading.limit_reached,
        "plan": reading.plan,
        "tokens_5h": reading.tokens_5h,
        "observed_at": reading.observed_at.map(Moment::to_json),
        "detail": detail,
    })
}

fn key_view(config: &VogtConfig, key: &UsageKey, fetch: &dyn KeyInfoFetch) -> Value {
    let threshold = dollars(
        key.daily_alert_usd_cents
            .or(config.usage_daily_spend_alert_usd_cents),
    );
    let Some(url) = config.usage_litellm_url.as_deref() else {
        return json!({
            "name": key.name,
            "alert_usd": threshold,
            "detail": "usage_litellm_url is not set",
        });
    };
    let secret = std::env::var(&key.key_env).unwrap_or_default();
    if secret.is_empty() {
        return json!({
            "name": key.name,
            "alert_usd": threshold,
            "detail": format!("{} is not set", key.key_env),
        });
    }
    let (info, why) = key_info(url, &secret, fetch);
    let Some(info) = info else {
        return json!({
            "name": key.name,
            "alert_usd": threshold,
            "detail": why,
        });
    };
    let spend = usage_board::key_spend(&info, threshold);
    json!({
        "name": key.name,
        "alias": spend.alias,
        "spend_usd": spend.spend_usd,
        "daily": spend.daily,
        "max_budget_usd": spend.max_budget_usd,
        "budget_duration": spend.budget_duration,
        "budget_reset_at": spend.budget_reset_at.map(Moment::to_json),
        "alert_usd": spend.alert_usd,
        "alert": spend.alert,
        "detail": spend.detail,
    })
}
