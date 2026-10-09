//! Tokens and the people who hold them. Ports `services/auth.py`.
//!
//! The secret a token carries is returned once, at issue, and never stored. The
//! database keeps the hash, and an audit row that contained the secret would be
//! a credential leak with a timestamp, so the payload records the actor and the
//! scopes and nothing else.

use serde_json::Value;

use crate::application::context::{write_of, AppContext, Built};
use crate::application::resolve;
use crate::application::writes::{audited_write, WriteContext, WriteOutcome};
use crate::auth::{self, TOKEN_ENTROPY_BYTES};
use crate::core::{Actor, ActorKind, AuthDecision, AuthOutcome, Clock, IdFactory, Moment, Token};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const TOKEN_ISSUE: &str = "token.issue";
const TOKEN_REVOKE: &str = "token.revoke";
const TOKEN_ISSUED_EVENT: &str = "token.issued";
const TOKEN_REVOKED_EVENT: &str = "token.revoked";
const USER_CREATE: &str = "user.create";
const USER_CREATED_EVENT: &str = "user.created";
const AUTH_LOGIN: &str = "auth.login";
const AUTH_LOGIN_EVENT: &str = "auth.login";

/// Five wrong passwords inside a minute locks the username out for the rest of
/// the minute. Python's `LOGIN_FAILURE_LIMIT` and `LOGIN_FAILURE_WINDOW`.
const LOGIN_FAILURE_LIMIT: usize = 5;
const LOGIN_FAILURE_WINDOW_SECONDS: i64 = 60;

/// Python's `PASSWORD_SALT_BYTES`.
const PASSWORD_SALT_BYTES: usize = 16;

/// `token.issue`, as the registry calls it.
pub fn issue_token_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| issue_token(ctx, &params))
}

fn issue_token<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = params["reason"].as_str().unwrap_or("");
    let name = params["name"].as_str().unwrap_or("").to_string();
    let holder_ref = params["actor"].as_str().unwrap_or("").to_string();
    let scopes = auth::parse_scopes(params["scopes"].as_str().unwrap_or(""))
        .map_err(VogtError::InvalidRequest)?
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let expires_in_days = params["expires_in_days"].as_i64();

    let (secret, token_hash) = auth::issue(&random_array::<TOKEN_ENTROPY_BYTES>()?);

    let mut writing = write_of(ctx);
    let issued = issue_recorded(
        &mut writing,
        reason,
        &IssueRequest {
            name,
            holder_ref,
            scopes,
            token_hash,
            expires_in_days,
        },
    )?;

    Ok(serde_json::json!({
        "token": issued,
        "secret": secret,
        "warning": "This is the only time the secret is shown. Store it in a file and point \
                    VOGT_TOKEN_FILE at it — never in argv or a URL.",
    }))
}

/// What an issue writes, gathered so the audit closure takes one argument.
struct IssueRequest {
    name: String,
    holder_ref: String,
    scopes: Vec<String>,
    token_hash: String,
    expires_in_days: Option<i64>,
}

/// The audited half of an issue, split out so the closure is built against the
/// concrete store rather than the generic `DeclaredStore` bound. Against the
/// bound the compiler treats the closure as needing a `'static` lifetime, and
/// it does not compile — the same split `record_check` makes.
fn issue_recorded<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut WriteContext<'_, C, I, crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>>,
    reason: &str,
    request: &IssueRequest,
) -> Result<Value, VogtError> {
    let name = request.name.clone();
    let holder_ref = request.holder_ref.clone();
    let scopes = request.scopes.clone();
    let token_hash = request.token_hash.clone();
    let expires_in_days = request.expires_in_days;
    let clock = std::sync::Arc::clone(write.clock());
    let ids = std::sync::Arc::clone(write.ids());
    audited_write(
        write,
        TOKEN_ISSUE,
        reason,
        move |txn: &mut _, _actor: &Actor| {
            // The expiry is read first and the id drawn last, matching Python:
            // `expires_at` is computed before the write, the holder is resolved
            // before the token id, and `created_at` is read inside the body, so
            // a hook that takes time falls between the two readings.
            let expires_at = match expires_in_days {
                Some(days) => Some(expiry_from(
                    clock.lock().expect("the clock lock").now(),
                    days,
                )?),
                None => None,
            };
            let holder = resolve::actor(txn, &holder_ref)?;
            let now = clock.lock().expect("the clock lock").now();
            let token_id = ids.lock().expect("the id lock").next("tok");
            let token = Token {
                id: token_id.clone(),
                actor_id: holder.id.clone(),
                actor_identity_ref: Some(holder.identity_ref.clone()),
                name: name.clone(),
                scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
                kind: crate::core::TokenKind::Api,
                created_at: now,
                expires_at,
                last_used_at: None,
                revoked_at: None,
                revoked_reason: None,
            };
            txn.insert_token(&token, &token_hash)?;
            let issued = serde_json::to_value(&token).expect("a token serialises");
            Ok(WriteOutcome::new(
                issued,
                "token",
                &token.id,
                serde_json::json!({"actor": holder.identity_ref, "scopes": scopes, "name": name}),
                TOKEN_ISSUED_EVENT,
                serde_json::json!({"actor": holder.identity_ref, "scopes": scopes}),
            ))
        },
    )
}

