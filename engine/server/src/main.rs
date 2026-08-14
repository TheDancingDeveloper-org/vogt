use std::process::ExitCode;

use clap::Parser;
use mydevenv2_server::{config, serve_forever};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "mydevenv2-server", version, about = "MyDevEnv2 server")]
struct Cli {
    /// Path to a TOML config file. Optional; env vars and CLI flags override.
    #[arg(short, long, env = "MYDEVENV2_CONFIG")]
    config: Option<std::path::PathBuf>,

    /// Bind address (host:port). Overrides config.
    #[arg(long, env = "MYDEVENV2_BIND")]
    bind: Option<String>,

    /// Bearer token required for all API/WS calls. Overrides config.
    /// SECURITY: prefer passing via env so it doesn't appear in process listings.
    #[arg(long, env = "MYDEVENV2_TOKEN", hide_env_values = true)]
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
    let cfg = match config::load(cli.config.as_deref(), cli.bind, cli.token) {
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
