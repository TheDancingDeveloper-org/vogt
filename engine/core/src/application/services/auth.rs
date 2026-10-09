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
const USER_SET_PASSWORD: &str = "user.set_password";
const USER_REMOVE: &str = "user.remove";
const USER_PASSWORD_SET_EVENT: &str = "user.password_set";
const USER_REMOVED_EVENT: &str = "user.removed";
const AUTH_LOGOUT: &str = "auth.logout";
const SESSION_CLOSED_EVENT: &str = "auth.session_closed";
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
    // Read before the write, as Python does. `ensure_actor` then auto-registers
    // a new principal, and only after that does the body read `created_at`, so a
    // hook that takes time falls between the expiry and the creation.
    let expires_at = match expires_in_days {
        Some(days) => {
            let now = ctx.clock.lock().expect("the clock lock").now();
            Some(expiry_from(now, days)?)
        }
        None => None,
    };

    let mut writing = write_of(ctx);
    let issued = issue_recorded(
        &mut writing,
        reason,
        &IssueRequest {
            name,
            holder_ref,
            scopes,
            token_hash,
            expires_at,
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
    expires_at: Option<Moment>,
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
    let expires_at = request.expires_at;
    let clock = std::sync::Arc::clone(write.clock());
    let ids = std::sync::Arc::clone(write.ids());
    audited_write(
        write,
        TOKEN_ISSUE,
        reason,
        move |txn: &mut _, _actor: &Actor| {
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
pub(super) fn random_array<const N: usize>() -> Result<[u8; N], VogtError> {
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

/// `user.set_password`. Replaces the hash, and by default every live session
/// the user holds, so a stolen session dies with the password it was issued
/// under. The hash is computed before the write, so a weak password fails
/// before any row is touched.
pub fn set_password_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| set_password(ctx, &params))
}

fn set_password<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = params["reason"].as_str().unwrap_or("");
    let username = username_of(params["username"].as_str().unwrap_or(""))?;
    let password_hash = password_hash_of(params["password"].as_str().unwrap_or(""))?;
    let scopes = match params.get("scopes").and_then(Value::as_str) {
        Some(scopes) => Some(scopes_of(scopes)?),
        None => None,
    };
    let revoke_sessions = params["revoke_sessions"].as_bool().unwrap_or(true);
    let mut writing = write_of(ctx);
    let user = set_password_recorded(
        &mut writing,
        reason,
        &SetPasswordRequest {
            username,
            password_hash,
            scopes,
            revoke_sessions,
        },
    )?;
    Ok(serde_json::json!({ "user": user }))
}

struct SetPasswordRequest {
    username: String,
    password_hash: String,
    scopes: Option<Vec<String>>,
    revoke_sessions: bool,
}

fn set_password_recorded<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut SqliteWrite<'_, C, I>,
    reason: &str,
    request: &SetPasswordRequest,
) -> Result<Value, VogtError> {
    let username = request.username.clone();
    let password_hash = request.password_hash.clone();
    let scopes = request.scopes.clone();
    let revoke_sessions = request.revoke_sessions;
    let clock = std::sync::Arc::clone(write.clock());
    audited_write(
        write,
        USER_SET_PASSWORD,
        reason,
        move |txn: &mut _, _actor: &Actor| {
            let current = txn
                .password_credential_by_username(&username)?
                .ok_or_else(|| VogtError::NotFound(format!("no user named '{username}'")))?;
            let now = clock.lock().expect("the clock lock").now();
            let scopes = scopes.unwrap_or_else(|| current.scopes.clone());
            txn.upsert_password_credential(
                &current.actor_id,
                &username,
                &password_hash,
                &scopes,
                now,
            )?;
            let revoked = if revoke_sessions {
                revoke_sessions_of(txn, &current.actor_id, reason, now)?
            } else {
                0
            };
            let user = txn
                .password_credential_for_actor(&current.actor_id)?
                .expect("the credential was just written in this transaction");
            Ok(WriteOutcome::new(
                serde_json::to_value(&user).expect("a credential serialises"),
                "user",
                &current.actor_id,
                serde_json::json!({
                    "username": username,
                    "scopes": user.scopes,
                    "sessions_revoked": revoked,
                }),
                USER_PASSWORD_SET_EVENT,
                serde_json::json!({ "username": username }),
            ))
        },
    )
}

/// `user.remove`. The login goes; the actor and its audit history stay, so the
/// writes the person made remain attributable.
pub fn remove_user_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| remove_user(ctx, &params))
}

