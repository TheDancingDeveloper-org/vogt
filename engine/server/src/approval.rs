//! Recognising an agent CLI's permission dialog on a session's screen.
//!
//! Claude Code and Codex stop and ask before running a command (or editing a
//! file) that their permission rules do not already allow, and Claude Code
//! denies the request on its own after a countdown. To a driver reading
//! `activity`, that dialog used to look exactly like an idle prompt
//! (`waiting-for-input`), so "approve me" and "done" were indistinguishable
//! and the request timed out unanswered — and the command it asked about had
//! often scrolled off the visible screen (WI-877).
//!
//! Detection runs on the **rendered** screen, not the raw byte tail: a
//! dialog that has been answered and redrawn over is gone from the screen,
//! while its text can linger in the raw tail for a long time. The raw tail is
//! used only as a cheap prefilter ([`mentions_approval`]) so the PTY reader
//! renders a screen only when a dialog may be showing.
//!
//! A dialog is recognised by two things together: a question line agents use
//! for permission ("Do you want to proceed?", "Do you want to make this edit
//! to …?", Codex's "Would you like to run the following command?" / "Allow
//! command?") and a numbered option line below it (`❯ 1. Yes`, `› 1. Yes,
//! proceed`). Either alone is ordinary output.
//!
//! Codex is read by its structure instead (WI-1355): its approval overlay
//! ends in "Press enter to confirm or esc to cancel" and its MCP forms run
//! from "Field 1/1" to "enter to submit | esc to cancel", the footer the last
//! thing on the screen. The question is read off the overlay's title or the
//! form's message, whatever its wording, length or wrap.
//!
//! Claude Code also stops at **startup gates** that are not permission
//! dialogs but stall a session just the same (WI-917): folder trust ("Do you
//! trust the files in this folder?" / "Is this a project you created or one
//! you trust?"), approval of a `CLAUDE.md` that imports files from outside
//! the directory ("Allow external CLAUDE.md file imports?"), and reading
//! outside the working directories. They are recognised the same way —
//! question plus numbered menu — and reported with a `kind`, so a driver can
//! tell a trust gate from a command approval. Every dialog carries its
//! enumerated `options`, with the highlighted one marked, so it can be
//! answered by choice (`POST /api/sessions/{id}/answer`) rather than by
//! counting arrow presses.

use once_cell::sync::Lazy;
use regex::Regex;

/// Characters a TUI draws its boxes and rules with, stripped from each line's
/// edges before matching.
const BORDER: &[char] = &[
    '│', '┃', '║', '|', '╭', '╮', '╰', '╯', '┌', '┐', '└', '┘', ' ', '\u{a0}',
];

/// Loose prefilter over the ANSI-stripped raw tail; whitespace-insensitive
/// because a TUI may position words with cursor moves rather than spaces.
static MENTIONS: Lazy<regex::bytes::Regex> = Lazy::new(|| {
    regex::bytes::Regex::new(
        r"(?i)permission\s*required|always\s*allow|allow\s*once|do\s*you\s*want\s*to|would\s*you\s*like\s*to\s*(?:run|make|apply|allow|grant|send)|allow\s*(?:command|.{1,80}?to\s*run\s*tool)|press\s*\S+\s*to\s*confirm\s*or\s*\S+\s*to\s*cancel|field\s*\d+\s*/\s*\d+|needs\s*your\s*approval|approve\s*network|grant\s*these\s*permissions|send\s*input\s*to|automatically\s*den|requires\s*approval|trust\s*the\s*files|trust\s*this\s*folder|project\s*you\s*(?:created|trust)|external\s*claude\.md|outside\s*(?:of\s*)?(?:the\s*)?(?:current\s*)?working\s*director|\(y\)\s*es\s*/\s*\(n\)\s*o",
    )
    .expect("approval prefilter compiles")
});

/// The question line of a permission dialog, matched on a border-stripped
/// line of the rendered screen.
static QUESTION: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^(?:do you want to (?:proceed|make this edit|make these edits|create|run|allow|delete|overwrite|fetch|use|write|execute|apply)\b.*\?|would you like to (?:run|make|apply|allow)\b.*\?|allow (?:command|this command|edit|tool)\b.*\??|.*\brequires approval\b.*|do you trust the files in this folder\??|trust this folder\??|.*\bis this a project you (?:created or one you )?trust\b.*\??|allow external claude\.md(?: file)? imports\??)$",
    )
    .expect("approval question regex compiles")
});

/// The footer of Codex's approval overlay (`approval_overlay.rs`), on the
/// footer line joined with the one after it, since it wraps on a narrow
/// screen: "Press enter to confirm or esc to cancel", with " or o to open
/// thread" when it comes from another thread. Codex's other pickers say "to
/// go back", so this footer is the overlay's alone (WI-1355).
static CODEX_OVERLAY_FOOTER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^press \S+ to confirm or \S+ to cancel\b").expect("codex footer compiles")
});

/// The progress line that heads every MCP elicitation form Codex draws
/// (`mcp_server_elicitation.rs`): "Field 1/1", "Field 2/3 (1 required
/// unanswered)". Its tool approvals and server elicitations alike.
static CODEX_FIELD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^Field \d+/\d+(?: \(\d+ required unanswered\))?$").expect("codex field compiles")
});

/// The elicitation form's footer: "enter to submit | esc to cancel", "enter
/// to submit all | ←/→ to navigate fields | esc to cancel", "⌃x enter to
/// submit | esc to cancel", read on up to three joined lines.
static CODEX_FORM_FOOTER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^(?:\S+ )?\S+ to submit\b.*\besc to cancel\b")
        .expect("codex form footer compiles")
});

/// The overlay's title, which `approval_overlay.rs` builds for each kind of
/// request: a command, input to a running terminal, network access, extra
/// permissions, a patch, and an MCP server's request.
static CODEX_OVERLAY_TITLE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^(?:would you like to (?:run the following command|make the following edits|grant these permissions|send input to (?:the existing terminal|terminal \S+))\?|do you want to approve network access to .+\?|.+ needs your approval\.)$",
    )
    .expect("codex overlay title compiles")
});

/// What the overlay is taken to ask when its title has scrolled out of
/// reach (a command taller than the screen and the scrollback read).
const CODEX_UNTITLED: &str = "Codex is waiting for approval";

/// A numbered menu option, with or without the selection glyph.
static OPTION: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(?:[❯›>▸]\s*)?1\.\s+\S").expect("option regex compiles"));

/// Any numbered menu option: the highlight glyph, the number, the label.
static ANY_OPTION: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?:([❯›>▸])\s*)?(\d{1,2})\.\s+(\S.*?)\s*$").expect("option regex compiles")
});

/// Text that makes a permission dialog a read outside the working
/// directories rather than any other tool call.
static READ_OUTSIDE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)outside\s+(?:of\s+)?(?:the\s+)?(?:current\s+)?working\s+director|read\s+(?:files?\s+)?outside")
        .expect("read-outside regex compiles")
});

/// "…automatically deny this request in 90s", "auto-deny in 1m".
static DEADLINE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)auto(?:matically)?[\s-]*(?:den(?:y|ied|ies)|reject\w*)\D{0,60}?(\d{1,5})\s*(seconds?|secs?|s|minutes?|mins?|m)\b",
    )
    .expect("deadline regex compiles")
});

