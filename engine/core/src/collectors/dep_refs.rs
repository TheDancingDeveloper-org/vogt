//! `dep-refs` — which projects reference which, and nothing more.
//!
//! Ports `src/vogt/collectors/dep_refs.py`.
//!
//! This reads manifests only — `Cargo.toml`, `package.json`, `pyproject.toml`
//! — and extracts only internal-looking references: `path`, `git`, `file:`,
//! `link:`, `workspace:` and direct git URLs. Registry dependencies are
//! ignored entirely.
//!
//! Every path reference is resolved against the manifest that declares it and
//! given a `scope`: `internal` when it lands inside the project's own root and
//! exists (or is a glob), `broken` when it lands inside and there is nothing
//! there, `external` when it lands outside or is a git URL. Containment is the
//! test rather than workspace membership.
//!
//! Every project also gets one scan record, whatever was found, so a project
//! that references nothing, a project whose manifests this collector does not
//! parse, and a project no sweep has covered stay distinguishable.

use std::path::{Component, Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};

use crate::core::{Moment, Project};

use super::base::{finding, walk_project, Collection, Collector, CollectorContext};

const KIND_DEP_REF: &str = "dep_ref";
const KIND_DEP_SCAN: &str = "dep_scan";

const MANIFESTS: &[&str] = &["Cargo.toml", "package.json", "pyproject.toml"];

/// Manifests this collector recognises and does not read. Named rather than
/// ignored, so a zero from a format nothing parses says so.
const UNSUPPORTED_MANIFESTS: &[&str] = &[
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "composer.json",
    "mix.exs",
    "requirements.txt",
    "Package.swift",
    "pubspec.yaml",
    "CMakeLists.txt",
];

const UNSUPPORTED_SUFFIXES: &[&str] = &[".csproj", ".fsproj", ".vbproj"];

/// Prefixes that mean "this is not a published package, it is that thing over
/// there". Everything else is a registry dependency and ignored.
const INTERNAL_PREFIXES: &[&str] = &["file:", "link:", "workspace:", "portal:"];

const SCOPE_INTERNAL: &str = "internal";
const SCOPE_EXTERNAL: &str = "external";
const SCOPE_BROKEN: &str = "broken";

/// `walk_project`'s default cap, which is what Python's `walk_project` uses
/// when a collector passes no `max_files` of its own.
const MAX_FILES: usize = 20_000;

fn git_url() -> Regex {
    Regex::new(r"^(https?://|git\+|git@|ssh://)").expect("git url pattern is a valid regex")
}

pub struct DepRefCollector;

impl Collector for DepRefCollector {
    fn name(&self) -> &str {
        "dep-refs"
    }

    fn requires_network(&self) -> bool {
        false
    }

    fn collect(
        &mut self,
        _ctx: &mut CollectorContext<'_>,
        project: &Project,
        _now: &mut dyn FnMut() -> Moment,
    ) -> Collection {
        let root = expanduser(Path::new(&project.root_path));
        let mut read = Vec::new();
        let mut unreadable = Vec::new();
        let mut unsupported = Vec::new();
        let mut references = 0_i64;
        let mut found = Vec::new();

        for path in walk_project(&root, &project.exclusions, &[], MAX_FILES) {
            let relative = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if !MANIFESTS.contains(&name) {
                let suffix = python_suffix(name);
                if UNSUPPORTED_MANIFESTS.contains(&name) || UNSUPPORTED_SUFFIXES.contains(&suffix) {
                    unsupported.push(relative);
                }
                continue;
            }
            let Some(refs) = references_of(&path) else {
                unreadable.push(relative);
                continue;
            };
            read.push(relative.clone());
            for (ref_kind, target) in refs {
                references += 1;
                let scope = scope_of(ref_kind, &target, &path, &root);
                found.push(finding(
                    KIND_DEP_REF,
                    &format!("depref:{}/{relative}->{target}", project.slug),
                    json!({
                        "ref_kind": ref_kind,
                        "raw_target": target,
                        "manifest": relative,
                        "scope": scope,
                    }),
                    Some(project),
                    None,
                    false,
                ));
            }
        }

        unreadable.sort();
        unsupported.sort();
        found.push(finding(
            KIND_DEP_SCAN,
            &format!("depscan:{}", project.slug),
            json!({
                "manifests_read": read.len(),
                "references": references,
                "unreadable_manifests": unreadable,
                "unsupported_manifests": unsupported,
                "root_exists": root.is_dir(),
            }),
            Some(project),
            None,
            false,
        ));
        Collection::ok(found)
    }
}

