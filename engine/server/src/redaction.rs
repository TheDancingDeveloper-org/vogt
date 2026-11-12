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
//! credential-named assignments (underscore words and camelCase), long hex,
//! and long mixed-case blobs. Over-redacting an ordinary word is the accepted
//! cost; leaving a value in a transcript is not.
//!
//! It is a guardrail, not a boundary: a value that matches no shape and sits
//! under no credential-named key still gets through. The real fix for a
//! printed secret is to stop printing it, and to stop the command that would
//! (the policy's environment guard).

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// A word that, as a whole word of a name, says the name holds a credential,
/// with the suffixes and infixes precomputed so a screen line does not build
/// them again for every word on every poll.
struct SensitiveWord {
    whole: &'static str,
    suffix: &'static str,
    infix: &'static str,
}

const SENSITIVE_WORDS: &[SensitiveWord] = &[
    SensitiveWord {
        whole: "TOKEN",
        suffix: "_TOKEN",
        infix: "_TOKEN_",
    },
    SensitiveWord {
        whole: "TOKENS",
        suffix: "_TOKENS",
        infix: "_TOKENS_",
    },
    SensitiveWord {
        whole: "SECRET",
        suffix: "_SECRET",
        infix: "_SECRET_",
    },
    SensitiveWord {
        whole: "SECRETS",
        suffix: "_SECRETS",
        infix: "_SECRETS_",
    },
    SensitiveWord {
        whole: "PASSWORD",
        suffix: "_PASSWORD",
        infix: "_PASSWORD_",
    },
    SensitiveWord {
        whole: "PASSWORDS",
        suffix: "_PASSWORDS",
        infix: "_PASSWORDS_",
    },
    SensitiveWord {
        whole: "PASSWD",
        suffix: "_PASSWD",
        infix: "_PASSWD_",
    },
    SensitiveWord {
        whole: "CREDENTIAL",
        suffix: "_CREDENTIAL",
        infix: "_CREDENTIAL_",
    },
    SensitiveWord {
        whole: "CREDENTIALS",
        suffix: "_CREDENTIALS",
        infix: "_CREDENTIALS_",
    },
    SensitiveWord {
        whole: "PRIVATE",
        suffix: "_PRIVATE",
        infix: "_PRIVATE_",
    },
];

/// `KEY` is matched only as a whole word (`WIN_SSH_KEY`), so `KEY_COUNT` and
/// `SORT_KEYS` stay ordinary names.
const KEY: SensitiveWord = SensitiveWord {
    whole: "KEY",
    suffix: "_KEY",
    infix: "_KEY_",
};

/// Substrings the old chat redactor matched inside a run-together name:
/// `apikey`, `accesstoken`, `dbpassword`. These are not word-bounded, so a
/// name such as `monkey` still reaches the whole-word test above and stays.
const SENSITIVE_SUBSTRINGS: &[&str] = &["APIKEY", "TOKEN", "SECRET", "PASSWORD", "AUTHORIZATION"];

/// Split camelCase into words: `accessToken` becomes `ACCESS_TOKEN`.
fn camel_words(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let mut after_word_char = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() && after_word_char {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
        after_word_char = c.is_ascii_lowercase() || c.is_ascii_digit();
    }
    out
}

/// Whether `name` says it holds a credential. The match is on words after
/// camelCase is split, so `MONKEY`, `KEY_COUNT` and `SORT_KEYS` are ordinary
/// names, while `accessToken`, `apiKey`, `clientSecret`, `apikey` and
/// `dbPassword` are not.
fn credential_name(name: &str) -> bool {
    let upper = camel_words(name);
    let word = |w: &SensitiveWord| {
        upper == w.whole || upper.ends_with(w.suffix) || upper.contains(w.infix)
    };
    word(&KEY)
        || SENSITIVE_WORDS.iter().any(word)
        || SENSITIVE_SUBSTRINGS.iter().any(|s| upper.contains(s))
}

/// A private key's BEGIN or END marker on one line. The trailing dashes are
/// optional so a marker a narrow PTY wrapped still counts; the block test in
/// [`redact`] needs the full header to know where the body starts.
static KEY_BEGIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-----BEGIN [A-Z0-9 ]+").expect("pattern"));
static KEY_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-----END [A-Z0-9 ]+").expect("pattern"));

/// Whether a line could hold any of the shapes below. Ordinary prose — a
/// status line, a path, a commit subject — has no `=`, `:`, `@`, `_`, `-`,
/// `.`, no long token and no bearer, so `/screen` polling can skip the six
/// regex passes for it.
fn might_hold_credential(text: &str) -> bool {
    text.bytes()
        .any(|b| matches!(b, b'=' | b':' | b'@' | b'_' | b'-' | b'.'))
        || text.split_ascii_whitespace().any(|w| w.len() >= 16)
        || text.contains("Bearer")
        || text.contains("bearer")
        || text.contains("BEARER")
}

