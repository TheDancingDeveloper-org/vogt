//! HTTP adapter. Ports `src/vogt/adapters/http/`.
//!
//! `health` serves the probes; `app` serves the registry routes under `/api`.

pub mod access_log;
pub mod app;
pub mod etag;
pub mod health;
pub mod mcp;
pub mod openapi;

#[cfg(test)]
pub(crate) mod test_harness;
