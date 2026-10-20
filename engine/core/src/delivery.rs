//! Did typed input land? Ports `src/vogt/core/delivery.py`.
//!
//! Judged from what the engine reported before the input and a few reads of
//! the screen after it. Evidence, not a promise.
//!
//! The input box readers (`codex_composer`, `claude_composer`) read the
//! screens captured live for WI-1669 (Codex) and WI-1870 (Claude Code): text
//! left in the box after an Enter is the only sign a submit did not land.

#![allow(dead_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Typed,
    Delivered,
    Queued,
    /// At rest with the text still in the input box: the submit did not land
    /// (WI-1870).
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

/// One read of the session after the input: its activity and screen.
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

/// Claude Code's hint under its input box while messages wait behind a turn
/// ("Press up to edit queued messages"), and Codex's queued-message line.
static QUEUED_HINT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)queued messages?|message(?:s)? queued").unwrap());

/// Codex's prompt glyph. It starts the composer and every copy of the
/// person's messages in the transcript, and marks the highlighted choice in
/// its dialogs, so a line starting with it proves nothing on its own.
pub const CODEX_PROMPT: &str = "\u{203a}";

/// Codex's footer under the composer: the model, a " · ", the directory
/// (`  grok high · ~/Working/...`). Captured live on 2026-10-16 (WI-1669).
static CODEX_FOOTER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\s{2}\S.* \x{00b7} \S").unwrap());

/// How much of the submitted text a composer has to show to count as still
/// holding it: enough to rule out the placeholder, short enough to survive
/// the composer wrapping a long text.
const COMPOSER_MATCH: usize = 40;

