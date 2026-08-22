//! Ensure the embedded PWA directory exists before `rust-embed` reads it.
//!
//! `src/assets.rs` embeds `../../web/dist/` at compile time. That directory is
//! produced by the web build (`cd web && pnpm build`) and is gitignored, so a
//! clean clone does not have it — and a missing folder makes the `RustEmbed`
//! derive fail to expand with an error that names `WebAssets`, not the missing
//! build step. To keep `cargo build` working from a clean clone with no prior
//! PWA build, write a minimal placeholder `index.html` into `web/dist/` when
//! the real bundle is absent.
//!
//! The Docker image build still runs the real `pnpm build` before `cargo
//! build`, so the shipped binary embeds the actual PWA; only a bare local
//! `cargo build` falls back to this placeholder. Deleting `web/dist/` and
//! rebuilding regenerates it.

use std::fs;
use std::path::PathBuf;

fn main() {
    // Resolve `../../web/dist/` the same way `rust-embed` does: relative to the
    // crate's manifest directory (`engine/server`), which lands at the
    // repository root's `web/dist`.
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dist = manifest_dir.join("../../web/dist");
    let index = dist.join("index.html");

    // Only act when the real bundle is absent: a present `index.html` means the
    // web build already ran and must not be clobbered by a placeholder.
    if !index.exists() {
        if let Err(e) = fs::create_dir_all(&dist) {
            println!(
                "cargo:warning=could not create placeholder web/dist ({}): {e}",
                dist.display()
            );
            return;
        }
        let placeholder = "<!doctype html>\n<meta charset=\"utf-8\">\n<title>Vogt engine</title>\n<p>Placeholder PWA bundle. Build the real one with <code>cd web &amp;&amp; pnpm build</code>.</p>\n";
        if let Err(e) = fs::write(&index, placeholder) {
            println!(
                "cargo:warning=could not write placeholder {} ({e})",
                index.display()
            );
        }
    }

    // rust-embed reads the directory at compile time; rerun if it changes so a
    // later real `pnpm build` is picked up.
    println!("cargo:rerun-if-changed=../../web/dist");
}
