use std::sync::LazyLock;

// What an agent did, reduced to something safe to keep. Ports
// `core/agent_activity.py`. Redaction runs before anything is kept, raw output
// is never stored, and the service tags and error flag are heuristics that say
// so. The patterns and their order are the Python's, because a difference is a
// credential kept or a useful row destroyed.

/// Python's `\w` is `str.isalnum()` or `_`, decided by Unicode 15. The engine
/// here is on Unicode 16, whose new letters are word characters it counts and
/// Python does not, so a boundary next to one drifts. The case-folding is
/// switched off inside the class: under `(?i)` the engine folds U+0345 to ι
/// and then counts that combining mark as a letter, which erases the word
/// boundary and lets a credential through. Python never case-folds a class.
const PY_WORD: &str = r"[\p{L}\p{N}_--[^\x00-\u{1c88}\u{1c8b}-\u{a7ca}\u{a7ce}-\u{a7d9}\u{a7dd}-\u{105bf}\u{105f4}-\u{10d3f}\u{10d66}-\u{10d6e}\u{10d86}-\u{10ec1}\u{10ec5}-\u{1137f}\u{1138a}\u{1138c}-\u{1138d}\u{1138f}\u{113b6}\u{113b8}-\u{113d0}\u{113d2}\u{113d4}-\u{116cf}\u{116e4}-\u{11bbf}\u{11be1}-\u{11bef}\u{11bfa}-\u{1345f}\u{143fb}-\u{160ff}\u{1611e}-\u{1612f}\u{1613a}-\u{16d3f}\u{16d6d}-\u{16d6f}\u{16d7a}-\u{18cfe}\u{18d00}-\u{1ccef}\u{1ccfa}-\u{1e5cf}\u{1e5ee}-\u{1e5ef}\u{1e5fb}-\u{2ebef}\u{2ee5e}-\u{10ffff}]]";
/// The characters an ASCII letter also matches under `(?i)`. Python case-folds
/// and the engine does not: `İ`/`ı` fold to `i`, `ſ` to `s`, `K` to `k`.
fn folded(ch: char) -> Option<&'static str> {
    match ch {
        'i' | 'I' => Some("İı"),
        's' | 'S' => Some("ſ"),
        'k' | 'K' => Some("K"),
        _ => None,
    }
}

/// The folded characters a range covers. Under `(?i)` Python's `[a-z]` matches
/// `İ` and `ı`, not only the literal `i`, so a range is widened by every letter
/// it contains.
fn folded_range(start: char, end: char) -> String {
    let mut extra = String::new();
    for (letter, fold) in [
        ('i', "İı"),
        ('I', "İı"),
        ('s', "ſ"),
        ('S', "ſ"),
        ('k', "K"),
        ('K', "K"),
    ] {
        if start <= letter && letter <= end && !extra.contains(fold) {
            extra.push_str(fold);
        }
    }
    extra
}
/// The body of `PY_WORD`, for a class that cannot hold a group. The trailing
/// `]` of the set is dropped.
const PY_WORD_CLASS: &str = r"\p{L}\p{N}_--[^\x00-\u{1c88}\u{1c8b}-\u{a7ca}\u{a7ce}-\u{a7d9}\u{a7dd}-\u{105bf}\u{105f4}-\u{10d3f}\u{10d66}-\u{10d6e}\u{10d86}-\u{10ec1}\u{10ec5}-\u{1137f}\u{1138a}\u{1138c}-\u{1138d}\u{1138f}\u{113b6}\u{113b8}-\u{113d0}\u{113d2}\u{113d4}-\u{116cf}\u{116e4}-\u{11bbf}\u{11be1}-\u{11bef}\u{11bfa}-\u{1345f}\u{143fb}-\u{160ff}\u{1611e}-\u{1612f}\u{1613a}-\u{16d3f}\u{16d6d}-\u{16d6f}\u{16d7a}-\u{18cfe}\u{18d00}-\u{1ccef}\u{1ccfa}-\u{1e5cf}\u{1e5ee}-\u{1e5ef}\u{1e5fb}-\u{2ebef}\u{2ee5e}-\u{10ffff}]";
/// Python's `\s`: Unicode whitespace plus the C0 controls U+001C to U+001F.
const PY_SPACE_CLASS: &str = r"\s\x1c-\x1f";

