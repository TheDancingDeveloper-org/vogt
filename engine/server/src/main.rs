use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use vogt_engine_server::{config, serve_forever};

#[derive(Debug, Parser)]
#[command(
    name = "vogt-engine",
    version,
    about = "Vogt engine — the front-door server"
)]
struct Cli {
    /// Path to a TOML config file. Optional; env vars and CLI flags override.
    /// Env: `ENGINE_CONFIG` (legacy `MYDEVENV2_CONFIG` still accepted).
    #[arg(short, long)]
    config: Option<std::path::PathBuf>,

    /// Bind address (host:port). Overrides config.
    /// Env: `ENGINE_BIND` (legacy `MYDEVENV2_BIND` still accepted).
    #[arg(long)]
    bind: Option<String>,

    /// Bearer token required for all API/WS calls. Overrides config.
    /// Env: `ENGINE_TOKEN` (legacy `MYDEVENV2_TOKEN` still accepted).
    /// SECURITY: prefer passing via env so it doesn't appear in process listings.
    #[arg(long)]
    token: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();
    // The three settings the CLI parser owns resolve their env forms through
    // config's `ENGINE_`-aware helper, so `ENGINE_CONFIG`/`ENGINE_BIND`/
    // `ENGINE_TOKEN` are the primary names and the legacy `MYDEVENV2_*` names
    // still work with a deprecation warning (#203). A `--config` flag wins over
    // the environment; the bind and token env forms are resolved inside `load`.
    let config_path = cli.config.or_else(config::config_path_from_env);
    let cfg = match config::load(config_path.as_deref(), cli.bind, cli.token) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::from(2);
        }
    };

    if let Err(e) = serve_forever(cfg).await {
        tracing::error!(error = %e, "server exited with error");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
