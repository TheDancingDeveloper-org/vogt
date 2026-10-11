//! `mirrored-source` — the same source in two places, reported and not judged.
//!
//! Ports `src/vogt/collectors/mirrored_source.py`.
//!
//! This reports the relationship: a path inside one project declares the
//! package another registered project publishes. It does not compare the two
//! copies, does not diff them, and never says one is behind. Both declared
//! versions are recorded as the facts they are; nothing here subtracts one
//! from the other.
//!
//! Matching is on the declared package name, the identity both copies agree
//! on. A name that two registered projects both claim is dropped rather than
//! guessed at: an ambiguous match is not a finding.
//!
//! The registered project list is handed down by the application layer.
//! Collectors never read declared data themselves.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::core::{Moment, Project};
use crate::errors::VogtError;

use super::base::{finding, walk_project, Collector, CollectorContext, Finding};

const KIND_MIRRORED_SOURCE: &str = "mirrored_source";

/// The manifests a package identity can be read from — the same three
/// `dep-refs` parses.
const MANIFESTS: [&str; 3] = ["Cargo.toml", "package.json", "pyproject.toml"];

const MAX_FILES: usize = 20_000;

/// One registered project, flattened for a collector to read.
///
/// Deliberately not a [`Project`]: what this collector is allowed to know is
/// where a project's checkout is and what it is called, not the declared
/// entity around it.
#[derive(Debug, Clone)]
pub struct RegisteredProject {
    pub id: String,
    pub slug: String,
    pub root_path: String,
    pub repo_url: Option<String>,
}

/// A package a registered project's own root manifest declares.
struct Published {
    project: RegisteredProject,
    version: Option<String>,
}

pub struct MirroredSourceCollector {
    projects: Vec<RegisteredProject>,
    /// Built once and reused for the rest of the sweep: the answer cannot
    /// change inside one sweep.
    index: Option<HashMap<String, Published>>,
}

impl MirroredSourceCollector {
    pub fn new(projects: Vec<RegisteredProject>) -> Self {
        Self {
            projects,
            index: None,
        }
    }

    fn published(&mut self) -> &HashMap<String, Published> {
        if self.index.is_none() {
            let mut index: HashMap<String, Published> = HashMap::new();
            let mut ambiguous: HashSet<String> = HashSet::new();
            for candidate in &self.projects {
                let (name, version) = root_package_of(&expanduser(Path::new(&candidate.root_path)));
                let Some(name) = name else {
                    continue;
                };
                if let Some(existing) = index.get(&name) {
                    if existing.project.id != candidate.id {
                        ambiguous.insert(name);
                        continue;
                    }
                }
                index.insert(
                    name,
                    Published {
                        project: candidate.clone(),
                        version,
                    },
                );
            }
            for name in &ambiguous {
                index.remove(name);
            }
            self.index = Some(index);
        }
        self.index.as_ref().expect("index was just built")
    }
}

impl Collector for MirroredSourceCollector {
    fn name(&self) -> &str {
        "mirrored-source"
    }

    fn requires_network(&self) -> bool {
        false
    }

