//! Conditional-read support for a deliberately small GET allow-list. Ports
//! `src/vogt/adapters/http/etag.py`.
//!
//! The validator is the declared store revision rather than a response-body
//! digest. That revision changes in the same transaction as every authoritative
//! declared write, so it is cheap and cannot make a declared read appear current
//! after a write. Observed-only and composite reads stay uncached until they have
//! an equally precise validator.

/// Keep this list aligned with the browser's stable metadata policy. These are
/// declared-only, read-only facets; the operation name is the registry's source
/// of truth, not a URL substring that could accidentally include a write route.
pub const CONDITIONAL_READ_OPERATIONS: [&str; 5] = [
    "project.list",
    "workflow.list",
    "label.list",
    "initiative.list",
    "actor.list",
];

/// Whether an operation is eligible for a revision ETag. Only GET reaches this
/// check on the registry surface, so the method is not repeated here.
pub fn is_conditional_read(operation: &str) -> bool {
    CONDITIONAL_READ_OPERATIONS.contains(&operation)
}

/// The weak validator for one declared-store revision.
pub fn etag_for_revision(revision: i64) -> String {
    format!("W/\"{revision}\"")
}

/// Apply the GET/HEAD weak comparison for an `If-None-Match` header.
pub fn if_none_match_matches(header: Option<&str>, current: &str) -> bool {
    let Some(header) = header.filter(|value| !value.is_empty()) else {
        return false;
    };
    let current_tag = opaque_tag(current);
    header.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || opaque_tag(candidate) == current_tag
    })
}

/// Remove the weak marker for the RFC weak comparison.
fn opaque_tag(value: &str) -> &str {
    match value.strip_prefix("W/") {
        Some(rest) => rest.trim(),
        None => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revision_is_a_weak_validator() {
        assert_eq!(etag_for_revision(0), r#"W/"0""#);
        assert_eq!(etag_for_revision(42), r#"W/"42""#);
    }

    #[test]
    fn only_the_allow_list_is_conditional() {
        assert!(is_conditional_read("project.list"));
        assert!(is_conditional_read("actor.list"));
        assert!(!is_conditional_read("project.get"));
        assert!(!is_conditional_read("engine.status"));
    }

    #[test]
    fn a_matching_tag_is_weak_compared_and_lists_are_split() {
        let current = r#"W/"3""#;
        assert!(if_none_match_matches(Some(r#"W/"3""#), current));
        assert!(if_none_match_matches(Some(r#""3""#), current));
        assert!(if_none_match_matches(Some(r#"W/"1", W/"3""#), current));
        assert!(if_none_match_matches(Some("*"), current));
        assert!(!if_none_match_matches(Some(r#"W/"4""#), current));
        assert!(!if_none_match_matches(Some(""), current));
        assert!(!if_none_match_matches(None, current));
    }
}
