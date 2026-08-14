//! The front door's view of vogt-core (NFR-D11).
//!
//! The merged product publishes one port, and this engine is what listens on
//! it. Vogt's Python core runs beside it on loopback and is never published;
//! everything a client asks of the core arrives here first:
//!
//! | Front door | vogt-core | Auth |
//! |---|---|---|
//! | `/api/vogt/*` | `/api/*` | front-door token, core token injected |
//! | `/mcp` | `/mcp` | the client's own core token, forwarded untouched |
//! | `/ui-legacy/*` | `/ui/*` | none — static assets, as at the core |
//!
//! Three decisions a future reader will ask about:
//!
//! **Why inject on `/api/vogt` but pass through on `/mcp`.** A browser holds a
//! front-door token: the engine's token namespace is the public one (FR-S9),
//! and the core token it maps to stays server-side. An MCP client holds a
//! *core* token already — that is how agents reach Vogt today, minted by
//! `vogt token issue` and bound to an actor — so rewriting its credential
//! would replace a real actor with a shared one and make the audit log worse.
//! Which core token gets injected is now the caller's own: each front-door
//! token may be paired with one (`vogt_core_token_file` on its `extra_tokens`
//! entry), and the proxy injects the pairing belonging to the token that
//! authenticated *this* request, so the core's audit names a real actor per
//! front-door holder rather than one shared proxy identity. The per-session
//! actor tokens of FR-S10 land with the session work.
//!
//! **Why the body is streamed rather than buffered.** `/mcp` is streamable
//! HTTP: a response is an SSE stream that stays open. Reading it to completion
//! before answering would turn every tool call into a hang.
//!
//! **Why an absent core is not a failure of this process.** The engine owns
//! PTYs. If the core is down, sessions must keep running (FR-E9) and the
//! surfaces that need it say so in as many words (FR-U21) — so these routes
//! answer 503 with a named reason, and readiness reports the outage without
//! declaring this container unready. See `api::check_vogt_core`.

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use serde_json::json;

use crate::app::AppState;
use crate::auth::AuthorizedIdentity;
use crate::config::Config;

/// Front-door mount points. Public so the router and its tests name the same
/// strings, and so a change to one is a change to both.
pub const API_PREFIX: &str = "/api/vogt";
pub const MCP_PREFIX: &str = "/mcp";
pub const LEGACY_GUI_PREFIX: &str = "/ui-legacy";

/// The core's own prefixes, which the ones above map onto.
const CORE_API_PREFIX: &str = "/api";
const CORE_GUI_PREFIX: &str = "/ui";
const CORE_READY_PATH: &str = "/health/ready";

/// Headers that describe one hop and must not be copied to the next. Listed
/// rather than derived because `reqwest` sets its own and a copied
/// `content-length` or `transfer-encoding` contradicts the body we actually
/// send.
const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub struct VogtCore {
    client: reqwest::Client,
    base: String,
    /// The core token to inject when the calling front-door token has no
    /// pairing of its own. This is the single configured `vogt_core_token`,
    /// and keeping it is a decision rather than an oversight: a deployment
    /// that provisioned one shared core token and no per-token pairings — the
    /// shape M9 shipped and the one `deploy/vogt-stack.compose.yml` still
    /// describes — keeps working across this change without editing its
    /// config. Pairings are how a deployment opts in to named actors, one
    /// token at a time.
    fallback_token: Option<String>,
}

impl VogtCore {
    /// Build the proxy, or don't, and say which by returning `None`.
    ///
    /// An engine with no core configured is a supported deployment: it is
    /// MyDevEnv2 as it shipped, and FR-E9 requires it to keep working.
    pub fn from_config(cfg: &Config) -> Option<Arc<Self>> {
        let base = cfg
            .vogt_core_url
            .as_ref()?
            .trim_end_matches('/')
            .to_string();
        let client = reqwest::Client::builder()
            // No overall timeout: `/mcp` responses are long-lived SSE streams
            // and a deadline on the whole request would cut them. The connect
            // timeout is what protects against a core that is not listening.
            .connect_timeout(Duration::from_secs(2))
            // A proxy hands redirects to its client rather than resolving
            // them, so the client sees the location the core actually sent.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok()?;
        Some(Arc::new(Self {
            client,
            base,
            fallback_token: cfg.vogt_core_token.clone(),
        }))
    }

