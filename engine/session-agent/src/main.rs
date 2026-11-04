use std::process::ExitCode;
use vogt_session_agent::{run, Config};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    match match Config::from_env() {
        Ok(config) => run(config).await,
        Err(error) => Err(error),
    }
    .map(|_| ())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("session agent failed: {error}");
            ExitCode::FAILURE
        }
    }
}
