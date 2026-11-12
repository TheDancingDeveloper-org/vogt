//! Best-effort masking of credential-shaped text at the engine boundary.
//!
//! A session's terminal output and history log are transcripts: an agent that
//! prints a key or a token puts it where another agent, a driver or a person
//! reads it back. The workspace rule is "redact before print", but a rule is
//! advisory; this module is the engine's own pass over the text it *serves*
//! (`/screen`, the history log preview, search snippets).
//!
//! The engine server cannot link the core crate — `vogt-core` is a binary —
//! so the shapes here mirror `core/src/decisions/activity.rs` rather than
//! sharing its code. They are deliberately the same classes: PEM blocks, JWT
//! and provider token shapes, auth headers and bearers, URL userinfo,
//! credential-named assignments (whole underscore words), long hex, and long
//! mixed-case blobs. Over-redacting an ordinary word is the accepted cost;
//! leaving a value in a transcript is not.
//!
//! It is a guardrail, not a boundary: a value that matches no shape and sits
//! under no credential-named key still gets through. The real fix for a
//! printed secret is to stop printing it, and to stop the command that would
//! (the policy's environment guard).

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// Underscore-separated words that, as a whole word of a name, say the name
/// holds a credential. `KEY` is matched only at the end (`WIN_SSH_KEY`), so
/// `KEY_COUNT` and `SORT_KEYS` stay ordinary names.
const SENSITIVE_WORDS: &[&str] = &[
    "TOKEN",
    "TOKENS",
    "SECRET",
    "SECRETS",
    "PASSWORD",
    "PASSWORDS",
    "PASSWD",
    "CREDENTIAL",
    "CREDENTIALS",
    "PRIVATE",
];

/// Whether `name` says it holds a credential. The match is on whole words,
/// not a substring, so `MONKEY`, `KEY_COUNT` and `SORT_KEYS` are ordinary
/// names.
fn credential_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let word = |w: &str| {
        upper == w || upper.ends_with(&format!("_{w}")) || upper.contains(&format!("_{w}_"))
    };
    upper == "KEY"
        || upper.ends_with("_KEY")
        || upper.contains("_KEY_")
        || SENSITIVE_WORDS.iter().any(|w| word(w))
}

