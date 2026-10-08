//! The rendered terminal screen of a session, for programs that drive it.
//!
//! A session's output lives as raw bytes in its scrollback ring. The engine
//! also keeps its own terminal: the PTY reader feeds each chunk
//! into a per-session `vt100` grid ([`Terminal`]), and `GET
//! /api/sessions/{id}/screen` reads that grid rather than replaying a tail of
//! the ring. A diff-painting TUI (opencode / OpenTUI) draws one full frame and
//! then only the cells that changed, with no newline at all, so once more than
//! the replay window's worth of diffs have followed the last full repaint a
//! tail replay onto a blank grid shows only the recently changed cells — a
//! spinner and a progress bar on an otherwise blank screen (WI-990). The live
//! grid has seen every byte since the session started, so it still holds the
//! whole frame. A resize reflows it; a hibernated session has no live grid and
//! falls back to replaying the tail it kept.

use std::sync::Arc;

use vogt_engine_contract::{ActivityState, ScreenCursor, SessionScreen};

use crate::{
    error::{ApiError, Result},
    pty::Session,
};

/// How much of the scrollback ring is replayed to render the screen of a
/// session that has no live grid (a hibernated one, whose grid was dropped
/// with its process). A live session's screen is read from its [`Terminal`]
/// and never replays. The tail is aligned to a ground-state boundary, exactly
/// as a bounded attach replay is.
pub const SCREEN_REPLAY_BYTES: usize = 1024 * 1024;

/// Scrollback lines the live grid keeps above the screen. The screen route
/// returns at most [`MAX_SCROLLBACK_LINES`] of them, and the grid is what
/// bounds the memory a session's emulator holds.
pub const GRID_SCROLLBACK_LINES: usize = MAX_SCROLLBACK_LINES;

/// The terminal one session's PTY output is parsed into.
///
/// Fed incrementally by the PTY reader ([`Terminal::process`]) and resized
/// with the PTY ([`Terminal::resize`]). `vt100` 0.16.2 (pinned in
/// `Cargo.lock`, already the crate the on-demand replay used) parses the
/// stream into a cell grid: it implements the alternate screen, scroll
/// regions and reflow on resize, consumes DEC private modes it does not act
/// on — including 2026, synchronized output, which it treats as a no-op so a
/// frame is never held back — and drops OSC queries and any other sequence it
/// does not implement without letting their bytes reach a cell. Holding one
/// per session is what lets `session_screen` answer from the grid instead of
/// from a tail of raw bytes.
pub struct Terminal {
    parser: vt100::Parser<TitleCapture>,
}

impl Terminal {
    /// A grid of `rows`×`cols` with [`GRID_SCROLLBACK_LINES`] of history.
    pub fn new(rows: u16, cols: u16) -> Self {
        let (rows, cols) = (rows.max(1), cols.max(1));
        Self {
            parser: vt100::Parser::new_with_callbacks(
                rows,
                cols,
                GRID_SCROLLBACK_LINES,
                TitleCapture::default(),
            ),
        }
    }