fn remove_user<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = params["reason"].as_str().unwrap_or("");
    let username = username_of(params["username"].as_str().unwrap_or(""))?;
    let mut writing = write_of(ctx);
    remove_user_recorded(&mut writing, reason, &username)
}

fn remove_user_recorded<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut SqliteWrite<'_, C, I>,
    reason: &str,
    username: &str,
) -> Result<Value, VogtError> {
    let username = username.to_string();
    let clock = std::sync::Arc::clone(write.clock());
    audited_write(
        write,
        USER_REMOVE,
        reason,
        move |txn: &mut _, _actor: &Actor| {
            let current = txn
                .password_credential_by_username(&username)?
                .ok_or_else(|| VogtError::NotFound(format!("no user named '{username}'")))?;
            let now = clock.lock().expect("the clock lock").now();
            txn.delete_password_credential(&current.actor_id)?;
            let revoked = revoke_sessions_of(txn, &current.actor_id, reason, now)?;
            Ok(WriteOutcome::new(
                serde_json::json!({ "username": username, "sessions_revoked": revoked }),
                "user",
                &current.actor_id,
                serde_json::json!({ "username": username, "sessions_revoked": revoked }),
                USER_REMOVED_EVENT,
                serde_json::json!({ "username": username }),
            ))
        },
    )
}

/// Revoke every live session token an actor holds. API tokens are untouched:
/// changing a password is not how those are retired.
fn revoke_sessions_of(
    txn: &mut impl WriteTxn,
    actor_id: &str,
    reason: &str,
    now: Moment,
) -> Result<i64, VogtError> {
    let mut count = 0;
    for existing in txn.tokens_for_actor(actor_id, false)? {
        if existing.kind == crate::core::TokenKind::Session
            && txn.revoke_token(&existing.id, reason, now)?
        {
            count += 1;
        }
    }
    Ok(count)
}

/// `auth.logout`. Revokes the token the call arrived with. A surface with no
/// token behind it — the local CLI — has nothing to revoke, and says so rather
/// than failing, so a client can call it unconditionally.
pub fn logout_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| logout(ctx, &params))
}

fn logout<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let Some(current) = ctx.token.clone() else {
        return Ok(serde_json::json!({ "revoked": false, "token": Value::Null }));
    };
    let reason = params["reason"].as_str().unwrap_or("");
    let mut writing = write_of(ctx);
    logout_recorded(&mut writing, reason, &current)
}

fn logout_recorded<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut SqliteWrite<'_, C, I>,
    reason: &str,
    current: &Token,
) -> Result<Value, VogtError> {
    let id = current.id.clone();
    let kind = match current.kind {
        crate::core::TokenKind::Session => "session",
        crate::core::TokenKind::Api => "api",
        crate::core::TokenKind::Agent => "agent",
    };
    let actor_ref = current.actor_identity_ref.clone();
    let clock = std::sync::Arc::clone(write.clock());
    audited_write(
        write,
        AUTH_LOGOUT,
        reason,
        move |txn: &mut _, _actor: &Actor| {
            let now = clock.lock().expect("the clock lock").now();
            let revoked = txn.revoke_token(&id, reason, now)?;
            let updated = txn.token_by_id(&id)?;
            Ok(WriteOutcome::new(
                serde_json::json!({ "revoked": revoked, "token": updated }),
                "token",
                &id,
                serde_json::json!({ "revoked": revoked, "kind": kind }),
                SESSION_CLOSED_EVENT,
                serde_json::json!({ "actor": actor_ref }),
            ))
        },
    )
}

