//! Recognising an LLM provider failure in a session's own output (WI-4119).
//!
//! A Codex or Claude turn whose provider refuses it (402 billing, 429 rate
//! limit, a 5xx outage) stops at its prompt with the provider's message on
//! screen. Read from the outside that looks exactly like a session resting
//! after a turn, so the oversight table said `idle` and nobody noticed the
//! worker had died. This module names the small set of messages that mean
//! "a provider refused this turn", so a summary can report `blocked` with
//! the message instead.
//!
//! It is a text matcher over the tail of the terminal, not a parser: the
//! phrases are deliberately specific so ordinary work (a test that prints
//! status codes, a sentence using "reconnecting") is not misreported.

pub(crate) const TAIL_BYTES: usize = 8 * 1024;
const TAIL_LINES: usize = 12;
const MAX_REASON_CHARS: usize = 200;

/// The provider message in `raw` (a session's raw terminal tail), redacted
/// and truncated, when the last lines name a provider failure. `None` for
/// ordinary output and for a session that has moved on.
pub fn detect(raw: &[u8]) -> Option<String> {
    let tail = &raw[raw.len().saturating_sub(TAIL_BYTES)..];
    let text = String::from_utf8_lossy(&crate::activity::strip_ansi(tail)).into_owned();
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let start = lines.len().saturating_sub(TAIL_LINES);
    lines[start..]
        .iter()
        .rev()
        .find_map(|line| matched(line))
        .map(|line| clip(&crate::redaction::redact(&line)))
}

/// The messages a provider refusal is printed with. Deliberately specific:
/// a bare status code is not one, because ordinary work prints those.
const SIGNATURES: &[&str] = &[
    "insufficient balance",
    "insufficient_quota",
    "credit balance is too low",
    "exceeded retry limit",
    "no deployments available",
    "experiencing high demand",
    "under high demand",
    "402 payment required",
    "429 too many requests",
    "500 internal server error",
    "502 bad gateway",
    "503 service unavailable",
    "504 gateway timeout",
    "529 overloaded",
    "overloaded_error",
];

fn matched(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    if SIGNATURES.iter().any(|signature| lower.contains(signature)) {
        return Some(line.to_string());
    }
    // A CLI retrying a provider connection prints `Reconnecting 2/5`; the
    // count is what distinguishes it from ordinary prose about reconnecting.
    if lower.contains("reconnecting") && has_retry_count(&lower) {
        return Some(line.to_string());
    }
    None
}

/// Whether `text` carries a small `N/M` retry counter (`2/5`).
fn has_retry_count(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'/' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > start {
                if let Ok(limit) = text[start..end].parse::<u32>() {
                    if (1..=10).contains(&limit) {
                        return true;
                    }
                }
            }
            i = end;
        }
    }
    false
}

fn clip(reason: &str) -> String {
    reason.chars().take(MAX_REASON_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_provider_failure_shape_is_recognised() {
        for line in [
            "Error: 402 Insufficient Balance",
            "DeepseekException - { \"error\": { \"message\": \"Insufficient Balance\" } }",
            "litellm.RateLimitError: 429 Too Many Requests",
            "API Error: Reconnecting 2/5",
            "API Error: 503 Service Unavailable",
            "502 Bad Gateway",
            "The provider is under high demand right now",
            "litellm.ServiceUnavailableError: No deployments available for selected model",
            "exceeded retry limit, last status: 429",
            "{\"error\":{\"type\":\"insufficient_quota\"}}",
            "Your credit balance is too low to access the Anthropic API",
        ] {
            let reason = detect(line.as_bytes()).unwrap_or_else(|| panic!("missed: {line}"));
            assert!(!reason.is_empty(), "{line}");
        }
    }

    #[test]
    fn ordinary_output_is_not_a_provider_failure() {
        for line in [
            "the 429 handler returns 402",
            "reconnecting the database client",
            "balance the two sides of the ledger",
            "tests passed: 500",
            "HTTP status codes 402 and 429 are documented",
            "the service has high demand in the benchmark",
        ] {
            assert_eq!(detect(line.as_bytes()), None, "false positive: {line}");
        }
    }

    #[test]
    fn a_reason_is_redacted_and_bounded() {
        let long = format!("Error: 402 Insufficient Balance {}", "x".repeat(500));
        let reason = detect(long.as_bytes()).unwrap();
        assert!(reason.chars().count() <= MAX_REASON_CHARS, "{reason}");
    }
}
