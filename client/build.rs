//! Embed a Windows application manifest declaring the Common Controls v6
//! dependency.
//!
//! GPUI's Windows backend imports `TaskDialogIndirect`, which exists only in
//! comctl32 v6. Without a manifest that activates `Microsoft.Windows.Common-
//! Controls 6.0.0.0`, Windows binds the legacy v5.82 comctl32 and the app fails
//! at load with "the procedure entry point TaskDialogIndirect could not be
//! located". GPUI only embeds this manifest when built *on* Windows
//! (`#[cfg(target_os = "windows")]` in its build.rs), so a Linux cross-compile
//! ships without it — we embed our own here.
//!
//! `embed-manifest` is pure Rust (writes the COFF resource directly), so it
//! works when cross-compiling from Linux with mingw-w64. Its default manifest
//! includes the Common Controls v6 dependency, which is exactly what we need.

fn main() {
    // build.rs runs on the host; gate on the *target* OS so this only fires for
    // Windows builds (native or cross).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        use embed_manifest::{embed_manifest, new_manifest};
        embed_manifest(new_manifest("MyDevEnv2.Client")).expect("failed to embed Windows manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
