//! What a collector is. Ports `src/vogt/collectors/base.py`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::VogtConfig;
use crate::core::Project;
use crate::decisions::digest_of;
use crate::errors::VogtError;
use crate::storage::observed_types::PendingObservation;

/// Findings are `PendingObservation`s; the alias exists so collectors read as
/// "return findings" rather than "return pending observations".
pub type Finding = PendingObservation;

/// A collector could not complete for one project.
///
/// Raised by collectors and caught by the sweeper, which records the sweep as
/// `partial` and names the project. One unreadable repository must not cost
/// the estate its whole sweep. Python's `CollectorError` subclasses
/// `VogtError` with the same code, so this is that variant.
///
/// `class_name` is what Python's `type(exc).__name__` would have been. The
/// sweeper records `f"{type(exc).__name__}: {exc}"`, and Rust has no class
/// name to read back off a `VogtError`, so the collector states it.
pub fn collector_error(message: impl Into<String>) -> VogtError {
    VogtError::CollectorFailed(format!("CollectorError: {}", message.into()))
}

/// The same failure carrying a Python class name other than `CollectorError`.
///
/// `OSError` and `FileNotFoundError` are the two the sweeper tests and the git
/// collector record; both display as the bare message.
pub fn typed_failure(class_name: &str, message: impl Into<String>) -> VogtError {
    VogtError::CollectorFailed(format!("{class_name}: {}", message.into()))
}

pub type CollectorError = VogtError;

/// Build a finding, digesting the payload for dedup.
///
/// The digest covers the payload only — not the sweep, not the timestamp — so
/// re-observing an unchanged subject writes nothing however often it is
/// observed.
pub fn finding(
    kind: &str,
    subject_key: &str,
    payload: Value,
    project: Option<&Project>,
    source_url: Option<&str>,
    promoted: bool,
) -> Finding {
    PendingObservation {
        kind: kind.to_string(),
        subject_key: subject_key.to_string(),
        content_digest: digest_of(&payload),
        project_id: project.map(|project| project.id.clone()),
        source_url: source_url.map(str::to_string),
        promoted,
        payload,
    }
}

/// What a collector is allowed to know.
///
/// The clock is not held here. Python's `ctx.clock()` is a call, and Rust's
/// `Clock::now` needs `&mut`, which would make [`Collector`] generic and so
/// not a trait object. The sweeper passes the clock in as `now` instead.
pub struct CollectorContext<'a> {
    pub config: &'a VogtConfig,
    /// Extra per-collector settings, e.g. a forge token file. Absent means the
    /// optional integration is not configured, never that it failed.
    pub options: serde_json::Map<String, Value>,
}

/// One source of observations about a project.
pub trait Collector {
    /// Stable identifier, recorded on every sweep and observation.
    fn name(&self) -> &str;

    /// Whether this collector talks to anything outside the machine.
    ///
    /// The forge-less test layer runs only collectors that answer `false`,
    /// which is how NFR-PO2 stays true rather than aspirational.
    fn requires_network(&self) -> bool {
        false
    }

    /// Whether this collector opts out of unnamed sweeps.
    fn on_demand_only(&self) -> bool {
        false
    }

    /// Whether a project-narrowed sweep with no named collectors should drop
    /// this one. The transcript indexer is the one that answers `false`.
    fn project_scoped(&self) -> bool {
        true
    }

    /// Look at one project and return what was found.
    ///
    /// `now` is the sweep's clock. It is a function rather than the `Clock`
    /// itself so a collector can be a trait object: `Clock::now` needs `&mut`,
    /// and a generic clock would make this trait not dyn-compatible.
    fn collect(
        &mut self,
        ctx: &mut CollectorContext<'_>,
        project: &Project,
        now: &mut dyn FnMut() -> crate::core::Moment,
    ) -> Result<Vec<Finding>, VogtError>;

    /// Bookkeeping committed once the append has landed.
    ///
    /// The incremental forge sync advances its watermark here rather than in
    /// `collect`, so it can never move past observations that failed to
    /// persist (D1). Default collectors do nothing.
    fn after_append(&mut self, _at: crate::core::Moment) {}
}

/// A collector that indexes agent transcripts rather than projects.
///
/// It is handed the stored cursors, reads a bounded batch from them, and
/// returns it; the sweeper stores the batch and moves the cursors in one
/// transaction.
pub trait ActivityIndexer: Collector {
    fn scan(
        &mut self,
        cursors: &std::collections::BTreeMap<
            String,
            crate::storage::observed_types::TranscriptCursor,
        >,
    ) -> Result<crate::storage::observed_types::ActivityBatch, VogtError>;
}

/// Walk a project's files, honouring its exclusions.
///
/// Exclusions are applied *before* a file is opened, so vendored and generated
/// content never becomes an observation at all — the cheapest of the three
/// noise mechanisms, and the only one that has to handle volume
/// (ARCHITECTURE.md).
///
/// The walk is depth-first over a stack, with each directory's entries sorted,
/// which is the order Python's `sorted(iterdir)` plus `list.pop()` produces.
/// An unreadable directory is skipped. A root that is not a directory yields
/// nothing.
pub fn walk_project(
    root: &Path,
    exclusions: &[String],
    extensions: &[String],
    max_files: usize,
) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(&directory) {
            Ok(read) => read
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .collect(),
            Err(_) => continue,
        };
        entries.sort();
        // Pushed in reverse so the first sorted entry is popped first, the way
        // Python appends in sorted order and then pops from the end.
        let mut descend = Vec::new();
        for entry in entries {
            if excluded(&entry, root, exclusions) {
                continue;
            }
            if entry.is_dir() {
                if std::fs::symlink_metadata(&entry)
                    .map(|meta| !meta.file_type().is_symlink())
                    .unwrap_or(false)
                {
                    descend.push(entry);
                }
                continue;
            }
            if !extensions.is_empty() && !extension_matches(&entry, extensions) {
                continue;
            }
            // Python counts the file, then returns without yielding once `seen`
            // has gone past the cap. The extra file is looked at and dropped.
            if found.len() >= max_files {
                return found;
            }
            found.push(entry);
        }
        for entry in descend.into_iter().rev() {
            stack.push(entry);
        }
    }
    found
}

