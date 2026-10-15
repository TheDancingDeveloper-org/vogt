//! `scripts/check_product_version.py` in Rust.
//!
//! Every user-facing manifest must carry one version. The optional first
//! argument is the version the caller expects `pyproject.toml` to hold.

use std::path::Path;

/// Check the checkout at `root`. `expected` is the version the caller claims.
pub fn run(root: &Path, expected: Option<&str>) -> Result<(), String> {
    let project = std::fs::read_to_string(root.join("pyproject.toml"))
        .map_err(|err| format!("pyproject.toml: {err}"))?;
    let version = toml_version(&project)
        .ok_or_else(|| "pyproject.toml has no project.version".to_string())?;
    if let Some(expected) = expected {
        if version != expected {
            return Err(format!(
                "pyproject version {version} does not match expected {expected}"
            ));
        }
    }
    let init = std::fs::read_to_string(root.join("src/vogt/__init__.py"))
        .map_err(|err| format!("src/vogt/__init__.py: {err}"))?;
    if !init.contains(&format!("__version__ = \"{version}\"")) {
        return Err("Python package version disagrees with pyproject.toml".to_string());
    }
    for relative in ["web/package.json", "mobile/package.json"] {
        let text = std::fs::read_to_string(root.join(relative))
            .map_err(|err| format!("{relative}: {err}"))?;
        if json_version(&text) != Some(version) {
            return Err(format!(
                "{relative} disagrees with product version {version}"
            ));
        }
    }
    let build = std::fs::read_to_string(root.join(".github/workflows/build.yml"))
        .map_err(|err| format!("build.yml: {err}"))?;
    if !build.contains(&format!("VOGT_PRODUCT_VERSION={version}")) {
        return Err("dev build does not inject the canonical product version".to_string());
    }
    let release = std::fs::read_to_string(root.join(".github/workflows/release.yml"))
        .map_err(|err| format!("release.yml: {err}"))?;
    if release.contains("docker/build-push-action") || release.contains("VOGT_PRODUCT_VERSION=") {
        return Err("tagged release must promote build.yml's images, not rebuild them".to_string());
    }
    if !release.contains("docker buildx imagetools create") {
        return Err("tagged release does not promote build.yml's images by digest".to_string());
    }
    let product = std::fs::read_to_string(root.join("engine/server/src/product.rs"))
        .map_err(|err| format!("product.rs: {err}"))?;
    if !product.contains("\"local/dev\"") {
        return Err("engine local/dev fallback is missing".to_string());
    }
    let core = std::fs::read_to_string(root.join("engine/core/src/main.rs"))
        .map_err(|err| format!("main.rs: {err}"))?;
    if !core.contains(&format!("_ => \"{version}\"")) {
        return Err("vogt-core VERSION fallback disagrees with pyproject.toml".to_string());
    }
    Ok(())
}

/// The value of `version = "…"` under `[project]`. A line scan, not a TOML
/// parser: the field is one quoted string on its own line.
fn toml_version(text: &str) -> Option<&str> {
    let mut in_project = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_project = trimmed == "[project]";
            continue;
        }
        if in_project {
            if let Some(rest) = trimmed.strip_prefix("version") {
                let rest = rest.trim().strip_prefix('=')?.trim();
                return rest.strip_prefix('"')?.strip_suffix('"');
            }
        }
    }
    None
}

/// The top-level `"version"` string of a package.json. Good enough for the
/// two manifests, which put it on its own line.
fn json_version(text: &str) -> Option<&str> {
    for line in text.lines() {
        let trimmed = line.trim().trim_end_matches(',');
        if let Some(rest) = trimmed.strip_prefix("\"version\"") {
            let rest = rest.trim().strip_prefix(':')?.trim();
            return rest.strip_prefix('"')?.strip_suffix('"');
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_version_under_project() {
        let text =
            "[build-system]\nversion = \"no\"\n\n[project]\nname = \"vogt\"\nversion = \"1.2.3\"\n";
        assert_eq!(toml_version(text), Some("1.2.3"));
    }

    #[test]
    fn reads_a_package_json_version() {
        assert_eq!(
            json_version("{\n  \"name\": \"web\",\n  \"version\": \"1.2.3\",\n}\n"),
            Some("1.2.3")
        );
    }

    #[test]
    fn this_checkout_agrees() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        run(&root, Some("0.7.9")).expect("this checkout's version is consistent");
    }
}
