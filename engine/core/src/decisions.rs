//! Decision-bearing core. Ports `ranking.py`, `digest.py`, `checks.py` and the
//! pure half of `observed.py`.
//!
//! Split into cohesive modules. This file re-exports them so existing
//! `crate::decisions::` paths are unchanged.

mod activity;
mod ci;
mod json;
mod oversight;
mod projection;
mod ranking;
mod runtime;

pub use activity::*;
pub use ci::*;
pub use json::*;
pub use oversight::*;
pub use projection::*;
pub use ranking::*;
pub use runtime::*;
