use std::sync::Arc;

use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

use crate::gui as gui_handlers;
use crate::{
    api, assets, auth, config::Config, events::EventBus, files, git, gui::GuiRegistry,
    sessions::SessionRegistry, ws,
};

pub struct AppState {
    pub config: Arc<Config>,
    pub sessions: Arc<SessionRegistry>,
    pub bus: EventBus,
    pub gui: Arc<GuiRegistry>,
}

pub fn router(cfg: Config) -> (Router, Arc<AppState>) {
    let cfg = Arc::new(cfg);
    let bus = EventBus::default();
    let sessions = Arc::new(SessionRegistry::new(Arc::clone(&cfg), bus.clone()));
    let gui = Arc::new(GuiRegistry::new());
    let state = Arc::new(AppState {
        config: cfg,
        sessions,
        bus,
        gui,
    });

    // Public: /healthz and /api/config (returns no secrets, used at boot).
    let public = Router::new()
        .route("/healthz", get(api::healthz))
        .route("/api/config", get(gui_handlers::public_config));

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

    let router = Router::new()
        .merge(public)
        .merge(api_routes)
        .merge(ws_routes)
        .merge(asset_routes)
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::very_permissive())
        .with_state(Arc::clone(&state));

    (router, state)
}

pub async fn serve_forever(cfg: Config) -> std::io::Result<()> {
    let bind = cfg.bind;
    let (router, _state) = router(cfg);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(addr = %bind, "mydevenv2-server listening");
    axum::serve(listener, router).await
}