/// Python's `Path.suffix`: the last component's final dot and what follows it.
///
/// `.weird` is suffix `.weird`, not `.d`. A name with no dot has no suffix.
fn extension_matches(path: &Path, extensions: &[String]) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(dot) = name.rfind('.') else {
        return false;
    };
    if dot == 0 {
        return false;
    }
    let suffix = &name[dot..];
    extensions.iter().any(|extension| extension == suffix)
}

fn excluded(path: &Path, root: &Path, exclusions: &[String]) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    for pattern in exclusions {
        let trimmed = pattern.trim_end_matches('/');
        if trimmed.is_empty() {
            continue;
        }
        if name == trimmed
            || path_match(relative, trimmed)
            || path_match(relative, &format!("{trimmed}/*"))
        {
            return true;
        }
        if relative
            .components()
            .any(|part| part.as_os_str() == trimmed)
        {
            return true;
        }
    }
    false
}

/// `pathlib.Path.match`: a path glob, `**`-aware, not `fnmatchcase`.
///
/// `*` and `?` do not cross a path separator. `**` matches across them. A
/// pattern with no separator is matched against the whole relative path, the
/// way `PurePath.match` does.
fn path_match(relative: &Path, pattern: &str) -> bool {
    let path: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    let pattern: Vec<&str> = pattern.split('/').filter(|part| !part.is_empty()).collect();
    match_parts(&path, &pattern)
}

fn match_parts(path: &[String], pattern: &[&str]) -> bool {
    match (path, pattern) {
        ([], []) => true,
        ([], ["**"]) => true,
        ([], _) => false,
        (_, []) => false,
        (path, ["**", rest @ ..]) => (0..=path.len()).any(|skip| match_parts(&path[skip..], rest)),
        ([head, tail @ ..], [part, rest @ ..]) if component_match(head, part) => {
            match_parts(tail, rest)
        }
        _ => false,
    }
}

fn component_match(text: &str, pattern: &str) -> bool {
    component_match_at(text.as_bytes(), pattern.as_bytes())
}

fn component_match_at(text: &[u8], pattern: &[u8]) -> bool {
    match (text, pattern) {
        ([], []) => true,
        (_, [b'*', rest @ ..]) => {
            (0..=text.len()).any(|skip| component_match_at(&text[skip..], rest))
        }
        ([_, tail @ ..], [b'?', rest @ ..]) => component_match_at(tail, rest),
        (_, [b'[', rest @ ..]) => {
            let Some(end) = rest.iter().position(|byte| *byte == b']') else {
                return false;
            };
            if text.is_empty() {
                return false;
            }
            let (set, negated) = if rest.first() == Some(&b'!') {
                (&rest[1..end], true)
            } else {
                (&rest[..end], false)
            };
            let hit = in_set(text[0], set);
            if hit == negated {
                return false;
            }
            component_match_at(&text[1..], &rest[end + 1..])
        }
        ([head, tail @ ..], [part, rest @ ..]) if head == part => component_match_at(tail, rest),
        _ => false,
    }
}

fn in_set(byte: u8, set: &[u8]) -> bool {
    let mut index = 0;
    while index < set.len() {
        if index + 2 < set.len() && set[index + 1] == b'-' {
            if byte >= set[index] && byte <= set[index + 2] {
                return true;
            }
            index += 3;
        } else {
            if byte == set[index] {
                return true;
            }
            index += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn exclusions_are_applied_before_a_file_is_opened() {
        let root = std::env::temp_dir().join(format!("vogt-walk-{}-excl", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root.join("src/keep.py"), "x = 1\n");
        write(&root.join("node_modules/pkg/skip.py"), "y = 2\n");

        let found: std::collections::BTreeSet<String> = walk_project(
            &root,
            &["node_modules/".to_string()],
            &[".py".to_string()],
            20_000,
        )
        .into_iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
        assert_eq!(found, ["keep.py".to_string()].into());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_walk_stops_at_the_file_cap() {
        let root = std::env::temp_dir().join(format!("vogt-walk-{}-cap", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..20 {
            write(&root.join(format!("f{index}.py")), "x = 1\n");
        }
        let walked = walk_project(&root, &[], &[".py".to_string()], 5);
        assert_eq!(walked.len(), 5);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_extension_is_the_real_suffix() {
        let root = std::env::temp_dir().join(format!("vogt-walk-{}-suffix", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        write(&root.join("a.weird"), "x\n");
        write(&root.join("b.md"), "x\n");
        let walked = walk_project(&root, &[], &[".d".to_string(), ".md".to_string()], 20);
        let names: Vec<String> = walked
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["b.md".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_root_yields_nothing() {
        let root = std::env::temp_dir().join(format!("vogt-walk-{}-missing", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(walk_project(&root, &[], &[], 20_000).is_empty());
    }
}