/// Rewrite a Python pattern so the `regex` crate reads its classes the way
/// Python does. Inside `[...]` a class stays a class: `\s` grows the C0
/// controls, `\w` becomes letters, numbers and underscore, and `\S`/`\W`/`\D`
/// become the complementary set. Outside, `\b` is a boundary and `\S` is a
/// negated space. `[\b]` is a backspace, not a boundary.
pub(super) fn python_pattern(pattern: &str) -> String {
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    let mut escaped = false;
    let mut class = false;
    let mut class_first = true;
    let mut class_prev = ' ';
    let mut range_start = ' ';
    let mut class_range = false;
    let mut class_held = false;
    let mut ignore_case = false;
    let mut group = 0;
    #[allow(clippy::while_let_on_iterator)]
    while let Some(ch) = chars.next() {
        if escaped {
            if class {
                match ch {
                    'w' => out.push_str(&format!("[{PY_WORD_CLASS}]")),
                    'W' => out.push_str(r"\W"),
                    's' => out.push_str(PY_SPACE_CLASS),
                    // Python's non-whitespace, as ranges: a class cannot hold
                    // the group `\S` becomes outside one.
                    'S' => out.push_str(r"\x00-\x08\x0e-\x1b\x21-\x84\x86-\x9f\u{a1}-\u{167f}\u{1681}-\u{1fff}\u{200b}-\u{2027}\u{202a}-\u{202e}\u{2030}-\u{205e}\u{2060}-\u{2fff}\u{3001}-\u{10ffff}"),
                    'd' => out.push_str(r"\d"),
                    'D' => out.push_str(r"\D"),
                    'b' => out.push('\u{0008}'),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            } else {
                match ch {
                    'b' => out.push_str(&format!(
                        r"(?-i:(?:(?<!{PY_WORD})(?={PY_WORD})|(?<={PY_WORD})(?!{PY_WORD})))"
                    )),
                    'B' => out.push_str(&format!(
                        r"(?-i:(?:(?<!{PY_WORD})(?!{PY_WORD})|(?<={PY_WORD})(?={PY_WORD})))"
                    )),
                    'w' => out.push_str(&format!("(?-i:{PY_WORD})")),
                    'W' => out.push_str(&format!(r"(?:(?!(?-i:{PY_WORD}))(?s:.))")),
                    's' => out.push_str(&format!("[{PY_SPACE_CLASS}]")),
                    'S' => out.push_str(&format!(r"(?:(?![{PY_SPACE_CLASS}])(?s:.))")),
                    'd' => out.push_str(r"\d"),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '(' && chars.peek() == Some(&'?') {
            out.push(ch);
            let mut flag = String::new();
            let mut closed = false;
            let mut named = false;
            while let Some(next) = chars.next() {
                out.push(next);
                if next == ':' || next == '=' || next == '!' {
                    // A group body or a lookahead. The body is translated by the
                    // loop, not copied, and it is not a flag string. A `=` after
                    // `P` is a backreference, whose name is copied unchanged.
                    if next == '=' && flag.ends_with('P') {
                        named = true;
                        while let Some(in_name) = chars.next() {
                            out.push(in_name);
                            if in_name == ')' {
                                closed = true;
                                break;
                            }
                        }
                    }
                    break;
                }
                if next == '<' {
                    // A lookbehind, or a named group. The name must be copied
                    // unchanged: folding it makes `(?P<kk>x)` fail to compile.
                    let name = matches!(flag.chars().last(), Some('P'))
                        || chars.peek() != Some(&'=') && chars.peek() != Some(&'!');
                    if name {
                        named = true;
                        while let Some(in_name) = chars.next() {
                            out.push(in_name);
                            if in_name == '>' {
                                break;
                            }
                        }
                    }
                    break;
                }
                if next == ')' {
                    closed = true;
                    break;
                }
                flag.push(next);
            }
            let flags = flag.trim_start_matches('?');
            if !named && !flags.starts_with('P') {
                if let Some(rest) = flags.strip_prefix('-') {
                    if rest.contains('i') {
                        ignore_case = false;
                    }
                } else if flags.contains('i') {
                    ignore_case = true;
                }
            }
            if !closed {
                group += 1;
            }
        } else if ch == '(' && !class {
            group += 1;
            out.push(ch);
        } else if ch == ')' && !class && group > 0 {
            group -= 1;
            out.push(ch);
        } else if ch == '[' && !class && starts_any_class(&mut chars) {
            out.push_str("(?s:.)");
        } else if ch == '[' && !class {
            class = true;
            class_first = true;
            class_prev = ' ';
            range_start = ' ';
            class_range = false;
            class_held = false;
            out.push(ch);
        } else if ch == ']' && class {
            if class_held && ignore_case {
                if let Some(extra) = folded(class_prev) {
                    out.push_str(extra);
                }
            }
            class = false;
            out.push(ch);
        } else if class {
            // A '-' is a range operator only between two endpoints. One that
            // ends the class, or follows another range, is a literal, so the
            // trailing '-' of `[A-Za-z0-9._~+/=-]` still matches a dash.
            let range = ch == '-'
                && !class_first
                && class_prev != '-'
                && !class_range
                && !matches!(chars.peek(), Some(']') | None);
            out.push(ch);
            let opens_range = range;
            if opens_range {
                range_start = class_prev;
                class_range = true;
                class_held = false;
            }
            if class_range && ignore_case && ch != '-' {
                // After the end, not after the start: `[k-z]` would otherwise
                // read `[kK-z]`, a range whose endpoints no longer sort.
                out.push_str(&folded_range(range_start, ch));
            } else if !opens_range && !class_range && ignore_case {
                // A letter's fold waits one character. Emitted immediately it
                // would land between a range's start and its dash.
                if class_held {
                    if let Some(extra) = folded(class_prev) {
                        out.push_str(extra);
                    }
                }
                class_held = folded(ch).is_some();
            }
            if !class_range && !opens_range && ch == ']' {
                class_held = false;
            }
            class_prev = ch;
            class_first = false;
            if ch != '-' {
                class_range = false;
            }
        } else if ignore_case && folded(ch).is_some() {
            let extra = folded(ch).expect("checked");
            out.push_str(&format!("[{ch}{extra}]"));
        } else {
            out.push(ch);
        }
    }
    out
}

/// `[\s\S]` and its swaps mean "any character, including a newline". Consumes
/// the class when the iterator starts with one.
fn starts_any_class(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    for pair in ["\\s\\S", "\\S\\s", "\\w\\W", "\\W\\w", "\\d\\D", "\\D\\d"] {
        let class = format!("{pair}]");
        let ahead: String = chars.clone().take(class.chars().count()).collect();
        if ahead == class {
            chars.nth(class.chars().count() - 1);
            return true;
        }
    }
    false
}

/// `re.sub` over a translated pattern: a `$1` in the replacement is the first
/// group, and `$$` is a literal dollar.
fn sub(pattern: &fancy_regex::Regex, replacement: &str, text: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0;
    let mut matches = pattern.captures_iter(text);
    loop {
        // A backtrack limit yields `Err` and does not advance the iterator, so
        // looping on it hangs. The rest of the text is redacted whole: failing
        // closed beats returning raw text that may hold a secret.
        let found = match matches.next() {
            Some(Ok(found)) => found,
            Some(Err(_)) => {
                return format!("{out}[REDACTED]");
            }
            None => break,
        };
        let whole = found.get(0).expect("match");
        out.push_str(&text[cursor..whole.start()]);
        let mut chars = replacement.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '$' {
                match chars.peek() {
                    Some('$') => {
                        chars.next();
                        out.push('$');
                    }
                    Some(digit) if digit.is_ascii_digit() => {
                        let index = digit.to_digit(10).expect("digit") as usize;
                        chars.next();
                        if let Some(group) = found.get(index) {
                            out.push_str(group.as_str());
                        }
                    }
                    _ => out.push('$'),
                }
            } else {
                out.push(ch);
            }
        }
        cursor = whole.end();
    }
    out.push_str(&text[cursor..]);
    out
}

fn python_regex(pattern: &str) -> fancy_regex::Regex {
    let translated = python_pattern(pattern);
    fancy_regex::Regex::new(&translated).unwrap_or_else(|err| panic!("pattern {translated}: {err}"))
}

pub const SUMMARY_LIMIT: usize = 300;
pub const EXCERPT_HEAD: usize = 200;
pub const EXCERPT_TAIL: usize = 200;
pub const SCAN_WINDOW: usize = 8_192;
pub const REDACTED: &str = "[REDACTED]";
pub const WITHHELD: &str = "[withheld: configuration or environment output]";

static PEM_BLOCK: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"(?s)-----BEGIN [A-Z0-9 ]+-----.*?(?:-----END [A-Z0-9 ]+-----|\z)")
        .expect("pattern")
});
static JWT_SHAPE: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| python_regex(r"\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}"));
static TOKEN_SHAPES: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    let source = concat!(
        r"(?:",
        r"\bgh[pousr]_[A-Za-z0-9]{20,}",
        r"|\bgithub_pat_[A-Za-z0-9_]{20,}",
        r"|\bglpat-[A-Za-z0-9_-]{16,}",
        r"|\bsk-[A-Za-z0-9_-]{16,}",
        r"|\bxox[abposr]-[A-Za-z0-9-]{10,}",
        r"|\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        r"|\bAIza[0-9A-Za-z_-]{30,}",
        r"|\bya29\.[0-9A-Za-z_-]{20,}",
        r"|\bnpm_[A-Za-z0-9]{30,}",
        r"|\bhf_[A-Za-z0-9]{30,}",
        r"|\bst\.[A-Za-z0-9-]{8,}\.[A-Fa-f0-9]{16,}\.[A-Fa-f0-9]{16,}",
        r"|\b(?:pk|rk|sk)_(?:live|test)_[A-Za-z0-9]{16,}",
        r"|\bAGE-SECRET-KEY-1[0-9A-Z]{20,}",
        r")",
    );
    let translated = python_pattern(source);
    fancy_regex::Regex::new(&translated).expect("pattern")
});
static AUTH_HEADER: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    python_regex(
        "(?i)\\b((?:authorization|x-api-key|api-key|x-auth-token|private-token)\\s*[:=]\\s*(?:bearer\\s+|basic\\s+|token\\s+)?)[^\\s\"',;]+",
    )
});
static BEARER_TOKEN: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| python_regex(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{8,}"));
static URL_USERINFO: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| python_regex(r"(://)[^/\s:@]+:[^/\s@]+@"));