/// `auth.whoami`. The effective scope set, implications applied, so a consumer
/// never re-derives them. With no token behind the call the answer is the
/// local grant: admin, writes enabled.
pub fn whoami_op(ctx: &Built, _params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| {
        let held: Vec<&str> = match &ctx.token {
            Some(token) => token.scopes.iter().map(String::as_str).collect(),
            // The local grant is admin with writes enabled.
            None => vec!["admin"],
        };
        let mut scopes = auth::effective(&held);
        scopes.sort_unstable();
        Ok(serde_json::json!({
            "identity_ref": ctx.principal.identity_ref,
            "kind": ctx.principal.kind,
            "display_name": ctx.principal.display_name,
            "scopes": scopes,
            "token": ctx.token,
        }))
    })
}

/// `auth.decisions`. The allow and deny log; the denials are the interesting
/// half.
pub fn decisions_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| {
        let decision = params.get("decision").and_then(Value::as_str);
        let limit = params["limit"].as_i64().unwrap_or(100);
        let view = ctx.declared.read()?;
        Ok(serde_json::json!({
            "decisions": view.list_auth_decisions(decision, limit)?
        }))
    })
}

/// `auth.login`. Not a registry operation: Python mounts it by hand at
/// `/api/auth/login`, because the caller holds no credential yet and every
/// registry route sits behind authorization. The service is what that route
/// will call.
///
/// Every refusal costs the same scrypt and answers with the same sentence, so
/// the timing and the text say nothing about whether the username exists, the
/// password was wrong, or the account is disabled. The `auth_decisions` row
/// names the real reason for the operator.
pub fn login_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| login(ctx, &params))
}

fn login<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    // A malformed username is the same refusal as a wrong password, not a
    // validation error that would confirm the shape Python accepts.
    let username = match auth::normalise_username(params["username"].as_str().unwrap_or("")) {
        Ok(username) => username,
        Err(_) => return Err(login_refused()),
    };
    let password = params["password"].as_str().unwrap_or("");
    let session_name = params["session_name"]
        .as_str()
        .unwrap_or("browser session")
        .to_string();
    let now = ctx.clock.lock().expect("the clock lock").now();

    // Before any lookup. Five failures inside a minute refuse even the right
    // password, and a wrong guess during the lockout gets the same sentence as
    // every other refusal, so the reply never confirms a guess.
    if let Some(retry) = throttle_check(&username, now) {
        return Err(VogtError::LoginThrottled(format!(
            "too many failed logins for {}; try again in {retry} seconds",
            crate::core::py_repr(&username)
        )));
    }

    let view = ctx.declared.read()?;
    let credential = view.password_credential_by_username(&username)?;
    let stored = match &credential {
        Some(credential) => view.password_hash(&credential.actor_id)?,
        None => None,
    };
    let actor = match &credential {
        Some(credential) => view.actor_by_id(&credential.actor_id)?,
        None => None,
    };
    drop(view);

    // A miss verifies against the dummy hash, so it costs the same scrypt as a
    // hit. A dangling credential, a disabled account and a wrong password all
    // fall through to the one refusal.
    let ok = auth::verify_password(password, stored.as_deref().unwrap_or(dummy_hash()));
    if credential.is_none()
        || stored.is_none()
        || actor.is_none()
        || actor.as_ref().is_some_and(|actor| actor.disabled)
        || !ok
    {
        throttle_failed(&username, now);
        record_decision(
            ctx,
            AuthOutcome::Deny,
            BAD_PASSWORD,
            None,
            actor.as_ref().map(|actor| actor.identity_ref.clone()),
            None,
        )?;
        return Err(login_refused());
    }
    throttle_succeeded(&username);

    let actor = actor.expect("the refusal above returned when there was no actor");
    let credential = credential.expect("the refusal above returned when there was none");
    let joined = credential.scopes.join(",");
    let scopes = auth::parse_scopes(&joined).map_err(VogtError::InvalidRequest)?;
    let (secret, token_hash) = auth::issue(&random_array::<TOKEN_ENTROPY_BYTES>()?);
    let expires_at = Moment::from_unix(
        now.unix_seconds() + ctx.config.session_ttl_days * 86_400,
        now.nanos(),
    );

    let mut writing = write_of(ctx);
    writing.set_principal(&actor.identity_ref, actor.kind, &actor.display_name);
    let token = login_recorded(
        &mut writing,
        &LoginRequest {
            session_name,
            scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
            token_hash,
            now,
            expires_at,
            reason: format!("password login by {username}"),
        },
    )?;
    record_decision(
        ctx,
        AuthOutcome::Allow,
        LOGIN_OK,
        token["actor_id"].as_str().map(str::to_string),
        Some(actor.identity_ref.clone()),
        token["id"].as_str().map(str::to_string),
    )?;
    Ok(serde_json::json!({ "actor": actor, "token": token, "secret": secret }))
}