/// Mask what looks like a credential in text the engine keeps, serves or logs.
pub fn redact(text: &str) -> String {
    // A private key is a block, not a line: mask it whole before the
    // line-shaped passes can keep its header and leave the body to the blob
    // heuristic. An unterminated block is masked to the end of the text.
    static PEM_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)-----BEGIN [A-Z0-9 ]+-----.*?(?:-----END [A-Z0-9 ]+-----|\z)")
            .expect("pattern")
    });
    static JWT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}")
            .expect("pattern")
    });
    static TOKEN_SHAPES: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}",
            r"|github_pat_[A-Za-z0-9_]{20,}",
            r"|glpat-[A-Za-z0-9_-]{16,}",
            r"|sk-[A-Za-z0-9_-]{16,}",
            r"|xox[abposr]-[A-Za-z0-9-]{10,}",
            r"|(?:AKIA|ASIA)[0-9A-Z]{16}",
            r"|AIza[0-9A-Za-z_-]{30,}",
            r"|ya29\.[0-9A-Za-z_-]{20,}",
            r"|npm_[A-Za-z0-9]{30,}",
            r"|hf_[A-Za-z0-9]{30,}",
            r"|(?:pk|rk|sk)_(?:live|test)_[A-Za-z0-9]{16,}",
            r"|AGE-SECRET-KEY-1[0-9A-Z]{20,})",
        ))
        .expect("pattern")
    });
    static AUTH_HEADER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            "(?i)\\b((?:authorization|x-api-key|api-key|x-auth-token|private-token)\\s*[:=]\\s*(?:bearer\\s+|basic\\s+|token\\s+)?)[^\\s\"',;]+",
        )
        .expect("pattern")
    });
    static BEARER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{8,}").expect("pattern"));
    static URL_USERINFO: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(://)[^/\s:@]+:[^/\s@]+@").expect("pattern"));

    let mut out = PEM_BLOCK.replace_all(text, "[redacted]").into_owned();
    out = JWT.replace_all(&out, "[redacted]").into_owned();
    out = TOKEN_SHAPES.replace_all(&out, "[redacted]").into_owned();
    out = AUTH_HEADER.replace_all(&out, "${1}[redacted]").into_owned();
    out = BEARER.replace_all(&out, "${1}[redacted]").into_owned();
    out = URL_USERINFO
        .replace_all(&out, "${1}[redacted]@")
        .into_owned();
    // `NAME=value`, `NAME: value` and `"NAME": "value"`, where the name is
    // credential-shaped by whole underscore word. The quotes are kept so the
    // redacted text still reads as the line it was.
    static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?m)("?)([A-Za-z_][A-Za-z0-9_.-]{0,80})("?)(\s*[=:]\s*)("[^"\n]*"|'[^'\n]*'|[^\s"',;&]+)"#,
        )
        .expect("assignment pattern")
    });
    out = ASSIGNMENT
        .replace_all(&out, |captures: &Captures<'_>| {
            if credential_name(&captures[2]) {
                format!(
                    "{}{}{}{}[redacted]",
                    &captures[1], &captures[2], &captures[3], &captures[4]
                )
            } else {
                captures[0].to_string()
            }
        })
        .into_owned();
    static LONG_HEX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b[A-Fa-f0-9]{48,}\b").expect("pattern"));
    out = LONG_HEX.replace_all(&out, "[redacted]").into_owned();
    // A long token mixing upper case, lower case and digits reads as a key;
    // a git SHA or a long identifier in one case does not, and stays.
    static LONG: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b[A-Za-z0-9_\-+/=]{32,}").expect("pattern"));
    out = LONG
        .replace_all(&out, |m: &Captures<'_>| {
            let t = &m[0];
            let mixed = t.chars().any(|c| c.is_ascii_uppercase())
                && t.chars().any(|c| c.is_ascii_lowercase())
                && t.chars().any(|c| c.is_ascii_digit());
            if mixed {
                "[redacted]".to_string()
            } else {
                t.to_string()
            }
        })
        .into_owned();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_private_key_block_is_masked() {
        let text = "-----BEGIN OPENSSH PRIVATE KEY-----\n\
                    b3BlbnNzaC1rZXktdjEAAAAA\n\
                    -----END OPENSSH PRIVATE KEY-----";
        let out = redact(text);
        assert!(
            !out.contains("BEGIN OPENSSH PRIVATE KEY"),
            "the private-key block survived: {out}"
        );
        assert!(out.contains("[redacted]"), "{out}");
    }

    #[test]
    fn a_credential_named_assignment_is_masked() {
        for text in [
            "WIN_SSH_KEY=abcd1234",
            "export WIN_SSH_KEY=abcd1234",
            "WIN_SSH_KEY: abcd1234",
            r#"{"WIN_SSH_KEY": "abcd1234"}"#,
        ] {
            let out = redact(text);
            assert!(!out.contains("abcd1234"), "{text}: {out}");
        }
    }

    #[test]
    fn names_that_merely_contain_key_are_kept() {
        for text in ["KEY_COUNT=3", "MONKEY=hunter2", "SORT_KEYS=asc"] {
            assert_eq!(redact(text), text, "{text}");
        }
    }

    #[test]
    fn a_jwt_is_masked() {
        let text = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbpp";
        let out = redact(text);
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"), "{out}");
    }

    #[test]
    fn ordinary_text_and_identifiers_stay() {
        let text = "cargo test passed at 9faaef2c1d3b4e5f60718293a4b5c6d7e8f90a1b";
        assert_eq!(redact(text), text);
        let ident = "a_very_long_snake_case_identifier_name_here";
        assert!(redact(ident).contains(ident));
    }
}
