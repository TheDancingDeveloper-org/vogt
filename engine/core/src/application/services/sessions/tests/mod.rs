use super::*;
use std::sync::{Arc, Mutex};

use serde_json::json;

use serde_json::Value;

use super::{at_prompt_for_input, not_ready_next_step};
use crate::adapters::engine::{EngineClient, EngineScreen, EngineWait};
use crate::application::context::{build_context, write_of};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::auth;
use crate::core::{
    ActorKind, CodingSession, Moment, Principal, Project, StepClock, Token, TokenKind,
};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};
use crate::with_ctx;

use super::{
    answers_as_person, hibernate, input, project_source, project_source_with_fallback,
    relayed_overseer, remove_session_op, rename_session_op, session_token_op, type_then_submit,
    wake, HibernateParams, InputParams, WakeParams,
};

mod answer;
mod input;
mod lifecycle;
mod manage;
mod start;
mod subject;
mod sweep;
mod wire;

/// An engine that records every request and answers from `script`: each GET
/// of a session pops the next payload, and a write echoes its own body so
/// the client reads it back as the session's new state.
fn engine(script: Vec<Vec<u8>>, seen: Arc<Mutex<Vec<(String, String, String)>>>) -> EngineClient {
    let remaining = Arc::new(Mutex::new(script));
    EngineClient::new(
        "http://engine",
        None,
        Some(std::sync::Arc::new(move |path, _, body, method| {
            seen.lock().unwrap().push((
                method.to_string(),
                path.to_string(),
                String::from_utf8_lossy(body).to_string(),
            ));
            let answer = if method == "GET" {
                remaining
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap_or_else(|| b"null".to_vec())
            } else {
                // An engine session row always has an id; a request body
                // echoed back as the reply gains one when it has none.
                match serde_json::from_slice::<serde_json::Value>(body) {
                    Ok(mut row) if row.is_object() && row.get("id").is_none() => {
                        row["id"] = json!("eng-echo");
                        serde_json::to_vec(&row).unwrap()
                    }
                    _ => body.to_vec(),
                }
            };
            (200, answer)
        })),
    )
}

#[expect(
    clippy::type_complexity,
    reason = "the nested type is local and reads clearer inline than behind an alias"
)]
fn context(
    name: &str,
    principal: Principal,
    token: Option<Token>,
    config_extra: impl FnOnce(&mut crate::config::VogtConfig),
    script: Vec<Vec<u8>>,
) -> (
    std::path::PathBuf,
    crate::application::context::Built,
    Arc<Mutex<Vec<(String, String, String)>>>,
) {
    let dir = std::env::temp_dir().join(format!("vogt-ses-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut clock = None;
    let mut ids = None;
    crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut config = crate::config::VogtConfig {
        data_dir: dir.clone(),
        ..crate::config::VogtConfig::default()
    };
    config_extra(&mut config);
    let built = build_context(
        config,
        Some(principal),
        Some(StepClock::new(Moment::from_unix(1_700_000_000, 123_000))),
        None,
        token,
        Some(engine(script, Arc::clone(&seen))),
        None,
        None,
    )
    .unwrap();
    (dir, built, seen)
}

fn person() -> Principal {
    Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()
}

fn agent(identity: &str) -> Principal {
    Principal::new(identity, ActorKind::Agent, "agent").unwrap()
}

fn live(id: &str) -> Vec<u8> {
    format!(r#"{{"id":"{id}","alive":true,"activity":"idle","role":"worker"}}"#).into_bytes()
}

/// Record a Vogt session and one token for its actor, and return the token's id.
fn record_session(built: &crate::application::context::Built, id: &str, engine_id: &str) -> String {
    with_ctx!(built, |ctx| {
        let mut write = write_of(ctx);
        let id = id.to_string();
        let engine_id = engine_id.to_string();
        let token_id = "tok_old".to_string();
        let kept = token_id.clone();
        audited_write(&mut write, "session.start", "test", move |txn, actor| {
            txn.insert_project(&Project::new(
                "prj_1",
                "proj",
                "Proj",
                "/work",
                Moment::from_unix(1_700_000_000, 0),
            ))?;
            txn.insert_session(&CodingSession {
                id: id.clone(),
                engine_session_id: engine_id,
                project_id: "prj_1".to_string(),
                work_item_id: None,
                actor_id: actor.id.clone(),
                cwd: "/work".to_string(),
                template: None,
                model: None,
                effort: None,
                reason: "test".to_string(),
                purpose: None,
                started_by_session: None,
                started_by_kind: None,
                started_at: Moment::from_unix(1_700_000_000, 0),
                stopped_at: None,
            })?;
            txn.insert_token(
                &Token {
                    id: token_id,
                    actor_id: actor.id.clone(),
                    actor_identity_ref: Some(actor.identity_ref.clone()),
                    name: "session token".to_string(),
                    scopes: vec!["read".to_string()],
                    kind: TokenKind::Api,
                    created_at: Moment::from_unix(1_700_000_000, 0),
                    expires_at: None,
                    last_used_at: None,
                    revoked_at: None,
                    revoked_reason: None,
                },
                "hash-old",
            )?;
            Ok(WriteOutcome::new(
                (),
                "session",
                &id,
                json!({}),
                "session.started",
                json!({}),
            ))
        })
        .unwrap();
        kept
    })
}

fn tokens_of(built: &crate::application::context::Built, actor_ref: &str) -> Vec<Token> {
    with_ctx!(built, |ctx| {
        let view = ctx.declared.read().unwrap();
        let actor = view.actor_by_identity(actor_ref).unwrap().unwrap();
        view.tokens_for_actor(&actor.id, true).unwrap()
    })
}

/// Sweep payload from the engine: two terminals, one of them a person's
/// own.
fn sweep_payload() -> Vec<u8> {
    // The engine knows two roles; the person's own terminal is `oversight`
    // here only so the test has a role to exclude.
    br#"[
      {"summary":{"id":"w-1","alive":true,"activity":"idle","role":"worker"},"screen_tail":[],"ready":true},
      {"summary":{"id":"o-1","alive":true,"activity":"idle","role":"oversight"},"screen_tail":[],"ready":true}
    ]"#
    .to_vec()
}