/// `now` plus `days`, keeping the sub-second part the way Python's
/// `timedelta` does. Checked arithmetic: an absurd number of days is a bad
/// request, never a panic in a request handler.
fn expiry_from(now: Moment, days: i64) -> Result<Moment, VogtError> {
    let seconds = days
        .checked_mul(86_400)
        .and_then(|span| now.unix_seconds().checked_add(span));
    match seconds {
        Some(seconds) => Ok(Moment::from_unix(seconds, now.nanos())),
        None => Err(VogtError::InvalidRequest(format!(
            "expires_in_days {days} is out of range"
        ))),
    }
}
/// the operation: falling back to a zeroed buffer would mint a credential from a
/// secret anyone can guess.
fn random_array<const N: usize>() -> Result<[u8; N], VogtError> {
    let mut entropy = [0u8; N];
    getrandom::getrandom(&mut entropy).map_err(|_| {
        VogtError::InvalidRequest("the operating system refused to supply random bytes".to_string())
    })?;
    Ok(entropy)
}

/// `token.list`, as the registry calls it. A read: no audit row, no reason.
pub fn list_tokens_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| list_tokens(ctx, &params))
}

fn list_tokens<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let include_revoked = params["include_revoked"].as_bool().unwrap_or(false);
    let limit = params["limit"].as_i64().unwrap_or(100);
    let view = ctx.declared.read()?;
    let tokens = view.list_tokens(include_revoked, limit)?;
    Ok(serde_json::json!({ "tokens": tokens }))
}

/// `token.revoke`, as the registry calls it.
pub fn revoke_token_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| revoke_token(ctx, &params))
}

fn revoke_token<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = params["reason"].as_str().unwrap_or("");
    let id = params["id"].as_str().unwrap_or("").to_string();
    let mut writing = write_of(ctx);
    let revoked = revoke_recorded(&mut writing, reason, &id)?;
    Ok(serde_json::json!({ "token": revoked }))
}

/// The audited half of a revocation, split out for the same reason
/// `issue_recorded` is: the closure must be built against the concrete store.
fn revoke_recorded<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut WriteContext<'_, C, I, crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>>,
    reason: &str,
    id: &str,
) -> Result<Value, VogtError> {
    let id = id.to_string();
    let clock = std::sync::Arc::clone(write.clock());
    audited_write(
        write,
        TOKEN_REVOKE,
        reason,
        move |txn: &mut _, _actor: &Actor| {
            let existing = txn.token_by_id(&id)?;
            if existing.is_none() {
                return Err(VogtError::NotFound(format!("no token '{id}'")));
            }
            let now = clock.lock().expect("the clock lock").now();
            if !txn.revoke_token(&id, reason, now)? {
                return Err(VogtError::Conflict(format!(
                    "token '{id}' is already revoked"
                )));
            }
            let updated = txn
                .token_by_id(&id)?
                .expect("the token was just written in this transaction");
            let payload = serde_json::to_value(&updated).expect("a token serialises");
            Ok(WriteOutcome::new(
                payload.clone(),
                "token",
                &id,
                payload,
                TOKEN_REVOKED_EVENT,
                serde_json::json!({ "actor": updated.actor_identity_ref }),
            ))
        },
    )
}

type SqliteWrite<'a, C, I> =
    WriteContext<'a, C, I, crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>>;

/// `user.create`, as the registry calls it. The hash is computed before the
/// write, as Python does, so a weak password fails before any row is touched.
pub fn create_user_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| create_user(ctx, &params))
}

