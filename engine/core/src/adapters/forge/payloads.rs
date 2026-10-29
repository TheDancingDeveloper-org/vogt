//! Payload shapes GitHub and Forgejo share. Ports `adapters/forge/_payloads.py`.

use super::models::ForgeComparison;

/// URL-quote a repository path or ref, keeping its slashes. Ports
/// `urllib.parse.quote(path.strip("/"), safe="/")`.
pub fn quote_path(path: &str) -> String {
    let trimmed = path.trim_matches('/');
    let mut out = String::with_capacity(trimmed.len());
    for byte in trimmed.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The bytes of a `contents` API answer, or `None` when it is not a file.
///
/// `base64.b64decode` accepts both the standard and URL-safe alphabets and
/// ignores non-alphabet characters, which is what this reproduces.
pub fn decoded_content(payload: &serde_json::Value) -> Option<Vec<u8>> {
    let object = payload.as_object()?;
    let kind = object
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("file");
    if kind != "file" {
        return None;
    }
    let content = object.get("content")?.as_str()?;
    decode_base64_permissive(content)
}

/// A compare answer in the shape GitHub and Forgejo share, or `None` when the
/// payload is not an object.
pub fn comparison(base: &str, head: &str, payload: &serde_json::Value) -> Option<ForgeComparison> {
    let object = payload.as_object()?;
    let mut commits: Vec<(String, String)> = Vec::new();
    if let Some(items) = object.get("commits").and_then(|v| v.as_array()) {
        for item in items {
            let Some(item) = item.as_object() else {
                continue;
            };
            let Some(sha) = item.get("sha").and_then(|v| v.as_str()) else {
                continue;
            };
            let message = item
                .get("commit")
                .and_then(|v| v.as_object())
                .and_then(|c| c.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let first = message.lines().next().unwrap_or("");
            commits.push((sha.to_owned(), first.to_owned()));
        }
    }
    let ahead_by = object
        .get("ahead_by")
        .and_then(json_int)
        .or_else(|| object.get("total_commits").and_then(json_int))
        .unwrap_or(commits.len() as i64);
    let behind_by = object.get("behind_by").and_then(json_int).unwrap_or(0);
    let status = object
        .get("status")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let head_sha = if let Some((sha, _)) = commits.last() {
        Some(sha.clone())
    } else if status.as_deref() == Some("identical") {
        Some(base.to_owned())
    } else {
        None
    };
    Some(ForgeComparison {
        base: base.to_owned(),
        head: head.to_owned(),
        head_sha,
        status,
        ahead_by,
        behind_by,
        commits,
    })
}

/// Python's `isinstance(value, int)` rejects JSON bools, which `as_i64` does
/// not, so a bool is not a count.
fn json_int(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

fn decode_base64_permissive(input: &str) -> Option<Vec<u8>> {
    fn value_of(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut buf: u32 = 0;
    let mut count = 0;
    let mut kept = 0usize;
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let Some(value) = value_of(byte) else {
            continue;
        };
        kept += 1;
        buf = (buf << 6) | u32::from(value);
        count += 1;
        if count == 4 {
            out.push((buf >> 16) as u8);
            out.push((buf >> 8) as u8);
            out.push(buf as u8);
            buf = 0;
            count = 0;
        }
    }
    // Python validates the length of the input with non-alphabet bytes removed
    // but `=` padding kept, and raises `Incorrect padding` unless that length
    // is a multiple of four. Everything up to the first pad was counted above;
    // the pads complete it.
    let pads = input.bytes().filter(|b| *b == b'=').count();
    if !(kept + pads).is_multiple_of(4) {
        return None;
    }
    if count == 2 {
        out.push((buf >> 4) as u8);
    } else if count == 3 {
        out.push((buf >> 10) as u8);
        out.push((buf >> 2) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quoting_keeps_slashes_and_encodes_the_rest() {
        assert_eq!(quote_path("/src/my file.rs"), "src/my%20file.rs");
        assert_eq!(quote_path("refs/heads/wi-7"), "refs/heads/wi-7");
        assert_eq!(quote_path("a+b"), "a%2Bb");
    }

    #[test]
    fn content_decodes_a_file_and_rejects_the_rest() {
        let payload = json!({"type": "file", "content": "aGVsbG8="});
        assert_eq!(decoded_content(&payload).unwrap(), b"hello");
        assert!(decoded_content(&json!({"type": "dir", "content": "aGVsbG8="})).is_none());
        // Non-alphabet bytes are skipped, as Python's default `b64decode` does.
        assert_eq!(decoded_content(&json!({"content": "!!!???"})).unwrap(), b"");
        // A length not divisible by four is Python's `Incorrect padding`.
        assert!(decoded_content(&json!({"content": "aGVsbG8"})).is_none());
        // Two sextets and implied padding still decode: `b64decode("aGVs")`.
        assert_eq!(
            decoded_content(&json!({"content": "aGVs"})).unwrap(),
            b"hel"
        );
        assert!(decoded_content(&json!("nope")).is_none());
    }

    #[test]
    fn comparison_reads_the_shared_envelope() {
        let payload = json!({
            "status": "ahead",
            "ahead_by": 4,
            "commits": [
                {"sha": "aaa", "commit": {"message": "first\nbody"}},
                "skip-me",
                {"sha": "bbb", "commit": {"message": ""}}
            ]
        });
        let got = comparison("main", "wi-7", &payload).unwrap();
        assert_eq!(got.ahead_by, 4);
        assert_eq!(got.behind_by, 0);
        assert_eq!(got.head_sha.as_deref(), Some("bbb"));
        assert_eq!(
            got.commits,
            vec![("aaa".into(), "first".into()), ("bbb".into(), "".into())]
        );

        let identical = comparison("main", "main", &json!({"status": "identical"})).unwrap();
        assert_eq!(identical.head_sha.as_deref(), Some("main"));

        let fallback = comparison("main", "wi-7", &json!({"total_commits": true})).unwrap();
        assert_eq!(fallback.ahead_by, 0);
        assert!(comparison("main", "wi-7", &json!([])).is_none());
    }
}
