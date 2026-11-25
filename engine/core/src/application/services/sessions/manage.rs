use super::*;

// -- sessions part II: rename, remove, token (WI-1074) -------------------------

crate::typed_op!(rename_session_op, RenameSessionParams, rename_session_typed);
crate::typed_op!(remove_session_op, RemoveSessionParams, remove_session_typed);
crate::typed_op!(session_token_op, SessionTokenParams, session_token_typed);

#[derive(Debug, Clone)]
pub struct RenameSessionParams {
    pub id: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct RemoveSessionParams {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct SessionTokenParams {
    pub engine_session_id: String,
    pub revoke: bool,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionTokenResult {
    pub engine_session_id: String,
    pub actor: String,
    /// `null` when the call revoked rather than minted, matching Python's
    /// `EngineSessionTokenResult.token`.
    pub token: Option<String>,
    pub revoked: i64,
}

/// The engine's session id: a UUID, exactly as the catalog's pattern.
pub(super) static ENGINE_SESSION_ID: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
            .unwrap()
    });

pub(super) const ENGINE_SESSION_ACTOR_PREFIX: &str = "agent:engine:";

/// The name the engine shows. Trimmed, and refused when empty or past the
/// engine's own byte limit, so a name it would reject is rejected here first.
const SESSION_NAME_MAX_BYTES: usize = 256;

pub fn rename_session<C, I>(
    ctx: &AppContext<C, I>,
    params: &RenameSessionParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let name = params.name.trim();
    if name.is_empty() {
        return Err(VogtError::InvalidRequest(
            "a session name cannot be empty".to_string(),
        ));
    }
    if name.len() > SESSION_NAME_MAX_BYTES {
        return Err(VogtError::InvalidRequest(format!(
            "a session name is at most {SESSION_NAME_MAX_BYTES} bytes"
        )));
    }
    let engine = engine_of(ctx)?;
    let found = resolve_target(ctx, &params.id)?;
    let renamed = engine.rename_session(&found.engine_session_id, name)?;
    if !renamed {
        return Err(VogtError::NotFound(format!(
            "the engine has no session {}",
            py_repr(&params.id)
        )));
    }
    let mut writing = write_of(ctx);
    let engine_id = found.engine_session_id.clone();
    let named = name.to_string();
    // A linked session is audited under Vogt's id; an unlinked one under the
    // engine's.
    let entity = found
        .session
        .as_ref()
        .map_or_else(|| engine_id.clone(), |session| session.id.clone());
    audited_action(
        &mut writing,
        SESSION_RENAME,
        &reason,
        "session",
        &entity,
        &json!({"name": named, "linked": found.session.is_some()}),
        "session.renamed",
        Some(&json!({"engine_session_id": engine_id})),
    )?;
    // The rename itself only says whether the engine had the session.
    let current = engine
        .get_session(&found.engine_session_id)?
        .ok_or_else(|| {
            VogtError::NotFound(format!("the engine has no session {}", py_repr(&params.id)))
        })?;
    let summary = match &found.session {
        Some(session) => summarize(ctx, session, Some(&current))?,
        None => unlinked_summary(&ctx.declared.read()?, &current)?,
    };
    Ok(SessionResult {
        session: summary,
        escalated_to: None,
    })
}

/// Kill a session if it still runs and have the engine forget it.
///
/// `session.stop` closes Vogt's record and leaves the engine's; this also
/// drops the engine's record, screen and brief. A session Vogt linked is
/// closed and has its token revoked, exactly as a stop would, so a removed
/// session cannot keep calling back.
pub fn remove_session<C, I>(
    ctx: &AppContext<C, I>,
    params: &RemoveSessionParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let engine = engine_of(ctx)?;
    let found = resolve_target(ctx, &params.id)?;
    let engine_id = found.engine_session_id.clone();
    // A live session is killed with who and why first, so the exit reads
    // `stopped` rather than `errored` (WI-913). DELETE alone kills without them.
    let before = engine.get_session(&engine_id)?;
    if before.as_ref().is_some_and(|session| session.alive) {
        engine.kill_session(&engine_id, Some(&reason), Some(&ctx.principal.identity_ref))?;
    }
    if found.session.is_none() {
        let Some(before) = before else {
            return Err(VogtError::NotFound(format!(
                "no session {}",
                py_repr(&engine_id)
            )));
        };
        if !engine.remove_session(&engine_id)? {
            return Err(VogtError::NotFound(format!(
                "no session {}",
                py_repr(&engine_id)
            )));
        }
        let mut writing = write_of(ctx);
        audited_action(
            &mut writing,
            SESSION_REMOVE,
            &reason,
            "session",
            &engine_id,
            &json!({"engine_session_id": engine_id, "linked": false}),
            "session.removed",
            Some(&json!({"engine_removed": true, "linked": false})),
        )?;
        let mut summary = unlinked_summary(&ctx.declared.read()?, &before)?;
        summary.alive = Some(false);
        summary.stopped_at = Some(now(&writing));
        return Ok(SessionResult {
            session: summary,
            escalated_to: None,
        });
    }

    let Some(session) = found.session else {
        // The branch above returns when there is no declared session. Getting
        // here without one means the resolution changed under us.
        return Err(VogtError::NotFound(format!(
            "no session {}",
            py_repr(&engine_id)
        )));
    };
    let removed = engine.remove_session(&engine_id)?;
    let mut writing = write_of(ctx);
    let session_id = session.id.clone();
    let actor_id = session.actor_id.clone();
    let why = reason.clone();
    let stopped_at = now(&writing);
    let result = audited_write(&mut writing, SESSION_REMOVE, &reason, move |txn, _actor| {
        let current = txn
            .session_by_id(&session_id)?
            .ok_or_else(|| VogtError::NotFound(format!("no session {}", py_repr(&session_id))))?;
        let closed = current.stopped_at.is_none();
        if closed {
            txn.mark_session_stopped(&session_id, stopped_at)?;
            for token in txn.tokens_for_actor(&actor_id, false)? {
                if token.revoked_at.is_none() {
                    txn.revoke_token(&token.id, &why, stopped_at)?;
                }
            }
        }
        let after = txn
            .session_by_id(&session_id)?
            .ok_or_else(|| VogtError::NotFound(format!("no session {}", py_repr(&session_id))))?;
        let summary = summarize(ctx, &after, None)?;
        Ok(WriteOutcome::new(
            SessionResult {
                session: summary,
                escalated_to: None,
            },
            "session",
            &session_id,
            audited_payload(&after),
            "session.removed",
            json!({"engine_removed": removed, "record_closed": closed}),
        ))
    })?;
    Ok(result)
}

/// Mint (or revoke) the credential of an agent session the engine started.
///
/// Only the engine's own credential may ask. The token is bound to
/// `agent:engine:<uuid>`, created on first use, and every earlier token of
/// that actor is revoked in the same write. `revoke` ends them and returns no
/// secret.
pub fn session_token<C, I>(
    ctx: &AppContext<C, I>,
    params: &SessionTokenParams,
) -> Result<SessionTokenResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    if !is_engine_credential(ctx)? {
        return Err(VogtError::EngineOnly(format!(
            "session.token is called by the session engine for the agents it starts, not by {}",
            ctx.principal.identity_ref
        )));
    }
    let identity_ref = format!("{ENGINE_SESSION_ACTOR_PREFIX}{}", params.engine_session_id);
    let scopes = session_scopes(ctx)?;
    let minted = if params.revoke {
        None
    } else {
        Some(minted_token()?)
    };
    let mut writing = write_of(ctx);
    let engine_session_id = params.engine_session_id.clone();
    let actor_ref = identity_ref.clone();
    let why = reason.clone();
    let revoke = params.revoke;
    let minted_scopes = scopes;
    let token_id = fresh(&writing, "tok");
    let minted_at = now(&writing);
    let actor_id_new = fresh(&writing, "act");
    let revoked = audited_write(&mut writing, SESSION_TOKEN, &reason, move |txn, _actor| {
        let existing = txn.actor_by_identity(&actor_ref)?;
        let holder_id = match &existing {
            Some(actor) => actor.id.clone(),
            None => {
                let short = engine_session_id.get(..8).unwrap_or(&engine_session_id);
                txn.insert_actor(&Actor {
                    id: actor_id_new.clone(),
                    kind: ActorKind::Agent,
                    display_name: format!("Engine session {short}"),
                    identity_ref: actor_ref.clone(),
                    disabled: false,
                    created_at: minted_at,
                })?;
                actor_id_new
            }
        };
        let mut revoked = 0i64;
        // Python: "session ended" on a revoke, "superseded: <reason>" on a mint.
        let revoke_why = if revoke {
            "session ended".to_string()
        } else {
            format!("superseded: {why}")
        };
        for token in txn.tokens_for_actor(&holder_id, false)? {
            if token.revoked_at.is_none() && txn.revoke_token(&token.id, &revoke_why, minted_at)? {
                revoked += 1;
            }
        }
        let secret = if let Some((secret, token_hash)) = minted {
            txn.insert_token(
                &Token {
                    id: token_id,
                    actor_id: holder_id.clone(),
                    actor_identity_ref: Some(actor_ref.clone()),
                    name: format!("engine session {engine_session_id}"),
                    scopes: minted_scopes,
                    kind: TokenKind::Api,
                    created_at: minted_at,
                    expires_at: None,
                    last_used_at: None,
                    revoked_at: None,
                    revoked_reason: None,
                },
                &token_hash,
            )?;
            Some(secret)
        } else {
            None
        };
        let event = if secret.is_none() && revoke {
            "session.token_revoked"
        } else {
            "session.token_issued"
        };
        let minted = secret.is_some();
        Ok(WriteOutcome::new(
            (revoked, secret),
            "actor",
            &holder_id,
            json!({"identity_ref": actor_ref, "kind": "agent"}),
            event,
            json!({
                "engine_session_id": engine_session_id,
                "minted": minted,
                "revoked": revoked,
            }),
        ))
    })?;
    let (revoked, secret) = revoked;
    Ok(SessionTokenResult {
        engine_session_id: params.engine_session_id.clone(),
        actor: identity_ref,
        token: secret,
        revoked,
    })
}