    fn collect(
        &mut self,
        _ctx: &mut CollectorContext<'_>,
        project: &Project,
        _now: &mut dyn FnMut() -> Moment,
    ) -> Result<Vec<Finding>, VogtError> {
        if self.projects.is_empty() || self.published().is_empty() {
            return Ok(Vec::new());
        }
        let root = expanduser(Path::new(&project.root_path));
        let Ok(resolved_root) = root.canonicalize() else {
            return Ok(Vec::new());
        };
        // Copied out so the walk can borrow `project` without holding the index.
        let index: Vec<(String, RegisteredProject, Option<String>)> = self
            .index
            .as_ref()
            .expect("index was just built")
            .iter()
            .map(|(name, published)| {
                (
                    name.clone(),
                    published.project.clone(),
                    published.version.clone(),
                )
            })
            .collect();
        let mut found = Vec::new();
        for path in walk_project(&root, &project.exclusions, &[], MAX_FILES) {
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !MANIFESTS.contains(&file_name) {
                continue;
            }
            let Some(parent) = path.parent() else {
                continue;
            };
            let Ok(member) = parent.canonicalize() else {
                continue;
            };
            if member == resolved_root {
                // The project's own root manifest declares the project's own
                // package. A thing is not a mirror of itself.
                continue;
            }
            let (name, version) = package_of(&path);
            let Some(name) = name else {
                continue;
            };
            let Some((_, published, published_version)) =
                index.iter().find(|(package, _, _)| package == &name)
            else {
                continue;
            };
            if published.id == project.id {
                continue;
            }
            if same_checkout(published, &member) {
                // The standalone project is checked out *at* this path — a
                // submodule, or one registration nested inside another. One
                // copy of the source, not two.
                continue;
            }
            let local_path = member
                .strip_prefix(&resolved_root)
                .unwrap_or(&member)
                .to_string_lossy()
                .replace('\\', "/");
            let manifest = path
                .strip_prefix(&resolved_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let mut payload = Map::new();
            payload.insert("package".to_string(), json!(name));
            payload.insert("local_path".to_string(), json!(local_path));
            payload.insert("manifest".to_string(), json!(manifest));
            payload.insert("mirrors_project_id".to_string(), json!(published.id));
            payload.insert("mirrors_project_slug".to_string(), json!(published.slug));
            payload.insert(
                "mirrors_repo_url".to_string(),
                match &published.repo_url {
                    Some(url) => Value::String(url.clone()),
                    None => Value::Null,
                },
            );
            payload.insert(
                "local_version".to_string(),
                match &version {
                    Some(version) => Value::String(version.clone()),
                    None => Value::Null,
                },
            );
            payload.insert(
                "published_version".to_string(),
                match published_version {
                    Some(version) => Value::String(version.clone()),
                    None => Value::Null,
                },
            );
            payload.insert(
                "matched_on".to_string(),
                json!("package name declared by both manifests"),
            );
            found.push(finding(
                KIND_MIRRORED_SOURCE,
                &format!("mirror:{}/{local_path}->{}", project.slug, published.slug),
                Value::Object(payload),
                Some(project),
                None,
                false,
            ));
        }
        Ok(found)
    }
}

fn same_checkout(project: &RegisteredProject, member: &Path) -> bool {
    expanduser(Path::new(&project.root_path))
        .canonicalize()
        .is_ok_and(|resolved| resolved == member)
}

/// The package a project's own root declares, from the first manifest that does.
fn root_package_of(root: &Path) -> (Option<String>, Option<String>) {
    for name in MANIFESTS {
        let (package, version) = package_of(&root.join(name));
        if package.is_some() {
            return (package, version);
        }
    }
    (None, None)
}

/// The package name and version one manifest declares, if it declares one.
///
/// A manifest that declares no package, or one that cannot be read, yields
/// nothing. An unreadable manifest is a fact about the project, not a
/// collector failure.
fn package_of(path: &Path) -> (Option<String>, Option<String>) {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return (None, None);
    };
    if name == "package.json" {
        let Ok(text) = std::fs::read_to_string(path) else {
            return (None, None);
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
            return (None, None);
        };
        let Some(document) = parsed.as_object() else {
            return (None, None);
        };
        return (
            string_field(document, "name"),
            string_field(document, "version"),
        );
    }
    let Ok(bytes) = std::fs::read(path) else {
        return (None, None);
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return (None, None);
    };
    let Ok(document) = toml::from_str::<toml::Value>(text) else {
        return (None, None);
    };
    let Some(document) = document.as_table() else {
        return (None, None);
    };
    if name == "Cargo.toml" {
        let Some(package) = document.get("package").and_then(toml::Value::as_table) else {
            return (None, None);
        };
        // `version.workspace = true` inherits from the workspace root. The
        // name is what identifies the package; an inherited version is
        // recorded as unknown rather than as the literal table.
        return (
            toml_string(package, "name"),
            toml_string(package, "version"),
        );
    }
    if let Some(project) = document.get("project").and_then(toml::Value::as_table) {
        if let Some(package) = toml_string(project, "name") {
            return (Some(package), toml_string(project, "version"));
        }
    }
    if let Some(tool) = document.get("tool").and_then(toml::Value::as_table) {
        if let Some(poetry) = tool.get("poetry").and_then(toml::Value::as_table) {
            return (toml_string(poetry, "name"), toml_string(poetry, "version"));
        }
    }
    (None, None)
}

