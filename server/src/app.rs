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
    api, assets, auth, config::Config, events::EventBus, files, git, gui::GuiRegistry,
    history::SessionHistory, push::PushManager, sessions::SessionRegistry, ws,
};

pub struct AppState {
    pub config: Arc<Config>,
    pub sessions: Arc<SessionRegistry>,
    pub bus: EventBus,
    pub gui: Arc<GuiRegistry>,
    pub push: Arc<PushManager>,
    pub history: Option<Arc<SessionHistory>>,
}

pub async fn router(cfg: Config) -> (Router, Arc<AppState>) {
    let cfg = Arc::new(cfg);
    let bus = EventBus::default();
    let sessions = Arc::new(SessionRegistry::new(Arc::clone(&cfg), bus.clone()));
    let gui = Arc::new(GuiRegistry::new());
    let push = Arc::new(
        PushManager::with_subject(
            &cfg.state_dir,
            cfg.fcm_service_account_json.as_deref(),
            &cfg.vapid_subject,
        )
        .expect("push manager init"),
    );

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

    let state = Arc::new(AppState {
        config: cfg,
        sessions,
        bus,
        gui,
        push,
        history,
    });

    // Background task: fan out a push notification whenever a session enters
    // `waiting-for-input`. Subscribes to the events bus.
    push_api::spawn_activity_watcher(Arc::clone(&state));

    // Public: /healthz, /api/config, /api/push/public-key. None reveal secrets.
    let public = Router::new()
        .route("/healthz", get(api::healthz))
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
        .route("/api/files", get(files::read_file).put(files::write_file))
        .route("/api/files/download", get(files::download_file))
        .route("/api/dir", get(files::list_dir))
        .route("/api/tree", get(files::tree))
        .route("/api/search", get(files::search))
        .route("/api/git/status", get(git::status))
        .route("/api/git/diff", get(git::diff))
        .route("/api/git/log", get(git::log))
        .route("/api/git/branch", get(git::branch))
        .route("/api/gui/launch", post(gui_handlers::launch))
        .route("/api/gui/processes", get(gui_handlers::processes))
        .route("/api/gui/kill", post(gui_handlers::kill_proc))
        .route("/api/push/subscribe", post(push_api::subscribe))
        .route("/api/push/unsubscribe", post(push_api::unsubscribe))
        .route("/api/push/list", get(push_api::list))
        .route("/api/push/test", post(push_api::test_dispatch))
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
