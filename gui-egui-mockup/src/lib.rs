//! A static mockup of the whole Vogt GUI on egui and fluent2-egui (WI-2162).
//!
//! Every screen draws fake data from [`fixtures`]; nothing talks to a server. The app shell is
//! in [`app`], one module per screen in [`screens`].

pub mod app;
pub mod fixtures;
pub mod screens;

pub use app::{Look, MockupApp, Screen};
