//! MyDevEnv2 native desktop client entry point.
//!
//! On Windows we use the `windows` subsystem so launching the app doesn't pop a
//! console window. This applies to all Windows builds (not just release):
//! distribution uses the `win-dist` profile, which keeps `debug-assertions` on
//! to dodge gpui's fxc-only release shader precompile when cross-compiling from
//! Linux — so a `not(debug_assertions)` gate would wrongly leave it a console
//! app. Build with `--features console` to keep stdio for debugging.

#![cfg_attr(
    all(target_os = "windows", not(feature = "console")),
    windows_subsystem = "windows"
)]

use mydevenv2_client::{bridge, config::ClientConfig};

#[cfg(feature = "gui")]
mod ui;

fn main() {
    init_logging();
    bridge::init();
    let cfg = ClientConfig::load();
    log::info!("server = {}", cfg.base());

    #[cfg(feature = "gui")]
    {
        ui::run(cfg);
    }

    #[cfg(not(feature = "gui"))]
    {
        headless_smoke(cfg);
    }
}

fn init_logging() {
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::Builder::from_env(env)
        .format_timestamp_millis()
        .init();
}

/// Built only without the `gui` feature: a tiny reachability probe so the core
/// networking layer can be exercised end-to-end without GPUI.
#[cfg(not(feature = "gui"))]
fn headless_smoke(cfg: ClientConfig) {
    use mydevenv2_client::client::ApiClient;

    bridge::handle().block_on(async move {
        let api = ApiClient::new(cfg.base(), cfg.token.clone());
        match api.healthz().await {
            Ok(()) => println!("healthz: ok ({})", cfg.base()),
            Err(e) => {
                eprintln!("healthz failed: {e:#}");
                return;
            }
        }
        if !cfg.is_configured() {
            println!("no token configured; skipping authed calls");
            return;
        }
        match api.list_sessions().await {
            Ok(sessions) => {
                println!("{} session(s):", sessions.len());
                for s in sessions {
                    println!("  {} {:?} {}", s.id, s.activity, s.name);
                }
            }
            Err(e) => eprintln!("list_sessions failed: {e:#}"),
        }
    });
}