fn create_user<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = params["reason"].as_str().unwrap_or("");
    let username = username_of(params["username"].as_str().unwrap_or(""))?;
    let scopes = scopes_of(params["scopes"].as_str().unwrap_or("read"))?;
    let display_name = match params["display_name"].as_str() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => username.clone(),
    };
    let named_actor = params["actor"]
        .as_str()
        .filter(|actor| !actor.is_empty())
        .map(str::to_string);
    let password_hash = password_hash_of(params["password"].as_str().unwrap_or(""))?;

    let mut writing = write_of(ctx);
    let user = create_user_recorded(
        &mut writing,
        reason,
        &CreateUserRequest {
            username,
            scopes,
            display_name,
            named_actor,
            password_hash,
        },
    )?;
    Ok(serde_json::json!({ "user": user }))
}

struct CreateUserRequest {
    username: String,
    scopes: Vec<String>,
    display_name: String,
    named_actor: Option<String>,
    password_hash: String,
}

fn create_user_recorded<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut SqliteWrite<'_, C, I>,
    reason: &str,
    request: &CreateUserRequest,
) -> Result<Value, VogtError> {
    let username = request.username.clone();
    let scopes = request.scopes.clone();
    let display_name = request.display_name.clone();
    let named_actor = request.named_actor.clone();
    let password_hash = request.password_hash.clone();
    let clock = std::sync::Arc::clone(write.clock());
    let ids = std::sync::Arc::clone(write.ids());
    audited_write(
        write,
        USER_CREATE,
        reason,
        move |txn: &mut _, _actor: &Actor| {
            if txn.password_credential_by_username(&username)?.is_some() {
                return Err(VogtError::Conflict(format!(
                    "a user named '{username}' already exists"
                )));
            }
            let now = clock.lock().expect("the clock lock").now();
            let holder = match &named_actor {
                Some(reference) => {
                    let holder = resolve::actor(txn, reference)?;
                    if holder.kind != ActorKind::Human {
                        return Err(VogtError::InvalidRequest(format!(
                            "{} is an agent; only a human may hold a password",
                            holder.identity_ref
                        )));
                    }
                    holder
                }
                None => {
                    let identity_ref = format!("human:{username}");
                    match txn.actor_by_identity(&identity_ref)? {
                        Some(existing) => existing,
                        None => {
                            let holder = Actor {
                                id: ids.lock().expect("the id lock").next("act"),
                                kind: ActorKind::Human,
                                display_name: display_name.clone(),
                                identity_ref: identity_ref.clone(),
                                disabled: false,
                                created_at: now,
                            };
                            txn.insert_actor(&holder)?;
                            holder
                        }
                    }
                }
            };
            if txn.password_credential_for_actor(&holder.id)?.is_some() {
                return Err(VogtError::Conflict(format!(
                    "{} already has a login",
                    holder.identity_ref
                )));
            }
            txn.upsert_password_credential(&holder.id, &username, &password_hash, &scopes, now)?;
            let user = txn
                .password_credential_for_actor(&holder.id)?
                .expect("the credential was just written in this transaction");
            let payload = serde_json::to_value(&user).expect("a credential serialises");
            Ok(WriteOutcome::new(
                payload,
                "user",
                &holder.id,
                serde_json::json!({
                    "actor": holder.identity_ref,
                    "username": username,
                    "scopes": scopes,
                }),
                USER_CREATED_EVENT,
                serde_json::json!({ "actor": holder.identity_ref, "username": username }),
            ))
        },
    )
}

/// `user.list`, as the registry calls it. A read: no audit row, no reason.
pub fn list_users_op(ctx: &Built, _params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| {
        let view = ctx.declared.read()?;
        Ok(serde_json::json!({ "users": view.list_password_credentials()? }))
    })
}

/// `auth.login`, as the registry calls it. Unauthenticated by construction: the
/// credential, not the principal, decides who the session belongs to.
pub fn login_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| login(ctx, &params))
}

fn login<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let username = username_of(params["username"].as_str().unwrap_or(""))?;
    let password = params["password"].as_str().unwrap_or("");
    let session_name = params["session_name"]
        .as_str()
        .unwrap_or("browser session")
        .to_string();
    let (secret, token_hash) = auth::issue(&random_array::<TOKEN_ENTROPY_BYTES>()?);

    let mut writing = write_of(ctx);
    let (actor, token) = login_recorded(
        &mut writing,
        &LoginRequest {
            username,
            password: password.to_string(),
            session_name,
            token_hash,
            ttl_seconds: ctx.config.session_ttl_days * 86_400,
        },
    )?;
    Ok(serde_json::json!({ "actor": actor, "token": token, "secret": secret }))
}

struct LoginRequest {
    username: String,
    password: String,
    session_name: String,
    token_hash: String,
    ttl_seconds: i64,
}