fn string_field(document: &Map<String, Value>, key: &str) -> Option<String> {
    match document.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

fn toml_string(table: &toml::map::Map<String, toml::Value>, key: &str) -> Option<String> {
    match table.get(key).and_then(toml::Value::as_str) {
        Some(value) if !value.is_empty() => Some(value.to_string()),
        _ => None,
    }
}

/// Python's `Path.expanduser`: only a leading `~` expands, and only when it is
/// the first component on its own.
fn expanduser(path: &Path) -> PathBuf {
    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return path.to_path_buf();
    };
    let first = first.to_string_lossy();
    if first != "~" {
        return path.to_path_buf();
    }
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(components.as_path()),
        None => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VogtConfig;

    fn project(root: &Path, slug: &str) -> Project {
        let mut project = Project::new(
            &format!("prj_{slug}"),
            slug,
            slug,
            &root.display().to_string(),
            Moment::from_unix(0, 0),
        );
        project.exclusions = vec![
            ".venv/".to_string(),
            "node_modules/".to_string(),
            "target/".to_string(),
            ".git/".to_string(),
        ];
        project
    }

    fn collect(projects: Vec<RegisteredProject>, root: &Path, slug: &str) -> Vec<Finding> {
        let config = VogtConfig::default();
        let mut context = CollectorContext {
            config: &config,
            options: Map::new(),
        };
        let mut now = || Moment::from_unix(0, 0);
        MirroredSourceCollector::new(projects)
            .collect(&mut context, &project(root, slug), &mut now)
            .unwrap()
    }

    fn crate_at(root: &Path, name: &str, version: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n"),
        )
        .unwrap();
    }

    fn scratch(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vogt-mirror-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_vendored_crate_that_is_also_a_project_is_reported() {
        let base = scratch("vendored");
        let vendoring = base.join("rustnzb");
        crate_at(
            &vendoring.join("crates").join("nzb-core"),
            "nzb-core",
            "0.3.0",
        );
        let published = base.join("nzb-core");
        crate_at(&published, "nzb-core", "0.4.0");

        let found = collect(
            vec![RegisteredProject {
                id: "prj_published".to_string(),
                slug: "nzb-core".to_string(),
                root_path: published.display().to_string(),
                repo_url: Some("https://github.com/o/nzb-core".to_string()),
            }],
            &vendoring,
            "rustnzb",
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, "mirrored_source");
        assert_eq!(
            found[0].subject_key,
            "mirror:rustnzb/crates/nzb-core->nzb-core"
        );
        assert_eq!(
            found[0].payload,
            json!({
                "package": "nzb-core",
                "local_path": "crates/nzb-core",
                "manifest": "crates/nzb-core/Cargo.toml",
                "mirrors_project_id": "prj_published",
                "mirrors_project_slug": "nzb-core",
                "mirrors_repo_url": "https://github.com/o/nzb-core",
                "local_version": "0.3.0",
                "published_version": "0.4.0",
                "matched_on": "package name declared by both manifests",
            })
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_project_is_not_a_mirror_of_itself() {
        let base = scratch("solo");
        let root = base.join("solo");
        crate_at(&root, "solo", "1.0.0");
        let found = collect(
            vec![RegisteredProject {
                id: "prj_solo".to_string(),
                slug: "solo".to_string(),
                root_path: root.display().to_string(),
                repo_url: None,
            }],
            &root,
            "solo",
        );
        assert!(found.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_package_name_two_projects_claim_is_not_a_finding() {
        let base = scratch("ambiguous");
        let vendoring = base.join("app");
        crate_at(&vendoring.join("vendor").join("shared"), "shared", "0.1.0");
        let first = base.join("first");
        crate_at(&first, "shared", "0.1.0");
        let second = base.join("second");
        crate_at(&second, "shared", "0.2.0");
        let found = collect(
            vec![
                RegisteredProject {
                    id: "prj_first".to_string(),
                    slug: "first".to_string(),
                    root_path: first.display().to_string(),
                    repo_url: None,
                },
                RegisteredProject {
                    id: "prj_second".to_string(),
                    slug: "second".to_string(),
                    root_path: second.display().to_string(),
                    repo_url: None,
                },
            ],
            &vendoring,
            "app",
        );
        assert!(found.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_project_checked_out_inside_another_is_one_copy_not_two() {
        let base = scratch("nested");
        let outer = base.join("outer");
        let inner = outer.join("vendor").join("inner");
        crate_at(&inner, "inner", "1.0.0");
        let found = collect(
            vec![RegisteredProject {
                id: "prj_inner".to_string(),
                slug: "inner".to_string(),
                root_path: inner.display().to_string(),
                repo_url: None,
            }],
            &outer,
            "outer",
        );
        assert!(found.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
