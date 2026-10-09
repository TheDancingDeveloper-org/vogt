//! Small text helpers shared by the adapters.
//!
//! `python_strip` is `str.strip()`: Unicode whitespace plus the four C0
//! separators `\x1c`–`\x1f`, which Rust's `trim` does not remove.

/// `str.strip()` over the whole string.
pub fn python_strip(text: &str) -> &str {
    text.trim_matches(|ch: char| ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch))
}
