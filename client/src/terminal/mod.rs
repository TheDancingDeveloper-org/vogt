//! Terminal subsystem: VT100 grid/parser, key mapping, and rasterizer.
//!
//! Everything here is GPUI-free and unit-tested. The GPUI element that wires a
//! [`grid::TermProcessor`] to an attach session lives in `crate::ui::terminal_view`.

pub mod grid;
pub mod keymap;
pub mod renderer;

pub use grid::{TermProcessor, DEFAULT_COLS, DEFAULT_ROWS};
pub use keymap::{key_to_bytes, KeyInput};
pub use renderer::{TermFrame, TermRenderer};