    /// Is the core answering, and what did it say?
    ///
    /// Deliberately reads `/health/ready` rather than any port-is-open check:
    /// the core reports its applied schema version there, and "listening but
    /// mid-migration" is the state worth telling an operator about.
    pub async fn probe(&self) -> (bool, String) {
        let url = format!("{}{CORE_READY_PATH}", self.base);
        let request = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await;
        match request {
            Ok(response) => {
                let status = response.status();
                let detail = response
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|body| {
                        let applied = body.get("observed_schema_version")?.to_string();
                        let declared = body.get("declared_schema_version")?.to_string();
                        Some(format!("schema {applied} of {declared}"))
                    })
                    .unwrap_or_else(|| status.to_string());
                if status.is_success() {
                    (true, format!("ready ({detail})"))
                } else {
                    (false, format!("not ready: {status} ({detail})"))
                }
            }
            Err(e) => (false, format!("unreachable: {}", terse(&e))),
        }
    }

    /// Forward one request to `upstream_path`, streaming both ways.
    ///
    /// `inject` is the core token to present as this request's credential, or
    /// `None` to hand the core whatever the caller sent — which is what `/mcp`
    /// and the legacy GUI want and what `/api/vogt` must never do.
    async fn forward(
        &self,
        upstream_path: &str,
        inject: Option<&str>,
        request: Request,
    ) -> Response {
        let query = request
            .uri()
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        let url = format!("{}{upstream_path}{query}", self.base);

        let method = request.method().clone();
        let headers = request.headers().clone();
        let body = reqwest::Body::wrap_stream(request.into_body().into_data_stream());

        let mut outbound = self.client.request(method, &url);
        for (name, value) in headers.iter() {
            if is_hop_by_hop(name) || name == header::HOST {
                continue;
            }
            // The core is told who to trust by us, not by the caller: a
            // front-door token in this header would only ever be rejected.
            if inject.is_some() && name == header::AUTHORIZATION {
                continue;
            }
            outbound = outbound.header(name, value);
        }
        if let Some(token) = inject {
            outbound = outbound.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        outbound = outbound.body(body);

        match outbound.send().await {
            Ok(response) => {
                let status = response.status();
                let mut out = Response::builder().status(status);
                if let Some(headers_mut) = out.headers_mut() {
                    for (name, value) in response.headers().iter() {
                        if is_hop_by_hop(name) || name == header::CONTENT_LENGTH {
                            continue;
                        }
                        headers_mut.append(name.clone(), value.clone());
                    }
                }
                out.body(Body::from_stream(response.bytes_stream()))
                    .unwrap_or_else(|_| unavailable("the core's answer could not be relayed"))
            }
            Err(e) => bad_gateway(&format!("vogt-core did not answer: {}", terse(&e))),
        }
    }
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    HOP_BY_HOP.contains(&name.as_str())
}

/// `reqwest`'s `Display` includes the full URL, which carries the loopback
/// port and every query parameter. A client is told what went wrong, not
/// where the core lives.
fn terse(error: &reqwest::Error) -> &'static str {
    if error.is_connect() {
        "connection refused"
    } else if error.is_timeout() {
        "timed out"
    } else if error.is_body() || error.is_decode() {
        "the response body ended early"
    } else {
        "the request failed"
    }
}

// -- handlers ---------------------------------------------------------------

/// `/api/vogt/*` → the core's `/api/*`, with the caller's paired core token
/// injected (FR-S9).
///
/// The identity comes from the request extension `require_bearer` inserted
/// after it authorized the caller; there is no second look at the incoming
/// credential here, and there must not be one.
pub async fn api(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let Some(core) = state.vogt_core.as_ref() else {
        return not_configured();
    };

    let identity = request.extensions().get::<AuthorizedIdentity>().cloned();
    // The caller's own pairing first, the deployment-wide token second. The
    // fallback is what keeps an M9 deployment — one configured
    // `vogt_core_token`, no pairings — working unchanged; it is deliberate,
    // not a leftover.
    let injected = identity
        .as_ref()
        .and_then(|caller| caller.vogt_core_token.clone())
        .or_else(|| core.fallback_token.clone());

    let Some(injected) = injected else {
        // Neither, so there is nothing to inject. Forwarding anyway would send
        // an unauthenticated request to the core and return its 401 as if the
        // caller had got something wrong, so say what is actually missing.
        return unavailable(&match identity.as_ref() {
            Some(caller) => format!(
                "front-door token \"{}\" has no paired vogt-core token, and no fallback \
                 vogt_core_token is configured for this front door",
                caller.token_name
            ),
            None => "this request carries no front-door identity, and no fallback \
                     vogt_core_token is configured for this front door"
                .to_string(),
        });
    };

    let path = map_prefix(request.uri(), API_PREFIX, CORE_API_PREFIX);
    core.forward(&path, Some(&injected), request).await
}

