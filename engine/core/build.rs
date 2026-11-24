use std::process::Command;

/// Capture the compiler that builds this crate, so a running binary can say it.
fn emit_rustc_version() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = Command::new(rustc)
        .arg("-V")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "rustc unknown".to_string());
    println!("cargo:rustc-env=VOGT_RUSTC_VERSION={version}");
    println!("cargo:rerun-if-env-changed=RUSTC");
}

fn main() {
    emit_rustc_version();
}
