//! The rendered terminal screen of a session, for programs that drive it.
//!
//! The engine keeps no terminal emulator of its own: a session's output lives
//! as raw bytes in its scrollback ring, and every client (xterm.js in the
//! PWA) renders it by replaying that ring on attach. `GET
//! /api/sessions/{id}/screen` does the same replay server-side, into a `vt100`
//! grid sized to the PTY's current rows and cols, and returns the visible
//! rows as text. Nothing is kept between requests and the PTY reader's hot
//! path is untouched; the cost is one bounded replay per request.

use std::sync::Arc;

use vogt_engine_contract::{ActivityState, ScreenCursor, SessionScreen};

use crate::{
    error::{ApiError, Result},
    pty::Session,
};

/// How much of the scrollback ring is replayed to render the screen. A
/// screen is at most a few tens of KiB of cells; 1 MiB of recent output is
/// ample to reach the last full redraw of any TUI while bounding the cost of
/// a poll. The tail is aligned to a ground-state boundary, exactly as a
/// bounded attach replay is.
pub const SCREEN_REPLAY_BYTES: usize = 1024 * 1024;

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
    let rows = rows.max(1);
    let cols = cols.max(1);
    let mut parser = vt100::Parser::new_with_callbacks(rows, cols, 0, TitleCapture::default());
    parser.process(bytes);
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
    Rendered {
        rows,
        cols,
        lines,
        cursor: ScreenCursor { row, col },
        title,
    }
}

/// Whether the screen shows an input prompt an agent TUI or REPL draws when
/// it is waiting for the user: a line in the lower part of the screen that,
/// with any box-drawing border stripped, starts with a prompt glyph
/// (`>`, `❯`, `›`) followed by a space or nothing. Claude Code's input box
/// (`│ > …`), Codex's composer (`› …`) and a Python REPL (`>>> `) all match.
///
/// Only consulted when the session is `idle`: these TUIs keep drawing their
/// input box while they work, so the glyph alone does not mean "ready".
pub fn shows_prompt(lines: &[String]) -> bool {
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

/// Whether a driver can type now. See [`SessionScreen::ready`].
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

/// Render a session's current screen. The replay runs on the blocking pool
/// (it is CPU-bound over up to [`SCREEN_REPLAY_BYTES`]), and a panic inside
/// the emulator on hostile output fails this one request, not the engine.
pub async fn session_screen(session: Arc<Session>) -> Result<SessionScreen> {
    let (bytes, rows, cols) = session.screen_source(SCREEN_REPLAY_BYTES);
    let rendered = tokio::task::spawn_blocking(move || render(&bytes, rows, cols))
        .await
        .map_err(|e| ApiError::Internal(format!("render screen: {e}")))?;
    let activity = session.activity();
    let alive = session.is_alive();
    let ready = is_ready(activity, alive, &rendered.lines);
    Ok(SessionScreen {
        id: session.id,
        cols: rendered.cols,
        rows: rendered.rows,
        lines: rendered.lines,
        cursor: rendered.cursor,
        title: rendered.title,
        activity,
        alive,
        ready,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