struct LoginRequest {
    session_name: String,
    scopes: Vec<String>,
    token_hash: String,
    now: Moment,
    expires_at: Moment,
    reason: String,
}

fn login_recorded<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut SqliteWrite<'_, C, I>,
    request: &LoginRequest,
) -> Result<Value, VogtError> {
    let session_name = request.session_name.clone();
    let scopes = request.scopes.clone();
    let token_hash = request.token_hash.clone();
    let now = request.now;
    let expires_at = request.expires_at;
    let reason = request.reason.clone();
    let ids = std::sync::Arc::clone(write.ids());
    audited_write(
        write,
        AUTH_LOGIN,
        &reason,
        move |txn: &mut _, holder: &Actor| {
            let token = Token {
                id: ids.lock().expect("the id lock").next("tok"),
                actor_id: holder.id.clone(),
                actor_identity_ref: Some(holder.identity_ref.clone()),
                name: session_name.clone(),
                scopes: scopes.clone(),
                kind: crate::core::TokenKind::Session,
                created_at: now,
                expires_at: Some(expires_at),
                last_used_at: None,
                revoked_at: None,
                revoked_reason: None,
            };
            txn.insert_token(&token, &token_hash)?;
            Ok(WriteOutcome::new(
                serde_json::to_value(&token).expect("a token serialises"),
                "token",
                &token.id,
                serde_json::json!({
                    "actor": holder.identity_ref,
                    "scopes": scopes,
                    "name": token.name,
                    "kind": "session",
                    "expires_at": expires_at.to_json(),
                }),
                SESSION_OPENED_EVENT,
                serde_json::json!({ "actor": holder.identity_ref }),
            ))
        },
    )
}

/// The one sentence every refusal uses. Distinct reasons stay in the decision
/// row, never in the reply.
fn login_refused() -> VogtError {
    VogtError::Unauthenticated("the username or password is not right".to_string())
}

const BAD_PASSWORD: &str = "bad_password";
const LOGIN_OK: &str = "login_ok";
const SESSION_OPENED_EVENT: &str = "auth.session_opened";

/// Verified against when the username names nobody, so a miss costs the same
/// scrypt as a hit. Computed once, at startup, so the first miss in a process
/// costs exactly one scrypt rather than the dummy's plus its own. If the
/// operating system refuses the salt, the dummy is a stored form no password
/// parses to: a miss then fails the check instead of panicking, which is the
/// same answer it would have given.
fn dummy_hash() -> &'static str {
    static HASH: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| match random_array::<PASSWORD_SALT_BYTES>() {
            Ok(salt) => auth::hash_password("not-a-real-password", &salt)
                .unwrap_or_else(|_| "scrypt$0$0$0$$".to_string()),
            Err(_) => "scrypt$0$0$0$$".to_string(),
        });
    &HASH
}