const SECRET_NAME: &str = concat!(
    r"[A-Za-z0-9_.-]*(?:token|secret|passw(?:or)?d|passwd|pwd|api[_-]?key|apikey",
    r"|private[_-]?key|access[_-]?key|client[_-]?secret|credential|auth[_-]?key",
    r"|session[_-]?key|signing[_-]?key|webhook[_-]?url|dsn)[A-Za-z0-9_.-]*"
);

static JSON_PAIR: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(&python_pattern(&format!(
        r#"(?i)("{SECRET_NAME}"\s*:\s*)("(?:[^"\\\n]|\\.)*"|[^\s,}}\]]+)"#
    )))
    .expect("pattern")
});
static FLAG_VALUE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(&python_pattern(&format!(
        r#"(?i)(--?{SECRET_NAME}[ =])("[^"\n]*"|'[^'\n]*'|[^\s"']+)"#
    )))
    .expect("pattern")
});
static ASSIGNMENT: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(&python_pattern(&format!(
        r#"(?i)\b({SECRET_NAME})(\s*[=:]\s*)("[^"\n]*"|'[^'\n]*'|[^\s"',;\}}&]+)"#
    )))
    .expect("pattern")
});
static LONG_HEX: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| python_regex(r"\b[A-Fa-f0-9]{48,}\b"));
static LONG_BLOB: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[A-Za-z0-9+_=-]{32,}").expect("pattern"));