/// `/mcp` → the core's `/mcp`, credential untouched.
///
/// Unchanged by the per-token mapping above, and deliberately: the credential
/// on an MCP request is already a core token bound to an actor.
pub async fn mcp(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let Some(core) = state.vogt_core.as_ref() else {
        return not_configured();
    };
    let path = map_prefix(request.uri(), MCP_PREFIX, MCP_PREFIX);
    core.forward(&path, None, request).await
}

/// `/ui-legacy/*` → the core's `/ui/*` (FR-U9).
///
/// The vanilla GUI keeps serving until the PWA reaches parity with it, and it
/// serves from here so that "the merged product publishes one port" has no
/// exception written into it. The bundle notices the prefix and asks
/// `/api/vogt` instead of `/api`; see `app.js`.
pub async fn legacy_gui(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let Some(core) = state.vogt_core.as_ref() else {
        return not_configured();
    };
    let path = map_prefix(request.uri(), LEGACY_GUI_PREFIX, CORE_GUI_PREFIX);
    core.forward(&path, None, request).await
}

/// `/ui-legacy` → `/ui-legacy/`, the same redirect the core does at `/ui`.
///
/// Not cosmetic. `index.html` links its stylesheet and module relatively, and
/// a browser resolves those against the *directory* of the current document:
/// served at `/ui-legacy`, `app.js` would be looked for at `/app.js`, which
/// out here is the PWA's catch-all. Serving the page only under a path that
/// ends in a slash is what makes the relative links land.
pub async fn legacy_gui_root(State(state): State<Arc<AppState>>) -> Response {
    if state.vogt_core.is_none() {
        return not_configured();
    }
    Redirect::permanent(&format!("{LEGACY_GUI_PREFIX}/")).into_response()
}

fn map_prefix(uri: &Uri, from: &str, to: &str) -> String {
    let path = uri.path();
    match path.strip_prefix(from) {
        Some(rest) => format!("{to}{rest}"),
        None => path.to_string(),
    }
}

// -- refusals, each with a reason (FR-U21) ----------------------------------

fn not_configured() -> Response {
    unavailable("this front door has no vogt-core configured (VOGT_CORE_URL is unset)")
}

fn unavailable(reason: &str) -> Response {
    refusal(StatusCode::SERVICE_UNAVAILABLE, reason)
}

fn bad_gateway(reason: &str) -> Response {
    refusal(StatusCode::BAD_GATEWAY, reason)
}

fn refusal(status: StatusCode, reason: &str) -> Response {
    let mut response = (status, Json(json!({ "error": { "message": reason } }))).into_response();
    // Says which half of the product refused, so an operator reading a 503 in
    // a browser console knows whether to look at the engine or the core.
    response.headers_mut().insert(
        HeaderName::from_static("x-vogt-front-door"),
        HeaderValue::from_static("engine"),
    );
    response
}

/// Everything the engine can say about the core without asking it, for
/// `/api/config` and the absent-state views built on it (FR-U21).
pub fn public_status(state: &AppState) -> serde_json::Value {
    json!({
        "configured": state.vogt_core.is_some(),
        "api_prefix": API_PREFIX,
        "mcp_prefix": MCP_PREFIX,
        "legacy_gui_prefix": LEGACY_GUI_PREFIX,
    })
}

/// The header map a proxied request should not carry onward. Exposed for the
/// tests, which assert the list rather than trusting it.
pub fn strip_hop_by_hop(headers: &HeaderMap) -> Vec<String> {
    headers
        .keys()
        .filter(|name| is_hop_by_hop(name))
        .map(|name| name.to_string())
        .collect()
}
