//! Product scripts ported from `scripts/*.py` (WI-1079).

mod capture;
mod check_docs;
mod fake_agent;
mod fixture_reset;
mod gen_config_docs;
mod load;
mod product_version;
mod sanitise;

pub use capture::run as capture_transcript;
pub use check_docs::run as check_docs;
pub use fake_agent::run as fake_agent;
pub use fixture_reset::run as fixture_reset;
pub use gen_config_docs::run as gen_config_docs;
pub use load::run as load;
pub use product_version::run as check_product_version;
pub use sanitise::run as sanitise_transcript;
