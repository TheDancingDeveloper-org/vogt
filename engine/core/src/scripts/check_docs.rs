//! `scripts/check_docs.py` and `scripts/check_engine_spec.py` in Rust.
//!
//! Two checks, because the docs workflow runs them as one step:
//!
//! - every relative link in a markdown file resolves to a file, and a `#`
//!   fragment names a heading in a markdown target;
//! - every `/api/sessions…` route literal in `engine/server/src/app.rs` is a
//!   path in `docs/engine-openapi.yaml`, and every path in that spec is a
//!   route the engine registers.
//!
//! External URLs are not fetched. A file inside a nested checkout (a worktree
//! or a clone, recognised by its own `.git`) is skipped.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".venv",
    "node_modules",
    "dist",
    "build",
    ".mypy_cache",
    "local",
];

const EXTERNAL: &[&str] = &["http://", "https://", "mailto:", "tel:"];

/// Run both checks against the checkout at `root`.
pub fn run(root: &Path) -> Result<(), String> {
    let mut problems = broken_links(root)?;
    problems.extend(engine_spec_problems(root)?);
    if problems.is_empty() {
        println!("docs checks passed");
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

fn broken_links(root: &Path) -> Result<Vec<String>, String> {
    let files = markdown_files(root)?;
    let mut cache: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    let mut problems = Vec::new();
    let link = regex::Regex::new(r#"\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)"#)
        .expect("link pattern compiles");
    for path in &files {
        let text =
            std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
        for found in link.captures_iter(&text) {
            let target = &found[1];
            if target.starts_with('/') {
                problems.push(format!("{target} (an absolute path is not a link)"));
                continue;
            }
            if EXTERNAL.iter().any(|prefix| target.starts_with(prefix)) {
                continue;
            }
            let (relative, fragment) = target
                .split_once('#')
                .map(|(rel, frag)| (rel, Some(frag)))
                .unwrap_or((target, None));
            let resolved = if relative.is_empty() {
                path.clone()
            } else {
                let candidate = path.parent().unwrap_or(root).join(relative);
                if !candidate.exists() {
                    problems.push(format!("{} -> {target}", display_relative(root, path)));
                    continue;
                }
                candidate
            };
            if let Some(fragment) = fragment {
                if resolved.extension().is_some_and(|ext| ext == "md") {
                    let anchors = cache
                        .entry(resolved.clone())
                        .or_insert_with(|| anchors_of(&resolved));
                    if !anchors.contains(fragment) {
                        problems.push(format!(
                            "{} -> {target} (no heading anchor #{fragment})",
                            display_relative(root, path)
                        ));
                    }
                }
            }
        }
    }
    if problems.is_empty() {
        println!(
            "checked {} markdown files; all relative links resolve",
            files.len()
        );
    }
    Ok(problems)
}

fn markdown_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    walk(root, root, &mut found)?;
    found.sort();
    Ok(found)
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("{}: {err}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            // A nested checkout's documents belong to that checkout.
            if path.join(".git").exists() && path != root {
                continue;
            }
            walk(root, &path, found)?;
        } else if name.ends_with(".md") {
            found.push(path);
        }
    }
    Ok(())
}

/// GitHub's heading slug, with `-1`, `-2` for repeats.
fn anchors_of(path: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    let heading = regex::Regex::new(r"(?m)^(#{1,6})\s+(.*?)\s*#*\s*$").expect("heading compiles");
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut found = BTreeSet::new();
    for capture in heading.captures_iter(&text) {
        let base = slugify(&capture[2]);
        if base.is_empty() {
            continue;
        }
        let count = seen.get(&base).copied().unwrap_or(0);
        let slug = if count == 0 {
            base.clone()
        } else {
            format!("{base}-{count}")
        };
        seen.insert(base, count + 1);
        found.insert(slug);
    }
    found
}

fn slugify(heading: &str) -> String {
    let mut out = String::new();
    let mut spaced = false;
    for ch in heading.trim().to_lowercase().chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            spaced = false;
        } else if ch.is_whitespace() && !spaced && !out.is_empty() {
            out.push('-');
            spaced = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|rel| rel.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// `scripts/check_engine_spec.py`. A partial checkout, missing either file,
/// skips the check.
fn engine_spec_problems(root: &Path) -> Result<Vec<String>, String> {
    let app = root.join("engine/server/src/app.rs");
    let spec = root.join("docs/engine-openapi.yaml");
    if !app.exists() || !spec.exists() {
        println!("engine spec check skipped: app.rs or the spec is absent");
        return Ok(Vec::new());
    }
    let app_rs = std::fs::read_to_string(&app).map_err(|err| format!("app.rs: {err}"))?;
    let spec_text = std::fs::read_to_string(&spec).map_err(|err| format!("spec: {err}"))?;
    let routes = engine_routes(&app_rs);
    let paths = spec_paths(&spec_text);
    let mut problems = Vec::new();
    for route in routes
        .iter()
        .filter(|route| route.starts_with("/api/sessions"))
    {
        if !paths.contains(route) {
            problems.push(format!(
                "{route} is an engine route with no path in the spec"
            ));
        }
    }
    for path in paths.difference(&routes) {
        problems.push(format!("{path} is in the spec but is not an engine route"));
    }
    if problems.is_empty() {
        println!("engine OpenAPI spec covers every session route");
    }
    Ok(problems)
}

fn engine_routes(app_rs: &str) -> BTreeSet<String> {
    let route = regex::Regex::new(r#"\.route\(\s*"(/[^"]*)""#).expect("route compiles");
    route
        .captures_iter(app_rs)
        .map(|found| found[1].to_string())
        .collect()
}

fn spec_paths(spec: &str) -> BTreeSet<String> {
    let Some((_, rest)) = spec.split_once("\npaths:\n") else {
        return BTreeSet::new();
    };
    // The block ends at the next top-level key: a line that starts with a
    // letter. `regex` has no lookahead, so the cut is a scan.
    let block = rest
        .lines()
        .take_while(|line| line.is_empty() || !line.starts_with(|c: char| c.is_ascii_alphabetic()))
        .collect::<Vec<_>>()
        .join("\n");
    let path = regex::Regex::new(r"(?m)^  (/[^:\s]*):\s*$").expect("path compiles");
    path.captures_iter(&block)
        .map(|found| found[1].to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_drops_punctuation_and_lowercases() {
        assert_eq!(slugify("7.4 Upgrade"), "74-upgrade");
        assert_eq!(slugify("Hello, World!"), "hello-world");
    }

    #[test]
    fn repeated_headings_are_disambiguated() {
        let dir = std::env::temp_dir().join(format!("vogt-anchors-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.md");
        std::fs::write(&path, "# Notes\n\n## Notes\n").unwrap();
        let anchors = anchors_of(&path);
        assert!(anchors.contains("notes"), "{anchors:?}");
        assert!(anchors.contains("notes-1"), "{anchors:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn spec_paths_stop_at_the_next_top_level_key() {
        let spec = "openapi: 3.0.0\npaths:\n  /api/sessions:\n    get: {}\n  /gone:\ncomponents:\n  schemas: {}\n";
        let paths = spec_paths(spec);
        assert!(paths.contains("/api/sessions"));
        assert!(paths.contains("/gone"));
        assert!(!paths.contains("/schemas"));
    }

    #[test]
    fn a_session_route_missing_from_the_spec_is_reported() {
        let routes = engine_routes(".route(\"/api/sessions/{id}/new\", get(handler))\n");
        assert!(routes.contains("/api/sessions/{id}/new"));
    }
}