fn login_recorded<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut SqliteWrite<'_, C, I>,
    request: &LoginRequest,
) -> Result<(Value, Value), VogtError> {
    let username = request.username.clone();
    let password = request.password.clone();
    let session_name = request.session_name.clone();
    let token_hash = request.token_hash.clone();
    let ttl_seconds = request.ttl_seconds;
    let clock = std::sync::Arc::clone(write.clock());
    let clock_for_body = std::sync::Arc::clone(&clock);
    let ids = std::sync::Arc::clone(write.ids());
    let ids_for_body = std::sync::Arc::clone(&ids);
    let outcome = audited_write(
        write,
        AUTH_LOGIN,
        "signed in",
        move |txn: &mut _, _actor: &Actor| {
            let now = clock_for_body.lock().expect("the clock lock").now();
            let credential = txn.password_credential_by_username(&username)?;
            let (holder, scopes) = match &credential {
                Some(credential) => {
                    let holder = txn
                        .actor_by_id(&credential.actor_id)?
                        .expect("a credential names an actor that exists");
                    if holder.disabled {
                        return Err(denied(
                            "actor_disabled",
                            &username,
                            "that account is disabled",
                        ));
                    }
                    (holder, credential.scopes.clone())
                }
                None => {
                    return Err(denied(
                        "unknown_user",
                        &username,
                        "unknown username or wrong password",
                    ));
                }
            };
            let stored = txn.password_hash(&holder.id)?.unwrap_or_default();
            if !auth::verify_password(&password, &stored) {
                return Err(denied(
                    "bad_password",
                    &username,
                    "unknown username or wrong password",
                ));
            }
            let window = now.unix_seconds() - LOGIN_FAILURE_WINDOW_SECONDS;
            let failures = recent_denials(txn, &username, window)?;
            if failures.len() >= LOGIN_FAILURE_LIMIT {
                let retry = failures[0].at.unix_seconds() + LOGIN_FAILURE_WINDOW_SECONDS
                    - now.unix_seconds();
                return Err(denied(
                    "rate_limited",
                    &username,
                    &format!("too many failed logins for '{username}'; try again in {retry}s"),
                ));
            }
            let token_id = ids_for_body.lock().expect("the id lock").next("tok");
            let token = Token {
                id: token_id.clone(),
                actor_id: holder.id.clone(),
                actor_identity_ref: Some(holder.identity_ref.clone()),
                name: session_name.clone(),
                scopes: scopes.clone(),
                kind: crate::core::TokenKind::Session,
                created_at: now,
                expires_at: Some(Moment::from_unix(
                    now.unix_seconds() + ttl_seconds,
                    now.nanos(),
                )),
                last_used_at: None,
                revoked_at: None,
                revoked_reason: None,
            };
            txn.insert_token(&token, &token_hash)?;
            let decision = AuthDecision {
                id: String::new(),
                at: now,
                decision: AuthOutcome::Allow,
                reason_code: "login".to_string(),
                operation: AUTH_LOGIN.to_string(),
                scope: None,
                actor_id: Some(holder.id.clone()),
                token_id: Some(token.id.clone()),
                identity_ref: Some(holder.identity_ref.clone()),
                transport: "http".to_string(),
                detail: Some(username.clone()),
            };
            let payload = serde_json::json!({
                "actor": serde_json::to_value(&holder).expect("an actor serialises"),
                "token": serde_json::to_value(&token).expect("a token serialises"),
                "decision": serde_json::to_value(&decision).expect("a decision serialises"),
            });
            Ok(WriteOutcome::new(
                payload,
                "session",
                &token.id,
                serde_json::json!({ "username": username, "scopes": scopes }),
                AUTH_LOGIN_EVENT,
                serde_json::json!({ "actor": holder.identity_ref }),
            ))
        },
    );
    let (actor, token) = match outcome {
        Ok(mut outcome) => {
            let mut decision: AuthDecision =
                serde_json::from_value(outcome["decision"].take()).expect("the decision is ours");
            decision.id = ids.lock().expect("the id lock").next("aud");
            write.store().record_auth_decision(&decision)?;
            (outcome["actor"].clone(), outcome["token"].clone())
        }
        Err(error) => {
            if let Some((reason_code, username, message)) = denial_of(&error) {
                let now = clock.lock().expect("the clock lock").now();
                let id = ids.lock().expect("the id lock").next("aud");
                write.store().record_auth_decision(&AuthDecision {
                    id,
                    at: now,
                    decision: AuthOutcome::Deny,
                    reason_code: reason_code.to_string(),
                    operation: AUTH_LOGIN.to_string(),
                    scope: None,
                    actor_id: None,
                    token_id: None,
                    identity_ref: None,
                    transport: "http".to_string(),
                    detail: Some(username.to_string()),
                })?;
                return Err(VogtError::Unauthenticated(message.to_string()));
            }
            return Err(error);
        }
    };
    Ok((actor, token))
}

