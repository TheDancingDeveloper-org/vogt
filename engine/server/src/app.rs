use std::sync::Arc;

use axum::{
    http::{header, HeaderValue, Method},
    middleware,
    routing::{any, get, post},
    Router,
};
use tower_http::{compression::CompressionLayer, cors::CorsLayer, trace::TraceLayer};

use crate::gui as gui_handlers;
use crate::push_api;
use crate::{
    agent_tasks::{self, AgentTaskRegistry},
    api, assets,
    assistant::AssistantRuntime,
    assistant_api,
    assistant_log::AssistantLog,
    assistant_speech::{self, AssistantSpeech},
    auth,
    config::Config,
    events::EventBus,
    files, git,
    gui::GuiRegistry,
    history::SessionHistory,
    history_api, observability,
    push::PushManager,
    sessions::SessionRegistry,
    vogt_core::{self, VogtCore},
    ws,
};

/// Path prefixes that belong to a machine API and must never be answered by
/// the PWA (#34). Each is owned all the way to its leaves — `/mcp` by the
/// proxy routes below, `/api` by the explicit JSON 404 below that — so an
/// unknown path under one is an error and not a front-end route.
///
/// Ordering the catch-all last is not enough on its own: it only protects
/// paths that are *registered*, so `/api/openapi.json` was claimed by the SPA
/// fallback and answered `200 text/html`. Public so the router and its tests
/// name the same strings, which is what makes the guarantee a property rather
/// than a comment.
pub const MACHINE_NAMESPACES: [&str; 2] = ["/api", "/mcp"];

pub struct AppState {
    pub config: Arc<Config>,
    pub auth: Arc<auth::AuthRuntime>,
    pub sessions: Arc<SessionRegistry>,
    pub bus: EventBus,
    pub gui: Arc<GuiRegistry>,
    pub push: Arc<PushManager>,
    pub agent_tasks: Arc<AgentTaskRegistry>,
    pub history: Option<Arc<SessionHistory>>,
    /// None when `assistant_api_key` is not configured; routes 404.
    pub assistant: Option<Arc<AssistantRuntime>>,
    /// Server-side speech proxy (FR-T12), or `None` when neither the STT nor
    /// the TTS half is configured — in which case both `/api/assistant/stt`
    /// and `/api/assistant/tts` answer 404 and the client falls back (FR-T6).
    /// Configured independently of the chat profile: its base URL, key and
    /// model come from the `assistant_stt_*` / `assistant_tts_*` keys, never
    /// from `assistant_profiles`.
    pub assistant_speech: Option<Arc<AssistantSpeech>>,
    /// The durable assistant interaction log (FR-T14). Engine-local, so an
    /// absent core costs it nothing (FR-E9). `None` only when the store could
    /// not be opened, which degrades to a live-only conversation rather than
    /// refusing the assistant.
    pub assistant_log: Option<Arc<AssistantLog>>,
    /// None when no vogt-core is configured, which is the engine running as
    /// it always has. The Vogt routes then answer 503 with a named reason
    /// and every session keeps working (FR-E9).
    pub vogt_core: Option<Arc<VogtCore>>,
}

