use std::process::ExitCode;
use vogt_session_agent::{run, Config};

/// `vogt-session-agent X.Y.Z (rustc ...)`: what `--version` prints.
pub fn version_line() -> String {
    format!(
        "vogt-session-agent {} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("VOGT_RUSTC_VERSION")
    )
}

#[tokio::main]
async fn main() -> ExitCode {
    if std::env::args()
        .nth(1)
        .is_some_and(|a| a == "--version" || a == "-V")
    {
        println!("{}", version_line());
        return ExitCode::SUCCESS;
    }
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

#[cfg(test)]
mod tests {
    #[test]
    fn version_line_names_the_compiler() {
        assert!(super::version_line().contains("rustc"));
    }
}
