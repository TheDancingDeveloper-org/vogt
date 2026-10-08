//! Branch binding. Ports `src/vogt/core/branches.py`.
//!
//! A branch belongs to a work item when its name carries that item's
//! reference. Nothing here reads git or the store.

#![allow(dead_code)]

use std::sync::LazyLock;

pub const DEFAULT_BRANCH_PATTERNS: [&str; 2] =
    [r"(?i)\bwi-?(?P<n>\d+)\b", r"(?i)\bgh-(?P<forge>\d+)\b"];
pub const DEFAULT_BRANCH_TEMPLATE: &str = "wi-{number}";

static WI_REF: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)^wi-(\d+)$").unwrap());
static FORGE_REF: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"#(\d+)$").unwrap());
static SLUG: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[^a-z0-9]+").unwrap());

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchMatch {
    pub work_ref: Option<String>,
    pub forge_number: Option<u64>,
}

/// First match wins. Patterns are compiled once by the caller; a pattern that
/// did not compile was already dropped there, so a bad estate-supplied pattern
/// costs nothing per branch. A captured number that does not parse returns on
/// that pattern rather than falling through to the next one, which is what
/// Python does.
pub fn match_branch(name: &str, patterns: &[regex::Regex]) -> Option<BranchMatch> {
    for pattern in patterns {
        let Some(found) = pattern.captures(name) else {
            continue;
        };
        if let Some(number) = found.name("n") {
            let digits = normalise_digits(number.as_str());
            if digits.is_empty() {
                return None;
            }
            return Some(BranchMatch {
                work_ref: Some(format!("WI-{digits}")),
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

/// Python's `\d` matches any Unicode decimal digit and `int()` strips leading
/// zeros. `char::to_digit` is ASCII only, so each Nd block is mapped from its
/// zero codepoint. A captured group that contains no digit returns none.
fn normalise_digits(raw: &str) -> String {
    let digits: String = raw
        .chars()
        .filter_map(decimal_digit)
        .map(|digit| char::from(b'0' + digit))
        .collect();
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() && !digits.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The value of a Unicode decimal digit, or none. Every Nd block runs zero to
/// nine contiguously, so the offset from its block's zero is the value.
fn decimal_digit(ch: char) -> Option<u8> {
    const ZEROS: &[u32] = &[
        0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66,
        0xDE6, 0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
        0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0,
        0xFF10, 0x104A0, 0x10D30, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0,
        0x11650, 0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x16A60, 0x16AC0,
        0x16B50, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E950,
        0x1FBF0,
    ];
    let code = ch as u32;
    ZEROS.iter().find_map(|&zero| {
        (zero..=zero + 9)
            .contains(&code)
            .then_some((code - zero) as u8)
    })
}

/// Compile the configured patterns once. One that does not compile is dropped.
pub fn compile_patterns(patterns: &[&str]) -> Vec<regex::Regex> {
    // `(?u)` makes `\d` match Unicode decimal digits, as Python's `\d` does.
    patterns
        .iter()
        .filter_map(|raw| regex::Regex::new(&format!("(?u){raw}")).ok())
        .collect()
}

/// The branch a Vogt-started session for `work_ref` declares. A native `WI-7`
/// renders through the template, an upstream subject key renders `gh-264`
/// regardless of it, and anything else becomes a slug.
pub fn default_branch_name(work_ref: &str, template: &str) -> String {
    if let Some(found) = WI_REF.captures(work_ref) {
        return template.replace("{number}", &found[1]);
    }
    if let Some(found) = FORGE_REF.captures(work_ref) {
        return format!("gh-{}", &found[1]);
    }
    let rendered = SLUG.replace_all(&work_ref.to_lowercase(), "-").into_owned();
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
        let patterns = compile_patterns(&DEFAULT_BRANCH_PATTERNS);
        assert_eq!(
            match_branch("wi-7/fix", &patterns)
                .unwrap()
                .work_ref
                .as_deref(),
            Some("WI-7")
        );
        assert_eq!(
            match_branch("feature/WI-7-login", &patterns)
                .unwrap()
                .work_ref
                .as_deref(),
            Some("WI-7")
        );
        // kiwi-7 is not wi-7: the word boundary holds.
        assert!(match_branch("kiwi-7", &patterns).is_none());
        assert_eq!(
            match_branch("gh-264-port", &patterns).unwrap().forge_number,
            Some(264)
        );
        // A pattern that does not compile is skipped, not fatal.
        assert!(compile_patterns(&["(unclosed"]).is_empty());
        // Pinned against vogt.core.branches: leading zeros collapse, and
        // Unicode decimal digits fold to their value.
        assert_eq!(
            match_branch("wi-007-fix", &patterns)
                .unwrap()
                .work_ref
                .as_deref(),
            Some("WI-7")
        );
        assert_eq!(
            match_branch("wi-\u{0667}", &patterns)
                .unwrap()
                .work_ref
                .as_deref(),
            Some("WI-7")
        );
        assert_eq!(
            match_branch("wi-1\u{0667}", &patterns)
                .unwrap()
                .work_ref
                .as_deref(),
            Some("WI-17")
        );
        assert_eq!(
            match_branch("wi-0", &patterns).unwrap().work_ref.as_deref(),
            Some("WI-0")
        );
        // A number bigger than u64 still binds, as Python's int() does.
        let huge = format!("wi-{}", "9".repeat(25));
        assert_eq!(
            match_branch(&huge, &patterns).unwrap().work_ref.as_deref(),
            Some(format!("WI-{}", "9".repeat(25)).as_str())
        );
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