/// Python's `Path.expanduser`: only a leading `~` or `~user` expands, and only
/// when it is the first component on its own.
fn expanduser(path: &Path) -> PathBuf {
    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return path.to_path_buf();
    };
    let first = first.to_string_lossy();
    if !first.starts_with('~') {
        return path.to_path_buf();
    }
    let home = if first == "~" {
        std::env::var_os("HOME").map(PathBuf::from)
    } else {
        // `~user` is not resolved here: vogt stores roots it created itself, and
        // the Python tests never exercise another user's home.
        None
    };
    match home {
        Some(home) => home.join(components.as_path()),
        None => path.to_path_buf(),
    }
}

/// Python's `Path.suffix`: the final dot and what follows it, empty when the
/// name has no dot or the dot is the first character.
fn python_suffix(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[dot..],
        _ => "",
    }
}

/// Where a reference lands, relative to the project that declares it.
fn scope_of(ref_kind: &str, target: &str, manifest: &Path, root: &Path) -> &'static str {
    if ref_kind != "path" {
        // A git URL names something by address, not by location; an inherited
        // dependency names the workspace root, which is this project.
        return if ref_kind == "git" {
            SCOPE_EXTERNAL
        } else {
            SCOPE_INTERNAL
        };
    }
    let target = stripped_prefix(target);
    let resolved = match resolve_like_python(&manifest.parent().unwrap_or(manifest).join(target)) {
        Some(resolved) => resolved,
        None => return SCOPE_EXTERNAL,
    };
    let root = match resolve_like_python(root) {
        Some(root) => root,
        None => return SCOPE_EXTERNAL,
    };
    if !resolved.starts_with(&root) {
        return SCOPE_EXTERNAL;
    }
    // A glob member (`crates/*`) does not exist as a path and is not broken.
    if target.contains('*') || target.contains('?') {
        return SCOPE_INTERNAL;
    }
    if resolved.exists() {
        SCOPE_INTERNAL
    } else {
        SCOPE_BROKEN
    }
}

/// `file:../thing`, `link:./thing`, `workspace:*` — the prefix is a protocol,
/// not a path segment. An empty remainder means `.`.
fn stripped_prefix(target: &str) -> &str {
    for prefix in INTERNAL_PREFIXES {
        if let Some(remainder) = target.strip_prefix(prefix) {
            return if remainder.is_empty() { "." } else { remainder };
        }
    }
    target
}

/// Python's `Path.resolve()`: the longest existing prefix is canonicalised and
/// the missing tail is appended with `..` collapsed, so a nonexistent tail
/// still loses its `..`.
fn resolve_like_python(path: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        if current.as_os_str().is_empty() {
            return None;
        }
        if current.exists() {
            let canonical = std::fs::canonicalize(&current).ok()?;
            return Some(push_missing(canonical, &missing));
        }
        let name = current.file_name()?;
        missing.push(name.to_os_string());
        if !current.pop() {
            return None;
        }
    }
}

fn push_missing(mut base: PathBuf, missing: &[std::ffi::OsString]) -> PathBuf {
    for name in missing.iter().rev() {
        if name == ".." {
            base.pop();
        } else if name != "." {
            base.push(name);
        }
    }
    base
}

/// Extract internal-looking references from one manifest.
///
/// `None` means the manifest could not be read, which is a different answer
/// from "it declares no internal references" and is reported as such in the
/// scan record. A malformed manifest is a fact about the project, not a
/// collector failure.
fn references_of(path: &Path) -> Option<Vec<(&'static str, String)>> {
    let name = path.file_name().and_then(|name| name.to_str())?;
    if name == "package.json" {
        let text = std::fs::read_to_string(path).ok()?;
        let document: Value = serde_json::from_str(&text).ok()?;
        return Some(from_package_json(&document));
    }
    let text = std::fs::read_to_string(path).ok()?;
    let document: toml::Value = text.parse().ok()?;
    if name == "Cargo.toml" {
        Some(from_cargo(&document))
    } else {
        Some(from_pyproject(&document))
    }
}

