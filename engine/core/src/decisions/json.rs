use serde_json::Value;
use sha2::{Digest, Sha256};

/// `json.dumps(sort_keys=True, separators=(",", ":"), ensure_ascii=True)`.
///
/// serde_json writes non-ASCII raw and renders floats in a different form, so
/// a digest computed here would not match one Python stored. Everything outside
/// 0x20..=0x7E becomes `\uXXXX`, with a surrogate pair above U+FFFF — DEL
/// (`\x7f`) included. Floats follow CPython's `repr`: the shortest round-trip
/// digit string, fixed notation for an exponent in `-4..=15`, otherwise
/// `d.ddde±XX` with an exponent of at least two digits, and `.0` on an integer.
pub fn canonical_json(payload: &Value) -> String {
    render_python_json(payload, false, true, 0)
}

/// `json.dumps` as Python stores it: `", "` / `": "` separators and
/// `ensure_ascii=True`. Keys are sorted only where the Python call sorts them
/// (`sort_keys=True` for a workflow definition, not for an exclusions list).
pub fn python_json_dumps(payload: &Value, sort_keys: bool) -> String {
    render_python_json(payload, true, sort_keys, 0)
}

/// `json.dumps` with `separators=(",", ":")`: no space after a comma or a
/// colon. This is what MCP tool text and stdio framing use; `python_json_dumps`
/// is the spaced form the stores write.
pub fn python_json_dumps_compact(payload: &Value) -> String {
    render_python_json(payload, false, false, 0)
}

/// `json.dumps(payload, indent=n)`: the same escaping as `python_json_dumps`,
/// with each nesting level indented by `n` spaces and no space before a colon.
pub fn python_json_dumps_indent(payload: &Value, indent: usize) -> String {
    render_python_json(payload, false, false, indent)
}

/// Compact `json.dumps` with sorted keys and no spaces: the form board tokens
/// and content digests are computed over.
pub fn render_python_json_compact(payload: &Value) -> String {
    render_python_json(payload, false, true, 0)
}

fn render_python_json(value: &Value, spaced: bool, sort_keys: bool, indent: usize) -> String {
    let mut out = String::new();
    write_python_json(&mut out, value, spaced, sort_keys, indent, 0);
    out
}

fn write_python_json(
    out: &mut String,
    value: &Value,
    spaced: bool,
    sort_keys: bool,
    indent: usize,
    depth: usize,
) {
    let (item_gap, key_gap) = if spaced { (", ", ": ") } else { (",", ":") };
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&python_number(number)),
        Value::String(text) => write_python_string(out, text),
        Value::Array(items) if indent > 0 && !items.is_empty() => write_indented(
            out,
            items.iter().map(|item| (None, item)).collect(),
            indent,
            depth,
        ),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(item_gap);
                }
                write_python_json(out, item, spaced, sort_keys, indent, depth + 1);
            }
            out.push(']');
        }
        Value::Object(map) if indent > 0 && !map.is_empty() => {
            let entries = map
                .iter()
                .map(|(key, value)| (Some(key.as_str()), value))
                .collect();
            write_indented(out, entries, indent, depth);
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            if sort_keys {
                keys.sort();
            }
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push_str(item_gap);
                }
                write_python_string(out, key);
                out.push_str(key_gap);
                write_python_json(out, &map[*key], spaced, sort_keys, indent, depth + 1);
            }
            out.push('}');
        }
    }
}

/// One indented array or object. Python breaks the line after the opener, pads
/// `indent` spaces per nesting level, and puts the closer back at the parent's
/// indent. An empty container stays on one line, which the caller handles.
fn write_indented(
    out: &mut String,
    entries: Vec<(Option<&str>, &Value)>,
    indent: usize,
    depth: usize,
) {
    let open = if entries.first().is_some_and(|(key, _)| key.is_some()) {
        '{'
    } else {
        '['
    };
    let close = if open == '{' { '}' } else { ']' };
    out.push(open);
    for (index, (key, value)) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('\n');
        for _ in 0..indent * (depth + 1) {
            out.push(' ');
        }
        if let Some(key) = key {
            write_python_string(out, key);
            out.push_str(": ");
        }
        write_python_json(out, value, false, false, indent, depth + 1);
    }
    out.push('\n');
    for _ in 0..indent * depth {
        out.push(' ');
    }
    out.push(close);
}

fn write_python_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if !('\u{0020}'..='\u{007e}').contains(&ch) => push_escaped(out, ch),
            ch => out.push(ch),
        }
    }
    out.push('"');
}

fn push_escaped(out: &mut String, ch: char) {
    let code = ch as u32;
    if code <= 0xFFFF {
        out.push_str(&format!("\\u{code:04x}"));
    } else {
        let adjusted = code - 0x10000;
        let high = 0xD800 + (adjusted >> 10);
        let low = 0xDC00 + (adjusted & 0x3FF);
        out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
    }
}

pub(super) fn python_number(number: &serde_json::Number) -> String {
    if let Some(value) = number.as_i64() {
        return value.to_string();
    }
    if let Some(value) = number.as_u64() {
        return value.to_string();
    }
    let Some(value) = number.as_f64() else {
        return "null".to_string();
    };
    if value.is_nan() || value.is_infinite() {
        return "null".to_string();
    }
    // CPython's `repr`: shortest round-trip digits, fixed notation while the
    // exponent sits in -4..=15, otherwise scientific with a signed exponent of
    // at least two digits. `{:e}` is not that — it omits the '+' and the
    // zero-padding, and `{:.16}` is not shortest.
    let negative = value.is_sign_negative();
    let magnitude = value.abs();
    let raw = format!("{magnitude:e}");
    let (digits, exponent) = raw.split_once('e').expect("debug exponent form");
    let mut significant: String = digits.chars().filter(|ch| *ch != '.').collect();
    significant = significant.trim_end_matches('0').to_string();
    if significant.is_empty() {
        significant.push('0');
    }
    let exp: i32 = exponent.parse().expect("debug exponent");
    let mut rendered = if (-4..16).contains(&exp) {
        fixed_notation(&significant, exp)
    } else {
        let mantissa = if significant.len() == 1 {
            significant
        } else {
            format!("{}.{}", &significant[..1], &significant[1..])
        };
        format!("{mantissa}e{exp:+03}")
    };
    if negative {
        rendered.insert(0, '-');
    }
    rendered
}

/// `digits` is the shortest significant digit string; `exp` is its order.
fn fixed_notation(digits: &str, exp: i32) -> String {
    if exp < 0 {
        return format!("0.{}{digits}", "0".repeat((-exp - 1) as usize));
    }
    let point = exp as usize + 1;
    if point >= digits.len() {
        format!("{digits}{}.0", "0".repeat(point - digits.len()))
    } else {
        format!("{}.{}", &digits[..point], &digits[point..])
    }
}

pub fn digest_of(payload: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_json(payload).as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}