/// opencode's dialog title (WI-933): "△ Permission required", and the
/// follow-up "△ Always allow" that "Allow always" opens.
static OPENCODE_TITLE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?:△\s*)?(Permission required|Always allow)$").expect("opencode title compiles")
});

/// opencode's button row: horizontal buttons, then its key hints.
static OPENCODE_BUTTONS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(Allow once\s{2,}Allow always\s{2,}Reject|Confirm\s{2,}Cancel)\b")
        .expect("opencode buttons compile")
});

/// Klaudia's prompt line (WI-1279): "Change this machine? (y)es / (n)o /
/// (s)omething else", "Allow Bash(…)? (y)es / (n)o / (s)omething else", "Run
/// these hooks? (y)es / (n)o". The question is everything before the answers.
static KLAUDIA_PROMPT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(.*?\?)\s+\(y\)es\s*/\s*\(n\)o(?:\s*/\s*\(s\)omething else)?\b")
        .expect("klaudia prompt regex compiles")
});

/// The head line of Klaudia's host-change card, which the excerpt reaches up
/// to: what changes, why, and the paths, services and commands it covers.
static KLAUDIA_CARD_HEAD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?:This changes your machine|This repository wants to run its own commands|This wasn't part of what you approved)$")
        .expect("klaudia card head compiles")
});

/// Klaudia keeps its live prompt in the bottom view, within this many lines
/// of the bottom of the screen.
const KLAUDIA_PROMPT_BOTTOM_LINES: usize = 8;

/// At most this many lines above the question are read for the command.
const MAX_CONTEXT_LINES: usize = 40;
/// The excerpt is cut at this many characters.
const MAX_EXCERPT_CHARS: usize = 4000;

/// One numbered choice in a dialog's menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuOption {
    pub number: u32,
    pub label: String,
    /// The highlight is on it: Enter alone would choose it.
    pub selected: bool,
}

/// What kind of dialog it is, as the wire names it.
pub fn kind_of(question: &str, excerpt: &str) -> &'static str {
    let q = question.to_ascii_lowercase();
    if q.contains("trust") {
        "folder-trust"
    } else if q.contains("external claude.md") {
        "external-imports"
    } else if READ_OUTSIDE.is_match(question) || READ_OUTSIDE.is_match(excerpt) {
        "read-outside-cwd"
    } else {
        "permission"
    }
}

/// A permission dialog read off the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    /// The dialog's question line, as shown.
    pub question: String,
    /// What it asks about — the command, edit or tool call — with borders
    /// stripped, from the screen and the scrollback above it.
    pub command_excerpt: String,
    /// Seconds until the CLI answers "no" by itself, when it shows a
    /// countdown.
    pub deadline_seconds: Option<u32>,
    /// `permission`, `folder-trust`, `external-imports` or
    /// `read-outside-cwd`.
    pub kind: &'static str,
    /// The menu, in order.
    pub options: Vec<MenuOption>,
    /// The options are buttons side by side (opencode): Left/Right move the
    /// highlight, not Up/Down.
    pub horizontal: bool,
}

/// How many scrollback lines are rendered above the screen when reading a
/// dialog, so a command taller than the screen is still read whole.
pub const SCROLLBACK_CONTEXT_LINES: usize = 200;

/// A dialog as a session holds it: what was detected, and when it was first
/// seen, so its countdown is anchored to the first sighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub detected: Detected,
    pub detected_at: time::OffsetDateTime,
    pub deadline_at: Option<time::OffsetDateTime>,
}

impl Pending {
    pub fn new(detected: Detected, now: time::OffsetDateTime) -> Self {
        let deadline_at = detected
            .deadline_seconds
            .map(|s| now + time::Duration::seconds(i64::from(s)));
        Self {
            detected,
            detected_at: now,
            deadline_at,
        }
    }

    /// Whether `other` is this same dialog, re-read (its countdown moved).
    pub fn is_same(&self, other: &Detected) -> bool {
        self.detected.question == other.question
            && self.detected.command_excerpt == other.command_excerpt
    }

    pub fn to_wire(&self, now: time::OffsetDateTime) -> vogt_engine_contract::ApprovalPrompt {
        let deadline_seconds = self
            .deadline_at
            .map(|at| (at - now).whole_seconds().clamp(0, i64::from(u32::MAX)) as u32);
        vogt_engine_contract::ApprovalPrompt {
            question: self.detected.question.clone(),
            command_excerpt: self.detected.command_excerpt.clone(),
            deadline_seconds,
            deadline_at: self.deadline_at.map(rfc3339),
            detected_at: rfc3339(self.detected_at),
            kind: self.detected.kind.to_string(),
            options: self
                .detected
                .options
                .iter()
                .map(|o| vogt_engine_contract::ApprovalOption {
                    number: o.number,
                    label: o.label.clone(),
                    selected: o.selected,
                })
                .collect(),
        }
    }
}

fn rfc3339(ts: time::OffsetDateTime) -> String {
    ts.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| ts.to_string())
}

/// Whether an ANSI-stripped tail may contain a permission dialog. Cheap; a
/// `true` only earns a screen render.
pub fn mentions_approval(stripped_tail: &[u8]) -> bool {
    MENTIONS.is_match(stripped_tail)
}

fn strip(line: &str) -> &str {
    line.trim_matches(BORDER)
}

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.chars().count() >= 3
        && t.chars()
            .all(|c| matches!(c, '─' | '━' | '═' | '-' | '╌' | '┄' | '╭' | '╮' | '┌' | '┐'))
}

/// Find a live permission dialog on the screen.
///
/// `visible` is the screen top to bottom; `scrollback` is the history above
/// it, oldest first (may be empty). The question must be on the visible
/// screen, with a numbered option within the next eight lines; the command
/// excerpt may reach up into the scrollback.
pub fn detect(visible: &[String], scrollback: &[String]) -> Option<Detected> {
    detect_codex(visible, scrollback)
        .or_else(|| detect_numbered(visible, scrollback))
        .or_else(|| detect_opencode(visible))
        .or_else(|| detect_klaudia(visible, scrollback))
}