    /// Parse the next chunk of PTY output into the grid.
    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// Reflow the grid to the PTY's new size. A no-op when it already matches.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if self.parser.screen().size() != (rows, cols) {
            self.parser.screen_mut().set_size(rows, cols);
        }
    }

    /// The visible screen and up to `scrollback_lines` lines above it.
    pub fn render(&mut self, scrollback_lines: usize) -> (Rendered, Vec<String>) {
        read_parser(&mut self.parser, scrollback_lines)
    }

    /// Escape codes that reproduce the grid's current screen, the scrollback
    /// it holds above it, and the modes and cursor, so a client can paint the
    /// whole frame with one write instead of replaying the raw byte history
    /// (WI-121).
    ///
    /// Scrollback is emitted oldest first, one `contents_formatted` per
    /// screenful, and the screen last with the cursor restored — the same
    /// order a tail replay would have produced it, so xterm's own scrollback
    /// ends up holding it. Every sequence here is one `vt100` already
    /// consumed while building the grid, so nothing reaches the client that
    /// the grid did not accept (WI-987).
    pub fn frame(&mut self) -> Vec<u8> {
        let (rows, _cols) = self.parser.screen().size();
        let mut out = Vec::new();
        // Clear the visible screen only. RIS (`ESC c`) would also wipe the
        // client's scrollback, which is exactly where the history below has
        // to land, and the client already resets its terminal on reset:true.
        // History rows, oldest first, one per line. Read straight off the
        // grid rather than sliced out of a formatted dump: a scrolled view
        // overlaps the next one, and slicing that overlap off dropped rows.
        let history: Vec<Vec<u8>> = {
            let screen = self.parser.screen_mut();
            screen.set_scrollback(usize::MAX);
            let total = screen.scrollback();
            let mut rows_out = Vec::with_capacity(total);
            let mut off = total;
            while off > 0 {
                let take = off.min(rows as usize);
                screen.set_scrollback(off);
                rows_out.extend(screen.rows_formatted(0, u16::MAX).take(take));
                off -= take;
            }
            rows_out
        };
        for row in &history {
            out.extend_from_slice(row);
            out.extend_from_slice(b"\r\n");
        }
        self.parser.screen_mut().set_scrollback(0);
        let screen = self.parser.screen();
        out.extend_from_slice(&screen.state_formatted());
        out.extend_from_slice(&screen.cursor_state_formatted());
        out
    }
}

/// Captures the window title a program sets (OSC 0 / OSC 2).
#[derive(Default)]
struct TitleCapture {
    title: Option<String>,
}

impl vt100::Callbacks for TitleCapture {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).into_owned());
    }
}

/// What rendering produced, before the session's live state is added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub rows: u16,
    pub cols: u16,
    pub lines: Vec<String>,
    pub cursor: ScreenCursor,
    pub title: Option<String>,
}

/// Replay `bytes` into a fresh `rows` x `cols` terminal and read it back.
pub fn render(bytes: &[u8], rows: u16, cols: u16) -> Rendered {
    render_with_scrollback(bytes, rows, cols, 0).0
}

/// The most scrollback lines a caller may ask for with the screen.
pub const MAX_SCROLLBACK_LINES: usize = 2000;

/// [`render`], plus up to `scrollback_lines` lines that scrolled off the top
/// of the screen, oldest first — the context a driver needs when a dialog's
/// command, or an agent's reply, is taller than the screen. Bounded by what
/// the replayed tail produced.
pub fn render_with_scrollback(
    bytes: &[u8],
    rows: u16,
    cols: u16,
    scrollback_lines: usize,
) -> (Rendered, Vec<String>) {
    let rows = rows.max(1);
    let cols = cols.max(1);
    let keep = scrollback_lines.min(MAX_SCROLLBACK_LINES);
    let mut parser = vt100::Parser::new_with_callbacks(rows, cols, keep, TitleCapture::default());
    parser.process(bytes);
    read_parser(&mut parser, scrollback_lines)
}

/// Read a parsed grid back as text. Shared by the on-demand replay and the
/// live [`Terminal`], so both answer in the same shape.
fn read_parser(
    parser: &mut vt100::Parser<TitleCapture>,
    scrollback_lines: usize,
) -> (Rendered, Vec<String>) {
    let (rows, cols) = parser.screen().size();
    let keep = scrollback_lines.min(MAX_SCROLLBACK_LINES);
    let mut history = Vec::new();
    if keep > 0 {
        // The view at offset `off` starts at history line `len - off`; read
        // it a screenful at a time, oldest first, then put the screen back.
        let screen = parser.screen_mut();
        screen.set_scrollback(usize::MAX);
        let mut off = screen.scrollback();
        while off > 0 {
            screen.set_scrollback(off);
            let take = off.min(rows as usize);
            history.extend(
                screen
                    .rows(0, cols)
                    .take(take)
                    .map(|row| row.trim_end().to_string()),
            );
            off -= take;
        }
        screen.set_scrollback(0);
    }
    let screen = parser.screen();
    let mut lines: Vec<String> = screen
        .rows(0, cols)
        .map(|row| row.trim_end().to_string())
        .collect();
    lines.resize(rows as usize, String::new());
    let (row, col) = screen.cursor_position();
    let title = parser
        .callbacks()
        .title
        .clone()
        .filter(|t| !t.trim().is_empty());
    (
        Rendered {
            rows,
            cols,
            lines,
            cursor: ScreenCursor { row, col },
            title,
        },
        history,
    )
}