/// Replace every credential-shaped substring. Whole blocks first, then named
/// shapes, then pairs whose name marks the value, then the shape-only fallback.
pub fn activity_redact(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = sub(&PEM_BLOCK, "[REDACTED:pem]", text);
    out = sub(&JWT_SHAPE, "[REDACTED:jwt]", &out);
    out = sub(&TOKEN_SHAPES, "[REDACTED:token]", &out);
    out = sub(&AUTH_HEADER, &format!("$1{REDACTED}"), &out);
    out = sub(&BEARER_TOKEN, &format!("$1{REDACTED}"), &out);
    out = sub(&URL_USERINFO, "$1[REDACTED]@", &out);
    out = sub(&JSON_PAIR, &format!("$1\"{REDACTED}\""), &out);
    out = sub(&FLAG_VALUE, &format!("$1{REDACTED}"), &out);
    out = sub(&ASSIGNMENT, &format!("$1$2{REDACTED}"), &out);
    out = sub(&LONG_HEX, REDACTED, &out);
    let mut kept = String::new();
    let mut cursor = 0;
    for found in LONG_BLOB.find_iter(&out) {
        kept.push_str(&out[cursor..found.start()]);
        kept.push_str(&activity_blob(found.as_str()));
        cursor = found.end();
    }
    kept.push_str(&out[cursor..]);
    kept
}

/// A long mixed-case alphanumeric with a digit is random by construction. A
/// commit id, a path segment or a word is not, and is kept.
fn activity_blob(text: &str) -> String {
    let mixed = text.chars().any(|c| c.is_ascii_digit())
        && text.chars().any(|c| c.is_ascii_uppercase())
        && text.chars().any(|c| c.is_ascii_lowercase());
    if mixed {
        REDACTED.to_string()
    } else {
        text.to_string()
    }
}

static DUMP_COMMAND: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    let source = concat!(
        r"(?i)\.config\.environment|\bconfig\.environment\b|\bprintenv\b",
        r"|(?:^|[;&|(]\s*|\bsudo\s+)env\s*(?:$|[;&|)])|\bdeclare\s+-x\b|\bexport\s+-p\b",
        r"|\bkubectl\s+config\s+view|kube/?config\b|\bk3s\.yaml\b",
        r"|\bcat\s+[^\s|;]*\.env\b|/proc/[^\s]*/environ\b",
        r"|\binfisical\s+(?:secrets|export|run)\b|\bsecrets?\s+(?:get|export|list|show)\b",
        r"|\bGetStack\b|\bGetVariable\b|\bListVariables\b|\bvault\s+(?:kv\s+)?read\b",
    );
    let translated = python_pattern(source);
    fancy_regex::RegexBuilder::new(&translated)
        .backtrack_limit(10_000_000)
        .build()
        .expect("pattern")
});
static KUBECONFIG_SHAPE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    let source = concat!(
        r"(?is)client-(?:key|certificate)-data\s*:|certificate-authority-data\s*:",
        r"|\bkind\s*:\s*Config\b[\s\S]*\busers\s*:",
    );
    let translated = python_pattern(source);
    fancy_regex::Regex::new(&translated).expect("pattern")
});
// No lookaround, so the linear engine: the fancy VM backtracks over the blank
// lines between assignments and gives up on a long dump.
static ENV_LINE: LazyLock<regex::Regex> = LazyLock::new(|| {
    // The regex crate's `\s` stops at U+001F; Python's includes U+001C–U+001F,
    // so a dump whose lines are prefixed with one would not be counted.
    regex::Regex::new(
        r"(?m)^[\s\x1c-\x1f]*(?:export[\s\x1c-\x1f]+)?[A-Z][A-Z0-9_]{2,}=[^\s\x1c-\x1f]",
    )
    .expect("pattern")
});

pub fn dumps_secrets(command: &str) -> bool {
    // An engine that gives up is not evidence of safety. Python has no backtrack
    // limit, so an error here withholds, the way a match does.
    DUMP_COMMAND.is_match(command).unwrap_or(true)
}

pub fn looks_like_dump(output: &str) -> bool {
    if KUBECONFIG_SHAPE.is_match(output).unwrap_or(true) {
        return true;
    }
    let window: String = output.chars().take(SCAN_WINDOW).collect();
    ENV_LINE.find_iter(&window).count() >= 3
}

fn one_line(text: &str) -> String {
    text.split(|ch: char| ch.is_whitespace() || matches!(ch, '\u{1c}'..='\u{1f}'))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn cut(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(limit - 1).collect::<String>())
    }
}