/// A refusal the login body returns. The reason code and username ride in front
/// of the message so the caller records the denial after the transaction rolls
/// back. Recorded inside it, the row would vanish with the rollback and the
/// throttle would never see it.
fn denied(reason_code: &str, username: &str, message: &str) -> VogtError {
    VogtError::Unauthenticated(format!(
        "denied\u{1f}{reason_code}\u{1f}{username}\u{1f}{message}"
    ))
}

fn denial_of(error: &VogtError) -> Option<(&str, &str, &str)> {
    let VogtError::Unauthenticated(text) = error else {
        return None;
    };
    let mut parts = text.split('\u{1f}');
    if parts.next() != Some("denied") {
        return None;
    }
    Some((parts.next()?, parts.next()?, parts.next()?))
}

/// The denials for this username inside the window, oldest first, as
/// `_recent_failures` reads them. The storage filter takes a decision, not a
/// window, so the window is applied here.
fn recent_denials(
    txn: &mut impl ReadView,
    username: &str,
    since_unix: i64,
) -> Result<Vec<AuthDecision>, VogtError> {
    let mut failures: Vec<AuthDecision> = txn
        .list_auth_decisions(Some("deny"), 500)?
        .into_iter()
        .filter(|decision| {
            decision.detail.as_deref() == Some(username) && decision.at.unix_seconds() >= since_unix
        })
        .collect();
    failures.sort_by_key(|decision| decision.at.unix_seconds());
    Ok(failures)
}

fn username_of(raw: &str) -> Result<String, VogtError> {
    auth::normalise_username(raw).map_err(VogtError::InvalidRequest)
}

fn scopes_of(raw: &str) -> Result<Vec<String>, VogtError> {
    auth::parse_scopes(raw)
        .map(|scopes| scopes.into_iter().map(str::to_string).collect())
        .map_err(VogtError::InvalidRequest)
}

fn password_hash_of(password: &str) -> Result<String, VogtError> {
    let salt = random_array::<PASSWORD_SALT_BYTES>()?;
    auth::hash_password(password, &salt).map_err(VogtError::InvalidRequest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refused login leaves the denial behind even though the write rolls
    /// back, and the next correct password still signs in. Both are easy to get
    /// wrong because the denial and the session live on different connections.
    #[test]
    fn a_wrong_password_is_recorded_and_the_right_one_signs_in() {
        let dir = std::env::temp_dir().join(format!("vogt-login-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let mut config = crate::config::VogtConfig::default();
        config = crate::config::VogtConfig {
            data_dir: dir.clone(),
            ..config
        };
        let built = crate::application::context::build_context(
            config, None, None, None, None, None, None, None,
        )
        .unwrap();
        let created = create_user_op(
            &built,
            serde_json::json!({
                "username": "ada",
                "password": "correct horse battery",
                "scopes": "read",
                "reason": "first user",
            }),
        )
        .unwrap();
        assert_eq!(created["user"]["username"], "ada");
        assert_eq!(created["user"]["actor_identity_ref"], "human:ada");

        let wrong = login_op(
            &built,
            serde_json::json!({"username": "ada", "password": "nope"}),
        )
        .unwrap_err();
        assert!(
            wrong
                .message()
                .contains("unknown username or wrong password"),
            "{wrong}"
        );

        let session = login_op(
            &built,
            serde_json::json!({"username": "ada", "password": "correct horse battery"}),
        )
        .unwrap();
        assert_eq!(session["actor"]["identity_ref"], "human:ada");
        assert_eq!(session["token"]["kind"], "session");
        assert_eq!(session["token"]["scopes"], serde_json::json!(["read"]));
        assert!(session["secret"].as_str().unwrap().starts_with("vogt_"));

        let view = crate::with_ctx!(&built, |ctx| ctx.declared.read()).unwrap();
        let decisions = view.list_auth_decisions(None, 10).unwrap();
        let kinds: Vec<&str> = decisions
            .iter()
            .map(|decision| decision.reason_code.as_str())
            .collect();
        assert_eq!(kinds, vec!["login", "bad_password"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