/// Klaudia's person-only prompts (WI-1279): a `RequestHostChange` card
///
/// ```text
/// This changes your machine
///   write the Komodo header for the deploy
///   why: the API needs it
///   paths: /tmp/komodo-header
///   approving covers every step inside that scope, for this session only
/// Change this machine? (y)es / (n)o / (s)omething else
/// ```
///
/// and its tool permissions ("Allow Bash(…)? (y)es / (n)o / …"). The answers
/// are letter keys, not a menu, so no options are offered: an agent's answer
/// refuses before any key is written and a person answers at the screen. The
/// excerpt is the card, from its head line down, when one is above the prompt.
fn detect_klaudia(visible: &[String], scrollback: &[String]) -> Option<Detected> {
    let from = visible.len().saturating_sub(KLAUDIA_PROMPT_BOTTOM_LINES);
    let at = from
        + visible[from..]
            .iter()
            .rposition(|line| KLAUDIA_PROMPT.is_match(strip(line).trim()))?;
    let question = KLAUDIA_PROMPT.captures(strip(&visible[at]).trim())?[1]
        .trim()
        .to_string();
    let above: Vec<&str> = scrollback
        .iter()
        .chain(visible[..at].iter())
        .map(|l| l.trim_end())
        .collect();
    let window = &above[above.len().saturating_sub(MAX_CONTEXT_LINES)..];
    let card: Vec<&str> = match window
        .iter()
        .rposition(|l| KLAUDIA_CARD_HEAD.is_match(l.trim()))
    {
        Some(head) => window[head..]
            .iter()
            .copied()
            .filter(|l| !l.trim().is_empty())
            .collect(),
        None => Vec::new(),
    };
    let mut command_excerpt = if card.is_empty() {
        question.clone()
    } else {
        card.join("\n")
    };
    if command_excerpt.chars().count() > MAX_EXCERPT_CHARS {
        command_excerpt = command_excerpt.chars().take(MAX_EXCERPT_CHARS).collect();
        command_excerpt.push('…');
    }
    Some(Detected {
        kind: kind_of(&question, &command_excerpt),
        question,
        command_excerpt,
        deadline_seconds: None,
        options: Vec::new(),
        horizontal: false,
    })
}