/// Forces the dummy hash to be computed. `main` calls it at startup; the first
/// login in a test process calls it too, which is the same moment.
pub fn warm_dummy_hash() {
    let _ = dummy_hash();
}

/// The process-local failure window. The store keeps the durable record in
/// `auth_decisions`; this only shapes the reply, which is why it is not read
/// back from there.
fn throttle() -> &'static std::sync::Mutex<std::collections::HashMap<String, Vec<Moment>>> {
    static THROTTLE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Vec<Moment>>>,
    > = std::sync::OnceLock::new();
    THROTTLE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Tests share one process and one throttle, so a reset in one test lands in
/// the middle of another's window. Every test that logs in holds this for its
/// whole body.
#[cfg(test)]
fn throttle_gate() -> &'static std::sync::Mutex<()> {
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &GATE
}

/// `Some(seconds)` when the username is locked out.
fn throttle_check(username: &str, now: Moment) -> Option<i64> {
    let mut guard = throttle()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let window = guard.get_mut(username)?;
    window.retain(|at| now.seconds_since(*at) <= LOGIN_FAILURE_WINDOW_SECONDS as f64);
    if window.len() >= LOGIN_FAILURE_LIMIT {
        // Python's `int(total_seconds())` truncates, so a remainder of 60.9
        // reads as 60, and a remainder below a second still says 1.
        let retry = LOGIN_FAILURE_WINDOW_SECONDS as f64 - now.seconds_since(window[0]);
        return Some((retry as i64).max(1));
    }
    None
}

fn throttle_failed(username: &str, now: Moment) {
    throttle()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(username.to_string())
        .or_default()
        .push(now);
}

fn throttle_succeeded(username: &str) {
    throttle()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(username);
}

/// Tests share one process, so one test's failures would lock the next test's
/// username. Production never calls this.
fn throttle_reset() {
    throttle()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

fn record_decision<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    decision: AuthOutcome,
    code: &str,
    actor_id: Option<String>,
    identity_ref: Option<String>,
    token_id: Option<String>,
) -> Result<(), VogtError> {
    // A decision id is `aut_`, from its own counter, not the audit counter:
    // under hooks the audit row and the decision rows must not share a
    // sequence. The moment is a fresh clock read, not the one the attempt
    // started with.
    let id = ctx.id_factory.lock().expect("the id lock").next("aut");
    let at = ctx.clock.lock().expect("the clock lock").now();
    ctx.declared.record_auth_decision(&AuthDecision {
        id,
        at,
        decision,
        reason_code: code.to_string(),
        operation: AUTH_LOGIN.to_string(),
        scope: None,
        actor_id,
        token_id,
        identity_ref,
        transport: "http".to_string(),
        detail: None,
    })
}

pub(super) fn username_of(raw: &str) -> Result<String, VogtError> {
    auth::normalise_username(raw).map_err(VogtError::InvalidRequest)
}

fn scopes_of(raw: &str) -> Result<Vec<String>, VogtError> {
    auth::parse_scopes(raw)
        .map(|scopes| scopes.into_iter().map(str::to_string).collect())
        .map_err(VogtError::InvalidRequest)
}