/// Whether the screen shows an input prompt an agent TUI or REPL draws when
/// it is waiting for the user: a line in the lower part of the screen that,
/// with any box-drawing border stripped, starts with a prompt glyph
/// (`>`, `❯`, `›`) followed by a space or nothing. Claude Code's input box
/// (`│ > …`), Codex's composer (`› …`) and a Python REPL (`>>> `) all match.
///
/// Only consulted when the session is `idle`: these TUIs keep drawing their
/// input box while they work, so the glyph alone does not mean "ready".
///
/// opencode draws no prompt glyph at all, so it has its own test
/// ([`shows_opencode_prompt`]); without it an opencode session was never
/// `ready` and a driver waiting for it never re-prompted it (WI-949).
pub fn shows_prompt(lines: &[String]) -> bool {
    shows_glyph_prompt(lines) || shows_opencode_prompt(lines)
}

fn shows_glyph_prompt(lines: &[String]) -> bool {
    const BORDER: &[char] = &['│', '┃', '║', '|', ' ', '\u{a0}'];
    lines
        .iter()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(10)
        .any(|line| {
            let body = line.trim_start_matches(BORDER);
            let rest = body
                .strip_prefix(">>>")
                .or_else(|| body.strip_prefix('>'))
                .or_else(|| body.strip_prefix('❯'))
                .or_else(|| body.strip_prefix('›'));
            match rest {
                Some(rest) => rest.is_empty() || rest.starts_with([' ', '\u{a0}']),
                None => false,
            }
        })
}

/// Whether the screen shows opencode's input box with no turn running.
///
/// opencode (1.18) draws its composer as a heavy left bar (`┃`) closed by a
/// `╹▀▀▀` footer, both idle and while it works; what differs is the status
/// line under the box, which says `esc interrupt` (beside a spinner) only
/// while a turn runs. So: the bar and its footer near the bottom of the
/// screen, and no `esc interrupt` among those lines.
pub fn shows_opencode_prompt(lines: &[String]) -> bool {
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(10)
        .collect();
    let footer = tail.iter().any(|l| {
        l.strip_prefix('╹')
            .is_some_and(|rest| rest.starts_with('▀'))
    });
    let bar = tail.iter().any(|l| l.starts_with('┃'));
    let working = tail.iter().any(|l| l.contains("esc interrupt"));
    footer && bar && !working
}

/// Whether a driver can type now. See [`SessionScreen::ready`].
///
/// A session `awaiting-approval` is not ready: it is at a permission dialog,
/// where typed text and Enter answer the dialog rather than reach the agent.
pub fn is_ready(activity: ActivityState, alive: bool, lines: &[String]) -> bool {
    if !alive {
        return false;
    }
    match activity {
        ActivityState::WaitingForInput => true,
        ActivityState::Idle => shows_prompt(lines),
        _ => false,
    }
}

/// Render a session's current screen, with up to `scrollback_lines` lines of
/// history above it. A live session is read from its [`Terminal`]; a
/// hibernated one, which has none, is replayed from the tail it kept. Either
/// way the work runs on the blocking pool, and a panic inside the emulator on
/// hostile output fails this one request, not the engine.
pub async fn session_screen(
    session: Arc<Session>,
    scrollback_lines: usize,
) -> Result<SessionScreen> {
    // The live state is read *before* the grid: the state was computed from
    // output already parsed into it, so the render includes the output that
    // state describes. Read after, a `waiting-for-input` that arrived
    // mid-render could be paired with a screen that does not yet show its
    // prompt.
    let summary = session.summary();
    let rendering = Arc::clone(&session);
    let (rendered, scrollback) =
        tokio::task::spawn_blocking(move || rendering.render_screen(scrollback_lines))
            .await
            .map_err(|e| ApiError::Internal(format!("render screen: {e}")))?;
    let ready = is_ready(summary.activity, summary.alive, &rendered.lines);
    Ok(SessionScreen {
        id: session.id,
        cols: rendered.cols,
        rows: rendered.rows,
        lines: rendered.lines,
        cursor: rendered.cursor,
        title: rendered.title,
        activity: summary.activity,
        alive: summary.alive,
        ready,
        scrollback,
        turn_started_at: summary.turn_started_at,
        last_output_at: summary.last_output_at,
        approval: summary.approval,
        blocked: summary.blocked,
    })
}

