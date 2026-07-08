use std::sync::Arc;

use axum::{
    http::{header, HeaderValue, Method},
    middleware,
    routing::{get, post},
    Router,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

use crate::gui as gui_handlers;
use crate::push_api;
use crate::{
    agent_tasks::{self, AgentTaskRegistry},
    api, assets, auth,
    config::Config,
    events::EventBus,
    files, git,
    gui::GuiRegistry,
    history::SessionHistory,
    history_api,
    push::PushManager,
    sessions::SessionRegistry,
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

    let state = Arc::new(AppState {
        config: cfg,
        auth: Arc::new(auth::AuthRuntime::default()),
        sessions,
        bus,
        gui,
        push,
        agent_tasks,
        history,
    });

    // Background task: fan out a push notification whenever a session enters
    // `waiting-for-input`. Subscribes to the events bus.
    push_api::spawn_activity_watcher(Arc::clone(&state));
    push_api::spawn_digest_flusher(Arc::clone(&state));
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
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::require_bearer,
        ));

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