fn from_cargo(document: &toml::Value) -> Vec<(&'static str, String)> {
    let mut found = Vec::new();
    for target in array_of(nested(document, &["workspace", "members"])) {
        if let Some(target) = target.as_str() {
            found.push(("path", target.to_string()));
        }
    }
    for table in [
        document.get("dependencies"),
        document.get("dev-dependencies"),
        document.get("build-dependencies"),
        nested(document, &["workspace", "dependencies"]),
        nested(document, &["patch", "crates-io"]),
    ] {
        let Some(table) = table.and_then(toml::Value::as_table) else {
            continue;
        };
        for spec in table.values() {
            found.extend(from_spec(spec));
        }
    }
    found
}

fn from_pyproject(document: &toml::Value) -> Vec<(&'static str, String)> {
    let mut found = Vec::new();
    if let Some(sources) =
        nested(document, &["tool", "uv", "sources"]).and_then(toml::Value::as_table)
    {
        for spec in sources.values() {
            found.extend(from_spec(spec));
        }
    }
    if let Some(poetry) =
        nested(document, &["tool", "poetry", "dependencies"]).and_then(toml::Value::as_table)
    {
        for spec in poetry.values() {
            found.extend(from_spec(spec));
        }
    }
    for requirement in array_of(nested(document, &["project", "dependencies"])) {
        let Some(requirement) = requirement.as_str() else {
            continue;
        };
        let Some((_, target)) = requirement.split_once('@') else {
            continue;
        };
        let target = target.trim();
        if git_url().is_match(target) {
            found.push(("git", target.to_string()));
        } else if starts_with_internal(target) {
            found.push(("path", target.to_string()));
        }
    }
    found
}

fn from_package_json(document: &Value) -> Vec<(&'static str, String)> {
    let mut found = Vec::new();
    for key in ["dependencies", "devDependencies", "peerDependencies"] {
        let Some(table) = document.get(key).and_then(Value::as_object) else {
            continue;
        };
        for spec in table.values() {
            let Some(spec) = spec.as_str() else {
                continue;
            };
            if starts_with_internal(spec) {
                found.push(("path", spec.to_string()));
            } else if git_url().is_match(spec) || spec.starts_with("github:") {
                found.push(("git", spec.to_string()));
            }
        }
    }
    for workspace in array_of_json(document.get("workspaces")) {
        if let Some(workspace) = workspace.as_str() {
            found.push(("path", workspace.to_string()));
        }
    }
    found
}

/// A dependency spec is internal only if it names a path or a git URL.
fn from_spec(spec: &toml::Value) -> Vec<(&'static str, String)> {
    if let Some(spec) = spec.as_str() {
        if starts_with_internal(spec) {
            return vec![("path", spec.to_string())];
        }
        if git_url().is_match(spec) {
            return vec![("git", spec.to_string())];
        }
        return Vec::new();
    }
    let Some(spec) = spec.as_table() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    if let Some(path) = spec.get("path").and_then(toml::Value::as_str) {
        found.push(("path", path.to_string()));
    }
    if let Some(git) = spec.get("git").and_then(toml::Value::as_str) {
        found.push(("git", git.to_string()));
    }
    if spec.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
        // Cargo dependency inheritance from the root `[workspace.dependencies]`
        // table. It is not a path and must never reach path resolution.
        found.push(("inherited", "workspace:.".to_string()));
    }
    found
}

fn starts_with_internal(spec: &str) -> bool {
    INTERNAL_PREFIXES
        .iter()
        .any(|prefix| spec.starts_with(prefix))
}

fn nested<'a>(document: &'a toml::Value, keys: &[&str]) -> Option<&'a toml::Value> {
    let mut current = document;
    for key in keys {
        current = current.as_table()?.get(*key)?;
    }
    Some(current)
}

