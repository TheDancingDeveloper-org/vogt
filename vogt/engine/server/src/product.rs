//! Product identity shared by the engine front door and the embedded PWA.
//!
//! The Rust crates retain their own package versions.  These values identify
//! the merged Vogt product and are injected by the image/release build.  A
//! locally compiled engine remains honest by identifying itself as local/dev.

pub const VERSION: &str = match option_env!("VOGT_PRODUCT_VERSION") {
    Some(value) if !value.is_empty() => value,
    _ => "local/dev",
};

pub const SOURCE_REF: &str = match option_env!("VOGT_SOURCE_REF") {
    Some(value) if !value.is_empty() => value,
    _ => "local/dev",
};

/// The revision the build came from, as the build names it. On a graft line
/// (rust-2) this is the changeset id, not a git commit.
pub const SOURCE_SHA: &str = match option_env!("VOGT_SOURCE_SHA") {
    Some(value) if !value.is_empty() => value,
    _ => "local/dev",
};

/// The git commit the image was built from (`github.sha` in CI). Kept apart from
/// `SOURCE_SHA` so a graft changeset id is never reported as a git commit.
pub const GIT_SHA: &str = match option_env!("VOGT_GIT_SHA") {
    Some(value) if !value.is_empty() => value,
    _ => "local/dev",
};

pub fn release_url() -> Option<String> {
    if SOURCE_REF.starts_with('v') && VERSION != "local/dev" {
        Some(format!(
            "https://github.com/TheDancingDeveloper-org/vogt/releases/tag/{SOURCE_REF}"
        ))
    } else {
        None
    }
}

/// The compiler that built this binary (`rustc -V`), captured by `build.rs`.
pub const RUSTC_VERSION: &str = env!("VOGT_RUSTC_VERSION");

#[cfg(test)]
mod rustc_tests {
    #[test]
    fn the_build_compiler_is_reported() {
        assert!(super::RUSTC_VERSION.contains("rustc"));
    }
}