pub async fn router(cfg: Config) -> (Router, Arc<AppState>) {
    let cfg = Arc::new(cfg);
    let bus = EventBus::default();

    // Initialize session history (optional, continues if init fails)
    let history = match SessionHistory::new(&cfg.state_dir).await {
        Ok(h) => {
            tracing::info!("session history enabled");
            Some(Arc::new(h))
        }
        Err(e) => {
            tracing::warn!("session history disabled: {}", e);
            None
        }
    };

    // One-shot backfill (#475): index raw session logs that survived a hard
    // restart but never got an index row (the engine was SIGKILLed before it
    // could archive). Spawned rather than awaited so a large log dir does not
    // delay the server binding its port; it runs once and reports the count.
    if let Some(history) = history.clone() {
        tokio::spawn(async move {
            match history.backfill_orphaned_logs().await {
                Ok(0) => {}
                Ok(recovered) => {
                    tracing::info!(recovered, "backfilled orphaned session logs into history")
                }
                Err(e) => tracing::warn!("history backfill failed: {e}"),
            }
        });
    }

    let sessions = Arc::new(SessionRegistry::new(
        Arc::clone(&cfg),
        bus.clone(),
        history.clone(),
    ));
    let gui = Arc::new(GuiRegistry::new());
    let push = Arc::new(
        PushManager::with_subject(
            &cfg.state_dir,
            cfg.fcm_service_account_json.as_deref(),
            &cfg.vapid_subject,
        )
        .expect("push manager init"),
    );
    let agent_tasks = Arc::new(
        AgentTaskRegistry::new(
            &cfg.state_dir,
            Arc::clone(&sessions),
            Arc::clone(&push),
            bus.clone(),
        )
        .expect("agent task registry init"),
    );

    // The durable assistant interaction log (FR-T14), opened like session
    // history: engine-local under `state_dir`, and a failed open degrades to a
    // live-only conversation rather than refusing the assistant.
    let assistant_log = match AssistantLog::new(&cfg.state_dir).await {
        Ok(log) => Some(Arc::new(log)),
        Err(e) => {
            tracing::warn!("assistant interaction log disabled: {e}");
            None
        }
    };

    let assistant = AssistantRuntime::from_config(
        &cfg,
        Arc::clone(&sessions),
        Arc::clone(&agent_tasks),
        Arc::clone(&push),
        assistant_log.clone(),
    );
    if assistant.is_some() {
        tracing::info!(model = %cfg.assistant_model, "assistant enabled");
    }

    // Server-side speech (FR-T12), configured independently of the chat
    // profile. `None` when neither half is set, and each route 404s per half.
    let assistant_speech = AssistantSpeech::from_config(&cfg);
    if let Some(speech) = assistant_speech.as_ref() {
        tracing::info!(
            stt = speech.stt_enabled(),
            tts = speech.tts_enabled(),
            "server-side speech enabled"
        );
    }

    let vogt_core = VogtCore::from_config(&cfg);
    match cfg.vogt_core_url.as_deref() {
        Some(url) => tracing::info!(
            url = %url,
            // Whether a fallback exists, and how many front-door tokens reach
            // the core as an actor of their own (FR-S9). Counts, never values.
            fallback_token = cfg.vogt_core_token.is_some(),
            paired_tokens = cfg
                .extra_tokens
                .iter()
                .filter(|t| t.vogt_core_token.is_some())
                .count(),
            "vogt-core front door enabled"
        ),
        // Logged at info, not warn: an engine with no core is a supported
        // deployment, not a misconfiguration (FR-E9).
        None => {
            tracing::info!("no vogt-core configured; /api/vogt and /mcp will refuse")
        }
    }

    let state = Arc::new(AppState {
        config: cfg,
        auth: Arc::new(auth::AuthRuntime::default()),
        sessions,
        bus,
        gui,
        push,
        agent_tasks,
        history,
        assistant,
        assistant_speech,
        assistant_log,
        vogt_core,
    });

    // Background task: fan out a push notification whenever a session enters
    // `waiting-for-input` or `errored`. Subscribes to the events bus.
    push_api::spawn_activity_watcher(Arc::clone(&state));
    // Background task: notify once when a session has sat `Idle` for a long
    // time without a recognizable prompt (the waiting-for-input heuristic
    // never fired).
    push_api::spawn_idle_stall_watcher(Arc::clone(&state));
    push_api::spawn_digest_flusher(Arc::clone(&state));
    // One background reader of vogt-core's event cursor, and everything that
    // cares about a core-side change hangs off it. The follower republishes
    // each change onto this server's bus as `VogtChanged` (FR-U10); the
    // browser gets it over SSE, and the drift watcher below gets it the same
    // way a session watcher gets an activity change. No-ops when no core or
    // no core token is configured (FR-E9).
    vogt_core::spawn_event_follower(Arc::clone(&state));
    // Background task: turn the drift among those republished events into a
    // push (FR-M2). Reads the bus, never the core — see the watcher's own
    // comment for what that costs across a restart, which is a decision
    // rather than an oversight.
    push_api::spawn_vogt_drift_watcher(Arc::clone(&state));
    state.agent_tasks.spawn_scheduler();
    state.agent_tasks.spawn_run_watcher(state.bus.clone());
    // Background task: subscribe to the same bus and start runs from matching
    // core-state events (#290). The follower above is what puts vogt-core's
    // events on this bus; this watcher turns the ones a task listens for into
    // runs, bound and audited, capped per task and without a retry storm.
    state.agent_tasks.spawn_trigger_watcher(state.bus.clone());
    // Background task: enforce the assistant log's retention horizon on a
    // schedule (FR-T14), so the horizon is a configured maximum rather than
    // whatever the last caller passed — the failure mode r18 named in the
    // session log.
    spawn_assistant_log_retention_sweeper(Arc::clone(&state));

    // Public: /healthz, /api/config, /api/push/public-key. None reveal secrets.
    let public = Router::new()
        .route("/healthz", get(api::healthz))
        .route("/readyz", get(api::readyz))
        .route("/api/config", get(gui_handlers::public_config))
        .route("/api/push/public-key", get(push_api::public_key));

    let api_routes = Router::new()
        .route(
            "/api/sessions",
            get(api::list_sessions).post(api::create_session),
        )
        .route(
            "/api/sessions/{id}",
            get(api::get_session)
                .patch(api::rename_session)
                .delete(api::delete_session),
        )
        .route("/api/sessions/{id}/kill", post(api::kill_session))
        .route("/api/sessions/{id}/input", post(api::session_input))
        .route("/api/assistant/message", post(assistant_api::message))
        .route(
            "/api/assistant/actions/{id}",
            post(assistant_api::resolve_action).patch(assistant_api::replace_reason),
        )
        .route("/api/assistant/history", get(assistant_api::history))
        .route("/api/assistant/log", get(assistant_api::log))
        .route("/api/assistant/reset", post(assistant_api::reset))
        // Server-side speech (FR-T12). Both POST under `/api/assistant`, so the
        // `starts_with("/api/assistant") && != GET ⇒ Assistant` rule in
        // `auth::required_capability` already gates them behind the `assistant`
        // capability; they 404 per half when unconfigured.
        .route("/api/assistant/stt", post(assistant_speech::stt))
        .route("/api/assistant/tts", post(assistant_speech::tts))
        .route("/api/events", get(api::events_stream))
        .route("/api/auth/check", get(api::auth_check))
        .route("/api/status", get(api::operational_status))
        .route("/api/files", get(files::read_file).put(files::write_file))
        .route("/api/files/op", post(files::operate))
        .route("/api/files/download", get(files::download_file))
        .route("/api/dir", get(files::list_dir))
        .route("/api/tree", get(files::tree))
        .route("/api/search", get(files::search))
        .route("/api/search/files", get(files::search_files))
        .route("/api/git/status", get(git::status))
        .route("/api/git/diff", get(git::diff))
        .route("/api/git/log", get(git::log))
        .route("/api/git/branch", get(git::branch))
        .route("/api/git/op", post(git::operate))
        .route("/api/gui/launch", post(gui_handlers::launch))
        .route("/api/gui/processes", get(gui_handlers::processes))
        .route("/api/gui/kill", post(gui_handlers::kill_proc))
        .route(
            "/api/agent-tasks",
            get(agent_tasks::list).post(agent_tasks::create),
        )
        .route(
            "/api/agent-tasks/{id}",
            get(agent_tasks::get)
                .patch(agent_tasks::update)
                .delete(agent_tasks::delete),
        )
        .route("/api/agent-tasks/{id}/pause", post(agent_tasks::pause))
        .route("/api/agent-tasks/{id}/resume", post(agent_tasks::resume))
        .route("/api/agent-tasks/{id}/run", post(agent_tasks::run_now))
        .route("/api/agent-tasks/{id}/steer", post(agent_tasks::steer))
        .route(
            "/api/agent-tasks/{id}/gates/{gate_id}/answer",
            post(agent_tasks::answer_gate),
        )
        .route(
            "/api/agent-tasks/artifacts/cleanup",
            post(agent_tasks::cleanup_prompt_artifacts),
        )
        .route("/api/push/subscribe", post(push_api::subscribe))
        .route("/api/push/update", post(push_api::update))
        .route("/api/push/unsubscribe", post(push_api::unsubscribe))
        .route("/api/push/list", get(push_api::list))
        .route("/api/push/test", post(push_api::test_dispatch))
        .route("/api/push/flush-digests", post(push_api::flush_digests))
        .route("/api/history/sessions", get(history_api::list_sessions))
        .route("/api/history/search", get(history_api::search_sessions))
        .route("/api/history/cleanup", post(history_api::cleanup_sessions))
        .route("/api/history/{id}/log", get(history_api::get_session_log))
        .route(
            "/api/history/{id}/download",
            get(history_api::download_session_log),
        )
        .route(
            "/api/history/{id}",
            get(history_api::get_session).delete(history_api::delete_session),
        )
        // Vogt's operations, reached through the front door (NFR-D11). They
        // carry the same bearer gate as every other API route here: the
        // engine's token namespace is the public one, and the core token this
        // proxy injects never leaves the process (FR-S9).
        // Three routes for two shapes: a wildcard segment needs at least one
        // character, so `/api/vogt/` matches neither `/api/vogt` nor
        // `/api/vogt/{*path}` and would fall through to the PWA's catch-all.
        .route("/api/vogt", any(vogt_core::api))
        .route("/api/vogt/", any(vogt_core::api))
        .route("/api/vogt/{*path}", any(vogt_core::api))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::require_bearer,
        ));

    // MCP and the probes are deliberately outside that gate.
    //
    // `/mcp` carries a *core* token minted by `vogt token issue` and bound to
    // an actor; the core validates it, and re-checking it against the
    // engine's unrelated token list would refuse every legitimate agent.
    // The probes come first because the catch-all below would otherwise
    // answer them with index.html at 200 — which is what it did (#24, FR-A7).
    // Built from `PROBE_PATHS` so the router and the requirement cannot drift.
    let mut vogt_open_routes = Router::new();
    for path in vogt_core::PROBE_PATHS {
        vogt_open_routes = vogt_open_routes.route(path, get(vogt_core::probe));
    }
    // The first-run install surface (#292) is open for the same reason the
    // probes are: it exists precisely for a browser that holds no token yet.
    // The core self-gates — its bootstrap answers only while its token store
    // is empty — and the door forwards the caller's own headers untouched,
    // exactly as `/mcp` does. Written as literals rather than built from
    // `vogt_core::INSTALL_PATHS` because the PWA's source-scan test reads
    // this router's route strings; a test below pins the two against the
    // constant so they cannot drift.
    let vogt_open_routes = vogt_open_routes
        .route("/api/install/status", get(vogt_core::install))
        .route("/api/install/bootstrap", post(vogt_core::install))
        .route("/mcp", any(vogt_core::mcp))
        .route("/mcp/", any(vogt_core::mcp))
        .route("/mcp/{*path}", any(vogt_core::mcp));

    // WS handles its own auth so query-param tokens work (browsers can't set
    // Authorization on a WebSocket handshake).
    let ws_routes = Router::new().route("/api/sessions/{id}/attach", get(ws::attach));

    // The `/api` namespace, owned to its leaves (#34). `/mcp` already is, by
    // the proxy routes above; `/api` was not, because it is a
    // list of individual routes rather than a subtree, and every path that was
    // not on that list fell through to the SPA fallback and came back as
    // `200 text/html`. A caller probing for `/api/openapi.json` read that as
    // "the spec is here and my parser is broken".
    //
    // Registered last-resort rather than gated: a path that does not exist
    // does not exist for any credential, and answering 401 first would make
    // the caller go looking for a token to reach nothing. Static routes and
    // the `/api/vogt` subtree still win — this only catches what nothing else
    // claimed. `any` so a POST to a typo'd route is a 404 too, not a 405.
    let api_fallback = Router::new()
        .route("/api", any(api_namespace_not_found))
        .route("/api/", any(api_namespace_not_found))
        .route("/api/{*path}", any(api_namespace_not_found));

    // Embedded PWA. The catch-all comes last so /healthz keeps priority, and
    // every machine namespace in `MACHINE_NAMESPACES` is owned above so this
    // can never claim a path under one. SPA-style fallback to index.html for
    // unknown paths is in `assets::not_found`.
    let asset_routes = Router::new()
        .route("/", get(assets::root))
        .route("/{*path}", get(assets::asset_wild));

    let cors = build_cors(&state.config.allowed_origins);

    let router = Router::new()
        .merge(public)
        .merge(api_routes)
        .merge(vogt_open_routes)
        .merge(ws_routes)
        .merge(api_fallback)
        .merge(asset_routes)
        // Negotiate gzip for the PWA and finite API responses. tower-http's
        // default predicate excludes SSE, images, ranges, and responses that
        // already have Content-Encoding; upgrade responses remain untouched.
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        // Outermost, so every request gets an id and a line — including the
        // ones CORS refuses and the ones no route claims (#139). It is also
        // where the id `vogt_core::forward` sends to the core is minted, so
        // the two halves of a proxied request share one identifier.
        .layer(middleware::from_fn(observability::access_log))
        .with_state(Arc::clone(&state));

    (router, state)
}