pub fn excerpt(output: &str) -> String {
    if output.chars().count() <= 2 * SCAN_WINDOW {
        let cleaned = one_line(&activity_redact(output));
        if cleaned.chars().count() <= EXCERPT_HEAD + EXCERPT_TAIL {
            return cleaned;
        }
        let head: String = cleaned.chars().take(EXCERPT_HEAD).collect();
        let tail: String = cleaned
            .chars()
            .rev()
            .take(EXCERPT_TAIL)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return format!("{head} … {tail}");
    }
    let head_src: String = output.chars().take(SCAN_WINDOW).collect();
    let head: String = one_line(&activity_redact(&head_src))
        .chars()
        .take(EXCERPT_HEAD)
        .collect();
    let tail_src: String = output
        .chars()
        .rev()
        .take(SCAN_WINDOW)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let tail: String = one_line(&activity_redact(&tail_src))
        .chars()
        .rev()
        .take(EXCERPT_TAIL)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head} … {tail}")
}

const SUMMARY_FIELDS: &[&str] = &[
    "command",
    "cmd",
    "file_path",
    "path",
    "notebook_path",
    "pattern",
    "query",
    "url",
    "description",
    "prompt",
    "skill",
];

/// One redacted line saying what a call did.
pub fn summarize_input(call_input: &serde_json::Value) -> String {
    let text = match call_input {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Object(map) => {
            let picked: Vec<String> = SUMMARY_FIELDS
                .iter()
                .filter_map(|key| match map.get(*key) {
                    Some(serde_json::Value::Null) => None,
                    Some(serde_json::Value::String(text)) if text.is_empty() => None,
                    Some(serde_json::Value::String(text)) => Some(text.clone()),
                    Some(value) => Some(crate::decisions::py_display(value)),
                    None => None,
                })
                .collect();
            if picked.is_empty() {
                crate::decisions::python_json_dumps(call_input, true)
            } else {
                picked.into_iter().take(2).collect::<Vec<_>>().join(" ")
            }
        }
        serde_json::Value::Array(parts) => parts
            .iter()
            .map(crate::decisions::py_display)
            .collect::<Vec<_>>()
            .join(" "),
        serde_json::Value::Null => String::new(),
        other => crate::decisions::py_display(other),
    };
    let window: String = text.chars().take(4 * SCAN_WINDOW).collect();
    cut(&one_line(&activity_redact(&window)), SUMMARY_LIMIT)
}

/// The default service heuristics. An operator's table extends and overrides it.
pub const DEFAULT_SERVICES: &[(&str, &str)] = &[
    (
        "github",
        r"\bgh\s+(?:pr|run|api|workflow|release|repo|issue|secret|variable|auth)\b|api\.github\.com|github\.com/|mcp__github",
    ),
    (
        "git",
        r"\bgit\s+(?:push|pull|fetch|clone|rebase|merge|commit|checkout|switch)\b",
    ),
    (
        "docker",
        r"\bdocker(?:\s+compose|-compose)?\s+\w+|\bghcr\.io\b|\bpodman\b",
    ),
    ("kubernetes", r"\bkubectl\b|\bhelm\s|\bk3s\b|\bkubeconfig\b"),
    (
        "komodo",
        r"\bkomodo\b|\bDeployStack\b|\bGetStack\b|\bWriteStackFile\b",
    ),
    ("infisical", r"\binfisical\b"),
    ("cloudflare", r"\bcloudflare|\bwrangler\b|\bcloudflared\b"),
    ("caddy", r"\bcaddy\b|\bCaddyfile\b"),
    ("tailscale", r"\btailscale\b|\btailnet\b"),
    ("dns", r"\bnslookup\b|\bdig\s+\S|\bresolvectl\b"),
    ("forgejo", r"\bforgejo\b|\bgitea\b"),
    ("woodpecker", r"\bwoodpecker\b"),
    ("firebase", r"\bfirebase\b|\bfcm\b"),
    (
        "play",
        r"\bandroidpublisher\b|\bfastlane\b|\bgoogle play\b|\bplay console\b",
    ),
    ("ssh", r#"(?:^|[\s;&|("'])(?:ssh|scp|rsync)\s"#),
    (
        "http",
        r#"(?:^|[\s;&|("'])(?:curl|wget|http)\s|\bWebFetch\b"#,
    ),
    ("vogt", r"\bmcp__vogt__|\bvogt\s+\w+"),
    ("cadastre", r"\bmcp__cadastre__"),
];

pub struct ServiceMatcher {
    patterns: Vec<(String, fancy_regex::Regex)>,
}

impl ServiceMatcher {
    pub fn build(overrides: &[(&str, &str)]) -> Result<Self, String> {
        let mut merged: Vec<(String, String)> = DEFAULT_SERVICES
            .iter()
            .map(|(name, pattern)| ((*name).to_string(), (*pattern).to_string()))
            .collect();
        for (name, pattern) in overrides {
            if pattern.is_empty() {
                merged.retain(|(existing, _)| existing != name);
            } else if let Some(slot) = merged.iter_mut().find(|(existing, _)| existing == name) {
                slot.1 = (*pattern).to_string();
            } else {
                merged.push(((*name).to_string(), (*pattern).to_string()));
            }
        }
        merged.sort_by(|left, right| left.0.cmp(&right.0));
        let mut patterns = Vec::new();
        for (name, pattern) in merged {
            patterns.push((
                name,
                fancy_regex::Regex::new(&python_pattern(&format!("(?i){pattern}")))
                    .map_err(|error| error.to_string())?,
            ));
        }
        Ok(Self { patterns })
    }

    pub fn tags(&self, tool: &str, call_input: &serde_json::Value) -> Vec<String> {
        let text = match call_input {
            serde_json::Value::String(text) => text.clone(),
            other => crate::decisions::python_json_dumps(other, true),
        };
        let window: String = text.chars().take(4 * SCAN_WINDOW).collect();
        let haystack = format!("{tool} {window}");
        self.patterns
            .iter()
            .filter(|(_, pattern)| pattern.is_match(&haystack).unwrap_or(true))
            .map(|(name, _)| name.clone())
            .collect()
    }
}

const SHELL_TOOLS: &[&str] = &[
    "Bash",
    "BashOutput",
    "exec",
    "exec_command",
    "shell",
    "write_stdin",
];
const FAILURE_WINDOW: usize = 1_000;

static EXIT_STATUS: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    python_regex(concat!(
        r"(?im)^\s*(?:Script failed\b|Process exited with code [1-9]",
        r"|Exit code:?\s*[1-9]|exit status [1-9]|Command failed with exit code [1-9])",
    ))
});
static FAILURE_LINE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    python_regex(concat!(
        r"(?im)^\s*(?:error\b|fatal:|ERROR\b|Error:|panic:",
        r"|Traceback \(most recent call last\)",
        r"|.*\bcommand not found\b|.*\bpermission denied\b|.*\bNo such file or directory\b",
        r"|.*\b(?:401 Unauthorized|403 Forbidden|HTTP (?:401|403|404|500|502|503|504))\b",
        r"|.*\bconnection refused\b|.*\b(?:timed out|deadline exceeded)\b|<tool_use_error>)",
    ))
});

