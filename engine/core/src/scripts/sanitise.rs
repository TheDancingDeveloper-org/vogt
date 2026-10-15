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
            // `allow` is a prefix, matching Python's `(?!user\b)`: `/home/user`
            // and `/home/user.x` are the placeholder, `/home/ada` is not.
            if rule
                .allow
                .is_some_and(|prefix| found.as_bytes().starts_with(prefix))
            {
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

/// Rewrite secret-shaped bytes in place, keeping non-UTF-8 bytes intact.
///
/// A transcript is raw PTY output, so this works on bytes. `regex::bytes`
/// matches the ASCII patterns and copies the surrounding bytes through, which
/// is what `scripts/sanitise_transcript.py` does. A lossy UTF-8 decode would
/// turn `0xFF 0xFE` into U+FFFD and change the fixture.
///
/// Two deliberate differences from the Python, both fixes:
///
/// - the specific shapes (`sk-`, `ghp_`, `AKIA`, `xoxb-`, JWTs) are rewritten
///   before the generic `token=` rule. Python's order turns `token=ghp_…`
///   into `token=REDACTED_TOKEN`, which hides that it was a GitHub token;
///   here it becomes `token=ghx_REDACTED`.
/// - the AKIA placeholder is `AKIAREDACTEDKEY000`. Python's
///   `AKIAREDACTEDREDACT00` matches its own forbidden `AKIA[0-9A-Z]{16}`
///   pattern, so `assert_clean` refuses every file that contained an AWS key.
fn rewrite(data: &[u8]) -> Vec<u8> {
    let rules: &[(&str, &[u8])] = &[
        (
            r"(?i)(authorization:\s*bearer\s+)[A-Za-z0-9._\-]+",
            b"${1}REDACTED_TOKEN",
        ),
        (r"sk-[A-Za-z0-9]{20,}", b"sk-REDACTED"),
        (r"gh[pousr]_[A-Za-z0-9]{20,}", b"ghx_REDACTED"),
        (r"AKIA[0-9A-Z]{16}", b"AKIAREDACTEDKEY000"),
        (r"xox[baprs]-[A-Za-z0-9-]{10,}", b"xoxb-REDACTED"),
        (
            r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}",
            b"eyJ.REDACTED.JWT",
        ),
        (
            r#"(?i)(token[=:\s\"']+)[A-Za-z0-9._\-]{16,}"#,
            b"${1}REDACTED_TOKEN",
        ),
        (
            r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
            b"user@example.invalid",
        ),
        (r"/home/[A-Za-z0-9._\-]+", b"/home/user"),
        (r"/Users/[A-Za-z0-9._\-]+", b"/Users/user"),
    ];
    let mut text = data.to_vec();
    for (pattern, replacement) in rules {
        let re = regex::bytes::Regex::new(pattern).expect("rewrite pattern compiles");
        text = re.replace_all(&text, *replacement).into_owned();
    }
    text
}

struct Forbidden {
    name: &'static str,
    pattern: regex::bytes::Regex,
    /// A match that starts with this is the placeholder, not a leak.
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
    fn non_utf8_bytes_pass_through_unchanged() {
        let raw = b"a\xff\xfeb /home/ada end";
        let cleaned = sanitise(raw).expect("clean");
        assert!(cleaned.windows(2).any(|w| w == [0xff, 0xfe]), "{cleaned:?}");
        assert!(cleaned.windows(10).any(|w| w == b"/home/user"));
        assert!(!cleaned.windows(3).any(|w| w == [0xef, 0xbf, 0xbd]));
    }

    #[test]
    fn a_home_placeholder_with_a_suffix_is_allowed() {
        assert_clean(b"cwd=/home/user.x/work").expect("user.x is the placeholder");
    }

    #[test]
    fn the_cli_writes_the_cleaned_bytes() {
        let dir = std::env::temp_dir().join(format!("vogt-sanitise-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.bin");
        let output = dir.join("out.bin");
        std::fs::write(&input, b"token=ghp_abcdefghijklmnopqrstuvwxyz0123\n").unwrap();
        run(&[input.display().to_string(), output.display().to_string()]).expect("rewritten");
        let written = std::fs::read(&output).unwrap();
        assert_eq!(written, b"token=ghx_REDACTED\n");
        run(&["--check".to_string(), output.display().to_string()]).expect("clean");
        std::fs::remove_dir_all(&dir).ok();
    }

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
