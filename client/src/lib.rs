//! MyDevEnv2 native desktop client — core library.
//!
//! GUI-free modules (protocol, terminal, client, ws, config, bridge) live here
//! and are unit-tested without compiling GPUI. The GPUI application lives in the
//! binary (`main.rs` + `ui/`) behind the default `gui` feature.

pub mod bridge;
pub mod client;
pub mod config;
pub mod protocol;
pub mod terminal;
pub mod ws;
