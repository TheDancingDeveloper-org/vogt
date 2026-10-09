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
use crate::core::{AuthDecision, AuthOutcome, Moment, Token, TokenKind};
use crate::errors::VogtError;
use crate::registry::{Operation, Scope, Transport};
use crate::storage::interface::{DeclaredStore, ReadView};

/// Why a request was turned away, in the order the checks run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Denial {
    /// No `Authorization: Bearer` header, or one that does not parse.
    NoBearer,
    /// A token was presented but does not resolve to a live credential.
    Rejected { code: &'static str, detail: String },
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
            Denial::Rejected { detail, .. } => VogtError::Unauthenticated(detail.clone()),
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

/// What a request that passed the gate may do.
pub struct Grant {
    pub actor_id: String,
    pub identity_ref: Option<String>,
    pub token_id: String,
    pub scopes: Vec<String>,
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
    pub decision_id: &'a str,
}

/// Authenticate, authorize, record, and either grant or deny.
pub fn authorize<S: DeclaredStore>(store: &S, request: Request<'_>) -> Result<Grant, Denial> {
    let Request {
        operation,
        transport,
        presented,
        no_auth,
        writes_enabled,
        now,
        decision_id,
    } = request;
    if no_auth {
        record(
            store,
            decision(
                decision_id,
                now,
                operation,
                transport,
                AuthOutcome::Allow,
                "no_auth",
                None,
                None,
                None,
            ),
        )?;
        return Ok(Grant {
            actor_id: "local".to_string(),
            identity_ref: None,
            token_id: "no-auth".to_string(),
            scopes: vec!["admin".to_string()],
        });
    }
    let Some(secret) = presented else {
        record(
            store,
            decision(
                decision_id,
                now,
                operation,
                transport,
                AuthOutcome::Deny,
                "no_bearer_token",
                None,
                None,
                None,
            ),
        )?;
        return Err(Denial::NoBearer);
    };
    let token = match lookup(store, secret, now) {
        Ok(token) => token,
        Err(rejection) => {
            record(
                store,
                decision(
                    decision_id,
                    now,
                    operation,
                    transport,
                    AuthOutcome::Deny,
                    rejection.code,
                    None,
                    None,
                    Some(rejection.detail.clone()),
                ),
            )?;
            return Err(Denial::Rejected {
                code: rejection.code,
                detail: rejection.detail,
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
            decision(
                decision_id,
                now,
                operation,
                transport,
                AuthOutcome::Deny,
                reason,
                Some(&token),
                None,
                None,
            ),
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
    record(
        store,
        decision(
            decision_id,
            now,
            operation,
            transport,
            AuthOutcome::Allow,
            reason,
            Some(&token),
            Some(operation.scope),
            None,
        ),
    )?;
    slide(store, &token, now);
    Ok(Grant {
        actor_id: token.actor_id,
        identity_ref: token.actor_identity_ref,
        token_id: token.id,
        scopes: token.scopes,
    })
}

struct Rejection {
    code: &'static str,
    detail: String,
}

fn lookup_failed(error: VogtError) -> Rejection {
    Rejection {
        code: "lookup_failed",
        detail: format!("the token could not be looked up: {error}"),
    }
}

fn lookup<S: DeclaredStore>(store: &S, secret: &str, now: Moment) -> Result<Token, Rejection> {
    let hashed = hash_token(secret);
    let found = store
        .read()
        .map_err(lookup_failed)?
        .token_by_hash(&hashed)
        .map_err(lookup_failed)?;
    let Some(token) = found else {
        return Err(Rejection {
            code: "unknown_token",
            detail: "the token is not recognized".to_string(),
        });
    };
    if token.revoked_at.is_some() {
        return Err(Rejection {
            code: "revoked",
            detail: "the token has been revoked".to_string(),
        });
    }
    if let Some(expires) = token.expires_at {
        if expires <= now {
            return Err(Rejection {
                code: "expired",
                detail: "the token has expired".to_string(),
            });
        }
    }
    Ok(token)
}

fn decision(
    id: &str,
    at: Moment,
    operation: &Operation,
    transport: Transport,
    outcome: AuthOutcome,
    reason: &str,
    token: Option<&Token>,
    scope: Option<Scope>,
    detail: Option<String>,
) -> AuthDecision {
    AuthDecision {
        id: id.to_string(),
        at,
        decision: outcome,
        reason_code: reason.to_string(),
        operation: operation.name.to_string(),
        scope: scope.map(|scope| scope.as_str().to_string()),
        actor_id: token.map(|token| token.actor_id.clone()),
        token_id: token.map(|token| token.id.clone()),
        identity_ref: token.and_then(|token| token.actor_identity_ref.clone()),
        transport: transport_name(transport).to_string(),
        detail,
    }
}

fn transport_name(transport: Transport) -> &'static str {
    match transport {
        Transport::Cli => "cli",
        Transport::Http => "http",
        Transport::Mcp => "mcp",
    }
}

fn record<S: DeclaredStore>(store: &S, decision: AuthDecision) -> Result<(), Denial> {
    store
        .record_auth_decision(&decision)
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
