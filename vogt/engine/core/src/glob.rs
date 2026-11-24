//! `fnmatchcase` matching shared by CI alerts and views.
//!
//! Case-sensitive `*`, `?` and `[seq]`/`[!seq]`, with no path-separator rules.
//! `?` and a character class match one Unicode scalar, not one byte.

/// `fnmatchcase`: `*`, `?` and `[seq]`/`[!seq]`, case-sensitive, no path
/// separator rules. `?` and a character class match one Unicode scalar, not one
/// byte. `ci_alerts` wants the same matching, so this is the one copy.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn rec(pattern: &[char], text: &[char]) -> bool {
        match (pattern, text) {
            ([], []) => true,
            ([head, rest @ ..], text) if *head == '*' => {
                (0..=text.len()).any(|at| rec(rest, &text[at..]))
            }
            ([head, rest @ ..], [_, tail @ ..]) if *head == '?' => rec(rest, tail),
            ([head, rest @ ..], text) if *head == '[' => match character_class(rest) {
                Some((class, after)) => {
                    !text.is_empty() && class_matches(&class, text[0]) && rec(after, &text[1..])
                }
                None => text.first().is_some_and(|other| *head == *other) && rec(rest, &text[1..]),
            },
            ([head, rest @ ..], [other, tail @ ..]) if head == other => rec(rest, tail),
            _ => false,
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    rec(&pattern, &text)
}

/// The body of a `[...]` class and what follows it, or `None` when the bracket
/// never closes — which is a literal `[`, the way `fnmatch` reads it. A `]`
/// straight after the `[` is part of the class, not its end, so `[]]` matches
/// a `]` and `[]]x` matches `]x`.
fn character_class(pattern: &[char]) -> Option<(Vec<char>, &[char])> {
    let mut end = pattern.iter().position(|char| *char == ']')?;
    let lead = pattern.first().is_some_and(|char| *char == '!');
    if end == 0 || (lead && end == 1) {
        end = pattern[end + 1..].iter().position(|char| *char == ']')? + end + 1;
    }
    Some((pattern[..end].to_vec(), &pattern[end + 1..]))
}

fn class_matches(class: &[char], candidate: char) -> bool {
    let (negated, class) = match class.first() {
        Some('!') => (true, &class[1..]),
        _ => (false, class),
    };
    let mut matched = false;
    let mut chars = class.iter().peekable();
    while let Some(start) = chars.next() {
        if chars.peek() == Some(&&'-') && chars.clone().nth(1).is_some() {
            chars.next();
            if let Some(end) = chars.next() {
                matched |= (*start..=*end).contains(&candidate);
                continue;
            }
        }
        matched |= *start == candidate;
    }
    matched != negated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matching_is_case_sensitive_and_star_only() {
        assert!(glob_match("v*", "v1.2.3"));
        assert!(glob_match("main", "main"));
        assert!(!glob_match("Main", "main"));
        assert!(glob_match("release-?", "release-a"));
        assert!(!glob_match("release-?", "release-ab"));
        assert!(glob_match("v[0-9]*", "v1"));
        assert!(!glob_match("v[0-9]*", "va"));
        assert!(glob_match("v[!0-9]", "va"));
        assert!(glob_match("relé-?", "relé-β"));
        assert!(!glob_match("[unterminated", "x"));
        assert!(glob_match("[^b]", "b"));
        assert!(glob_match("[^b]", "^"));
        assert!(!glob_match("[!a-]", "-"));
    }
}