/// Render the screen a hibernated session kept: `bytes` is the tail of its
/// output when it stopped, `rows`×`cols` the size it had. Never `ready` —
/// nothing is there to type into until it is woken.
pub async fn kept_screen(
    id: uuid::Uuid,
    bytes: bytes::Bytes,
    rows: u16,
    cols: u16,
    summary: vogt_engine_contract::SessionSummary,
    scrollback_lines: usize,
) -> Result<SessionScreen> {
    let (rendered, scrollback) = tokio::task::spawn_blocking(move || {
        render_with_scrollback(&bytes, rows, cols, scrollback_lines)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("render screen: {e}")))?;
    Ok(SessionScreen {
        id,
        cols: rendered.cols,
        rows: rendered.rows,
        lines: rendered.lines,
        cursor: rendered.cursor,
        title: rendered.title,
        activity: summary.activity,
        alive: false,
        ready: false,
        scrollback,
        turn_started_at: None,
        last_output_at: None,
        approval: None,
        blocked: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WI-121: the frame reproduces the screen a raw replay would, in a
    /// handful of bytes, including a frame the tail replay loses.
    #[test]
    fn the_frame_reproduces_the_grid_and_stays_small() {
        let mut term = Terminal::new(4, 20);
        // A full paint, then far more than a screenful of cell diffs — the
        // shape that makes a tail replay blank (WI-990).
        term.process(b"\x1b[2J\x1b[1;1Hbuild ok");
        for n in 0..4000 {
            term.process(format!("\x1b[4;1Hspin {n:04}").as_bytes());
        }
        let frame = term.frame();
        // The raw history is tens of KiB; the frame is the current cells.
        assert!(frame.len() < 4_096, "frame is {} bytes", frame.len());
        let again = render(&frame, 4, 20);
        assert_eq!(again.lines[0], "build ok");
        assert_eq!(again.lines[3], "spin 3999");
        assert_eq!(again.cursor, ScreenCursor { row: 3, col: 9 });
    }

    #[test]
    fn the_frame_keeps_scrollback_above_the_screen() {
        let mut term = Terminal::new(2, 10);
        term.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let frame = term.frame();
        let text = String::from_utf8_lossy(&frame);
        let plain: String = text
            .chars()
            .filter(|c| *c == '\n' || !c.is_control())
            .collect();
        // Oldest history first, then the visible screen, each on its own line.
        // History rows come first, oldest first, then the visible screen.
        let screen_at = plain.find("four").expect("screen missing");
        let history = &plain[..screen_at];
        assert!(history.contains("one"), "{plain:?}");
        assert!(history.contains("two"), "{plain:?}");
        assert!(history.contains("three"), "{plain:?}");
        assert!(history.find("one") < history.find("two"), "{plain:?}");
        assert!(history.find("two") < history.find("three"), "{plain:?}");
        assert!(plain[screen_at..].contains("five"), "{plain:?}");
    }

    #[test]
    fn cursor_positioned_text_keeps_its_spaces() {
        // "If nothing is captured" drawn word by word with absolute moves —
        // what a stripped log turns into "Ifnothingiscaptured".
        let bytes = b"\x1b[2J\x1b[1;1HIf\x1b[1;4Hnothing\x1b[1;12His\x1b[1;15Hcaptured";
        let r = render(bytes, 4, 40);
        assert_eq!(r.lines[0], "If nothing is captured");
        assert_eq!(r.lines.len(), 4);
        assert_eq!(r.lines[1], "");
        assert_eq!(r.cursor, ScreenCursor { row: 0, col: 22 });
    }

    #[test]
    fn redraws_and_spinner_frames_leave_only_the_last_frame() {
        let bytes = "working ✢\r\x1b[Kworking ✶\r\x1b[Kworking ✻\r\x1b[Kdone\r\n> ".as_bytes();
        let r = render(bytes, 3, 20);
        assert_eq!(r.lines, vec!["done", ">", ""]);
        assert_eq!(r.cursor, ScreenCursor { row: 1, col: 2 });
    }

    #[test]
    fn a_dismissed_menu_is_gone_from_the_screen() {
        // Draw a menu, then clear the screen and draw the prompt.
        let bytes = b"\x1b[?1049h1. Yes\r\n2. No\x1b[?1049l\x1b[2J\x1b[Hready> ";
        let r = render(bytes, 3, 20);
        assert!(r.lines.iter().all(|l| !l.contains("1. Yes")), "{r:?}");
        assert_eq!(r.lines[0], "ready>");
    }

    #[test]
    fn scrollback_lines_come_back_oldest_first() {
        let bytes = b"one\r\ntwo\r\nthree\r\nfour\r\nfive";
        let (r, history) = render_with_scrollback(bytes, 2, 10, 100);
        assert_eq!(r.lines, vec!["four", "five"]);
        assert_eq!(history, vec!["one", "two", "three"]);
        // Bounded by what was asked for: the most recent lines are kept.
        let (_, two) = render_with_scrollback(bytes, 2, 10, 2);
        assert_eq!(two, vec!["two", "three"]);
        // None asked for, none returned — the plain render is unchanged.
        let (plain, none) = render_with_scrollback(bytes, 2, 10, 0);
        assert!(none.is_empty());
        assert_eq!(plain, render(bytes, 2, 10));
    }

    #[test]
    fn captures_the_window_title() {
        let r = render(b"\x1b]0;claude: fixing tests\x07hello", 2, 20);
        assert_eq!(r.title.as_deref(), Some("claude: fixing tests"));
        assert_eq!(render(b"hello", 2, 20).title, None);
    }

    #[test]
    fn a_degenerate_size_still_renders() {
        let r = render(b"x", 0, 0);
        assert_eq!((r.rows, r.cols), (1, 1));
        assert_eq!(r.lines, vec!["x"]);
    }

    /// Every non-blank cell's text, for asserting that nothing leaked.
    fn visible(r: &Rendered) -> String {
        r.lines.join("\n").trim().to_string()
    }

    /// WI-987: DEC private modes the emulator implements, ignores or has never
    /// heard of, and the terminal queries a modern TUI sends at startup, are
    /// all consumed whole. Not one parameter byte reaches the grid as text,
    /// and a synchronized update (mode 2026) never holds the screen back.
    #[test]
    fn private_modes_and_queries_never_reach_the_grid() {
        let sequences: &[&[u8]] = &[
            b"\x1b[?2026h",                       // begin synchronized update
            b"\x1b[?2026l",                       // end synchronized update
            b"\x1b[?9999h",                       // a mode nobody implements
            b"\x1b[?9999;2026;1049l",             // several at once
            b"\x1b[?2027h\x1b[?2031h\x1b[?1016h", // grapheme, theme, pixel mouse
            b"\x1b]11;?\x07",                     // OSC 11 background query, BEL
            b"\x1b]10;?\x1b\\",                   // OSC 10 foreground query, ST
            b"\x1b]4;0;?\x07",                    // palette query
            b"\x1b[?2026$p",                      // DECRQM: is 2026 supported?
            b"\x1b[>0q",                          // XTVERSION
            b"\x1b[?u",                           // kitty keyboard query
            b"\x1b[>4;1m",                        // modifyOtherKeys
            b"\x1bP+q4d73\x1b\\",                 // XTGETTCAP
            b"\x1b]66;w=1; \x1b\\",               // kitty text sizing probe
            b"\x1b[6n",                           // cursor position report
        ];
        for seq in sequences {
            let r = render(seq, 3, 40);
            assert_eq!(visible(&r), "", "{:?} leaked", String::from_utf8_lossy(seq));
            // Text after the sequence lands at the origin, unshifted.
            let mut bytes = seq.to_vec();
            bytes.extend_from_slice(b"ok");
            let r = render(&bytes, 3, 40);
            assert_eq!(r.lines[0], "ok", "{:?}", String::from_utf8_lossy(seq));
        }
        // A frame drawn inside a synchronized update shows, whether or not the
        // update is ever closed: there is no held state to get stuck in.
        let open = render(b"\x1b[?2026h\x1b[2;3Hframe", 3, 20);
        assert_eq!(open.lines[1], "  frame");
        let closed = render(b"\x1b[?2026h\x1b[2;3Hframe\x1b[?2026l", 3, 20);
        assert_eq!(closed.lines, open.lines);
    }

    /// WI-987 end to end: an opencode-shaped stream (no newline anywhere, every
    /// frame a synchronized update) cut by the scrollback at every possible
    /// size renders without a fragment of an escape sequence as text — the
    /// `26l` an operator saw on a blanked opencode session.
    #[test]
    fn a_tui_tail_cut_anywhere_renders_no_sequence_fragments() {
        let mut stream = b"\x1b[?1049h\x1b[?2027h".to_vec();
        for n in 0..30 {
            stream.extend(
                format!(
                    "\x1b[?2026h\x1b[?25l\x1b[2;1H\x1b[38;5;114m\x1b[48;5;232m~\x1b[0m\
                     \x1b[2;3H\x1b[38;5;255mtick{n:02}\x1b[0m\x1b[?2026l"
                )
                .bytes(),
            );
        }
        let mut sb = crate::scrollback::Scrollback::new(stream.len());
        sb.push(&stream);
        let frame = stream.len() / 30;
        for limit in frame..stream.len() {
            let tail = sb.snapshot_tail(limit);
            let r = render(&tail, 3, 20);
            // A tail that starts after a frame's cursor move draws that
            // frame's first cells at the origin — a diff stream carries no
            // more context than that — but never a sequence's bytes as text.
            for line in &r.lines {
                assert!(
                    !line.contains(|c: char| "[;?hlm".contains(c)),
                    "limit {limit}: fragment in {r:?}"
                );
            }
            assert_eq!(r.lines[1], "~ tick29", "limit {limit}: {r:?}");
        }
    }

    #[test]
    fn recognises_agent_prompts() {
        let claude = vec![
            "● Done.".to_string(),
            "╭──────────────╮".to_string(),
            "│ >            │".to_string(),
            "╰──────────────╯".to_string(),
            "  ? for shortcuts".to_string(),
        ];
        assert!(shows_prompt(&claude));
        assert!(shows_prompt(&["› Ask Codex to do anything".to_string()]));
        assert!(shows_prompt(&[">>> ".to_string()]));
        assert!(shows_prompt(&["❯".to_string()]));
        assert!(!shows_prompt(&["compiling foo v0.1.0".to_string()]));
        assert!(!shows_prompt(&["->x".to_string()]));
        assert!(!shows_prompt(&[]));
    }

    fn screen(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    /// The three screens opencode 1.18.31 drew in a 120x40 PTY (2026-10-06),
    /// trimmed to their bottom: a fresh session, one whose run has ended, and
    /// one mid-run.
    #[test]
    fn opencode_is_ready_at_its_idle_box_and_not_while_a_turn_runs() {
        let fresh = screen(&[
            "                       ┃",
            "                       ┃  Ask anything… \"What is the tech stack of this project?\"",
            "                       ┃",
            "                       ┃  Build · DeepSeek V4.1 Flash OpenRouter",
            "                       ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
            "                                     tab agents  ctrl+p commands",
            "",
            "  ~/Working/Active/javascan  ⊙ 2 MCP /status    1.18.31",
        ]);
        let finished = screen(&[
            "  ┃  Reply with the single word hi. Do not use tools.",
            "  ┃",
            "     hi",
            "     ▣  Build · DeepSeek V4.1 Flash · 3.8s",
            "  ┃",
            "  ┃",
            "  ┃",
            "  ┃  Build · DeepSeek V4.1 Flash OpenRouter",
            "  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
            "   ~/Working/Active/javascan          36.4K (3%) · $0. ctrl+p",
            "   commands",
        ]);
        let running = screen(&[
            "  ┃  Write a 400 word essay about tea. Do not use tools.",
            "  ┃",
            "     ▣  Build · DeepSeek V4.1 Flash",
            "  ┃",
            "  ┃",
            "  ┃",
            "  ┃  Build · DeepSeek V4.1 Flash OpenRouter",
            "  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
            "   ⬝⬝⬝⬝⬝⬝⬝⬝  esc interrupt                 tab agents  ctrl+p commands",
        ]);
        assert!(shows_prompt(&fresh));
        assert!(shows_prompt(&finished));
        assert!(!shows_prompt(&running), "a running turn is not ready");
        assert!(is_ready(ActivityState::Idle, true, &finished));
        assert!(!is_ready(ActivityState::Running, true, &finished));
        // A bar with no footer is just a box-drawing character in output.
        assert!(!shows_opencode_prompt(&screen(&["┃ some table cell"])));
    }

    /// WI-990: a diff-painting TUI draws one full frame, then only the cells
    /// that changed. Once more than the replay window of diffs has followed,
    /// replaying the tail onto a blank grid loses the frame; a grid that was
    /// fed every byte keeps it, and still shows none of the control bytes.
    #[test]
    fn a_live_grid_keeps_the_frame_a_tail_replay_loses() {
        let rows = 4u16;
        let cols = 30u16;
        let mut full = b"\x1b[?1049h\x1b[2J".to_vec();
        for row in 1..=rows {
            full.extend(format!("\x1b[{row};1Hline {row} of the frame").bytes());
        }
        let mut diffs = Vec::new();
        for n in 0..50000 {
            diffs
                .extend(format!("\x1b[?2026h\x1b[{};{}H{n:06}\x1b[?2026l", rows, cols - 5).bytes());
        }
        // The diffs alone outrun the replay window, so a tail replay starts
        // after the full frame and can only draw the last counter.
        assert!(diffs.len() > SCREEN_REPLAY_BYTES);
        let tail = &diffs[diffs.len() - SCREEN_REPLAY_BYTES..];
        let replayed = render(tail, rows, cols);
        assert!(
            replayed.lines.iter().all(|l| !l.contains("line 1")),
            "a tail replay should have lost the frame: {replayed:?}"
        );

        let mut live = Terminal::new(rows, cols);
        live.process(&full);
        live.process(&diffs);
        let (rendered, _) = live.render(0);
        assert_eq!(rendered.lines[0], "line 1 of the frame");
        assert_eq!(rendered.lines[1], "line 2 of the frame");
        // The counter the diffs paint sits at the end of the last row, over
        // the tail of that row's text, and nothing of the escape sequences
        // around it leaked as text.
        assert!(
            rendered.lines[3].starts_with("line 4 of the"),
            "{:?}",
            rendered.lines[3]
        );
        assert!(
            rendered.lines[3].ends_with("049999"),
            "{:?}",
            rendered.lines[3]
        );
        for line in &rendered.lines {
            assert!(!line.contains('\u{1b}'), "{line:?}");
        }
    }

    /// WI-990: resize reflows the live grid, the alternate screen restores the
    /// main one, and a mode the emulator has never heard of changes nothing.
    #[test]
    fn resize_reflows_and_the_alternate_screen_restores() {
        let mut t = Terminal::new(3, 10);
        t.process(b"main line");
        t.process(b"\x1b[?1049h\x1b[2J\x1b[Halt screen");
        assert_eq!(t.render(0).0.lines[0], "alt screen");
        t.process(b"\x1b[?1049l");
        assert_eq!(t.render(0).0.lines[0], "main line");

        t.process(b"\x1b[?9999h");
        assert_eq!(t.render(0).0.lines[0], "main line");

        t.resize(3, 4);
        let (r, _) = t.render(0);
        assert_eq!((r.rows, r.cols), (3, 4));
        assert_eq!(r.lines[0], "main");
    }

    #[test]
    fn readiness_needs_a_live_quiet_prompt() {
        let prompt = vec!["│ > ".to_string()];
        let plain = vec!["building".to_string()];
        assert!(is_ready(ActivityState::WaitingForInput, true, &plain));
        assert!(is_ready(ActivityState::Idle, true, &prompt));
        assert!(!is_ready(ActivityState::Idle, true, &plain));
        // A TUI keeps its input box on screen while it works.
        assert!(!is_ready(ActivityState::Running, true, &prompt));
        assert!(!is_ready(ActivityState::WaitingForInput, false, &prompt));
        assert!(!is_ready(ActivityState::Exited, false, &prompt));
    }
}
