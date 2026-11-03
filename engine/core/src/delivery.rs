//! Did typed input land? Ports `src/vogt/core/delivery.py`.
//!
//! Judged from what the engine reported before the input and a few reads of
//! the screen after it. Evidence, not a promise.

#![allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Typed,
    Delivered,
    Queued,
    Stuck,
    Unconfirmed,
}

impl Delivery {
    /// The wire value, matching `delivery.py`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Typed => "typed",
            Self::Delivered => "delivered",
            Self::Queued => "queued",
            Self::Stuck => "stuck",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub activity: Option<String>,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub delivery: Delivery,
    pub evidence: String,
}

use std::sync::LazyLock;

static QUEUED_HINT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)queued messages?|message(?:s)? queued").unwrap());
static CODEX_FOOTER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\s{2}\S.* · \S").unwrap());
static CLAUDE_RULE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\s*─{20,}\s*$").unwrap());

const COMPOSER_MATCH: usize = 40;

fn codex_composer(lines: &[String]) -> Option<String> {
    let mut end = lines.len();
    while end > 0 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    let start = end.saturating_sub(2);
    let footer = (start..end).find(|&index| CODEX_FOOTER.is_match(&lines[index]))?;
    let mut index = footer.checked_sub(1)?;
    while lines[index].trim().is_empty() {
        index = index.checked_sub(1)?;
    }
    let mut wrapped = Vec::new();
    while lines[index].starts_with("  ") && !lines[index].trim().is_empty() {
        wrapped.push(lines[index].trim().to_string());
        index = index.checked_sub(1)?;
    }
    let first = lines[index].strip_prefix('›')?.trim();
    wrapped.push(first.to_string());
    wrapped.reverse();
    Some(wrapped.join(" ").trim().to_string())
}

fn claude_composer(lines: &[String]) -> Option<String> {
    let rules: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| CLAUDE_RULE.is_match(line).then_some(index))
        .collect();
    let (&top, &bottom) = (rules.get(rules.len().checked_sub(2)?)?, rules.last()?);
    let mut inside = lines[top + 1..bottom].iter().map(|line| line.trim());
    let first = inside.next()?;
    let first = first
        .strip_prefix('❯')
        .or_else(|| first.strip_prefix('>'))?
        .trim();
    let shown = std::iter::once(first)
        .chain(inside)
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if QUEUED_HINT.is_match(&shown) || (shown.starts_with("Try \"") && shown.ends_with('"')) {
        Some(String::new())
    } else {
        Some(shown)
    }
}

pub fn composer_text(lines: &[String]) -> Option<String> {
    codex_composer(lines).or_else(|| claude_composer(lines))
}

pub fn composer_holds(lines: &[String], text: &str) -> bool {
    let wanted = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(shown) = composer_text(lines) else {
        return false;
    };
    if wanted.is_empty() || shown.is_empty() {
        return false;
    }
    if text.trim().contains('\n') && shown.contains("[Pasted text #") {
        return true;
    }
    let shown = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    let length = wanted.chars().count().min(COMPOSER_MATCH);
    shown.chars().take(length).eq(wanted.chars().take(length))
}

pub fn safe_to_resubmit(seen: &Observation, text: &str) -> bool {
    !matches!(
        seen.activity.as_deref(),
        Some("awaiting-approval" | "blocked")
    ) && composer_holds(&seen.lines, text)
}

pub fn judge(
    submitted: bool,
    before: Option<&str>,
    after: &[Observation],
    pending: Option<&str>,
) -> Verdict {
    if !submitted {
        return Verdict {
            delivery: Delivery::Typed,
            evidence: "no Enter was pressed: the text is in the input, not sent".to_string(),
        };
    }
    if after
        .iter()
        .any(|seen| seen.lines.iter().any(|line| QUEUED_HINT.is_match(line)))
    {
        return Verdict {
            delivery: Delivery::Queued,
            evidence: "the agent shows a queued-message hint: it will take the input when its \
                       current turn ends"
                .to_string(),
        };
    }
    if let (Some(pending), Some(last)) = (pending, after.last()) {
        if last.activity.as_deref() != Some("running") && composer_holds(&last.lines, pending) {
            return Verdict {
                delivery: Delivery::Stuck,
                evidence: "the text is still in the input box and no turn is running: the submit did not land; read session_screen".to_string(),
            };
        }
    }
    if before == Some("running") {
        return Verdict {
            delivery: Delivery::Queued,
            evidence: "a turn was already running when it was sent; agent CLIs queue input behind \
                       the running turn"
                .to_string(),
        };
    }
    for seen in after {
        if seen.activity.as_deref() == Some("running") {
            return Verdict {
                delivery: Delivery::Delivered,
                evidence: "the agent started a turn after it".to_string(),
            };
        }
        if seen.activity.as_deref() == Some("awaiting-approval") {
            return Verdict {
                delivery: Delivery::Delivered,
                evidence: "the agent took it and is now asking for approval".to_string(),
            };
        }
    }
    Verdict {
        delivery: Delivery::Unconfirmed,
        evidence: "sent, but no turn started and no queued hint showed while Vogt watched; read \
                   session_screen"
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(activity: &str, line: &str) -> Observation {
        Observation {
            activity: (!activity.is_empty()).then(|| activity.to_string()),
            lines: vec![line.to_string()],
        }
    }

    #[test]
    fn the_four_verdicts() {
        assert_eq!(judge(false, None, &[], None).delivery, Delivery::Typed);
        assert_eq!(
            judge(
                true,
                Some("idle"),
                &[seen("", "Press up to edit queued messages")],
                None,
            )
            .delivery,
            Delivery::Queued
        );
        assert_eq!(
            judge(true, Some("running"), &[], None).delivery,
            Delivery::Queued
        );
        assert_eq!(
            judge(true, Some("idle"), &[seen("running", "")], None).delivery,
            Delivery::Delivered
        );
        assert_eq!(
            judge(true, Some("idle"), &[seen("awaiting-approval", "")], None).delivery,
            Delivery::Delivered
        );
        assert_eq!(
            judge(true, Some("idle"), &[], None).delivery,
            Delivery::Unconfirmed
        );
    }

    #[test]
    fn codex_composer_must_be_above_its_footer() {
        let lines = vec![
            "› carry on".to_string(),
            String::new(),
            "  gpt high · ~/work".to_string(),
            "  ? for shortcuts".to_string(),
        ];
        assert_eq!(composer_text(&lines).as_deref(), Some("carry on"));
        assert!(composer_holds(&lines, "carry on"));

        let transcript_only = vec!["› carry on".to_string(), String::new()];
        assert_eq!(composer_text(&transcript_only), None);
    }

    #[test]
    fn a_held_composer_at_rest_is_stuck_and_safe_to_resubmit() {
        let held = Observation {
            activity: Some("idle".to_string()),
            lines: vec![
                "› carry on".to_string(),
                String::new(),
                "  gpt high · ~/work".to_string(),
                "  ? for shortcuts".to_string(),
            ],
        };
        assert!(safe_to_resubmit(&held, "carry on"));
        assert_eq!(
            judge(true, Some("running"), &[held], Some("carry on")).delivery,
            Delivery::Stuck
        );
    }
}