/// opencode's permission dialog (WI-933), read off the screen:
///
/// ```text
/// △ Permission required
///   ← Access external directory /etc
/// Patterns
/// - /etc/*
///  Allow once   Allow always   Reject         ⇆ select  enter confirm
/// ```
///
/// and the "△ Always allow … Confirm  Cancel" that "Allow always" opens. The
/// buttons sit side by side with the first highlighted when it opens; the
/// highlight is a colour the text screen does not carry, so the first is
/// taken as selected, which is what a dialog the driver has not touched
/// shows.
fn detect_opencode(visible: &[String]) -> Option<Detected> {
    let title_at = visible
        .iter()
        .rposition(|line| OPENCODE_TITLE.is_match(strip(line).trim()))?;
    let title = OPENCODE_TITLE.captures(strip(&visible[title_at]).trim())?[1].to_string();
    let after = &visible[title_at + 1..];
    let buttons_rel = after
        .iter()
        .take(24)
        .position(|line| OPENCODE_BUTTONS.is_match(strip(line).trim()))?;
    let row = strip(&after[buttons_rel]).trim();
    let captured = OPENCODE_BUTTONS.captures(row)?;
    let labels: Vec<&str> = captured
        .get(1)?
        .as_str()
        .split("  ")
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let body: Vec<String> = after[..buttons_rel]
        .iter()
        .map(|l| strip(l).trim().trim_start_matches('←').trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let question = match title.as_str() {
        // "← Access external directory /etc": what is being asked.
        "Permission required" => body.first().cloned().unwrap_or(title.clone()),
        _ => format!("{title}: {}", body.first().cloned().unwrap_or_default()),
    };
    let mut command_excerpt = body.join("\n");
    if command_excerpt.chars().count() > MAX_EXCERPT_CHARS {
        command_excerpt = command_excerpt.chars().take(MAX_EXCERPT_CHARS).collect();
        command_excerpt.push('…');
    }
    let kind = if question.to_ascii_lowercase().contains("external directory") {
        "read-outside-cwd"
    } else {
        "permission"
    };
    Some(Detected {
        question,
        command_excerpt,
        deadline_seconds: None,
        kind,
        options: labels
            .iter()
            .enumerate()
            .map(|(i, label)| MenuOption {
                number: i as u32 + 1,
                label: (*label).to_string(),
                selected: i == 0,
            })
            .collect(),
        horizontal: true,
    })
}

/// Codex's approval surfaces (WI-1355), recognised by their structure rather
/// than by what they ask, so a long body, a wrapped question or a message an
/// MCP server wrote itself does not hide them:
///
/// ```text
///   Would you like to run the following command?
///
///   Environment: local
///
///   Reason: write a probe file outside the workspace
///
///   $ touch /etc/probe
///
///
/// › 1. Yes, proceed (y)
///   2. No, and tell Codex what to do differently (esc)
///
///   Press enter to confirm or esc to cancel
/// ```
///
/// is the approval overlay — commands, patches, network access, input to a
/// running terminal, extra permissions and MCP requests — and
///
/// ```text
///   Field 1/1
///   Allow the echo MCP server to run tool "echo"?
///
///   › 1. Allow                   Run the tool and continue
///     2. Allow for this session  Run the tool and remember this choice …
///   enter to submit | esc to cancel
/// ```
///
/// is an MCP elicitation form: a tool approval for a server, an app or a
/// connector, or a question the server asks in its own words. Either is live
/// only while its footer is the last thing on the screen, which is where
/// Codex draws it and where ordinary output never leaves those words: an
/// idle or working Codex shows its composer and status line under them.
fn detect_codex(visible: &[String], scrollback: &[String]) -> Option<Detected> {
    let last_visible = visible.iter().rposition(|l| !strip(l).trim().is_empty())?;
    let all: Vec<&str> = scrollback
        .iter()
        .chain(visible.iter())
        .map(String::as_str)
        .collect();
    let last = scrollback.len() + last_visible;
    // The footer wraps over up to three lines on a phone-width screen.
    for start in last.saturating_sub(2)..=last {
        let joined = join_stripped(&all[start..=last]);
        if CODEX_OVERLAY_FOOTER.is_match(&joined) {
            return codex_overlay(&all, start);
        }
        if CODEX_FORM_FOOTER.is_match(&joined) {
            return codex_form(&all, start);
        }
    }
    None
}

fn join_stripped(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|l| strip(l).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn cut_excerpt(lines: &[&str]) -> String {
    let mut excerpt = lines
        .iter()
        .map(|l| strip(l).trim_end())
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if excerpt.chars().count() > MAX_EXCERPT_CHARS {
        excerpt = excerpt.chars().take(MAX_EXCERPT_CHARS).collect();
        excerpt.push('…');
    }
    excerpt
}

fn codex_overlay(all: &[&str], footer: usize) -> Option<Detected> {
    let from = footer.saturating_sub(MAX_CONTEXT_LINES);
    let opt = from
        + all[from..footer]
            .iter()
            .rposition(|l| OPTION.is_match(strip(l).trim()))?;
    let options = codex_options(&all[opt..footer]);
    // The title is the header's first paragraph, with the reason, the
    // command or the diff between it and the menu. Searched upward a
    // paragraph at a time, stopping at the transcript ("• Running …").
    let mut title: Option<(usize, String)> = None;
    let mut top = opt;
    let mut end = opt;
    let floor = opt.saturating_sub(SCROLLBACK_CONTEXT_LINES);
    let mut i = opt;
    while i > floor {
        let line = strip(all[i - 1]).trim();
        if line.starts_with('•') || line.starts_with('›') {
            break;
        }
        if line.is_empty() {
            if end > i {
                let text = join_stripped(&all[i..end]);
                if CODEX_OVERLAY_TITLE.is_match(&text) {
                    title = Some((end, text));
                    break;
                }
            }
            end = i - 1;
        }
        i -= 1;
        top = i;
    }
    if title.is_none() && end > i {
        let text = join_stripped(&all[i..end]);
        if CODEX_OVERLAY_TITLE.is_match(&text) {
            title = Some((end, text));
        }
    }
    let (body_from, question) = match title {
        Some((after, text)) => (after, text),
        None => (top, CODEX_UNTITLED.to_string()),
    };
    Some(Detected {
        question,
        command_excerpt: cut_excerpt(&all[body_from..opt]),
        deadline_seconds: None,
        kind: "permission",
        options,
        horizontal: false,
    })
}

fn codex_form(all: &[&str], footer: usize) -> Option<Detected> {
    let from = footer.saturating_sub(SCROLLBACK_CONTEXT_LINES);
    let field = from
        + all[from..footer]
            .iter()
            .rposition(|l| CODEX_FIELD.is_match(strip(l).trim()))?;
    let body = &all[field + 1..footer];
    let opt = body
        .iter()
        .position(|l| OPTION.is_match(strip(l).trim()))
        .unwrap_or(body.len());
    let options = codex_options(&body[opt..]);
    // The message is the first paragraph; a question wrapped over several
    // lines ends at its question mark.
    let prompt = &body[..opt];
    let first = prompt
        .iter()
        .position(|l| !strip(l).trim().is_empty())
        .unwrap_or(prompt.len());
    let mut upto = first;
    while upto < prompt.len() && !strip(prompt[upto]).trim().is_empty() {
        upto += 1;
        if strip(prompt[upto - 1]).trim_end().ends_with('?') {
            break;
        }
    }
    let question = join_stripped(&prompt[first..upto]);
    if question.is_empty() && options.is_empty() {
        return None;
    }
    let rest = cut_excerpt(&prompt[upto..]);
    let command_excerpt = if rest.is_empty() {
        question.clone()
    } else {
        rest
    };
    Some(Detected {
        question,
        command_excerpt,
        deadline_seconds: None,
        kind: "permission",
        options,
        horizontal: false,
    })
}

/// Codex's numbered menu, from option 1 to the footer. A choice that has a
/// description shows it after a run of two or more spaces ("Allow for this
/// session  Run the tool and remember …"); the label is the part before it.
/// A line indented to the label's column continues a label that wrapped.
fn codex_options(lines: &[&str]) -> Vec<MenuOption> {
    let mut options: Vec<MenuOption> = Vec::new();
    let mut label_col = usize::MAX;
    for raw in lines {
        let line = strip(raw).trim();
        if let Some(c) = ANY_OPTION.captures(line) {
            let number: u32 = c[2].parse().unwrap_or(0);
            if number as usize == options.len() + 1 {
                let text = c.get(3).map_or("", |m| m.as_str());
                label_col = raw
                    .find(text)
                    .map_or(usize::MAX, |at| raw[..at].chars().count());
                options.push(MenuOption {
                    number,
                    label: label_part(text).to_string(),
                    selected: c.get(1).is_some(),
                });
                continue;
            }
        }
        let indent = raw.chars().take_while(|c| *c == ' ').count();
        if let Some(last) = options.last_mut() {
            if !line.is_empty() && indent == label_col {
                last.label.push(' ');
                last.label.push_str(label_part(line));
            }
        }
    }
    options
}

/// A menu line's label, without the description after a run of spaces.
fn label_part(text: &str) -> &str {
    text.split("  ").next().unwrap_or(text).trim()
}

fn detect_numbered(visible: &[String], scrollback: &[String]) -> Option<Detected> {
    let q_visible = visible
        .iter()
        .rposition(|line| QUESTION.is_match(strip(line).trim()))?;
    let after = &visible[q_visible + 1..];
    let opt_rel = after
        .iter()
        .take(8)
        .position(|line| OPTION.is_match(strip(line).trim()))?;

    let all: Vec<&str> = scrollback
        .iter()
        .chain(visible.iter())
        .map(String::as_str)
        .collect();
    let q = scrollback.len() + q_visible;
    let opt = q + 1 + opt_rel;
    let question = strip(all[q]).trim().to_string();

    // Codex draws what it asks about *between* the question and the options;
    // Claude Code draws it *above* the question, under the dialog's title.
    let between: Vec<&str> = all[q + 1..opt]
        .iter()
        .map(|l| strip(l).trim_end())
        .filter(|l| !l.trim().is_empty())
        .collect();
    let body: Vec<&str> = if !between.is_empty() {
        between
    } else {
        let mut start = q;
        while start > 0 && q - start < MAX_CONTEXT_LINES && !is_rule(all[start - 1]) {
            start -= 1;
        }
        all[start..q]
            .iter()
            .map(|l| strip(l).trim_end())
            .filter(|l| !l.trim().is_empty())
            .collect()
    };
    let mut command_excerpt = body.join("\n");
    if command_excerpt.chars().count() > MAX_EXCERPT_CHARS {
        command_excerpt = command_excerpt.chars().take(MAX_EXCERPT_CHARS).collect();
        command_excerpt.push('…');
    }

    let deadline_seconds = all[q.saturating_sub(MAX_CONTEXT_LINES)..]
        .iter()
        .rev()
        .find_map(|line| DEADLINE.captures(line))
        .and_then(|c| {
            let n: u32 = c.get(1)?.as_str().parse().ok()?;
            let unit = c.get(2)?.as_str().to_ascii_lowercase();
            Some(if unit.starts_with('m') {
                n.saturating_mul(60)
            } else {
                n
            })
        });

    // The menu: consecutive numbered lines from the first option, a blank
    // or wrapped line between them tolerated, numbering in sequence.
    let mut options: Vec<MenuOption> = Vec::new();
    for line in all[opt..].iter().take(24) {
        let line = strip(line).trim();
        if let Some(c) = ANY_OPTION.captures(line) {
            let number: u32 = c[2].parse().unwrap_or(0);
            if number as usize != options.len() + 1 {
                break;
            }
            options.push(MenuOption {
                number,
                // A choice and its description on one line, split by a run
                // of spaces (Codex, WI-1355): the choice is the part before.
                label: label_part(&c[3]).to_string(),
                selected: c.get(1).is_some(),
            });
        } else if !options.is_empty()
            && !line.is_empty()
            && !line.starts_with(char::is_alphanumeric)
        {
            break;
        }
    }

    let kind = kind_of(&question, &command_excerpt);
    Some(Detected {
        question,
        command_excerpt,
        deadline_seconds,
        kind,
        options,
        horizontal: false,
    })
}

/// The arrow presses, then Enter, that choose `target` in `options` from
/// whichever is highlighted now (the first, when none is marked). Down/Up
/// for a menu, Right/Left for side-by-side buttons. `None` when there is no
/// such option.
pub fn keys_to_choose(options: &[MenuOption], target: u32, horizontal: bool) -> Option<String> {
    let to = options.iter().position(|o| o.number == target)?;
    let from = options.iter().position(|o| o.selected).unwrap_or(0);
    let step = match (horizontal, to >= from) {
        (false, true) => "\x1b[B",
        (false, false) => "\x1b[A",
        (true, true) => "\x1b[C",
        (true, false) => "\x1b[D",
    };
    let mut keys = step.repeat(to.abs_diff(from));
    keys.push('\r');
    Some(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn claude_code_bash_dialog_with_a_countdown() {
        let screen = lines(
            "● I'll clean up the containers.\n\
             ╭──────────────────────────────────────────────╮\n\
             │ Bash command                                 │\n\
             │                                              │\n\
             │   sh -c 'docker stop x && docker rm -f x'    │\n\
             │   Stop and remove the test container         │\n\
             │                                              │\n\
             │ Do you want to proceed?                      │\n\
             │ ❯ 1. Yes                                     │\n\
             │   2. Yes, and don't ask again for sh         │\n\
             │   3. No, and tell Claude what to do (esc)    │\n\
             ╰──────────────────────────────────────────────╯\n\
             Claude will automatically deny this request in 87s",
        );
        let d = detect(&screen, &[]).expect("dialog detected");
        assert_eq!(d.question, "Do you want to proceed?");
        assert!(d.command_excerpt.contains("docker rm -f x"), "{d:?}");
        assert!(d.command_excerpt.starts_with("Bash command"), "{d:?}");
        assert!(!d.command_excerpt.contains("clean up"), "{d:?}");
        assert_eq!(d.deadline_seconds, Some(87));
    }

    #[test]
    fn the_command_can_reach_up_into_the_scrollback() {
        let scrollback = lines(
            "older output\n\
             ────────────────────────\n\
             Bash command\n\
             \n\
             rm -rf /tmp/build-cache &&",
        );
        let screen = lines(
            "  docker system prune -f\n\
             \n\
             Do you want to proceed?\n\
             ❯ 1. Yes\n\
             2. No",
        );
        let d = detect(&screen, &scrollback).expect("dialog detected");
        assert!(
            d.command_excerpt.contains("rm -rf /tmp/build-cache"),
            "{d:?}"
        );
        assert!(
            d.command_excerpt.contains("docker system prune -f"),
            "{d:?}"
        );
        assert!(!d.command_excerpt.contains("older output"), "{d:?}");
        assert_eq!(d.deadline_seconds, None);
    }

    #[test]
    fn codex_mcp_tool_approval() {
        // An MCP tool call waiting on its person in the WI-1127 Codex fork's
        // TUI (v0.162.0, WI-1355).
        let screen = lines(
            "Field 1/1\n\
             Allow the vogt MCP server to run tool \"board_list\"?\n\
              cells: [{\"state\": \"\"}]\n\
             › 1. Allow                   Run the tool and continue\n\
               2. Allow for this session  Run the tool and remember this choice for this session\n\
               3. Always allow            Run the tool and remember this choice for future tool calls\n\
               4. Cancel                  Cancel this tool call\n\
             enter to submit | esc to cancel",
        );
        let d = detect(&screen, &[]).expect("mcp approval detected");
        assert_eq!(
            d.question,
            "Allow the vogt MCP server to run tool \"board_list\"?"
        );
        assert_eq!(d.kind, "permission");
        assert_eq!(d.options.len(), 4);
        assert!(d.options[0].selected && !d.options[1].selected);
        assert_eq!(d.options[0].label, "Allow");
        // Two spaces part the widest label from its description.
        assert_eq!(d.options[1].label, "Allow for this session");
        assert_eq!(d.options[3].label, "Cancel");
        assert!(d.command_excerpt.contains("cells:"), "{d:?}");
        assert!(mentions_approval(screen.join("\n").as_bytes()));
    }

    #[test]
    fn codex_exec_approval() {
        let screen = lines(
            "Would you like to run the following command?\n\
             \n\
             Reason: clean the build directory\n\
             \n\
             $ rm -rf target\n\
             \n\
             › 1. Yes, proceed (y)\n\
               2. Yes, and don't ask again for this command (a)\n\
               3. No, and tell Codex what to do differently (esc)",
        );
        let d = detect(&screen, &[]).expect("dialog detected");
        assert_eq!(d.question, "Would you like to run the following command?");
        assert!(d.command_excerpt.contains("$ rm -rf target"), "{d:?}");
        assert!(d.command_excerpt.contains("Reason:"), "{d:?}");
    }

    #[test]
    fn edit_dialog_and_minute_deadlines() {
        let screen = lines(
            "Edit file src/main.rs\n\
             Do you want to make this edit to main.rs?\n\
             ❯ 1. Yes\n\
               2. No\n\
             auto-deny in 2m",
        );
        let d = detect(&screen, &[]).expect("dialog detected");
        assert_eq!(d.deadline_seconds, Some(120));
    }

    #[test]
    fn a_question_without_a_menu_or_a_menu_without_a_question_is_not_a_dialog() {
        // Prose that asks the question, with no menu under it.
        assert_eq!(
            detect(&lines("Do you want to proceed? I can wait.\n> "), &[]),
            None
        );
        // An ordinary numbered list.
        assert_eq!(detect(&lines("Steps:\n1. build\n2. test\n> "), &[]), None);
        // An idle Claude prompt.
        assert_eq!(detect(&lines("● Done.\n╭────╮\n│ >  │\n╰────╯"), &[]), None);
    }

    #[test]
    fn a_pending_dialog_counts_down_from_its_first_sighting() {
        let found = Detected {
            question: "Do you want to proceed?".into(),
            command_excerpt: "rm -rf x".into(),
            deadline_seconds: Some(90),
            kind: "permission",
            options: Vec::new(),
            horizontal: false,
        };
        let seen = time::OffsetDateTime::UNIX_EPOCH;
        let pending = Pending::new(found.clone(), seen);
        let later = pending.to_wire(seen + time::Duration::seconds(30));
        assert_eq!(later.deadline_seconds, Some(60));
        assert!(later.deadline_at.is_some());
        let gone = pending.to_wire(seen + time::Duration::seconds(300));
        assert_eq!(gone.deadline_seconds, Some(0));
        // The countdown ticking is the same dialog; a different command is not.
        let ticked = Detected {
            deadline_seconds: Some(80),
            ..found.clone()
        };
        assert!(pending.is_same(&ticked));
        let other = Detected {
            command_excerpt: "ls".into(),
            ..found
        };
        assert!(!pending.is_same(&other));
    }

    #[test]
    fn startup_gates_are_dialogs_with_a_kind_and_their_options() {
        let trust = lines(
            "Accessing workspace:\n\
             /home/sprooty/Working/Active/vogt\n\
             \n\
             Quick safety check: Is this a project you created or one you trust?\n\
             \n\
             ❯ 1. Yes, I trust this folder\n\
               2. No, exit\n\
             \n\
             Enter to confirm · Esc to cancel",
        );
        let d = detect(&trust, &[]).expect("trust gate detected");
        assert_eq!(d.kind, "folder-trust");
        assert_eq!(d.options.len(), 2);
        assert!(d.options[0].selected && !d.options[1].selected);
        assert_eq!(d.options[1].label, "No, exit");

        // WI-1667: Codex's folder-trust gate (codex-cli 0.162, captured from
        // the binary's TUI strings). The question is "Trust this folder?",
        // the first option is preselected, and Enter alone chooses it —
        // typing "1" and Enter quits the session. It is a startup gate, so
        // session_answer can move the highlight and the change feed sees it.
        let codex_trust = lines(
            "Folder access\n\
             Trust this folder?\n\
             Codex can read, edit, and run files here, subject to your permission settings.\n\
             \n\
             ❯ 1. Trust and continue\n\
               2. Open restricted\n",
        );
        let d = detect(&codex_trust, &[]).expect("codex trust gate detected");
        assert_eq!(d.kind, "folder-trust");
        assert_eq!(d.question, "Trust this folder?");
        assert_eq!(d.options.len(), 2);
        assert!(d.options[0].selected && !d.options[1].selected);
        assert_eq!(d.options[0].label, "Trust and continue");
        assert_eq!(d.options[1].label, "Open restricted");
        assert!(mentions_approval(b"Trust this folder?"));

        // The screen reported from prod named the second option "Quit"
        // rather than "Open restricted". Detection keys on the question, so
        // that variant is the same gate.
        let quit = lines(
            "Folder access\n\
             Trust this folder?\n\
             ❯ 1. Trust and continue\n\
               2. Quit\n",
        );
        let d = detect(&quit, &[]).expect("quit variant detected");
        assert_eq!(d.kind, "folder-trust");
        assert_eq!(d.options[1].label, "Quit");

        let imports = lines(
            "Allow external CLAUDE.md file imports?\n\
             This project's CLAUDE.md imports files outside the current working directory.\n\
               1. Yes, allow external imports\n\
             ❯ 2. No, disable external imports",
        );
        let d = detect(&imports, &[]).expect("imports gate detected");
        assert_eq!(d.kind, "external-imports");
        assert!(d.options[1].selected);

        let read = lines(
            "Read file\n\
             Claude requested permissions to read from /home/x/.local/share/brief.md,\n\
             which is outside the current working directories.\n\
             Do you want to proceed?\n\
             ❯ 1. Yes\n\
               2. Yes, allow reading from share/ during this session\n\
               3. No, and tell Claude what to do differently (esc)",
        );
        let d = detect(&read, &[]).expect("read gate detected");
        assert_eq!(d.kind, "read-outside-cwd");
        assert_eq!(d.options.len(), 3);

        let bash = detect(
            &lines("Bash command\n  ls\nDo you want to proceed?\n❯ 1. Yes\n  2. No"),
            &[],
        )
        .unwrap();
        assert_eq!(bash.kind, "permission");
    }

    #[test]
    fn choosing_an_option_moves_from_the_highlight() {
        let menu = |selected: u32| {
            (1..=3)
                .map(|n| MenuOption {
                    number: n,
                    label: format!("o{n}"),
                    selected: n == selected,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(keys_to_choose(&menu(1), 1, false).unwrap(), "\r");
        assert_eq!(
            keys_to_choose(&menu(1), 3, false).unwrap(),
            "\x1b[B\x1b[B\r"
        );
        assert_eq!(keys_to_choose(&menu(3), 2, false).unwrap(), "\x1b[A\r");
        assert_eq!(keys_to_choose(&menu(1), 3, true).unwrap(), "\x1b[C\x1b[C\r");
        assert_eq!(keys_to_choose(&menu(3), 2, true).unwrap(), "\x1b[D\r");
        assert_eq!(keys_to_choose(&menu(1), 4, false), None);
    }

    #[test]
    fn the_prefilter_is_whitespace_insensitive() {
        assert!(mentions_approval(b"Doyouwanttoproceed?"));
        assert!(mentions_approval(b"will automatically deny this request"));
        assert!(!mentions_approval(b"compiling vogt v0.7.4"));
    }

    /// WI-933: opencode's dialogs, as its screen renders them (captured from a
    /// real session on 2026-10-05).
    #[test]
    fn opencode_permission_dialogs_are_read_with_their_buttons() {
        let screen = lines(
            "  ┃  Use your read tool to read the file /etc/hostname.\n\
             \x20    ⠙ Read /etc/hostname\n\
             \x20    ▣  Build · DeepSeek V4.1 Flash\n\
             \x20 ┃\n\
             \x20 ┃  △ Permission required\n\
             \x20 ┃    ← Access external directory /etc\n\
             \x20 ┃\n\
             \x20 ┃  Patterns\n\
             \x20 ┃\n\
             \x20 ┃  - /etc/*\n\
             \x20 ┃\n\
             \x20 ┃   Allow once   Allow always   Reject                ctrl+f fullscreen  ⇆ select  enter confirm\n\
             \x20 ┃",
        );
        let found = detect(&screen, &[]).expect("opencode's dialog is a dialog");
        assert_eq!(found.question, "Access external directory /etc");
        assert_eq!(found.kind, "read-outside-cwd");
        assert!(found.horizontal);
        assert!(
            found.command_excerpt.contains("- /etc/*"),
            "{}",
            found.command_excerpt
        );
        let labels: Vec<&str> = found.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Allow once", "Allow always", "Reject"]);
        assert!(found.options[0].selected);
        assert_eq!(
            keys_to_choose(&found.options, 3, found.horizontal).unwrap(),
            "\x1b[C\x1b[C\r"
        );
        assert!(mentions_approval(b"Permission required Allow once"));

        let confirm = lines(
            "  ┃  △ Always allow\n\
             \x20 ┃\n\
             \x20 ┃  This will allow the following patterns until OpenCode is restarted\n\
             \x20 ┃\n\
             \x20 ┃  - /etc/*\n\
             \x20 ┃\n\
             \x20 ┃   Confirm   Cancel                     ⇆ select  enter confirm",
        );
        let found = detect(&confirm, &[]).expect("the follow-up is a dialog too");
        assert!(found.question.starts_with("Always allow: This will allow"));
        let labels: Vec<&str> = found.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Confirm", "Cancel"]);

        // opencode's ordinary screen is not a dialog.
        let idle = lines("  ┃  Build · DeepSeek\n  ┃  > ask anything");
        assert_eq!(detect(&idle, &[]), None);
    }

    /// WI-1279: Klaudia's host-change card and prompt, as its TUI draws them
    /// (`internal/tui/hostcard.go`), with the card partly in the scrollback.
    #[test]
    fn klaudia_host_change_prompts_are_person_only_dialogs() {
        let scrollback = lines(
            "› deploy the stack\n\
             \x20 ⚙ Write /tmp/komodo-header\n\
             This changes your machine",
        );
        let screen = lines(
            "  write the Komodo API header for the deploy\n\
             \x20 why: the deploy call needs the key in a header file\n\
             \x20 paths: /tmp/komodo-header\n\
             \x20 approving covers every step inside that scope, for this session only\n\
             \n\
             Change this machine? (y)es / (n)o / (s)omething else\n\
             \x20 grok-4.7 · 12 turns · 1.1M in+out",
        );
        let found = detect(&screen, &scrollback).expect("the host change is a dialog");
        assert_eq!(found.question, "Change this machine?");
        assert_eq!(found.kind, "permission");
        assert!(found.options.is_empty(), "letter keys are not offered");
        assert!(found
            .command_excerpt
            .starts_with("This changes your machine\n"));
        assert!(found.command_excerpt.contains("paths: /tmp/komodo-header"));
        assert!(found.command_excerpt.contains("why: the deploy call"));
        assert!(!found.command_excerpt.contains("deploy the stack"));
        assert!(mentions_approval(
            b"Change this machine? (y)es / (n)o / (s)omething else"
        ));

        // The hooks and drift variants, and a plain tool permission.
        let hooks = lines("This repository wants to run its own commands\n  .klaudia/hooks.toml\nRun these hooks? (y)es / (n)o");
        let found = detect(&hooks, &[]).expect("hooks prompt");
        assert_eq!(found.question, "Run these hooks?");
        assert!(found.command_excerpt.contains(".klaudia/hooks.toml"));

        let drift = lines("This wasn't part of what you approved\n  paths: ~/.klaudia/config.toml\nApprove this too? (y)es / (n)o / (s)omething else");
        let found = detect(&drift, &[]).expect("drift prompt");
        assert_eq!(found.question, "Approve this too?");
        assert!(found.command_excerpt.contains("config.toml"));

        let tool = crate::screen::klaudia_frames::approval();
        let found = detect(&tool, &[]).expect("tool permission");
        assert_eq!(found.question, "Allow Bash(cargo test --workspace)?");
        assert_eq!(found.command_excerpt, found.question);

        // An answered prompt is echoed as an answer line, not the prompt; and
        // a prompt quoted far up the screen is not live.
        let answered = lines("This changes your machine\n  paths: /tmp/x\ndeclined — Klaudia will carry on without it\n› ");
        assert_eq!(detect(&answered, &[]), None);
        let mut quoted = lines("Change this machine? (y)es / (n)o / (s)omething else");
        quoted.extend(std::iter::repeat_n(String::new(), 10));
        assert_eq!(detect(&quoted, &[]), None);
    }

    /// A screen captured off the WI-1127 Codex fork (0.162.0+fork.ca04973b4)
    /// under the engine's launch flags (`-a on-request`, `agent-state` title,
    /// no animations), with a fake Responses API and a harmless stdio MCP
    /// server (WI-1355), rendered by the engine's own terminal.
    fn codex_capture(name: &str, cols: u16) -> (Vec<String>, Vec<String>) {
        let path = format!(
            "{}/tests/fixtures/codex-approvals/{name}.pty",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(&path).expect("capture reads");
        let (rendered, scrollback) =
            crate::screen::render_with_scrollback(&bytes, 40, cols, SCROLLBACK_CONTEXT_LINES);
        (rendered.lines, scrollback)
    }

    fn captured(name: &str, cols: u16) -> Detected {
        let (screen, scrollback) = codex_capture(name, cols);
        // The PTY reader renders only when the stripped raw bytes mention one.
        let path = format!(
            "{}/tests/fixtures/codex-approvals/{name}.pty",
            env!("CARGO_MANIFEST_DIR")
        );
        let raw = std::fs::read(path).expect("capture reads");
        assert!(
            mentions_approval(&crate::activity::strip_ansi(&raw)),
            "{name}: prefilter"
        );
        // Found by the layout, not by the older question patterns.
        let found = detect_codex(&screen, &scrollback)
            .unwrap_or_else(|| panic!("{name}: no dialog in {screen:#?}"));
        assert_eq!(detect(&screen, &scrollback), Some(found.clone()));
        found
    }

    fn labels(d: &Detected) -> Vec<&str> {
        d.options.iter().map(|o| o.label.as_str()).collect()
    }

    #[test]
    fn codex_command_approval_with_a_reason_is_read_however_far_down_its_menu_is() {
        // Option 1 sits nine lines under the question: past where the old
        // detector looked, and the title stays `codex: working` throughout.
        let d = captured("exec-120", 120);
        assert_eq!(d.question, "Would you like to run the following command?");
        assert_eq!(d.kind, "permission");
        assert!(
            d.command_excerpt.contains("Reason: write a probe file"),
            "{d:?}"
        );
        assert!(
            d.command_excerpt.contains("$ touch /etc/vogt-probe-file"),
            "{d:?}"
        );
        assert_eq!(d.options.len(), 3);
        assert!(d.options[0].selected);
        assert_eq!(d.options[0].label, "Yes, proceed (y)");
        assert_eq!(
            d.options[2].label,
            "No, and tell Codex what to do differently (esc)"
        );

        let d = captured("execlong-120", 120);
        assert_eq!(d.question, "Would you like to run the following command?");
        assert!(d.command_excerpt.contains("a long reason"), "{d:?}");
        assert_eq!(d.options.len(), 3);
    }

    #[test]
    fn codex_prompts_wrapped_on_a_phone_width_screen_are_read_whole() {
        let d = captured("exec-45", 45);
        assert_eq!(d.question, "Would you like to run the following command?");
        assert_eq!(d.options.len(), 3);
        assert_eq!(
            d.options[2].label,
            "No, and tell Codex what to do differently (esc)"
        );

        let d = captured("mcp-45", 45);
        assert_eq!(
            d.question,
            "Allow the echo MCP server to run tool \"echo\"?"
        );
        assert_eq!(
            labels(&d),
            ["Allow", "Allow for this session", "Always allow", "Cancel"]
        );

        let d = captured("patch-45", 45);
        assert_eq!(d.question, "Would you like to make the following edits?");
        assert_eq!(d.options.len(), 3);
    }

    #[test]
    fn codex_patch_approval() {
        let d = captured("patch-120", 120);
        assert_eq!(d.question, "Would you like to make the following edits?");
        assert!(
            d.command_excerpt.contains("Destination: /var/tmp/"),
            "{d:?}"
        );
        assert_eq!(
            d.options[1].label,
            "Yes, and don't ask again for these files (a)"
        );
    }

    #[test]
    fn codex_mcp_tool_approval_captured() {
        let d = captured("mcp-120", 120);
        assert_eq!(
            d.question,
            "Allow the echo MCP server to run tool \"echo\"?"
        );
        assert_eq!(d.command_excerpt, "text: hello");
        assert_eq!(
            labels(&d),
            ["Allow", "Allow for this session", "Always allow", "Cancel"]
        );
        assert!(d.options[0].selected && !d.options[1].selected);
    }

    #[test]
    fn codex_mcp_elicitation_is_read_by_its_form_not_its_words() {
        // The server chose this message; nothing in it says "approve".
        let d = captured("elicitation-120", 120);
        assert_eq!(
            d.question,
            "Pick the deployment lane for this harmless probe"
        );
        assert_eq!(d.kind, "permission");
        assert_eq!(labels(&d), ["dev", "prod"]);
        assert!(d.command_excerpt.contains("Lane"), "{d:?}");
    }

    #[test]
    fn an_idle_codex_is_no_dialog() {
        let (screen, scrollback) = codex_capture("idle-120", 120);
        assert_eq!(detect(&screen, &scrollback), None);
    }

    /// The overlay as `approval_overlay.rs` builds it for the request kinds
    /// that need a sandbox or a tool the harness could not offer, from the
    /// fork's own strings and snapshots (ca04973b4).
    fn overlay(title: &[&str], body: &[&str], options: &[&str]) -> Vec<String> {
        let mut screen = vec!["• Working".to_string(), String::new()];
        screen.extend(title.iter().map(|l| format!("  {l}")));
        screen.push(String::new());
        screen.extend(body.iter().map(|l| format!("  {l}")));
        screen.extend([String::new(), String::new()]);
        for (i, o) in options.iter().enumerate() {
            let glyph = if i == 0 { "› " } else { "  " };
            screen.push(format!("{glyph}{}. {o}", i + 1));
        }
        screen.push(String::new());
        screen.push("  Press enter to confirm or esc to cancel".to_string());
        screen.extend(std::iter::repeat_n(String::new(), 3));
        screen
    }

    #[test]
    fn every_codex_overlay_kind_is_a_dialog() {
        let cases: &[(&[&str], &[&str], &[&str])] = &[
            (
                &["Do you want to approve network access to \"example.com\"?"],
                &["Reason: network request blocked"],
                &[
                    "Yes, just this once (y)",
                    "Yes, and allow this host for this conversation (a)",
                    "Yes, and allow this host in the future (p)",
                    "No, and tell Codex what to do differently (esc)",
                ],
            ),
            (
                &["Would you like to send input to terminal 4?"],
                &["$ y"],
                &[
                    "Yes, proceed (y)",
                    "No, and tell Codex what to do differently (esc)",
                ],
            ),
            (
                &["Would you like to send input to the existing terminal?"],
                &[],
                &[
                    "Yes, proceed (y)",
                    "No, and tell Codex what to do differently (esc)",
                ],
            ),
            (
                &["Would you like to grant these permissions?"],
                &[
                    "Reason: need workspace access",
                    "",
                    "Permission rule: network; read `/tmp/readme.txt`; write `/tmp/out.txt`",
                ],
                &[
                    "Yes, grant these permissions for this turn (y)",
                    "Yes, grant for this turn with strict auto review (r)",
                    "Yes, grant these permissions for this session (a)",
                    "No, continue without permissions (d)",
                ],
            ),
            (
                &["vogt needs your approval."],
                &["Approve the pending action."],
                &[
                    "Yes, provide the requested info (y)",
                    "No, but continue without it (n)",
                    "Cancel this request (esc)",
                ],
            ),
            (
                // Wrapped at 40 columns, as the fork's clipping snapshot shows.
                &["Would you like to run the following", "command?"],
                &["Thread: Robie [explorer]", "", "$ echo hi"],
                &[
                    "Yes, proceed (y)",
                    "No, and tell Codex what to do differently (esc)",
                ],
            ),
        ];
        for (title, body, options) in cases {
            let screen = overlay(title, body, options);
            let d = detect(&screen, &[]).unwrap_or_else(|| panic!("{screen:#?}"));
            assert_eq!(d.question, title.join(" "));
            assert_eq!(d.kind, "permission");
            assert_eq!(labels(&d), *options);
            assert!(mentions_approval(screen.join("\n").as_bytes()), "{title:?}");
        }

        // A command taller than the screen: the title is out of reach, the
        // menu and footer are not.
        let clipped = lines(
            "  [… 43 lines] ⌃g view all\n\
             › 1. Yes, proceed (y)\n\
             \x20 2. No, and tell Codex what to do\n\
             \x20    differently (esc)\n\
             \x20 Press enter to confirm or esc to\n\
             \x20 cancel",
        );
        let d = detect(&clipped, &[]).expect("clipped overlay");
        assert_eq!(d.question, CODEX_UNTITLED);
        assert_eq!(
            labels(&d),
            [
                "Yes, proceed (y)",
                "No, and tell Codex what to do differently (esc)"
            ]
        );
    }

    #[test]
    fn every_codex_elicitation_form_is_a_dialog() {
        // MCP tool approvals for an app and for a connector
        // (`mcp_tool_call.rs`), and forms whose wording the server chose,
        // from the fork's snapshots.
        for (screen, question) in [
            (
                "  Field 1/1\n  Allow this app to run tool \"run_action\"?\n  › 1. Allow   Run the tool and continue\n    2. Cancel  Cancel this tool call\n\n  enter to submit | esc to cancel",
                "Allow this app to run tool \"run_action\"?",
            ),
            (
                "  Field 1/1\n  Allow Google Calendar to run tool \"create_event\"?\n\n  Calendar: primary\n\n  › 1. Allow   Run the tool and continue\n    2. Cancel  Cancel this tool call\n  enter to submit | esc to cancel",
                "Allow Google Calendar to run tool \"create_event\"?",
            ),
            (
                "  Field 1/1\n  Boolean elicit MCP example: do you confirm?\n  › 1. Allow   Allow this request and continue\n    2. Deny    Decline this request and continue\n    3. Cancel  Cancel this request\n  enter to submit | esc to cancel",
                "Boolean elicit MCP example: do you confirm?",
            ),
            (
                "  Field 1/1 (1 required unanswered)\n  Allow this request?\n\n  Confirm\n  Approve the pending action.\n  › 1. True\n    2. False\n  enter to submit | esc to cancel",
                "Allow this request?",
            ),
            (
                "  Field 2/3 (1 required unanswered)\n  Enter a name\n\n  Name\n\n  › Type your answer\n\n  ⌃x enter to submit all | ⌃p / ⌃n change field | esc to cancel",
                "Enter a name",
            ),
        ] {
            let screen = lines(screen);
            let d = detect(&screen, &[]).unwrap_or_else(|| panic!("{screen:#?}"));
            assert_eq!(d.question, question);
            assert!(mentions_approval(screen.join("\n").as_bytes()), "{question}");
        }
    }

    #[test]
    fn codex_words_in_ordinary_output_are_not_a_dialog() {
        // The footer's words in the transcript, with the composer under them.
        let working = lines(
            "• Ran printf 'Press enter to confirm or esc to cancel'\n\
             \x20 └ 1. first\n\
             \x20   Press enter to confirm or esc to cancel\n\
             \n\
             • Working (3s • esc to interrupt)\n\
             \n\
             › Ask Codex to do anything\n\
             \x20 gpt-5 default · ~/vogt",
        );
        assert_eq!(detect(&working, &[]), None);
        let form_words = lines(
            "• Field 1/1 is the progress line\n\
             › 1. Allow\n\
             enter to submit | esc to cancel\n\
             \n\
             › Ask Codex to do anything",
        );
        assert_eq!(detect(&form_words, &[]), None);
        // An agent reply quoting a Codex question above an ordinary list,
        // with Codex's input box below: no dialog is up.
        for quote in [
            "Allow this app to run tool \"x\"?",
            "Would you like to grant these permissions?",
            "Would you like to send input to terminal 4?",
            "Do you want to approve network access to \"example.com\"?",
        ] {
            let reply = lines(&format!(
                "• Codex would show this:\n{quote}\n› 1. Allow\n  2. Cancel\n\n› Ask Codex to do anything\n  gpt-5 default · ~/vogt"
            ));
            assert_eq!(detect(&reply, &[]), None, "{quote}");
        }
        // A picker's footer goes back rather than cancelling.
        let picker = lines(
            "  Select model\n› 1. gpt-5\n  2. gpt-5-mini\n\n  Press enter to confirm or esc to go back",
        );
        assert_eq!(detect(&picker, &[]), None);
    }
}