pub(super) fn password_hash_of(password: &str) -> Result<String, VogtError> {
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
        let _gate = throttle_gate()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        throttle_reset();
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
        assert_eq!(wrong.message(), "the username or password is not right");

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
        assert_eq!(kinds, vec!["login_ok", "bad_password"]);
        // Decision ids come from the `aut` counter, not the audit counter, and
        // the allow row names the actor the session was minted for.
        assert!(
            decisions.iter().all(|d| d.id.starts_with("aut_")),
            "{decisions:?}"
        );
        let allow = &decisions[0];
        assert_eq!(
            allow.actor_id.as_deref(),
            session["token"]["actor_id"].as_str()
        );
        assert_eq!(allow.token_id.as_deref(), session["token"]["id"].as_str());
        let deny = &decisions[1];
        assert!(deny.actor_id.is_none());
        assert!(deny.token_id.is_none());
        assert_eq!(deny.identity_ref.as_deref(), Some("human:ada"));
        // Each row reads the clock again, so the stepped clock puts them a
        // second apart rather than sharing the attempt's moment.
        assert!(allow.at.unix_seconds() > deny.at.unix_seconds());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Five failures inside the minute refuse even the right password, and the
    /// refusal names the wait rather than repeating the generic sentence.
    #[test]
    fn a_username_that_keeps_failing_is_locked_out() {
        let _gate = throttle_gate()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        throttle_reset();
        let (dir, built) = fresh_at("throttle", crate::core::Moment::from_unix(1_700_000_000, 0));
        // The credential is stored at a test cost, so the five refusals and the
        // lockout verify cheaply. The clock steps one second a read, so the
        // window is the injected clock's, not the wall.
        create_user_op(
            &built,
            serde_json::json!({"username": "lock", "password": "correct horse battery", "reason": "first"}),
        )
        .unwrap();
        cheapen(&dir, "lock", "correct horse battery");
        for _ in 0..5 {
            let wrong = login_op(
                &built,
                serde_json::json!({"username": "lock", "password": "nope"}),
            )
            .unwrap_err();
            assert_eq!(wrong.message(), "the username or password is not right");
        }
        let locked = login_op(
            &built,
            serde_json::json!({"username": "lock", "password": "correct horse battery"}),
        )
        .unwrap_err();
        assert!(
            locked
                .message()
                .contains("too many failed logins for 'lock'"),
            "{locked}"
        );
        assert!(matches!(locked, VogtError::LoginThrottled(_)));
        let view = crate::with_ctx!(&built, |ctx| ctx.declared.read()).unwrap();
        assert_eq!(view.list_auth_decisions(Some("deny"), 10).unwrap().len(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unknown username, a disabled account and a wrong password all answer
    /// with the same sentence, and the miss still verifies a hash.
    #[test]
    fn an_unknown_user_and_a_disabled_user_read_the_same() {
        let _gate = throttle_gate()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        throttle_reset();
        let (dir, built) = fresh("same");
        create_user_op(
            &built,
            serde_json::json!({
                "username": "disa",
                "password": "correct horse battery",
                "reason": "first user",
            }),
        )
        .unwrap();
        cheapen(&dir, "ada", "correct horse battery");
        let wrong = login_op(
            &built,
            serde_json::json!({"username": "disa", "password": "nope"}),
        )
        .unwrap_err();
        let unknown = login_op(
            &built,
            serde_json::json!({"username": "nobody", "password": "correct horse battery"}),
        )
        .unwrap_err();
        assert_eq!(wrong.message(), unknown.message());
        assert_eq!(unknown.message(), "the username or password is not right");

        // The store interface has no disable method; the row is the flag.
        let db = dir.join("declared.sqlite3");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE actors SET disabled = 1 WHERE identity_ref = 'human:disa'",
            [],
        )
        .unwrap();
        drop(conn);
        let disabled = login_op(
            &built,
            serde_json::json!({"username": "disa", "password": "correct horse battery"}),
        )
        .unwrap_err();
        assert_eq!(disabled.message(), wrong.message());
        let view = crate::with_ctx!(&built, |ctx| ctx.declared.read()).unwrap();
        let denials = view.list_auth_decisions(Some("deny"), 10).unwrap();
        assert!(denials.iter().all(|d| d.reason_code == "bad_password"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fresh(name: &str) -> (std::path::PathBuf, crate::application::context::Built) {
        fresh_at(name, crate::core::Moment::from_unix(1_700_000_000, 0))
    }

    fn fresh_at(
        name: &str,
        start: crate::core::Moment,
    ) -> (std::path::PathBuf, crate::application::context::Built) {
        let dir = std::env::temp_dir().join(format!("vogt-login-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir.clone(),
            ..crate::config::VogtConfig::default()
        };
        let built = crate::application::context::build_context(
            config,
            None,
            Some(crate::core::StepClock::new(start)),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        (dir, built)
    }

    /// Changing the password ends the sessions issued under the old one, and the
    /// old password stops working.
    #[test]
    fn setting_a_password_revokes_the_sessions() {
        let _gate = throttle_gate()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        throttle_reset();
        let (dir, built) = fresh("passwd");
        create_user_op(
            &built,
            serde_json::json!({"username": "setp", "password": "correct horse battery", "reason": "first"}),
        )
        .unwrap();
        cheapen(&dir, "setp", "correct horse battery");
        let session = login_op(
            &built,
            serde_json::json!({"username": "setp", "password": "correct horse battery"}),
        )
        .unwrap();
        assert_eq!(session["token"]["kind"], "session");
        let changed = set_password_op(
            &built,
            serde_json::json!({"username": "setp", "password": "a new passphrase", "reason": "rotated"}),
        )
        .unwrap();
        assert_eq!(changed["user"]["username"], "setp");
        let view = crate::with_ctx!(&built, |ctx| ctx.declared.read()).unwrap();
        let tokens = view
            .tokens_for_actor(session["token"]["actor_id"].as_str().unwrap(), true)
            .unwrap();
        assert!(tokens.iter().all(|token| token.revoked_at.is_some()));
        drop(view);
        let old = login_op(
            &built,
            serde_json::json!({"username": "setp", "password": "correct horse battery"}),
        )
        .unwrap_err();
        assert_eq!(old.message(), "the username or password is not right");
        let fresh_session = login_op(
            &built,
            serde_json::json!({"username": "setp", "password": "a new passphrase"}),
        )
        .unwrap();
        assert!(fresh_session["secret"]
            .as_str()
            .unwrap()
            .starts_with("vogt_"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Removing a login keeps the actor and refuses the password afterwards.
    #[test]
    fn removing_a_user_keeps_the_actor() {
        let _gate = throttle_gate()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        throttle_reset();
        let (dir, built) = fresh("remove");
        create_user_op(
            &built,
            serde_json::json!({"username": "gone", "password": "correct horse battery", "reason": "first"}),
        )
        .unwrap();
        cheapen(&dir, "gone", "correct horse battery");
        let removed = remove_user_op(
            &built,
            serde_json::json!({"username": "gone", "reason": "left"}),
        )
        .unwrap();
        assert_eq!(removed["username"], "gone");
        let view = crate::with_ctx!(&built, |ctx| ctx.declared.read()).unwrap();
        assert!(view.actor_by_identity("human:gone").unwrap().is_some());
        assert!(view
            .password_credential_by_username("gone")
            .unwrap()
            .is_none());
        drop(view);
        let refused = login_op(
            &built,
            serde_json::json!({"username": "gone", "password": "correct horse battery"}),
        )
        .unwrap_err();
        assert_eq!(refused.message(), "the username or password is not right");
        let missing = remove_user_op(
            &built,
            serde_json::json!({"username": "gone", "reason": "again"}),
        )
        .unwrap_err();
        assert!(
            missing.message().contains("no user named 'gone'"),
            "{missing}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Replace a stored hash with one at a test cost, so login verifies in
    /// microseconds. The production cost stays in `hash_password`.
    fn cheapen(dir: &std::path::Path, username: &str, password: &str) {
        let cheap = auth::hash_password_at(password, b"0123456789abcdef", 1 << 4, 8, 1).unwrap();
        let conn = rusqlite::Connection::open(dir.join("declared.sqlite3")).unwrap();
        conn.execute(
            "UPDATE password_credentials SET password_hash = ?1 WHERE username = ?2",
            [cheap, username.to_string()],
        )
        .unwrap();
    }
}
