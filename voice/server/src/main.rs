use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use vogt_voice_server::{router, Config};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    let config = Config::parse();
    let bind = config.bind;
    let app = router(vogt_voice_server::AppState::from_config(config));
    let listener = match tokio::net::TcpListener::bind(bind).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, %bind, "could not bind voice sidecar");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(%bind, "vogt voice sidecar listening");
    if let Err(error) = axum::serve(listener, app).await {
        tracing::error!(%error, "voice sidecar exited");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
