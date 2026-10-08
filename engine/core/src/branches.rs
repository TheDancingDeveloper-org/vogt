//! Branch binding. Ports `src/vogt/core/branches.py`.
//!
//! A branch belongs to a work item when its name carries that item's
//! reference. Nothing here reads git or the store.

#![allow(dead_code)]

pub const DEFAULT_BRANCH_PATTERNS: [&str; 2] =
    [r"(?i)\bwi-?(?P<n>\d+)\b", r"(?i)\bgh-(?P<forge>\d+)\b"];
pub const DEFAULT_BRANCH_TEMPLATE: &str = "wi-{number}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchMatch {
    pub work_ref: Option<String>,
    pub forge_number: Option<u64>,
}

/// First match wins. A pattern that does not compile is skipped: a bad
/// estate-supplied pattern must not cost a sweep every other branch.
pub fn match_branch(name: &str, patterns: &[&str]) -> Option<BranchMatch> {
    for raw in patterns {
        let Ok(pattern) = regex::Regex::new(raw) else {
            continue;
        };
        let Some(found) = pattern.captures(name) else {
            continue;
        };
        if let Some(number) = found.name("n").and_then(|m| m.as_str().parse::<u64>().ok()) {
            return Some(BranchMatch {
                work_ref: Some(format!("WI-{number}")),
                forge_number: None,
            });
        }
        if let Some(forge) = found
            .name("forge")
            .and_then(|m| m.as_str().parse::<u64>().ok())
        {
            return Some(BranchMatch {
                work_ref: None,
                forge_number: Some(forge),
            });
        }
    }
    None
}

/// The branch a Vogt-started session for `work_ref` declares. A native `WI-7`
/// renders through the template, an upstream subject key renders `gh-264`
/// regardless of it, and anything else becomes a slug.
pub fn default_branch_name(work_ref: &str, template: &str) -> String {
    let wi = regex::Regex::new(r"(?i)^wi-(\d+)$").expect("constant");
    if let Some(found) = wi.captures(work_ref) {
        return template.replace("{number}", &found[1]);
    }
    let forge = regex::Regex::new(r"#(\d+)$").expect("constant");
    if let Some(found) = forge.captures(work_ref) {
        return format!("gh-{}", &found[1]);
    }
    let slug = regex::Regex::new(r"[^a-z0-9]+").expect("constant");
    let rendered = slug.replace_all(&work_ref.to_lowercase(), "-").into_owned();
    let trimmed = rendered.trim_matches('-');
    if trimmed.is_empty() {
        "work".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_patterns_bind_the_documented_shapes() {
        let patterns = &DEFAULT_BRANCH_PATTERNS;
        assert_eq!(
            match_branch("wi-7/fix", patterns)
                .unwrap()
                .work_ref
                .as_deref(),
            Some("WI-7")
        );
        assert_eq!(
            match_branch("feature/WI-7-login", patterns)
                .unwrap()
                .work_ref
                .as_deref(),
            Some("WI-7")
        );
        // kiwi-7 is not wi-7: the word boundary holds.
        assert!(match_branch("kiwi-7", patterns).is_none());
        assert_eq!(
            match_branch("gh-264-port", patterns).unwrap().forge_number,
            Some(264)
        );
        // A pattern that does not compile is skipped, not fatal.
        assert!(match_branch("wi-7", &["(unclosed"]).is_none());
    }

    #[test]
    fn a_session_branch_renders_by_ref_shape() {
        assert_eq!(default_branch_name("WI-7", DEFAULT_BRANCH_TEMPLATE), "wi-7");
        assert_eq!(
            default_branch_name("gh:owner/repo#264", DEFAULT_BRANCH_TEMPLATE),
            "gh-264"
        );
        assert_eq!(
            default_branch_name("Some Ref!", DEFAULT_BRANCH_TEMPLATE),
            "some-ref"
        );
        assert_eq!(default_branch_name("---", DEFAULT_BRANCH_TEMPLATE), "work");
    }
}
