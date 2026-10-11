//! `source-markers` — TODOs and FIXMEs, and the reason most of them are noise.
//!
//! Ports `src/vogt/collectors/source_markers.py`.
//!
//! This collector observes every marker it finds, and promotes only those
//! matching a configured pattern (default `TODO(vogt)` / `FIXME(vogt)`). The
//! rest stay queryable; they do not claim to be work.

use std::path::Path;

use regex::Regex;
use serde_json::{json, Value};

use crate::core::{Moment, Project};
use crate::errors::VogtError;

use super::base::{finding, walk_project, Collector, CollectorContext, Finding};

const KIND_MARKER: &str = "marker";

/// A line longer than this is minified or generated.
const MAX_LINE_LENGTH: usize = 500;

const MAX_FILES: usize = 20_000;

/// A marker is a leading annotation, not a mention. Only comment leaders, list
/// bullets and whitespace may precede the tag.
fn marker_pattern() -> Regex {
    Regex::new(
        r"^[\s\-*+#/;%<!>=|]*\b(?P<tag>TODO|FIXME|HACK|XXX)\b(?P<scope>\([^)]*\))?\s*:?\s*(?P<text>.*)",
    )
    .expect("marker pattern is a valid regex")
}

pub struct SourceMarkerCollector;

impl Collector for SourceMarkerCollector {
    fn name(&self) -> &str {
        "source-markers"
    }

    fn requires_network(&self) -> bool {
        false
    }