/// Enforce the assistant interaction log's retention horizon on a daily
/// schedule (FR-T14). Runs one sweep at startup and then every 24 hours, so a
/// long-running server never lets the log outgrow its configured maximum. A
/// no-op when the log could not be opened.
fn spawn_assistant_log_retention_sweeper(state: Arc<AppState>) {
    let Some(log) = state.assistant_log.clone() else {
        return;
    };
    let retention_days = state.config.assistant_log_retention_days;
    tokio::spawn(async move {
        loop {
            match log.cleanup(retention_days).await {
                Ok(removed) if removed > 0 => tracing::info!(
                    removed,
                    retention_days,
                    "assistant interaction log retention sweep"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!("assistant log retention sweep failed: {e}"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
        }
    });
}

/// The engine's ordinary error body, at 404, for a path under `/api` that no
/// route claimed. Deliberately the same shape every other failure here uses —
/// a client that already parses `{"error": ...}` needs nothing new to read it.
async fn api_namespace_not_found() -> crate::error::ApiError {
    crate::error::ApiError::NotFound
}

fn build_cors(origins: &[String]) -> CorsLayer {
    let parsed: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    let layer = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::ACCEPT])
        .allow_credentials(false)
        .max_age(std::time::Duration::from_secs(600));
    if parsed.is_empty() {
        // No origins configured = same-origin only. Don't emit Access-Control-*.
        layer
    } else {
        layer.allow_origin(parsed)
    }
}

pub async fn serve_forever(cfg: Config) -> std::io::Result<()> {
    let bind = cfg.bind;
    let (router, state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(addr = %bind, "vogt-engine listening");
    // Graceful shutdown (#475): the engine had no shutdown path, so every
    // redeploy SIGKILLed the PTYs and nothing was archived. Kubernetes/compose
    // send SIGTERM with a grace window before the KILL; we use it to drain every
    // live session into history first, then let axum finish in-flight requests.
    let drain_state = Arc::clone(&state);
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            tracing::info!("shutdown signal received; draining live sessions to history");
            drain_state.sessions.drain_to_history().await;
        })
        .await
}

/// Resolve when the process is asked to terminate: SIGTERM (the redeploy
/// signal on Kubernetes/compose) or SIGINT/Ctrl-C. On the off chance the
/// SIGTERM handler cannot be installed, fall back to Ctrl-C alone rather than
/// losing the shutdown hook entirely.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = ctrl_c => {}
                    _ = term.recv() => {}
                }
            }
            Err(e) => {
                tracing::warn!("failed to install SIGTERM handler: {e}");
                ctrl_c.await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await;
    }
}

#[cfg(test)]
mod tests {
    /// The install routes above are literals so the PWA's source-scan test
    /// can read them out of the router; this pins them to the constant the
    /// rest of the code names, so neither can move without the other.
    #[test]
    fn the_install_route_literals_match_the_constant() {
        let source = include_str!("app.rs");
        for path in crate::vogt_core::INSTALL_PATHS {
            assert!(
                source.contains(&format!("\"{path}\"")),
                "{path} is in INSTALL_PATHS but not registered as a route literal"
            );
        }
    }
}