/// Whether a digest or checksum label sits immediately before `start`.
/// Image refs (`node@sha256:...`), Cargo.lock checksums (`checksum = "..."`)
/// and cosign digests (`sha256-...`) are public integrity data, not
/// credentials, so their value must survive the blob rules.
fn digest_label(text: &str, start: usize) -> bool {
    let before =
        text[..start].trim_end_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace());
    let lower = before.to_ascii_lowercase();
    const LABELS: &[&str] = &[
        "sha1:",
        "sha224:",
        "sha256:",
        "sha384:",
        "sha512:",
        "sha1-",
        "sha256-",
        "sha512-",
        "checksum =",
        "checksum=",
        "checksum:",
        "digest =",
        "digest=",
        "digest:",
        "integrity =",
        "integrity=",
    ];
    LABELS.iter().any(|label| lower.ends_with(label))
}

/// An auth scheme word or the marker a previous pass left. The auth-header
/// pass has already masked the value behind it; masking the scheme again
/// would turn `Authorization: Bearer [redacted]` into two markers.
fn already_a_scheme(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "bearer" | "basic" | "token" | "[redacted]"
    )
}

/// Mask what looks like a credential in text the engine keeps, serves or logs.
pub fn redact(text: &str) -> String {
    if !might_hold_credential(text) {
        return text.to_string();
    }
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
            if credential_name(&captures[2]) && !already_a_scheme(&captures[5]) {
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
    out = LONG_HEX
        .replace_all(&out, |m: &Captures<'_>| {
            if digest_label(&out, m.get(0).expect("a match").start()) {
                m[0].to_string()
            } else {
                "[redacted]".to_string()
            }
        })
        .into_owned();
    // A long token mixing upper case, lower case and digits reads as a key;
    // a git SHA or a long identifier in one case does not, and stays.
    static LONG: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b[A-Za-z0-9_\-+/=]{32,}").expect("pattern"));
    out = LONG
        .replace_all(&out, |m: &Captures<'_>| {
            let t = &m[0];
            let lower = t.to_ascii_lowercase();
            let labeled = lower.starts_with("sha1-")
                || lower.starts_with("sha256-")
                || lower.starts_with("sha512-");
            let mixed = t.chars().any(|c| c.is_ascii_uppercase())
                && t.chars().any(|c| c.is_ascii_lowercase())
                && t.chars().any(|c| c.is_ascii_digit());
            if mixed && !labeled && !digest_label(&out, m.get(0).expect("a match").start()) {
                "[redacted]".to_string()
            } else {
                t.to_string()
            }
        })
        .into_owned();
    out
}

/// Mask a run of lines as one text, so a private key block wrapped across
/// lines is redacted as a block. Every line keeps its place: each line of the
/// block becomes the marker rather than the block collapsing to one line,
/// which would move the grid under a reader. A BEGIN with no END masks to the
/// end of the run, and an END whose BEGIN has scrolled out of the run is
/// masked too.
pub fn redact_lines(lines: &mut [String]) {
    let mut in_block = false;
    for line in lines {
        if in_block {
            let ended = KEY_END.is_match(line);
            *line = "[redacted]".to_string();
            in_block = !ended;
            continue;
        }
        if KEY_END.is_match(line) {
            *line = "[redacted]".to_string();
            continue;
        }
        if KEY_BEGIN.is_match(line) {
            let ended = KEY_END.is_match(line);
            *line = "[redacted]".to_string();
            in_block = !ended;
            continue;
        }
        *line = redact(line);
    }
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

    #[test]
    fn camel_case_and_run_together_names_are_masked() {
        for text in [
            r#"{"accessToken": "abc123"}"#,
            r#"{"authorization": "Bearer abc123"}"#,
            "apiKey=abc123",
            "clientSecret: abc123",
            "apikey=abc123",
            "dbPassword=abc123",
            "privateKey=abc123",
        ] {
            let out = redact(text);
            assert!(!out.contains("abc123"), "{text}: {out}");
        }
    }

    #[test]
    fn ordinary_labels_still_read() {
        assert_eq!(redact("error: build failed"), "error: build failed");
        assert_eq!(redact("note: ran 3 tests"), "note: ran 3 tests");
        let out = redact("Tokens: 12");
        assert!(out.contains("Tokens:"), "the label was lost: {out}");
    }

    #[test]
    fn image_digests_and_checksums_are_not_mistaken_for_credentials() {
        let sha = "9faaef2c1d3b4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7";
        let cosign = "sha256-AbCdEf0123456789GhIjKl0123456789MnOpQrStUv";
        for text in [
            format!("node@sha256:{sha}"),
            format!("sha256:{sha}"),
            format!("checksum = \"{sha}\""),
            format!("image: ghcr.io/x/y@sha1:{sha}"),
            cosign.to_string(),
        ] {
            assert_eq!(redact(&text), text, "{text}");
        }
        // A bare long hex value still reads as a possible key and is masked.
        let out = redact(&format!("secret {sha}"));
        assert!(!out.contains(sha), "{out}");
    }

    #[test]
    fn an_unterminated_private_key_block_is_masked_to_the_end() {
        let text = "before\n\
                    -----BEGIN OPENSSH PRIVATE KEY-----\n\
                    b3BlbnNzaC1rZXktdjEAAAAA\n\
                    AAAA";
        let out = redact(text);
        assert!(out.starts_with("before"), "{out}");
        assert!(!out.contains("BEGIN OPENSSH PRIVATE KEY"), "{out}");
        assert!(!out.contains("b3BlbnNzaC1rZXktdjEAAAAA"), "{out}");
        assert!(out.contains("[redacted]"), "{out}");
    }
}