    fn collect(
        &mut self,
        ctx: &mut CollectorContext<'_>,
        project: &Project,
        _now: &mut dyn FnMut() -> Moment,
    ) -> Result<Vec<Finding>, VogtError> {
        let root = Path::new(&project.root_path);
        let pattern = marker_pattern();
        let mut found = Vec::new();
        for path in walk_project(
            root,
            &project.exclusions,
            &ctx.config.marker_file_extensions,
            MAX_FILES,
        ) {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            for (index, line) in text.split('\n').enumerate() {
                let line = line.strip_suffix('\r').unwrap_or(line);
                if line.len() > MAX_LINE_LENGTH {
                    continue;
                }
                let Some(captures) = pattern.captures(line) else {
                    continue;
                };
                let marker = captures.get(0).map(|m| m.as_str().trim()).unwrap_or("");
                let promoted_by = ctx
                    .config
                    .marker_promotion_patterns
                    .iter()
                    .find(|pattern| marker.contains(pattern.as_str()))
                    .cloned();
                let scope = captures
                    .name("scope")
                    .map(|scope| scope.as_str().trim_matches(|ch| ch == '(' || ch == ')'))
                    .filter(|scope| !scope.is_empty());
                let text = captures
                    .name("text")
                    .map(|text| text.as_str().trim())
                    .unwrap_or("");
                let text = if text.len() > 500 { &text[..500] } else { text };
                let number = index + 1;
                let payload: Value = json!({
                    "tag": captures.name("tag").map(|tag| tag.as_str()).unwrap_or(""),
                    "scope": scope,
                    "text": text,
                    "path": relative,
                    "line": number,
                    "promoted_by": promoted_by,
                });
                found.push(finding(
                    KIND_MARKER,
                    &format!("mark:{}/{relative}#L{number}", project.slug),
                    payload,
                    Some(project),
                    None,
                    promoted_by.is_some(),
                ));
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VogtConfig;
    use crate::core::Moment;

    fn project(root: &Path) -> Project {
        Project::new(
            "prj_fixture",
            "fixture",
            "fixture",
            &root.display().to_string(),
            Moment::from_unix(0, 0),
        )
    }

    fn ctx(config: &VogtConfig) -> CollectorContext<'_> {
        CollectorContext {
            config,
            options: serde_json::Map::new(),
        }
    }

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn collect(config: &VogtConfig, root: &Path) -> Vec<Finding> {
        let mut collector = SourceMarkerCollector;
        let mut context = ctx(config);
        let mut now = || Moment::from_unix(0, 0);
        collector
            .collect(&mut context, &project(root), &mut now)
            .unwrap()
    }

    #[test]
    fn only_configured_patterns_are_promoted() {
        let root = std::env::temp_dir().join(format!("vogt-mark-{}-promo", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(
            &root.join("a.py"),
            b"# TODO(vogt): this one is work\n# TODO: this one is a note to self\n# FIXME(vogt): so is this\n# XXX: and this is not\n",
        );
        let found = collect(&VogtConfig::default(), &root);
        assert_eq!(found.len(), 4);
        let promoted: std::collections::BTreeSet<String> = found
            .iter()
            .filter(|finding| finding.promoted)
            .map(|finding| finding.payload["text"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            promoted,
            ["so is this".to_string(), "this one is work".to_string()].into()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn marker_subject_keys_are_stable() {
        let root = std::env::temp_dir().join(format!("vogt-mark-{}-key", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root.join("a.py"), b"\n# TODO(vogt): here\n");
        let once = collect(&VogtConfig::default(), &root);
        let twice = collect(&VogtConfig::default(), &root);
        assert_eq!(
            once.iter()
                .map(|f| f.subject_key.clone())
                .collect::<Vec<_>>(),
            vec!["mark:fixture/a.py#L2".to_string()]
        );
        assert_eq!(once[0].content_digest, twice[0].content_digest);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn binary_and_long_lines_are_skipped() {
        let root = std::env::temp_dir().join(format!("vogt-mark-{}-skip", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let long = format!("# TODO(vogt): {}\n", "x".repeat(600));
        write(&root.join("big.py"), long.as_bytes());
        write(&root.join("bin.py"), b"\xff\xfe# TODO(vogt): binary\n");
        assert!(collect(&VogtConfig::default(), &root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_scanned_extensions_are_configuration() {
        let root = std::env::temp_dir().join(format!("vogt-mark-{}-ext", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root.join("a.weird"), b"# TODO(vogt): findable\n");
        assert!(collect(&VogtConfig::default(), &root).is_empty());
        let widened = VogtConfig {
            marker_file_extensions: vec![".weird".to_string()],
            ..VogtConfig::default()
        };
        assert_eq!(collect(&widened, &root).len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn leading_annotations_are_markers() {
        let lines = [
            "# TODO(vogt): a real one",
            "    // FIXME(vogt): indented, in a C-style comment",
            "-- TODO(vogt): SQL comment",
            "- TODO(vogt): a markdown bullet",
            "TODO(vogt): bare at the start of a line",
            "<!-- TODO(vogt): an HTML comment -->",
        ];
        for (index, line) in lines.iter().enumerate() {
            let root =
                std::env::temp_dir().join(format!("vogt-mark-{}-lead-{index}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            write(&root.join("a.py"), format!("{line}\n").as_bytes());
            let found = collect(&VogtConfig::default(), &root);
            assert_eq!(found.len(), 1, "{line:?} should be a marker");
            assert!(found[0].promoted);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn mentions_in_prose_are_not_markers() {
        let lines = [
            "marker_promotion_patterns = [\"TODO(vogt)\", \"FIXME(vogt)\"]",
            "| `marker_promotion_patterns` | default `TODO(vogt)` | behaviour |",
            "   pattern — default `TODO(vogt):` / `FIXME(vogt):` — enter backlog",
            "promotion by convention (`TODO(vogt):`), audited suppress",
            "assert promoted == {\"TODO(vogt)\"}",
        ];
        for (index, line) in lines.iter().enumerate() {
            let root = std::env::temp_dir()
                .join(format!("vogt-mark-{}-prose-{index}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            write(&root.join("a.py"), format!("{line}\n").as_bytes());
            assert!(
                collect(&VogtConfig::default(), &root).is_empty(),
                "{line:?} is a mention, not an annotation"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}
