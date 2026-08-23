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
//! | `/version`, `/connection-info`, `/health/{ready,live}` | the same | none — these are the probes (FR-A7) |
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
    http::{header, HeaderName, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use serde_json::json;

use crate::app::AppState;
use crate::auth::AuthorizedIdentity;
use crate::config::Config;
use crate::events::ServerEvent;
use crate::observability::{RequestId, REQUEST_ID_HEADER};

/// Front-door mount points. Public so the router and its tests name the same
/// strings, and so a change to one is a change to both.
pub const API_PREFIX: &str = "/api/vogt";
pub const MCP_PREFIX: &str = "/mcp";
pub const LEGACY_GUI_PREFIX: &str = "/ui-legacy";

/// The core's own prefixes, which the ones above map onto.
const CORE_API_PREFIX: &str = "/api";
const CORE_GUI_PREFIX: &str = "/ui";
const CORE_READY_PATH: &str = "/health/ready";

/// Vogt's unauthenticated probes (FR-A7). Served here, at the same paths, by
/// the process that publishes the port — which is this one. Before r10 they
/// were not routed at all and the PWA catch-all answered them with
/// `index.html` at 200 (#24).
pub const PROBE_PATHS: [&str; 4] = [
    "/version",
    "/connection-info",
    "/health/ready",
    "/health/live",
];

/// How this door tells the core where clients actually arrive (FR-A8).
///
/// The core renders `connect` and `/connection-info`; it cannot know this
/// door's address or mount points, and inventing them is what made `connect`
/// hand out an unreachable URL (#26). It honours these only when configured
/// as `fronted` — and they are **stripped from every inbound request** below,
/// so what reaches the core is what this process said and never what a caller
/// claimed.
const IDENTITY_HEADERS: [&str; 3] = ["x-vogt-public-url", "x-vogt-api-path", "x-vogt-mcp-path"];

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
    /// The address this door is published at, stated to the core on every
    /// forwarded request (FR-A8). `None` when nobody has configured one, in
    /// which case nothing is stated and the core answers for itself — an
    /// exposure value carries no default (NFR-D2), and a URL this process
    /// guessed would be wrong in exactly the deployment the field exists for.
    public_url: Option<String>,
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
            public_url: cfg.public_url.clone(),
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

    /// One page of the core's `events.list` feed, on the engine's own behalf.
    ///
    /// Unlike everything else here this is not a proxy: no browser asked for
    /// it, so there is no caller identity to map and the deployment-wide
    /// `vogt_core_token` is the only credential available. A front door with
    /// no fallback token configured cannot read the feed at all, and says so
    /// rather than sending an unauthenticated request the core will refuse.
    ///
    /// Returns `(events, next_cursor)`. The core orders by `seq` ascending
    /// and returns the caller's own cursor when the page is empty, so a
    /// poller built on this never rewinds — see `list_events` in
    /// `services/history.py`, which is where that guarantee is made.
    pub async fn events_after(
        &self,
        cursor: u64,
        limit: u32,
    ) -> std::result::Result<(Vec<serde_json::Value>, u64), String> {
        let token = self
            .fallback_token
            .as_deref()
            .ok_or("no vogt_core_token is configured for this front door")?;
        let url = format!(
            "{}{CORE_API_PREFIX}/events?after={cursor}&limit={limit}",
            self.base
        );
        let response = self
            .client
            .get(&url)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| terse(&e).to_string())?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("the core answered {status}"));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("the core's answer did not parse: {}", terse(&e)))?;
        let events = body
            .get("events")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        // Falling back to the cursor we sent, never to 0: a malformed answer
        // must not rewind the feed and replay every event as new.
        let next = body
            .get("next_cursor")
            .and_then(|v| v.as_u64())
            .unwrap_or(cursor);
        Ok((events, next))
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
        // Whatever `access_log` decided this request is called — either the
        // caller's own id or the one minted for it. Stated to the core rather
        // than forwarded from the caller, for the same reason the identity
        // headers below are: what reaches the core is what this process said.
        let request_id = request
            .extensions()
            .get::<RequestId>()
            .map(|id| id.0.clone());
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
            // Where clients arrive is this process's fact, and a caller must
            // not be able to state it: `connect` renders a document meant to
            // be pasted beside a token, so a forwarded identity a caller chose
            // is a phishing primitive. Dropped unconditionally, then set from
            // configuration below.
            if is_identity_header(name) {
                continue;
            }
            // Dropped here and set below, so the core reads one id and it is
            // the one this door's own log line carries.
            if name == REQUEST_ID_HEADER {
                continue;
            }
            outbound = outbound.header(name, value);
        }
        if let Some(id) = request_id.as_deref() {
            outbound = outbound.header(REQUEST_ID_HEADER, id);
        }
        if let Some(token) = inject {
            outbound = outbound.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(url) = self.public_url.as_deref() {
            outbound = outbound
                .header("x-vogt-public-url", url)
                .header("x-vogt-api-path", API_PREFIX)
                .header("x-vogt-mcp-path", MCP_PREFIX);
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

/// Does this header claim to state where clients arrive? See `IDENTITY_HEADERS`
/// for why a caller is never allowed to.
fn is_identity_header(name: &HeaderName) -> bool {
    IDENTITY_HEADERS.contains(&name.as_str())
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

/// The probes of FR-A7, at the same paths, answered by the core.
///
/// `AGENTS.md` and `connect` both advertise these as the way to discover a
/// Vogt instance without a credential, and behind this door they were not
/// routed at all: the PWA catch-all answered them with `index.html` at **200**
/// (#24). A caller could not tell from the status that it had been handed a
/// web page, which is why FR-A7 now says a probe is served or refused and
/// never answered by a fallback. It broke `vogt-mcp-remote` at launch and made
/// a healthy `/mcp` look like a dead server.
///
/// Unauthenticated, like `/mcp` and `/ui-legacy` and for the plainer reason:
/// a probe that needs a token is not a probe, and a compose healthcheck calls
/// one.
///
/// `/connection-info` is not synthesised here. The core renders it — and
/// `connect` with it — against the identity `forward` states on every request,
/// so the hundred lines of prose and JSON that make up a client configuration
/// live in one place rather than being mirrored into this language.
pub async fn probe(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let Some(core) = state.vogt_core.as_ref() else {
        return not_configured();
    };
    let path = request.uri().path().to_string();
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

// -- republishing the core's changes onto this stream (FR-U10) -------------

/// How often the front door asks vogt-core what has changed.
///
/// A poll and not a subscription, because the core's feed is a cursor
/// (`events.list`, `after` + `limit`) and not a stream. The client-side
/// difference is what matters: one poll here, in one process, replaces every
/// open board polling on its own, and what reaches the browser is a push on
/// the event stream it already has open. A surface can then say "live"
/// without it being a lie.
const EVENT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The most events one poll will republish.
///
/// A burst larger than this is a sweep or an import, and a client that has
/// been told a hundred times that something changed learns nothing it did
/// not learn on the first. The cursor still advances past all of them, so
/// nothing is replayed later.
const EVENT_POLL_LIMIT: u32 = 50;

/// Follow vogt-core's event feed and republish onto the engine's bus.
///
/// Starts from the core's *current* head rather than from zero: an event
/// stream is for what happens next, and replaying an estate's history into a
/// live UI at boot would be indistinguishable from everything changing at
/// once. A restart therefore misses whatever happened while the engine was
/// down — which is correct for a notification, and is why nothing in the
/// product treats this stream as a record. `events.list` is the record.
pub fn spawn_event_follower(state: Arc<AppState>) {
    let Some(core) = state.vogt_core.clone() else {
        return;
    };
    if core.fallback_token.is_none() {
        // Without a core token the feed cannot be read, and asking anyway
        // would produce a 401 every five seconds for as long as the process
        // lives.
        tracing::info!("vogt event follower not started: no core token configured");
        return;
    }
    tokio::spawn(async move {
        // Start at the head: ask for a page of one and keep only the cursor.
        let mut cursor = match core.events_after(0, 1).await {
            Ok((_, next)) => next,
            Err(detail) => {
                tracing::info!(%detail, "vogt event follower: following from 0");
                0
            }
        };
        loop {
            tokio::time::sleep(EVENT_POLL_INTERVAL).await;
            // A core that is down is not an error here: the surfaces report
            // their own outage, and the cursor is kept so the first
            // successful poll after it returns catches up.
            let Ok((events, next)) = core.events_after(cursor, EVENT_POLL_LIMIT).await else {
                continue;
            };
            for event in events {
                let Some(kind) = event.get("kind").and_then(|v| v.as_str()) else {
                    continue;
                };
                state.bus.publish(ServerEvent::VogtChanged {
                    kind: kind.to_string(),
                    entity_kind: event
                        .get("entity_kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    entity_id: event
                        .get("entity_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    seq: event
                        .get("seq")
                        .and_then(|v| v.as_i64())
                        .unwrap_or_default(),
                    // The per-kind payload, verbatim, so an agent-task trigger
                    // (#290) can match on what changed and not only that
                    // something did. A malformed or absent summary becomes
                    // `null`, which the matcher treats as "no fields to match".
                    summary: event.get("summary").cloned().unwrap_or(serde_json::Value::Null),
                });
            }
            cursor = next;
        }
    });
}
