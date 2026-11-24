//! Reading the PR to work-item edge a forge already records. Ports
//! `adapters/forge/edges.py`.
//!
//! Observed-first and forward-only: the edge is read, never declared by hand,
//! and it reports rather than enforces. Every edge carries its provenance so a
//! reader can see why Vogt thinks the two are the same stream of work.

use regex::Regex;
use std::sync::LazyLock;

/// GitHub's closing keywords, every tense. A keyword, an optional colon, then
/// `#n` or the cross-repo `owner/repo#n`. The word boundary keeps `encloses #3`
/// from matching.
static CLOSING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\b\s*:?\s*(?:(?P<owner>[A-Za-z0-9._-]+)/(?P<repo>[A-Za-z0-9._-]+))?#(?P<number>\d+)",
    )
    .expect("closing-keyword pattern")
});

/// The branch-naming shapes this build understands, until a configurable
/// `branch_patterns` lands.
static DEFAULT_BRANCH_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|/)(?:gh|wi|issue|feat|feature|fix|bug)[-/](?P<number>\d+)\b")
        .expect("branch pattern")
});

pub const FROM_BODY: &str = "from PR body";
pub const FROM_TITLE: &str = "from PR title";
pub const FROM_BRANCH: &str = "from branch name";

/// One observed "this PR implements work item N" edge. `owner`/`repo` are set
/// only for a cross-repo reference; `None` means the PR's own repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEdge {
    pub number: i64,
    pub provenance: String,
    pub owner: Option<String>,
    pub repo: Option<String>,
}

/// Every edge this PR asserts, deduplicated, most-explicit provenance first.
///
/// Body closing keywords, then the title's, then the branch — so when the same
/// target turns up in more than one place the recorded provenance is the most
/// deliberate one. A PR that names no work yields an empty list, not a guess.
pub fn parse_edges(
    title: Option<&str>,
    body: Option<&str>,
    branch: Option<&str>,
) -> Vec<ParsedEdge> {
    let mut edges = Vec::new();
    edges.extend(closing_edges(body.unwrap_or(""), FROM_BODY));
    edges.extend(closing_edges(title.unwrap_or(""), FROM_TITLE));
    if let Some(branch) = branch {
        if let Some(found) = DEFAULT_BRANCH_PATTERN.captures(branch) {
            if let Ok(number) = found["number"].parse() {
                edges.push(ParsedEdge {
                    number,
                    provenance: FROM_BRANCH.to_owned(),
                    owner: None,
                    repo: None,
                });
            }
        }
    }
    let mut seen = Vec::new();
    edges.retain(|edge| {
        let key = (edge.owner.clone(), edge.repo.clone(), edge.number);
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    edges
}

fn closing_edges(text: &str, provenance: &str) -> Vec<ParsedEdge> {
    CLOSING
        .captures_iter(text)
        .filter_map(|found| {
            let number = found["number"].parse().ok()?;
            Some(ParsedEdge {
                number,
                provenance: provenance.to_owned(),
                owner: found.name("owner").map(|m| m.as_str().to_owned()),
                repo: found.name("repo").map(|m| m.as_str().to_owned()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_beats_title_beats_branch_and_cross_repo_is_kept() {
        let edges = parse_edges(
            Some("also fixes #12"),
            Some("Closes #12 and fixes acme/widgets#9"),
            Some("feature/gh-12-thing"),
        );
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].number, 12);
        assert_eq!(edges[0].provenance, FROM_BODY);
        assert!(edges[0].owner.is_none());
        assert_eq!(edges[1].number, 9);
        assert_eq!(edges[1].owner.as_deref(), Some("acme"));
        assert_eq!(edges[1].repo.as_deref(), Some("widgets"));
    }

    #[test]
    fn a_keyword_inside_a_word_is_not_an_edge() {
        assert!(parse_edges(None, Some("encloses #3"), None).is_empty());
        let edges = parse_edges(None, None, Some("bug/44-crash"));
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].number, 44);
        assert_eq!(edges[0].provenance, FROM_BRANCH);
    }
}
