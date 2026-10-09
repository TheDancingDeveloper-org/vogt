//! The gate in front of a registry operation, shared by the HTTP and MCP
//! adapters. Ports `src/vogt/application/services/auth.py`.
//!
//! Both adapters used to grow their own copy of this. The MCP one was keyed by
//! tool name with the transport hard-coded, so a second copy for HTTP would
//! have drifted from it. This one takes the operation and the transport, and
//! both adapters call it.
//!
//! The order is Python's. The caller validates the request first, because a
//! malformed body is a 422 even with no token. Then this module authenticates,
//! then authorizes, records the decision, and only then lets the operation run.
//! A decision that cannot be recorded fails closed: the operation does not run.

use crate::auth::{allows, hash_token, MISSING_SCOPE, TOKEN_OK, WRITES_DISABLED};
use crate::core::{Actor, ActorKind, AuthDecision, AuthOutcome, Moment, Token, TokenKind};
use crate::errors::VogtError;
use crate::registry::{Operation, Scope, Transport};
use crate::storage::interface::{DeclaredStore, ReadView};

/// Why a request was turned away, in the order the checks run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Denial {
    /// No `Authorization: Bearer` header, or one that does not parse.
    NoBearer,
    /// A token was presented but does not resolve to a live credential.
    Rejected { code: &'static str },
    /// The token is live but does not carry the operation's scope.
    Forbidden { held: Vec<String>, needed: String },
    /// The instance refuses writes and the operation mutates.
    WritesDisabled,
    /// The decision could not be recorded, so nothing may proceed.
    Unrecorded { failure: String },
}

impl Denial {
    /// The error the adapter renders. The status and envelope come from the
    /// error, so both adapters answer the same way.
    pub fn error(&self) -> VogtError {
        match self {
            Denial::NoBearer => VogtError::Unauthenticated("no bearer token presented".to_string()),
            Denial::Rejected { .. } => VogtError::Unauthenticated(TOKEN_INVALID.to_string()),
            Denial::Forbidden { .. } => VogtError::Forbidden(
                "the token does not carry the scope this operation needs".to_string(),
            ),
            Denial::WritesDisabled => {
                VogtError::Forbidden("writes are disabled on this instance".to_string())
            }
            Denial::Unrecorded { failure } => VogtError::MigrationError(format!(
                "the authorization decision could not be recorded: {failure}"
            )),
        }
    }
}

/// What a request that passed the gate may do. `kind` and `display_name` come
/// from the actor row the gate already loaded to check `disabled`, so a route
/// builds its principal from them instead of assuming a human.
pub struct Grant {
    pub actor_id: String,
    pub identity_ref: Option<String>,
    pub kind: ActorKind,
    pub display_name: String,
    pub token_id: String,
    pub scopes: Vec<String>,
    /// The token that authenticated the request, secret-free. `None` for the
    /// `--no-auth` caller, who holds no credential.
    pub token: Option<Token>,
}

/// How long a session token's expiry moves forward on each use, in seconds.
const SLIDING_SESSION_SECONDS: i64 = 12 * 60 * 60;

/// The request-shaped inputs to a decision, kept together so the check itself
/// stays readable.
pub struct Request<'a> {
    pub operation: &'a Operation,
    pub transport: Transport,
    pub presented: Option<&'a str>,
    pub no_auth: bool,
    pub writes_enabled: bool,
    pub now: Moment,
}

/// Resolve the credential and nothing more. A refusal is recorded, because a
/// rejected token is the fact worth keeping; a success is not, because the
/// authorization that follows records the one row for the request. MCP uses
/// this so a ping or an initialize writes nothing for a live token.
pub fn authenticate<S: DeclaredStore>(store: &S, request: Request<'_>) -> Result<Grant, Denial> {
    resolve(store, request, false)
}

/// Authenticate, then authorize, recording the one decision for the request.
pub fn authorize<S: DeclaredStore>(store: &S, request: Request<'_>) -> Result<Grant, Denial> {
    resolve(store, request, true)
}

fn resolve<S: DeclaredStore>(
    store: &S,
    request: Request<'_>,
    record_allow: bool,
) -> Result<Grant, Denial> {
    let Request {
        operation,
        transport,
        presented,
        no_auth,
        writes_enabled,
        now,
    } = request;
    if no_auth {
        // Python's loopback caller is `local:<os-user>`, a human principal, not
        // a bare "local". The actor id stays empty because no actor row exists.
        let principal = crate::core::local_principal(&crate::core::os_user());
        return Ok(Grant {
            actor_id: String::new(),
            identity_ref: Some(principal.identity_ref.clone()),
            kind: principal.kind,
            display_name: principal.display_name,
            token_id: "no-auth".to_string(),
            scopes: vec!["admin".to_string()],
            token: None,
        });
    }
    let Some(secret) = presented else {
        return Err(Denial::NoBearer);
    };
    let (token, actor) = match lookup(store, secret, now) {
        Ok(token) => token,
        Err(rejection) => {
            record(
                store,
                &mut decision(Recorded {
                    id: "",
                    at: now,
                    operation,
                    transport: Transport::Http,
                    outcome: AuthOutcome::Deny,
                    reason: rejection.code,
                    token: rejection.token.as_ref(),
                    scope: None,
                    detail: None,
                    operation_name: "authenticate",
                }),
            )?;
            return Err(Denial::Rejected {
                code: rejection.code,
            });
        }
    };
    let held: Vec<&str> = token.scopes.iter().map(String::as_str).collect();
    let (permitted, reason) = allows(
        &held,
        writes_enabled,
        operation.scope.as_str(),
        operation.mutating,
    );
    if !permitted {
        record(
            store,
            &mut decision(Recorded {
                id: "",
                at: now,
                operation,
                transport,
                outcome: AuthOutcome::Deny,
                reason,
                token: Some(&token),
                scope: Some(operation.scope),
                detail: None,
                operation_name: operation.name,
            }),
        )?;
        return Err(if reason == WRITES_DISABLED {
            Denial::WritesDisabled
        } else {
            Denial::Forbidden {
                held: token.scopes.clone(),
                needed: operation.scope.as_str().to_string(),
            }
        });
    }
    debug_assert!(reason == TOKEN_OK || reason == MISSING_SCOPE);
    if record_allow {
        record(
            store,
            &mut decision(Recorded {
                id: "",
                at: now,
                operation,
                transport,
                outcome: AuthOutcome::Allow,
                reason,
                token: Some(&token),
                scope: Some(operation.scope),
                detail: None,
                operation_name: operation.name,
            }),
        )?;
        slide(store, &token, now);
    }
    Ok(Grant {
        actor_id: token.actor_id.clone(),
        identity_ref: token.actor_identity_ref.clone(),
        kind: actor.kind,
        display_name: actor.display_name,
        token_id: token.id.clone(),
        scopes: token.scopes.clone(),
        token: Some(token),
    })
}