pub fn is_error(output: &str, flagged: Option<bool>, tool: &str) -> bool {
    if flagged == Some(true) {
        return true;
    }
    let window: String = output.chars().take(FAILURE_WINDOW).collect();
    if EXIT_STATUS.is_match(&window).unwrap_or(true) {
        return true;
    }
    if !SHELL_TOOLS.contains(&tool) {
        return false;
    }
    FAILURE_LINE.is_match(&window).unwrap_or(true)
}

pub fn result_excerpt(output: &str, error: bool, withheld: bool) -> Option<String> {
    if !error {
        return None;
    }
    if withheld || looks_like_dump(&output.chars().take(2 * SCAN_WINDOW).collect::<String>()) {
        return Some(WITHHELD.to_string());
    }
    Some(excerpt(output))
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    /// Secrets assembled from pieces, the way the Python test builds them, so
    /// this file never holds a string a scanner would read as a credential.
    fn github_classic() -> String {
        format!("gh{}{}", "p_", "Zq3".repeat(12))
    }

    fn password() -> String {
        format!("{}{}{}", "Tr0ub4dor", "&3-", "horse-battery")
    }

    fn hex_key() -> String {
        "9f".repeat(32)
    }

    #[test]
    fn a_named_secret_and_a_token_shape_are_removed_and_a_commit_id_survives() {
        for (text, secret) in [
            (
                format!("export GH_TOKEN={}", github_classic()),
                github_classic(),
            ),
            (
                format!("DB_PASSWORD='{}' ./migrate", password()),
                password(),
            ),
            (format!("echo {} > key.bin", hex_key()), hex_key()),
        ] {
            let redacted = activity_redact(&text);
            assert!(!redacted.contains(&secret), "{redacted}");
            assert!(redacted.contains("REDACTED"));
        }
        let sha = "4a4cf5a0".repeat(5);
        let kept = format!("git show {sha} -- src/vogt/storage");
        assert_eq!(activity_redact(&kept), kept);
    }

    #[test]
    fn a_private_key_block_goes_whole_even_when_cut_off() {
        let body = format!(
            "-----BEGIN {}-----\n{}{}\n-----END {}-----",
            "OPENSSH PRIVATE KEY",
            "b3BlbnNzaC1rZXktdjEAAAAA",
            "BG5vbmUAAAAEbm9uZQ",
            "OPENSSH PRIVATE KEY"
        );
        let redacted = activity_redact(&format!("cat key\n{body}\ndone"));
        assert!(!redacted.contains("PRIVATE KEY"));
        assert!(!redacted.contains("b3BlbnNzaC1rZXkt"));
        assert!(redacted.contains("[REDACTED:pem]"));
        assert!(redacted.starts_with("cat key"));
        assert!(redacted.ends_with("done"));

        let cut = &body[..body.find("-----END").expect("end")];
        assert!(activity_redact(&format!("prefix {cut}")).contains("[REDACTED:pem]"));
    }

    #[test]
    fn a_dump_is_withheld_and_only_a_failure_keeps_an_excerpt() {
        assert!(dumps_secrets("printenv | sort"));
        assert!(dumps_secrets("kubectl config view --raw"));
        assert!(dumps_secrets("cat deploy/.env"));
        assert!(!dumps_secrets("git status --short"));
        assert_eq!(
            result_excerpt("anything", true, true).as_deref(),
            Some(WITHHELD)
        );
        let env = "HOME=/root\nPATH=/usr/bin\nSHELL=/bin/sh\n";
        assert!(looks_like_dump(env));
        assert!(!looks_like_dump("error: build failed\nsee log"));
        assert!(result_excerpt("all good", false, false).is_none());
        assert!(is_error("fatal: not a git repository", Some(false), "Bash"));
        assert!(!is_error(
            "Script completed\nWall time 1.0 seconds",
            None,
            ""
        ));
        let tags = ServiceMatcher::build(&[])
            .expect("default patterns")
            .tags("Bash", &serde_json::json!({"command": "gh pr view 1"}));
        assert!(tags.contains(&"github".to_string()), "{tags:?}");

        // A combining mark is a boundary in Python, so the value is still redacted.
        let marked = format!("x\u{0301}password={}", "hunter2");
        assert!(!activity_redact(&marked).contains("hunter2"));
        // The env window is the first 8192 characters, and a line only counts
        // when its match ends inside it. At 4100 accented characters the first
        // line starts past the window, so only two count and nothing is withheld.
        assert!(!looks_like_dump(&format!("{}{env}", "é".repeat(4100))));
        assert!(looks_like_dump(&format!("{env}{}", "é".repeat(4100))));

        // The window is counted in characters: a multi-byte character at byte
        // 8192 must not panic, and must not change which half is kept.
        let long = format!("Error: {}", "é".repeat(9_000));
        let excerpted = excerpt(&long);
        assert!(excerpted.starts_with("Error:"));
        assert!(excerpted.contains('é'));

        let summary = summarize_input(&serde_json::json!({"command": ["bash", "-lc", "ls"]}));
        assert_eq!(summary, "['bash', '-lc', 'ls']");
        assert_eq!(
            summarize_input(&serde_json::json!({"pattern": true})),
            "True"
        );

        // A kubeconfig is withheld even with a newline between its markers, and
        // a word boundary holds next to a combining mark.
        assert!(looks_like_dump("kind: Config\nclusters:\n- x\nusers:\n"));
        let marked = format!("\t\u{345}sk-{}", "ab".repeat(9));
        assert!(activity_redact(&marked).contains("[REDACTED:token]"));
        assert_eq!(summarize_input(&serde_json::json!(["it's", 1])), "it's 1");

        // `(?i)` folds İ and ı onto i, so a dotted capital still names the header.
        let dotted = format!("AUTHOR{}ZATION: {}", "\u{130}", "s3cr3tvalue");
        assert!(
            !activity_redact(&dotted).contains("s3cr3tvalue"),
            "{dotted}"
        );
        let dotless = format!("author{}zation: {}", "\u{131}", "s3cr3tvalue");
        assert!(
            !activity_redact(&dotless).contains("s3cr3tvalue"),
            "{dotless}"
        );

        // U+1CCF0 is a word character in Unicode 16 but not in Python's Unicode
        // 15, so the boundary beside it holds and the run is still redacted.
        let hex = "a".repeat(48);
        let newer = format!("{hex}\u{1ccf0}");
        assert!(!activity_redact(&newer).contains(&hex), "{newer}");

        // `(?i)` widens a range, not only a literal, so `ı` inside `[A-Za-z0-9]`
        // is still part of the token and the tail does not survive.
        let ranged = format!("Bearer secrettoKen{};", "\u{131}");
        let redacted = activity_redact(&ranged);
        assert!(redacted.contains("[REDACTED]"), "{redacted}");
        assert!(!redacted.contains('\u{131}'), "{redacted}");
        let tail = format!("Bearer abcdefgh{}SECRETSECRETTAIL", "\u{131}");
        assert!(!activity_redact(&tail).contains("TAIL"), "{tail}");
        let named = format!("password{}x=hunter2", "\u{131}");
        assert!(!activity_redact(&named).contains("hunter2"), "{named}");

        // A class inside a group stays a class: the escapes are rewritten, not
        // pasted in as the literal text of the rewrite.
        let grouped = python_pattern(r"(?i)(?:[\w.-]+)");
        assert!(
            !grouped.contains("?-i:"),
            "a class rewrite leaked as text: {grouped}"
        );

        // A trailing '-' is a literal dash, and a range's folded characters come
        // after the range rather than inside it.
        assert!(!activity_redact("Bearer abcd-efgh-SECRETTAIL").contains("SECRETTAIL"));
        assert!(!activity_redact("x bearer ab-cdefghSECRETTAIL").contains("SECRETTAIL"));
        assert!(!activity_redact(r#""secret-token": "hunter2""#).contains("hunter2"));
        let assigned = activity_redact("password=hunter2:x");
        assert!(
            !assigned.contains("hunter2"),
            "the value survived: {assigned}"
        );
        let ranged = python_pattern("(?i)[a-z]");
        assert!(
            ranged.contains("z"),
            "the range end was swallowed: {ranged}"
        );
        // The fold spans the range's real start, not the dash that precedes the
        // end, so [h-j] does not take in K, S or İ.
        let narrow = python_pattern("(?i)[h-j]");
        assert!(
            !narrow.contains('\u{212a}'),
            "the fold spanned the dash: {narrow}"
        );
        // A range whose start itself folds must still compile: the extras come
        // after the end, so `[k-z]` is not read as a range from `k` to `ſ`.
        for pattern in [
            "(?i)[k-z]",
            "(?i)[I-Z]",
            "(?i)[S-Z]",
            "(?i)[^i-n]",
            "(?i)[i-i]",
        ] {
            let translated = python_pattern(pattern);
            fancy_regex::Regex::new(&translated)
                .unwrap_or_else(|err| panic!("{pattern} did not compile: {translated}: {err}"));
        }
        // A lookbehind's body is translated too, so its `\w` is Python's word
        // class rather than the engine's.
        let lookbehind = python_pattern(r"(?i)(?<!\w)tok");
        assert!(
            lookbehind.contains(PY_WORD_CLASS),
            "the lookbehind was copied raw: {lookbehind}"
        );
        // A lookahead is translated too, and its text is not read as flags.
        let lookahead = python_pattern(r"(?!\w)\S");
        assert!(
            lookahead.contains(PY_WORD_CLASS),
            "the lookahead was copied raw: {lookahead}"
        );
        let not_flags = python_pattern(r"(?!xi)k");
        assert_eq!(
            not_flags, r"(?!xi)k",
            "the lookahead set flags: {not_flags}"
        );
        // A named group's name is copied unchanged, so it still compiles.
        let named = python_pattern(r"(?i)(?P<kk>x)");
        fancy_regex::Regex::new(&named)
            .unwrap_or_else(|err| panic!("the group name was folded: {named}: {err}"));
        // A backreference's name is copied too, so it still compiles.
        let backref = python_pattern(r"(?i)(?P<sik>k)(?P=sik)");
        fancy_regex::Regex::new(&backref)
            .unwrap_or_else(|err| panic!("the backreference was folded: {backref}: {err}"));
    }

    #[test]
    fn a_quoted_string_matches_python_repr() {
        // `py_quote` is Python's repr: an apostrophe inside double quotes is not
        // escaped, and a control character is.
        let quoted = crate::decisions::py_display(&serde_json::json!(["it's"]));
        assert_eq!(quoted, r#"["it's"]"#);
        let control = crate::decisions::py_display(&serde_json::json!(["a\u{000b}b"]));
        assert_eq!(control, r#"['a\x0bb']"#);
        // `str(True)` is `True`, and a small float uses an exponent.
        assert_eq!(summarize_input(&serde_json::json!(true)), "True");
        assert_eq!(
            crate::decisions::py_display(&serde_json::json!(1e-7)),
            "1e-07"
        );
        assert_eq!(
            crate::decisions::py_display(&serde_json::json!(1e-5)),
            "1e-05"
        );
        assert_eq!(
            crate::decisions::py_display(&serde_json::json!(1e16)),
            "1e+16"
        );
        assert_eq!(
            crate::decisions::py_display(&serde_json::json!(2.5e20)),
            "2.5e+20"
        );
        assert_eq!(
            crate::decisions::py_display(&serde_json::json!(1.2345678901234567e-5)),
            "1.2345678901234568e-05"
        );
        assert_eq!(
            crate::decisions::py_display(&serde_json::json!(10_000_000_000_000_000u64)),
            "10000000000000000"
        );
    }

    #[test]
    fn a_backtrack_limit_redacts_instead_of_hanging() {
        // fancy-regex yields `Err` without advancing when it gives up, so
        // iterating it naively never returns. The window is inside Python's
        // 32,768-character summary, and the tail carries a real token.
        let noise = "\\éb\u{a7cb}\u{2028}\u{1c89}\"\u{2029}\u{0}".repeat(4_000);
        let input = format!("{noise} ghp_{}", "a".repeat(30));
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(summarize_input(&serde_json::json!(input)));
        });
        let summary = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("summarize_input hung on a backtrack limit");
        assert!(!summary.contains("ghp_"), "{summary}");
    }

    #[test]
    fn a_long_dump_is_still_withheld() {
        // Blank lines between assignments used to exhaust the backtrack limit,
        // and the error was read as "not a dump", so the secrets were printed.
        let dump = "SMTP_HOST=mail.internal\nSMTP_USER=ops\nSMTP_PASS=correct-horse-battery\nADMIN_PIN=482913\n".to_string()
            + &"\n".repeat(1000);
        assert!(looks_like_dump(&dump), "the env dump was not recognised");
        let excerpt = result_excerpt(&dump, true, false).unwrap();
        assert!(!excerpt.contains("SMTP_PASS"), "{excerpt}");
        assert!(!excerpt.contains("482913"), "{excerpt}");

        let command = "\\\"=:/ ́".repeat(4000) + " && infisical secrets get SMTP_PASS --plain";
        assert!(
            dumps_secrets(&command),
            "the dump command was not recognised"
        );

        // Python's `\s` includes U+001C–U+001F, which the regex crate's does not.
        let prefixed = "\u{1c}FOO=1\n\u{1c}BAR=2\n\u{1c}BAZ=3\n";
        assert!(
            looks_like_dump(prefixed),
            "the prefixed dump was not counted"
        );
    }
}
