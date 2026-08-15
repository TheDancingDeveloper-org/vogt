use std::sync::Arc;

use axum::{
    http::{header, HeaderValue, Method},
    middleware,
    routing::{any, get, post},
    Router,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

use crate::gui as gui_handlers;
use crate::push_api;
use crate::{
    agent_tasks::{self, AgentTaskRegistry},
    api, assets,
    assistant::AssistantRuntime,
    assistant_api, auth,
    config::Config,
    contextkeeper::ContextKeeperRuntime,
    contextkeeper_api,
    events::EventBus,
    files, git,
    gui::GuiRegistry,
    history::SessionHistory,
    history_api,
    push::PushManager,
    sessions::SessionRegistry,
    vogt_core::{self, VogtCore},
    ws,
};

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
    /// None when ContextKeeper is not configured. Every terminal then reads as
    /// unprotected and the continuity routes answer 404 — MyDevEnv2 does not
    /// depend on the sidecar for anything it owns.
    pub contextkeeper: Option<Arc<ContextKeeperRuntime>>,
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
        AgentTaskRegistry::new(&cfg.state_dir, Arc::clone(&sessions), Arc::clone(&push))
            .expect("agent task registry init"),
    );

    let contextkeeper = ContextKeeperRuntime::from_config(&cfg);
    if let Some(runtime) = contextkeeper.as_ref() {
        tracing::info!("contextkeeper continuity enabled");
        runtime.spawn_refresher();
    }

    let assistant = AssistantRuntime::from_config(&cfg, Arc::clone(&sessions));
    if assistant.is_some() {
        tracing::info!(model = %cfg.assistant_model, "assistant enabled");
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
            tracing::info!("no vogt-core configured; /api/vogt, /mcp and /ui-legacy will refuse")
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
        contextkeeper,
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
            post(assistant_api::resolve_action),
        )
        .route("/api/assistant/history", get(assistant_api::history))
        .route("/api/assistant/reset", post(assistant_api::reset))
        .route("/api/events", get(api::events_stream))
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
        .route("/api/contextkeeper/health", get(contextkeeper_api::health))
        .route(
            "/api/contextkeeper/terminals/{id}",
            get(contextkeeper_api::session_continuity),
        )
        .route(
            "/api/contextkeeper/sessions/{session_id}",
            get(contextkeeper_api::session),
        )
        .route(
            "/api/contextkeeper/sessions/{session_id}/continuation",
            get(contextkeeper_api::continuation),
        )
        .route(
            "/api/contextkeeper/sessions/{session_id}/preview",
            get(contextkeeper_api::preview),
        )
        .route(
            "/api/contextkeeper/sessions/{session_id}/approve",
            post(contextkeeper_api::approve),
        )
        .route(
            "/api/contextkeeper/sessions/{session_id}/launch",
            post(contextkeeper_api::launch),
        )
        .route(
            "/api/contextkeeper/work/{work_id}",
            get(contextkeeper_api::work_session),
        )
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

    // MCP and the legacy GUI are deliberately outside that gate.
    //
    // `/mcp` carries a *core* token minted by `vogt token issue` and bound to
    // an actor; the core validates it, and re-checking it against the
    // engine's unrelated token list would refuse every legitimate agent.
    // `/ui-legacy` is static files, which need no token at the core either —
    // there has to be a page on which to enter one.
    // The probes come first because the catch-all below would otherwise
    // answer them with index.html at 200 — which is what it did (#24, FR-A7).
    // Built from `PROBE_PATHS` so the router and the requirement cannot drift.
    let mut vogt_open_routes = Router::new();
    for path in vogt_core::PROBE_PATHS {
        vogt_open_routes = vogt_open_routes.route(path, get(vogt_core::probe));
    }
    let vogt_open_routes = vogt_open_routes
        .route("/mcp", any(vogt_core::mcp))
        .route("/mcp/", any(vogt_core::mcp))
        .route("/mcp/{*path}", any(vogt_core::mcp))
        .route("/ui-legacy", get(vogt_core::legacy_gui_root))
        .route("/ui-legacy/", get(vogt_core::legacy_gui))
        .route("/ui-legacy/{*path}", get(vogt_core::legacy_gui));

    // WS handles its own auth so query-param tokens work (browsers can't set
    // Authorization on a WebSocket handshake).
    let ws_routes = Router::new().route("/api/sessions/{id}/attach", get(ws::attach));

    // Embedded PWA. The catch-all comes last so /healthz and /api/* keep
    // priority. SPA-style fallback to index.html for unknown paths is in
    // `assets::not_found`.
    let asset_routes = Router::new()
        .route("/", get(assets::root))
        .route("/{*path}", get(assets::asset_wild));

    let cors = build_cors(&state.config.allowed_origins);

    let router = Router::new()
        .merge(public)
        .merge(api_routes)
        .merge(vogt_open_routes)
        .merge(ws_routes)
        .merge(asset_routes)
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(Arc::clone(&state));

    (router, state)
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
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(addr = %bind, "mydevenv2-server listening");
    axum::serve(listener, router).await
}