struct Rejection {
    code: &'static str,
    /// The token the refusal belongs to, when one was found. An unknown token
    /// has none; a revoked, expired or disabled one keeps its actor and token
    /// id, because the operator needs to see whose credential was refused.
    token: Option<Token>,
    /// The actor behind the token, kept so a refusal can name it. `None` when
    /// the actor was never loaded, which is every refusal except a disabled one.
    actor: Option<Actor>,
}

fn lookup_failed(_: VogtError) -> Box<Rejection> {
    Box::new(Rejection {
        code: "lookup_failed",
        token: None,
        actor: None,
    })
}

/// The one message every bad token gets. The reason code in the recorded row
/// says which check failed; the caller is told none of them.
const TOKEN_INVALID: &str = "the presented token is not valid";

fn lookup<S: DeclaredStore>(
    store: &S,
    secret: &str,
    now: Moment,
) -> Result<(Token, Actor), Box<Rejection>> {
    let hashed = hash_token(secret);
    let view = store.read().map_err(lookup_failed)?;
    let found = view.token_by_hash(&hashed).map_err(lookup_failed)?;
    let Some(token) = found else {
        return Err(Box::new(Rejection {
            code: "unknown_token",
            token: None,
            actor: None,
        }));
    };
    if token.revoked_at.is_some() {
        return Err(Box::new(Rejection {
            code: "token_revoked",
            token: Some(token),
            actor: None,
        }));
    }
    if let Some(expires) = token.expires_at {
        if expires <= now {
            return Err(Box::new(Rejection {
                code: "token_expired",
                token: Some(token),
                actor: None,
            }));
        }
    }
    let actor = view.actor_by_id(&token.actor_id).map_err(lookup_failed)?;
    if actor.as_ref().is_none_or(|actor| actor.disabled) {
        return Err(Box::new(Rejection {
            code: "actor_disabled",
            token: Some(token),
            actor: None,
        }));
    }
    Ok((
        token,
        actor.expect("the check above returned when there was none"),
    ))
}

struct Recorded<'a> {
    id: &'a str,
    at: Moment,
    operation: &'a Operation,
    transport: Transport,
    outcome: AuthOutcome,
    reason: &'a str,
    token: Option<&'a Token>,
    scope: Option<Scope>,
    detail: Option<String>,
    /// Set when the row is about the token itself rather than the operation,
    /// which is how an authentication refusal is recorded.
    operation_name: &'a str,
}

fn decision(recorded: Recorded<'_>) -> AuthDecision {
    AuthDecision {
        id: recorded.id.to_string(),
        at: recorded.at,
        decision: recorded.outcome,
        reason_code: recorded.reason.to_string(),
        operation: recorded.operation_name.to_string(),
        scope: recorded.scope.map(|scope| scope.as_str().to_string()),
        actor_id: recorded.token.map(|token| token.actor_id.clone()),
        token_id: recorded.token.map(|token| token.id.clone()),
        identity_ref: recorded
            .token
            .and_then(|token| token.actor_identity_ref.clone()),
        transport: transport_name(recorded.transport).to_string(),
        detail: recorded.detail,
    }
}

fn transport_name(transport: Transport) -> &'static str {
    match transport {
        Transport::Cli => "cli",
        Transport::Http => "http",
        // Python's MCP route records `mcp-http`, not the registry's transport
        // name. The registry says which surface this is; the row says which
        // adapter wrote it.
        Transport::Mcp => "mcp-http",
    }
}

fn record<S: DeclaredStore>(store: &S, decision: &mut AuthDecision) -> Result<(), Denial> {
    // The id is minted here and nowhere earlier. A request that writes no row
    // — a missing bearer, a switched-off check — must not consume one, or the
    // next real row skips a number and the hooks-on sequence drifts from Python.
    let (at, id) = store.stamp_and_id("aut");
    decision.at = at;
    decision.id = id;
    store
        .record_auth_decision(decision)
        .map_err(|error| Denial::Unrecorded {
            failure: error.to_string(),
        })
}

/// A session token's expiry slides forward on use. A failure here is not a
/// reason to refuse the request: the decision is already recorded.
fn slide<S: DeclaredStore>(store: &S, token: &Token, now: Moment) {
    let expires = match token.kind {
        TokenKind::Session => Some(Moment::from_unix(
            now.unix_seconds().saturating_add(SLIDING_SESSION_SECONDS),
            now.nanos(),
        )),
        _ => None,
    };
    if let Err(error) = store.touch_token(&token.id, now, expires) {
        tracing::warn!("the token's last use could not be recorded: {error}");
    }
}
