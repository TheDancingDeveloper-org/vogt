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

pub const SOURCE_SHA: &str = match option_env!("VOGT_SOURCE_SHA") {
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
