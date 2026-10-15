//! `scripts/sanitise_transcript.py` in Rust.
//!
//! A captured terminal transcript is raw PTY bytes. Before it is committed as
//! a fixture this pass rewrites home paths, emails and secret-shaped tokens to
//! inert placeholders, then asserts that none of the forbidden patterns
//! survive. `--check` asserts without writing.

use std::path::Path;

/// Rewrite `data` and refuse the result if a secret pattern remains.
pub fn sanitise(data: &[u8]) -> Result<Vec<u8>, String> {
    let cleaned = rewrite(data);
    assert_clean(&cleaned)?;
    Ok(cleaned)
}

/// Refuse `data` if a secret pattern is present.
pub fn assert_clean(data: &[u8]) -> Result<(), String> {
    let mut leaks = Vec::new();
    for rule in forbidden() {
        for found in rule.pattern.find_iter(data) {
            if rule.allow == Some(found.as_bytes()) {
                continue;
            }
            let shown: String = found
                .as_bytes()
                .iter()
                .take(32)
                .map(|b| *b as char)
                .collect();
            leaks.push(format!("{} matched {shown:?}", rule.name));
            break;
        }
    }
    if leaks.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "sanitise_transcript: secret pattern survived sanitisation:\n  {}",
            leaks.join("\n  ")
        ))
    }
}

/// `sanitise_transcript INPUT OUTPUT` or `sanitise_transcript --check INPUT`.
pub fn run(args: &[String]) -> Result<(), String> {
    let mut check = false;
    let mut paths = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--check" => check = true,
            "-h" | "--help" => {
                println!(
                    "usage: vogt-core sanitise-transcript [--check] INPUT [OUTPUT]\n\nRewrite a captured transcript in place of secret-shaped bytes, then assert nothing survives."
                );
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unrecognized argument: {other}"));
            }
            other => paths.push(other),
        }
    }
    let input = paths.first().ok_or("INPUT is required")?;
    let data = std::fs::read(input).map_err(|err| format!("{input}: {err}"))?;
    if check {
        assert_clean(&data)?;
        println!("sanitise_transcript: {input} clean ({} bytes)", data.len());
        return Ok(());
    }
    let output = paths
        .get(1)
        .ok_or("OUTPUT is required unless --check is given")?;
    let cleaned = sanitise(&data)?;
    let out = Path::new(output);
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| format!("{output}: {err}"))?;
        }
    }
    std::fs::write(out, &cleaned).map_err(|err| format!("{output}: {err}"))?;
    println!(
        "sanitise_transcript: {input} -> {output} ({} -> {} bytes, clean)",
        data.len(),
        cleaned.len()
    );
    Ok(())
}

fn rewrite(data: &[u8]) -> Vec<u8> {
    let mut text = String::from_utf8_lossy(data).into_owned();
    let rules: &[(&str, &str)] = &[
        (
            r"(?i)(authorization:\s*bearer\s+)[A-Za-z0-9._\-]+",
            "${1}REDACTED_TOKEN",
        ),
        // The specific shapes come before the generic `token=` rule, which
        // would otherwise swallow a `ghp_` token whole.
        (r"sk-[A-Za-z0-9]{20,}", "sk-REDACTED"),
        (r"gh[pousr]_[A-Za-z0-9]{20,}", "ghx_REDACTED"),
        (r"AKIA[0-9A-Z]{16}", "AKIAREDACTEDKEY000"),
        (
            r#"(?i)(token[=:\s\"']+)[A-Za-z0-9._\-]{16,}"#,
            "${1}REDACTED_TOKEN",
        ),
        (r"xox[baprs]-[A-Za-z0-9-]{10,}", "xoxb-REDACTED"),
        (
            r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}",
            "eyJ.REDACTED.JWT",
        ),
        (
            r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
            "user@example.invalid",
        ),
        (r"/home/[A-Za-z0-9._\-]+", "/home/user"),
        (r"/Users/[A-Za-z0-9._\-]+", "/Users/user"),
    ];
    for (pattern, replacement) in rules {
        let re = regex::Regex::new(pattern).expect("rewrite pattern compiles");
        text = re.replace_all(&text, *replacement).into_owned();
    }
    text.into_bytes()
}

struct Forbidden {
    name: &'static str,
    pattern: regex::bytes::Regex,
    /// A match whose text equals this exactly is the placeholder, not a leak.
    allow: Option<&'static [u8]>,
}

fn forbidden() -> Vec<Forbidden> {
    let specs: &[(&str, &str, Option<&[u8]>)] = &[
        (r"(?i)bearer\s+[A-Za-z0-9._\-]{16,}", "bearer token", None),
        (r"sk-[A-Za-z0-9]{20,}", "sk- key", None),
        (r"gh[pousr]_[A-Za-z0-9]{20,}", "github token", None),
        (r"AKIA[0-9A-Z]{16}", "aws key", None),
        (r"xox[baprs]-[A-Za-z0-9-]{10,}", "slack token", None),
        (
            r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}",
            "jwt",
            None,
        ),
        (r"/home/[A-Za-z0-9._\-]+", "home path", Some(b"/home/user")),
        (
            r"/Users/[A-Za-z0-9._\-]+",
            "users path",
            Some(b"/Users/user"),
        ),
    ];
    specs
        .iter()
        .map(|(pattern, name, allow)| Forbidden {
            name,
            pattern: regex::bytes::Regex::new(pattern).expect("forbidden pattern compiles"),
            allow: *allow,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_every_secret_shape() {
        let raw = b"Authorization: Bearer abc.def-GHI_1234567890\n\
token=ghp_abcdefghijklmnopqrstuvwxyz0123456789\n\
sk-abcdefghijklmnopqrstuv\n\
AKIA1234567890ABCDEF\n\
xoxb-1234567890-abcdefghij\n\
eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c\n\
ada@example.com lives at /home/sprooty/code and /Users/ada/code\n";
        let cleaned = sanitise(raw).expect("clean");
        let text = String::from_utf8(cleaned).unwrap();
        assert!(text.contains("REDACTED_TOKEN"), "{text}");
        assert!(text.contains("sk-REDACTED"));
        assert!(text.contains("ghx_REDACTED"));
        assert!(text.contains("AKIAREDACTEDKEY000"));
        assert!(text.contains("xoxb-REDACTED"));
        assert!(text.contains("eyJ.REDACTED.JWT"));
        assert!(text.contains("user@example.invalid"));
        assert!(text.contains("/home/user"));
        assert!(text.contains("/Users/user"));
        assert!(!text.contains("sprooty"));
    }

    #[test]
    fn the_placeholder_home_is_allowed() {
        assert_clean(b"cwd=/home/user/work").expect("placeholder is not a leak");
    }

    #[test]
    fn a_real_home_is_refused() {
        let err = assert_clean(b"cwd=/home/ada/work").unwrap_err();
        assert!(err.contains("home path"), "{err}");
    }
}
