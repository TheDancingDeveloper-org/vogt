//! HTTP adapter. Ports `src/vogt/adapters/http/`.
//!
//! `health` serves the probes; `app` serves the registry routes under `/api`.

pub mod app;
pub mod health;