fn array_of(value: Option<&toml::Value>) -> &[toml::Value] {
    value
        .and_then(toml::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn array_of_json(value: Option<&Value>) -> &[Value] {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::base::Finding;
    use crate::config::VogtConfig;

    /// The exclusions a project records at registration. The worktree test is
    /// the one that needs the agent-scratch entry.
    const DEFAULT_EXCLUSIONS: &[&str] = &[
        ".venv/",
        "node_modules/",
        "target/",
        "dist/",
        "build/",
        ".git/",
        ".claude/",
    ];

    fn project(root: &Path, exclusions: &[&str]) -> Project {
        let mut project = Project::new(
            "prj_fixture",
            "fixture",
            "fixture",
            &root.display().to_string(),
            Moment::from_unix(0, 0),
        );
        project.exclusions = exclusions.iter().map(|entry| entry.to_string()).collect();
        project
    }

    fn collect(root: &Path, exclusions: &[&str]) -> Vec<Finding> {
        let config = VogtConfig::default();
        let mut context = CollectorContext { config: &config };
        let mut now = || Moment::from_unix(0, 0);
        DepRefCollector
            .collect(&mut context, &project(root, exclusions), &mut now)
            .findings
    }

    fn dep_refs(root: &Path, exclusions: &[&str]) -> Vec<Finding> {
        collect(root, exclusions)
            .into_iter()
            .filter(|finding| finding.kind == KIND_DEP_REF)
            .collect()
    }

    fn scan(root: &Path, exclusions: &[&str]) -> Finding {
        let scans: Vec<Finding> = collect(root, exclusions)
            .into_iter()
            .filter(|finding| finding.kind == KIND_DEP_SCAN)
            .collect();
        assert_eq!(
            scans.len(),
            1,
            "exactly one scan record per project, always"
        );
        scans.into_iter().next().unwrap()
    }

    fn temp(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vogt-deps-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    const BARE: &[&str] = &[".venv/", "node_modules/", "target/", ".git/"];

    #[test]
    fn only_internal_looking_references_are_extracted() {
        let root = temp("internal");
        write(
            &root.join("Cargo.toml"),
            "[dependencies]\nserde = \"1.0\"\nnzb-core = { path = \"../nzb-core\" }\nother = { git = \"https://github.com/o/other\" }\n",
        );
        let targets: std::collections::BTreeSet<(String, String)> = dep_refs(&root, BARE)
            .iter()
            .map(|finding| {
                (
                    finding.payload["ref_kind"].as_str().unwrap().to_string(),
                    finding.payload["raw_target"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            targets,
            [
                ("path".to_string(), "../nzb-core".to_string()),
                ("git".to_string(), "https://github.com/o/other".to_string()),
            ]
            .into()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn package_json_workspaces_and_file_refs() {
        let root = temp("pkg");
        write(
            &root.join("package.json"),
            "{\"dependencies\": {\"react\": \"^18\", \"sib\": \"file:../sib\"}, \"workspaces\": [\"packages/*\"]}",
        );
        let targets: std::collections::BTreeSet<String> = dep_refs(&root, BARE)
            .iter()
            .map(|finding| finding.payload["raw_target"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            targets,
            ["file:../sib".to_string(), "packages/*".to_string()].into()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_workspace_member_is_internal_not_an_unregistered_project() {
        let root = temp("member");
        std::fs::create_dir_all(root.join("crates/nzb-core")).unwrap();
        std::fs::create_dir_all(root.join("crates/nzb-web")).unwrap();
        write(
            &root.join("crates/nzb-web/Cargo.toml"),
            "[dependencies]\nnzb-core = { path = \"../nzb-core\" }\n",
        );
        let scopes: Vec<String> = dep_refs(&root, BARE)
            .iter()
            .map(|finding| finding.payload["scope"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(scopes, vec!["internal".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_nested_workspace_reaching_back_is_still_internal() {
        let root = temp("nested");
        std::fs::create_dir_all(root.join("crates/nzb-core")).unwrap();
        std::fs::create_dir_all(root.join("fuzz")).unwrap();
        write(
            &root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"fuzz\"]\n",
        );
        write(
            &root.join("fuzz/Cargo.toml"),
            "[dependencies]\nnzb-core = { path = \"../crates/nzb-core\" }\n",
        );
        let scopes: std::collections::BTreeMap<String, String> = dep_refs(&root, BARE)
            .iter()
            .map(|finding| {
                (
                    finding.payload["raw_target"].as_str().unwrap().to_string(),
                    finding.payload["scope"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            scopes.get("../crates/nzb-core").map(String::as_str),
            Some("internal")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_reference_outside_the_tree_is_external() {
        let root = temp("outside");
        std::fs::create_dir_all(root.join("repo")).unwrap();
        std::fs::create_dir_all(root.join("sibling")).unwrap();
        write(
            &root.join("repo/Cargo.toml"),
            "[dependencies]\nsib = { path = \"../sibling\" }\n",
        );
        let scopes: Vec<String> = dep_refs(&root.join("repo"), BARE)
            .iter()
            .map(|finding| finding.payload["scope"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(scopes, vec!["external".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_in_tree_path_that_resolves_to_nothing_is_broken() {
        let root = temp("broken");
        std::fs::create_dir_all(root.join("crates")).unwrap();
        write(
            &root.join("Cargo.toml"),
            "[dependencies]\ngone = { path = \"crates/moved-away\" }\n",
        );
        let scopes: Vec<String> = dep_refs(&root, BARE)
            .iter()
            .map(|finding| finding.payload["scope"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(scopes, vec!["broken".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_glob_member_is_internal_rather_than_broken() {
        let root = temp("glob");
        write(
            &root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        );
        let scopes: Vec<String> = dep_refs(&root, BARE)
            .iter()
            .map(|finding| finding.payload["scope"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(scopes, vec!["internal".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dependency_inheritance_is_not_a_path_at_all() {
        let root = temp("inherit");
        write(
            &root.join("Cargo.toml"),
            "[dependencies]\nopentelemetry = { workspace = true }\n",
        );
        let found = dep_refs(&root, BARE);
        let kinds: Vec<&str> = found
            .iter()
            .map(|finding| finding.payload["ref_kind"].as_str().unwrap())
            .collect();
        let scopes: Vec<&str> = found
            .iter()
            .map(|finding| finding.payload["scope"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, vec!["inherited"]);
        assert_eq!(scopes, vec!["internal"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_git_reference_is_external_however_it_looks() {
        let root = temp("git");
        write(
            &root.join("Cargo.toml"),
            "[dependencies]\nother = { git = \"https://github.com/o/other\" }\n",
        );
        let scopes: Vec<String> = dep_refs(&root, BARE)
            .iter()
            .map(|finding| finding.payload["scope"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(scopes, vec!["external".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn agent_worktrees_are_not_part_of_the_dependency_graph() {
        let root = temp("worktree");
        write(
            &root.join("engine/Cargo.toml"),
            "[dependencies]\ncontract = { path = \"contract\" }\n",
        );
        write(
            &root.join(".claude/worktrees/docs-pass/engine/Cargo.toml"),
            "[dependencies]\ncontract = { path = \"contract\" }\n",
        );
        let found = dep_refs(&root, DEFAULT_EXCLUSIONS);
        assert_eq!(
            found.len(),
            1,
            "one manifest is the project's; the other is scratch"
        );
        assert_eq!(
            found[0].payload["manifest"].as_str(),
            Some("engine/Cargo.toml")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_malformed_manifest_is_a_fact_not_a_failure() {
        let root = temp("malformed");
        write(&root.join("Cargo.toml"), "this is not toml [[[");
        assert!(dep_refs(&root, BARE).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_scan_record_names_a_manifest_format_it_does_not_read() {
        let root = temp("gomod");
        write(&root.join("go.mod"), "module example.com/thing\n");
        let scan = scan(&root, BARE);
        assert_eq!(scan.payload["references"].as_i64(), Some(0));
        assert_eq!(scan.payload["manifests_read"].as_i64(), Some(0));
        assert_eq!(scan.payload["unsupported_manifests"], json!(["go.mod"]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_scan_record_names_a_manifest_that_would_not_parse() {
        let root = temp("unreadable");
        write(&root.join("Cargo.toml"), "this is not toml [[[");
        let scan = scan(&root, BARE);
        assert_eq!(scan.payload["unreadable_manifests"], json!(["Cargo.toml"]));
        assert_eq!(scan.payload["manifests_read"].as_i64(), Some(0));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_scan_record_is_written_even_for_a_project_with_nothing_in_it() {
        let root = temp("empty");
        let scan = scan(&root, BARE);
        assert_eq!(
            scan.payload,
            json!({
                "manifests_read": 0,
                "references": 0,
                "unreadable_manifests": [],
                "unsupported_manifests": [],
                "root_exists": true,
            })
        );
        assert_eq!(scan.subject_key, "depscan:fixture");
        let _ = std::fs::remove_dir_all(&root);
    }
}
