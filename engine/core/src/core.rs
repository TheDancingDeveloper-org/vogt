//! Domain types. Ports `src/vogt/core/`.
//!
//! Split into cohesive modules. This file re-exports them so existing
//! `crate::core::` paths are unchanged. Nothing here reads storage or
//! configuration.

#![allow(dead_code)]
pub mod session_economics;
pub mod usage_board;

mod clock;
mod entities;
mod ids;
mod principal;
mod session_report;
mod workflow;

pub use clock::*;
pub use entities::*;
pub use ids::*;
pub use principal::*;
pub use session_report::*;
pub use workflow::*;