/// Claude Code's placeholder for a paste it folds: `[Pasted text #1 +4 lines]`.
static PASTED: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\[Pasted text #\d+").unwrap());

/// Claude Code's greyed suggestion in an empty box (`❯ Try "fix lint
/// errors"`). The screen carries no colour, so it is told by its shape.
static SUGGESTION: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"^Try "[^"]*"$"#).unwrap());

/// Claude Code's input box is drawn between two full-width rules of `─`.
static CLAUDE_RULE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\s*\x{2500}{20,}\s*$").unwrap());

/// Claude Code's prompt glyphs: `❯` now, `>` in the older boxed input.
const CLAUDE_PROMPTS: [&str; 2] = ["\u{276f}", ">"];

/// Python's `" ".join(text.split())`: whitespace runs folded to one space.
fn folded(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first `n` characters, as Python slices a `str`.
fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// The text in Codex's composer, or `None` when no composer is drawn.
///
/// The composer is the prompt line in the block directly above the footer:
/// `› text`, its wrapped continuation lines, one blank line, then the
/// model · directory line and a hint line. A `›` line anywhere else is a
/// transcript copy or a dialog's choice. An empty string is a composer
/// showing only its placeholder or nothing.
pub fn codex_composer(lines: &[String]) -> Option<String> {
    let mut rows: Vec<&str> = lines.iter().map(String::as_str).collect();
    while rows.last().is_some_and(|row| row.trim().is_empty()) {
        rows.pop();
    }
    // The upper of the last two lines: the hint line under it can carry a
    // " · " too ("⚠ 1 warning · f2 to view").
    let footer =
        (rows.len().saturating_sub(2)..rows.len()).find(|&i| CODEX_FOOTER.is_match(rows[i]))?;
    let mut i = footer as isize - 1;
    while i >= 0 && rows[i as usize].trim().is_empty() {
        i -= 1;
    }
    let mut block: Vec<String> = Vec::new();
    while i >= 0 && rows[i as usize].starts_with("  ") && !rows[i as usize].trim().is_empty() {
        block.push(rows[i as usize].trim().to_string());
        i -= 1;
    }
    if i < 0 || !rows[i as usize].starts_with(CODEX_PROMPT) {
        return None;
    }
    let prompt = rows[i as usize];
    block.push(prompt[CODEX_PROMPT.len()..].trim().to_string());
    block.reverse();
    Some(block.join(" ").trim().to_string())
}

/// The text in Claude Code's input box, or `None` when none is drawn.
///
/// The box is the last pair of `─` rules on the screen with a prompt line
/// (`❯ text`, then any wrapped continuation lines) between them, as
/// captured live on 2026-10-18 (WI-1870). An empty string is a box showing
/// nothing, or only the "Press up to edit queued messages" hint.
pub fn claude_composer(lines: &[String]) -> Option<String> {
    let rules: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| CLAUDE_RULE.is_match(line))
        .map(|(i, _)| i)
        .collect();
    if rules.len() < 2 {
        return None;
    }
    let (top, bottom) = (rules[rules.len() - 2], rules[rules.len() - 1]);
    let inside: Vec<&str> = lines[top + 1..bottom]
        .iter()
        .map(|line| line.trim())
        .collect();
    let first = *inside.first()?;
    if !CLAUDE_PROMPTS.iter().any(|glyph| first.starts_with(glyph)) {
        return None;
    }
    let mut first = first;
    for glyph in CLAUDE_PROMPTS {
        first = first.strip_prefix(glyph).unwrap_or(first);
    }
    let mut parts = vec![first.trim()];
    parts.extend(&inside[1..]);
    let shown = parts.join(" ").trim().to_string();
    if QUEUED_HINT.is_match(&shown) || SUGGESTION.is_match(&shown) {
        return Some(String::new());
    }
    Some(shown)
}

/// What an agent's input box holds — Codex's composer or Claude Code's input
/// box — or `None` when neither is drawn.
pub fn composer_text(lines: &[String]) -> Option<String> {
    codex_composer(lines).or_else(|| claude_composer(lines))
}

/// Whether the input box still shows `text`, unsubmitted (WI-1669, WI-1870).
///
/// A resumed Codex can swallow the Enter while it reads as working, and a
/// Claude Code turn can end with a queued message left in its box; the text
/// left in the input is the only sign.
pub fn composer_holds(lines: &[String], text: &str) -> bool {
    let wanted = folded(text);
    let Some(shown) = composer_text(lines) else {
        return false;
    };
    if wanted.is_empty() || shown.is_empty() {
        return false;
    }
    if text.trim().contains('\n') && PASTED.is_match(&shown) {
        // Claude Code folds a long paste into a placeholder in its box.
        return true;
    }
    let n = wanted.chars().count().min(COMPOSER_MATCH);
    head(&folded(&shown), n) == head(&wanted, n)
}

/// Whether one more Enter can only submit `text` (WI-1669).
///
/// Only while the composer is drawn and still holds the text, and nothing
/// asks for an answer: an Enter on a dialog or an approval would accept
/// whatever it has highlighted.
pub fn safe_to_resubmit(seen: &Observation, text: &str) -> bool {
    if matches!(
        seen.activity.as_deref(),
        Some("awaiting-approval") | Some("blocked")
    ) {
        return false;
    }
    composer_holds(&seen.lines, text)
}

/// What became of the input, from the activity before it and the reads
/// after it, in order.
///
/// `pending` is the submitted text: while the latest read shows the agent off
/// its turn with the text still in its input box, the Enter has not landed
/// (WI-1669) — `stuck`, not `queued`, even when a turn was running as it was
/// sent (WI-1870).
pub fn judge(
    submitted: bool,
    before: Option<&str>,
    after: &[Observation],
    pending: Option<&str>,
) -> Verdict {
    let verdict = |delivery: Delivery, evidence: &str| Verdict {
        delivery,
        evidence: evidence.to_string(),
    };
    if !submitted {
        return verdict(
            Delivery::Typed,
            "no Enter was pressed: the text is in the input, not sent",
        );
    }
    if after
        .iter()
        .any(|seen| seen.lines.iter().any(|line| QUEUED_HINT.is_match(line)))
    {
        return verdict(
            Delivery::Queued,
            "the agent shows a queued-message hint: it will take the input when its \
             current turn ends",
        );
    }
    let pending = pending.filter(|text| !text.is_empty());
    let last = after.last();
    if let (Some(text), Some(last)) = (pending, last) {
        if last.activity.as_deref() != Some("running") && composer_holds(&last.lines, text) {
            return verdict(
                Delivery::Stuck,
                "the text is still in the input box and no turn is running: the submit did \
                 not land; read session_screen",
            );
        }
    }
    if before == Some("running") {
        return verdict(
            Delivery::Queued,
            "a turn was already running when it was sent; agent CLIs queue input behind \
             the running turn",
        );
    }
    if let (Some(text), Some(last)) = (pending, last) {
        if composer_holds(&last.lines, text) {
            return verdict(
                Delivery::Unconfirmed,
                "the text is still in the composer after Enter: the submit did not land; \
                 read session_screen",
            );
        }
    }
    for seen in after {
        if seen.activity.as_deref() == Some("running") {
            return verdict(Delivery::Delivered, "the agent started a turn after it");
        }
        if seen.activity.as_deref() == Some("awaiting-approval") {
            return verdict(
                Delivery::Delivered,
                "the agent took it and is now asking for approval",
            );
        }
    }
    verdict(
        Delivery::Unconfirmed,
        "sent, but no turn started and no queued hint showed while Vogt watched; read \
         session_screen",
    )
}

/// The text an agent at rest still holds unsent in its input box, or `None`
/// (WI-1870). Only meaningful for a session not running a turn.
pub fn input_pending(lines: &[String]) -> Option<String> {
    composer_text(lines).filter(|shown| !shown.is_empty())
}

#[cfg(test)]
pub(crate) mod tests {
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
                None
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

    /// One of the Claude Code screens captured live on 2026-10-18 (WI-1870),
    /// shared with `tests/test_delivery.py`.
    pub(crate) struct Screen {
        pub(crate) activity: String,
        pub(crate) text: String,
        pub(crate) held: bool,
        pub(crate) lines: Vec<String>,
    }

    pub(crate) fn claude(name: &str) -> Screen {
        let raw = match name {
            "drained_idle" => {
                include_str!("../../../tests/fixtures/claude_screens/drained_idle.json")
            }
            "held_idle" => include_str!("../../../tests/fixtures/claude_screens/held_idle.json"),
            "queued_behind_turn" => {
                include_str!("../../../tests/fixtures/claude_screens/queued_behind_turn.json")
            }
            other => panic!("no screen {other}"),
        };
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        Screen {
            activity: value["activity"].as_str().unwrap().to_string(),
            text: value["text"].as_str().unwrap().to_string(),
            held: value["held"].as_bool().unwrap(),
            lines: value["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line.as_str().unwrap().to_string())
                .collect(),
        }
    }

    fn observed(name: &str) -> Observation {
        let screen = claude(name);
        Observation {
            activity: Some(screen.activity),
            lines: screen.lines,
        }
    }

    const NAMES: [&str; 3] = ["drained_idle", "held_idle", "queued_behind_turn"];

    #[test]
    fn the_claude_input_box_holds_only_what_is_unsent() {
        for name in NAMES {
            let screen = claude(name);
            assert_eq!(
                composer_holds(&screen.lines, &screen.text),
                screen.held,
                "{name}"
            );
            assert_eq!(
                input_pending(&screen.lines).is_some(),
                screen.held,
                "{name}"
            );
        }
    }

    #[test]
    fn the_claude_box_is_read_between_its_rules() {
        assert_eq!(
            claude_composer(&claude("held_idle").lines).as_deref(),
            Some("reply with the single word stuck-ok")
        );
        assert_eq!(
            claude_composer(&claude("drained_idle").lines).as_deref(),
            Some("")
        );
        // The queued-messages hint is not text in the box.
        assert_eq!(
            claude_composer(&claude("queued_behind_turn").lines).as_deref(),
            Some("")
        );
        // No box drawn.
        let bare = vec!["● Done.".to_string(), "> hello".to_string()];
        assert_eq!(claude_composer(&bare), None);
    }

    #[test]
    fn text_left_in_the_box_at_rest_is_stuck_not_queued() {
        // WI-1870: sent behind a turn, the turn ended, the text never left.
        let found = judge(
            true,
            Some("running"),
            &[observed("held_idle")],
            Some(&claude("held_idle").text),
        );
        assert_eq!(found.delivery, Delivery::Stuck);
        assert!(found.evidence.contains("input box"));
    }

    #[test]
    fn a_message_queued_behind_a_turn_is_still_queued() {
        let found = judge(
            true,
            Some("running"),
            &[observed("queued_behind_turn")],
            Some(&claude("queued_behind_turn").text),
        );
        assert_eq!(found.delivery, Delivery::Queued);
    }

    const PROMPT: &str = "\u{276f}";
    const MULTI: &str = "first line of the body\nsecond line\n\nthird, after a blank";

    pub(crate) fn boxed(inside: &[&str]) -> Vec<String> {
        let rule = "─".repeat(80);
        let mut lines = vec!["● Done.".to_string(), String::new(), rule.clone()];
        lines.extend(inside.iter().map(|line| line.to_string()));
        lines.push(rule);
        lines.push("  ⏵⏵ auto mode on".to_string());
        lines
    }

    #[test]
    fn a_multi_line_body_left_in_the_box_is_held() {
        // WI-1870: a multi-line body sits in the box over several lines, or
        // folded into Claude Code's paste placeholder.
        let first = format!("{PROMPT} first line of the body");
        let wrapped = boxed(&[&first, "  second line", "", "  third, after a blank"]);
        assert!(composer_holds(&wrapped, MULTI));
        let placeholder = format!("{PROMPT} [Pasted text #1 +3 lines]");
        let folded_box = boxed(&[&placeholder]);
        assert!(composer_holds(&folded_box, MULTI));
        assert!(
            !composer_holds(&folded_box, "one line"),
            "a placeholder is not one line"
        );
        assert!(!composer_holds(&boxed(&[PROMPT]), MULTI));
    }

    #[test]
    fn a_multi_line_body_stuck_at_rest_is_stuck() {
        let placeholder = format!("{PROMPT} [Pasted text #1 +3 lines]");
        let held = Observation {
            activity: Some("idle".to_string()),
            lines: boxed(&[&placeholder]),
        };
        let found = judge(true, Some("running"), &[held], Some(MULTI));
        assert_eq!(found.delivery, Delivery::Stuck);
    }

    #[test]
    fn a_greyed_suggestion_is_not_input_pending() {
        // Review of #1112: Claude's suggestion after the prompt marker.
        let suggestion = format!("{PROMPT} Try \"fix lint errors\"");
        let box_lines = boxed(&[&suggestion]);
        assert_eq!(claude_composer(&box_lines).as_deref(), Some(""));
        assert_eq!(input_pending(&box_lines), None);
        let typed = format!("{PROMPT} fix lint errors");
        assert_eq!(
            input_pending(&boxed(&[&typed])).as_deref(),
            Some("fix lint errors")
        );
    }

    #[test]
    fn a_codex_composer_is_read_above_its_footer() {
        // The WI-1669 shape `composer_text` tries first: `› text`, a blank,
        // then the model · directory footer and a hint line.
        let lines: Vec<String> = [
            "› an older message in the transcript",
            "",
            "› fix the sweep",
            "  and push it",
            "",
            "  grok high · ~/Working/vogt",
            "  ⚠ 1 warning · f2 to view",
        ]
        .iter()
        .map(|line| line.to_string())
        .collect();
        assert_eq!(
            codex_composer(&lines).as_deref(),
            Some("fix the sweep and push it")
        );
        assert!(composer_holds(&lines, "fix the sweep and push it"));
        let resubmit = Observation {
            activity: Some("awaiting-approval".to_string()),
            lines: lines.clone(),
        };
        assert!(!safe_to_resubmit(&resubmit, "fix the sweep and push it"));
        let quiet = Observation {
            activity: Some("idle".to_string()),
            lines,
        };
        assert!(safe_to_resubmit(&quiet, "fix the sweep and push it"));
        assert_eq!(codex_composer(&["no footer".to_string()]), None);
    }
}
