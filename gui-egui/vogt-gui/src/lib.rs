//! Vogt GUI proof of concept on egui and fluent2-egui (WI-1806).
//!
//! Layers, from the wire up: [`api`] (types), [`sse`] and [`backoff`] (stream
//! plumbing), [`backend`] (the [`backend::Backend`] trait with an HTTP and a
//! fixture implementation), [`model`] (state and rules, no egui) and [`app`]
//! (the egui shell).

pub mod api;
pub mod app;
pub mod backend;
pub mod backoff;
pub mod model;
pub mod sse;

#[cfg(target_arch = "wasm32")]
mod web;

pub use app::VogtApp;
