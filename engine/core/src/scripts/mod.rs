//! Product scripts ported from `scripts/*.py` (WI-1079).
//!
//! These are developer checks, not product operations: they read the checkout
//! and exit non-zero on drift. They are wired as `vogt-core` subcommands so a
//! checkout with no Python can run them. The Python originals stay, because
//! the `python` half of `scripts/check.sh` and the docs workflow still call
//! them; the Rust copies are what a core-only tree runs.

mod capture;
mod check_docs;
mod gen_config_docs;
mod product_version;
mod sanitise;

pub use capture::run as capture_transcript;
pub use check_docs::run as check_docs;
pub use gen_config_docs::run as gen_config_docs;
pub use product_version::run as check_product_version;
pub use sanitise::run as sanitise_transcript;
