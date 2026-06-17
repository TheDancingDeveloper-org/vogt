//! VT100/ANSI terminal grid + parser.
//!
//! Ported from the in-house `rdpapp` terminal (`rdpapp/src/ssh/terminal.rs`),
//! which is a renderer-agnostic emulator built on `vte`. The grid stores cells
//! as `(char, fg, bg, bold)` using a local [`TermColor`] type so it has no GUI
//! dependency — the GPUI layer rasterizes [`TermGrid::view_rows`] separately.
//!
//! Kept deliberately close to the source so fixes can be cross-ported. The
//! attach path feeds server PTY bytes straight into [`TermProcessor::process`].

use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};

// ── TermColor — lightweight RGB colour type ──────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TermColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    /// When true this cell uses the terminal's default background colour.
    pub is_default: bool,
}

impl TermColor {
    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        is_default: true,
    };

    pub fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self {
            r,
            g,
            b,
            is_default: false,
        }
    }
}

const HISTORY_MAX: usize = 50_000;

pub const DEFAULT_COLS: usize = 200;
pub const DEFAULT_ROWS: usize = 50;

/// Default foreground (light grey) used at reset and on a fresh cell.
pub const DEFAULT_FG: TermColor = TermColor {
    r: 204,
    g: 204,
    b: 204,
    is_default: false,
};

// ── Cell ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub struct Cell {
    pub ch: char,
    pub fg: TermColor,
    pub bg: TermColor,
    pub bold: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: DEFAULT_FG,
            bg: TermColor::TRANSPARENT,
            bold: false,
        }
    }
}

// ── TermGrid — the logical terminal state ─────────────────────────────────────
// Implements vte::Perform; does NOT own the parser (avoids aliased &mut self).

pub struct TermGrid {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<Vec<Cell>>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    /// Lines that have scrolled off the top of the screen, oldest first.
    pub history: VecDeque<Vec<Cell>>,
    /// 0 = live view (bottom); N = scrolled N lines up into history.
    pub scroll_offset: usize,
    /// Maximum number of lines to retain in the scrollback history.
    pub history_max: usize,
    cur_fg: TermColor,
    cur_bg: TermColor,
    cur_bold: bool,
    /// Set when a BEL (0x07) control code is received. Cleared by `take_bell()`.
    bell_pending: bool,
    /// Set when an OSC 0/2 title sequence is received. Cleared by `take_osc_title()`.
    osc_title: Option<String>,
    // ── Alternate screen buffer (?1049h / ?47h) ───────────────────────────
    alt_cells: Vec<Vec<Cell>>,
    saved_cursor_row: usize,
    saved_cursor_col: usize,
    pub using_alt_screen: bool,
    // ── DECSTBM scrolling region ──────────────────────────────────────────
    scroll_top: usize,
    /// Inclusive bottom margin (default = rows - 1).
    scroll_bottom: usize,
    // ── Cursor visibility ─────────────────────────────────────────────────
    pub cursor_visible: bool,
    // ── Mouse selection (viewport coordinates: row in 0..rows, col in 0..cols) ──
    sel_anchor: Option<(usize, usize)>,
    sel_cursor: Option<(usize, usize)>,
}

impl TermGrid {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self::with_scrollback(cols, rows, HISTORY_MAX)
    }

    pub fn with_scrollback(cols: usize, rows: usize, history_max: usize) -> Self {
        let scroll_bottom = rows.saturating_sub(1);
        Self {
            cols,
            rows,
            cells: vec![vec![Cell::default(); cols]; rows],
            cursor_row: 0,
            cursor_col: 0,
            history: VecDeque::new(),
            scroll_offset: 0,
            history_max,
            cur_fg: DEFAULT_FG,
            cur_bg: TermColor::TRANSPARENT,
            cur_bold: false,
            bell_pending: false,
            osc_title: None,
            alt_cells: vec![vec![Cell::default(); cols]; rows],
            saved_cursor_row: 0,
            saved_cursor_col: 0,
            using_alt_screen: false,
            scroll_top: 0,
            scroll_bottom,
            cursor_visible: true,
            sel_anchor: None,
            sel_cursor: None,
        }
    }

    // ── Mouse selection ───────────────────────────────────────────────────

    /// Start a selection at the given viewport cell, clamped to the grid.
    pub fn begin_selection(&mut self, row: usize, col: usize) {
        let cell = self.clamp_cell(row, col);
        self.sel_anchor = Some(cell);
        self.sel_cursor = Some(cell);
    }

    /// Extend the in-progress selection to the given viewport cell.
    pub fn update_selection(&mut self, row: usize, col: usize) {
        if self.sel_anchor.is_some() {
            self.sel_cursor = Some(self.clamp_cell(row, col));
        }
    }

    /// Drop any active selection.
    pub fn clear_selection(&mut self) {
        self.sel_anchor = None;
        self.sel_cursor = None;
    }

    /// True when there is a non-empty selection (a pure click does not count).
    pub fn has_selection(&self) -> bool {
        matches!((self.sel_anchor, self.sel_cursor), (Some(a), Some(c)) if a != c)
    }

    fn clamp_cell(&self, row: usize, col: usize) -> (usize, usize) {
        (
            row.min(self.rows.saturating_sub(1)),
            col.min(self.cols.saturating_sub(1)),
        )
    }

    /// Selection ordered in reading order (by row, then col), or `None`.
    fn normalized_selection(&self) -> Option<((usize, usize), (usize, usize))> {
        if !self.has_selection() {
            return None;
        }
        let a = self.sel_anchor?;
        let c = self.sel_cursor?;
        if (a.0, a.1) <= (c.0, c.1) {
            Some((a, c))
        } else {
            Some((c, a))
        }
    }

    /// Whether the given viewport cell is within the current selection.
    pub fn is_cell_selected(&self, row: usize, col: usize) -> bool {
        match self.normalized_selection() {
            Some((a, b)) => {
                (row > a.0 || (row == a.0 && col >= a.1))
                    && (row < b.0 || (row == b.0 && col <= b.1))
            }
            None => false,
        }
    }

    /// Text inside the current selection (viewport cells), or `None` if none.
    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.normalized_selection()?;
        let rows = self.view_rows();
        let last = rows.len().saturating_sub(1);
        let end_row = b.0.min(last);
        let mut lines: Vec<String> = Vec::new();
        for (r, &row) in rows.iter().enumerate().take(end_row + 1).skip(a.0) {
            if row.is_empty() {
                lines.push(String::new());
                continue;
            }
            let sc = if r == a.0 { a.1 } else { 0 };
            let ec = if r == b.0 {
                b.1
            } else {
                self.cols.saturating_sub(1)
            };
            let ec = ec.min(row.len() - 1);
            let line: String = if sc <= ec {
                row[sc..=ec]
                    .iter()
                    .map(|cell| cell.ch)
                    .filter(|&ch| ch != '\0')
                    .collect()
            } else {
                String::new()
            };
            lines.push(line.trim_end().to_string());
        }
        Some(lines.join("\n"))
    }

    /// Returns `true` and clears the bell flag if a BEL was received since the
    /// last call. Used by the app tick to propagate bell alerts to the tab bar.
    pub fn take_bell(&mut self) -> bool {
        if self.bell_pending {
            self.bell_pending = false;
            true
        } else {
            false
        }
    }

    /// Returns and clears the pending OSC title if an OSC 0/2 title sequence
    /// was received since the last call. Returns `None` if no title was set.
    pub fn take_osc_title(&mut self) -> Option<String> {
        self.osc_title.take()
    }

    /// Rows visible at the current scroll offset (top-to-bottom order).
    /// May include history rows when scrolled up.
    pub fn view_rows(&self) -> Vec<&Vec<Cell>> {
        let h = self.history.len();
        let start = (h as isize - self.scroll_offset as isize).max(0) as usize;
        let mut rows: Vec<&Vec<Cell>> = Vec::with_capacity(self.rows);
        for i in start..start + self.rows {
            if i < h {
                rows.push(&self.history[i]);
            } else {
                let live = i - h;
                if live < self.cells.len() {
                    rows.push(&self.cells[live]);
                }
            }
        }
        rows
    }

    /// Scroll by `delta` lines (positive = up into history, negative = toward live).
    /// Returns true when the visible scroll offset changed.
    pub fn scroll_by(&mut self, delta: isize) -> bool {
        let max = self.history.len();
        let old = self.scroll_offset;
        self.scroll_offset = (self.scroll_offset as isize + delta).clamp(0, max as isize) as usize;
        // Scrolling moves content under the selection's viewport coords; drop it.
        if self.scroll_offset != old {
            self.clear_selection();
            true
        } else {
            false
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.cols = cols;
        self.rows = rows;
        self.cells.resize(rows, vec![Cell::default(); cols]);
        for row in &mut self.cells {
            row.resize(cols, Cell::default());
        }
        self.alt_cells.resize(rows, vec![Cell::default(); cols]);
        for row in &mut self.alt_cells {
            row.resize(cols, Cell::default());
        }
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
        self.clear_selection();
    }

    pub fn clear_all(&mut self) {
        self.cells = vec![vec![Cell::default(); self.cols]; self.rows];
        self.alt_cells = vec![vec![Cell::default(); self.cols]; self.rows];
        self.history.clear();
        self.scroll_offset = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.saved_cursor_row = 0;
        self.saved_cursor_col = 0;
        self.using_alt_screen = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows.saturating_sub(1);
        self.cursor_visible = true;
        self.bell_pending = false;
        self.osc_title = None;
        self.clear_selection();
    }

    pub fn visible_text(&self) -> String {
        rows_to_text(self.view_rows().into_iter())
    }

    pub fn buffer_text(&self) -> String {
        rows_to_text(self.history.iter().chain(self.cells.iter()))
    }

    pub fn match_count(&self, query: &str) -> usize {
        count_text_matches(&self.buffer_text(), query)
    }

    fn scroll_up(&mut self) {
        let full_screen = self.scroll_top == 0 && self.scroll_bottom == self.rows.saturating_sub(1);
        if full_screen && !self.using_alt_screen {
            // Full-screen scroll on main buffer — push to scrollback history.
            let row = self.cells.remove(0);
            self.history.push_back(row);
            if self.history.len() > self.history_max {
                self.history.pop_front();
            }
            self.cells.push(vec![Cell::default(); self.cols]);
            if self.scroll_offset > 0 {
                self.scroll_offset = (self.scroll_offset + 1).min(self.history.len());
            }
        } else {
            // Region scroll — remove top of region, insert blank at bottom.
            let top = self.scroll_top.min(self.cells.len().saturating_sub(1));
            let bot = (self.scroll_bottom + 1).min(self.cells.len());
            if top < bot {
                self.cells.remove(top);
                self.cells.insert(bot - 1, vec![Cell::default(); self.cols]);
            }
        }
    }

    fn scroll_down(&mut self) {
        let top = self.scroll_top.min(self.cells.len().saturating_sub(1));
        let bot = (self.scroll_bottom + 1).min(self.cells.len());
        if top < bot {
            // Remove bottom of region, insert blank at top.
            if bot - 1 < self.cells.len() {
                self.cells.remove(bot - 1);
            }
            self.cells.insert(top, vec![Cell::default(); self.cols]);
        }
    }

    fn switch_to_alt_screen(&mut self) {
        if self.using_alt_screen {
            return;
        }
        self.saved_cursor_row = self.cursor_row;
        self.saved_cursor_col = self.cursor_col;
        std::mem::swap(&mut self.cells, &mut self.alt_cells);
        for row in &mut self.cells {
            *row = vec![Cell::default(); self.cols];
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.scroll_offset = 0;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows.saturating_sub(1);
        self.using_alt_screen = true;
    }

    fn switch_to_main_screen(&mut self) {
        if !self.using_alt_screen {
            return;
        }
        std::mem::swap(&mut self.cells, &mut self.alt_cells);
        self.cursor_row = self.saved_cursor_row.min(self.rows.saturating_sub(1));
        self.cursor_col = self.saved_cursor_col.min(self.cols.saturating_sub(1));
        self.scroll_offset = 0;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows.saturating_sub(1);
        self.using_alt_screen = false;
    }

    fn put_char(&mut self, ch: char) {
        let width = ch.width().unwrap_or(1).max(1);
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            if self.cursor_row == self.scroll_bottom {
                self.scroll_up();
            } else {
                self.cursor_row = (self.cursor_row + 1).min(self.rows - 1);
            }
        }
        if self.cursor_row >= self.rows {
            self.scroll_up();
            self.cursor_row = self.rows - 1;
        }
        self.cells[self.cursor_row][self.cursor_col] = Cell {
            ch,
            fg: self.cur_fg,
            bg: self.cur_bg,
            bold: self.cur_bold,
        };
        self.cursor_col += 1;
        // For double-width characters place a blank placeholder in the next cell.
        if width == 2 && self.cursor_col < self.cols {
            self.cells[self.cursor_row][self.cursor_col] = Cell {
                ch: ' ',
                fg: self.cur_fg,
                bg: self.cur_bg,
                bold: self.cur_bold,
            };
            self.cursor_col += 1;
        }
    }

    fn handle_sgr(&mut self, params: &Params) {
        let mut iter = params.iter();
        loop {
            match iter.next() {
                None => break,
                Some(p) => match p[0] {
                    0 => {
                        self.cur_fg = DEFAULT_FG;
                        self.cur_bg = TermColor::TRANSPARENT;
                        self.cur_bold = false;
                    }
                    1 => self.cur_bold = true,
                    22 => self.cur_bold = false,
                    30..=37 => self.cur_fg = ansi_color(p[0] - 30, false),
                    90..=97 => self.cur_fg = ansi_color(p[0] - 90, true),
                    38 => match iter.next().map(|p| p[0]) {
                        Some(5) => {
                            if let Some(n) = iter.next() {
                                self.cur_fg = color256(n[0] as u8);
                            }
                        }
                        Some(2) => {
                            let r = iter.next().map_or(0, |p| p[0] as u8);
                            let g = iter.next().map_or(0, |p| p[0] as u8);
                            let b = iter.next().map_or(0, |p| p[0] as u8);
                            self.cur_fg = TermColor::from_rgb(r, g, b);
                        }
                        _ => {}
                    },
                    39 => self.cur_fg = DEFAULT_FG,
                    40..=47 => self.cur_bg = ansi_color(p[0] - 40, false),
                    100..=107 => self.cur_bg = ansi_color(p[0] - 100, true),
                    48 => match iter.next().map(|p| p[0]) {
                        Some(5) => {
                            if let Some(n) = iter.next() {
                                self.cur_bg = color256(n[0] as u8);
                            }
                        }
                        Some(2) => {
                            let r = iter.next().map_or(0, |p| p[0] as u8);
                            let g = iter.next().map_or(0, |p| p[0] as u8);
                            let b = iter.next().map_or(0, |p| p[0] as u8);
                            self.cur_bg = TermColor::from_rgb(r, g, b);
                        }
                        _ => {}
                    },
                    49 => self.cur_bg = TermColor::TRANSPARENT,
                    _ => {}
                },
            }
        }
    }
}

impl Perform for TermGrid {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | b'\x0b' | b'\x0c' => {
                if self.cursor_row == self.scroll_bottom {
                    self.scroll_up();
                } else {
                    self.cursor_row = (self.cursor_row + 1).min(self.rows - 1);
                }
            }
            b'\r' => self.cursor_col = 0,
            b'\x08' if self.cursor_col > 0 => {
                self.cursor_col -= 1;
            }
            0x07 => {
                self.bell_pending = true;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _: bool, action: char) {
        let p = |n: usize, d: u16| {
            params
                .iter()
                .nth(n)
                .and_then(|p| p.first().copied())
                .unwrap_or(d)
        };

        // Private mode sequences (ESC[?...h / ESC[?...l)
        if intermediates == b"?" {
            match action {
                'h' => {
                    for sub in params.iter() {
                        match sub[0] {
                            47 | 1049 => self.switch_to_alt_screen(),
                            25 => self.cursor_visible = true,
                            _ => {}
                        }
                    }
                }
                'l' => {
                    for sub in params.iter() {
                        match sub[0] {
                            47 | 1049 => self.switch_to_main_screen(),
                            25 => self.cursor_visible = false,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        match action {
            'H' | 'f' => {
                self.cursor_row =
                    (p(0, 1).saturating_sub(1) as usize).min(self.rows.saturating_sub(1));
                self.cursor_col =
                    (p(1, 1).saturating_sub(1) as usize).min(self.cols.saturating_sub(1));
            }
            'A' => self.cursor_row = self.cursor_row.saturating_sub(p(0, 1) as usize),
            'B' => self.cursor_row = (self.cursor_row + p(0, 1) as usize).min(self.rows - 1),
            'C' => self.cursor_col = (self.cursor_col + p(0, 1) as usize).min(self.cols - 1),
            'D' => self.cursor_col = self.cursor_col.saturating_sub(p(0, 1) as usize),
            'E' => {
                self.cursor_row = (self.cursor_row + p(0, 1) as usize).min(self.rows - 1);
                self.cursor_col = 0;
            }
            'F' => {
                self.cursor_row = self.cursor_row.saturating_sub(p(0, 1) as usize);
                self.cursor_col = 0;
            }
            'G' => {
                self.cursor_col =
                    (p(0, 1).saturating_sub(1) as usize).min(self.cols.saturating_sub(1))
            }
            'd' => {
                self.cursor_row =
                    (p(0, 1).saturating_sub(1) as usize).min(self.rows.saturating_sub(1));
            }
            'J' => match p(0, 0) {
                0 => {
                    for c in self.cursor_col..self.cols {
                        self.cells[self.cursor_row][c] = Cell::default();
                    }
                    for r in (self.cursor_row + 1)..self.rows {
                        self.cells[r] = vec![Cell::default(); self.cols];
                    }
                }
                1 => {
                    for r in 0..self.cursor_row {
                        self.cells[r] = vec![Cell::default(); self.cols];
                    }
                    for c in 0..=self.cursor_col {
                        self.cells[self.cursor_row][c] = Cell::default();
                    }
                }
                2 | 3 => {
                    for r in &mut self.cells {
                        *r = vec![Cell::default(); self.cols];
                    }
                    self.cursor_row = 0;
                    self.cursor_col = 0;
                }
                _ => {}
            },
            'K' => match p(0, 0) {
                0 => {
                    for c in self.cursor_col..self.cols {
                        self.cells[self.cursor_row][c] = Cell::default();
                    }
                }
                1 => {
                    for c in 0..=self.cursor_col {
                        self.cells[self.cursor_row][c] = Cell::default();
                    }
                }
                2 => self.cells[self.cursor_row] = vec![Cell::default(); self.cols],
                _ => {}
            },
            // Erase n characters (fill with spaces, don't move cursor)
            'X' => {
                let n = (p(0, 1) as usize).min(self.cols - self.cursor_col);
                for c in self.cursor_col..self.cursor_col + n {
                    self.cells[self.cursor_row][c] = Cell::default();
                }
            }
            // Delete n characters
            'P' => {
                let n = p(0, 1) as usize;
                let row = &mut self.cells[self.cursor_row];
                let col = self.cursor_col;
                if col < row.len() {
                    for i in col..row.len() {
                        row[i] = if i + n < row.len() {
                            row[i + n]
                        } else {
                            Cell::default()
                        };
                    }
                }
            }
            // Scroll up n lines
            'S' => {
                for _ in 0..p(0, 1) {
                    self.scroll_up();
                }
            }
            // Scroll down n lines
            'T' => {
                for _ in 0..p(0, 1) {
                    self.scroll_down();
                }
            }
            // Insert n blank lines at cursor (within scroll region)
            'L' => {
                let n = p(0, 1) as usize;
                let top = self.cursor_row.max(self.scroll_top);
                let bot = (self.scroll_bottom + 1).min(self.cells.len());
                for _ in 0..n {
                    if bot > top && bot <= self.cells.len() {
                        self.cells.remove(bot - 1);
                        self.cells.insert(top, vec![Cell::default(); self.cols]);
                    }
                }
            }
            // Delete n lines at cursor (within scroll region)
            'M' => {
                let n = p(0, 1) as usize;
                let top = self.cursor_row.max(self.scroll_top);
                let bot = (self.scroll_bottom + 1).min(self.cells.len());
                for _ in 0..n {
                    if top < bot && top < self.cells.len() {
                        self.cells.remove(top);
                        self.cells.insert(bot - 1, vec![Cell::default(); self.cols]);
                    }
                }
            }
            // Set scrolling region (DECSTBM)
            'r' => {
                let top = p(0, 1).saturating_sub(1) as usize;
                let bot = p(1, self.rows as u16).saturating_sub(1) as usize;
                self.scroll_top = top.min(self.rows.saturating_sub(1));
                self.scroll_bottom = bot.min(self.rows.saturating_sub(1));
                if self.scroll_top >= self.scroll_bottom {
                    self.scroll_top = 0;
                    self.scroll_bottom = self.rows.saturating_sub(1);
                }
                self.cursor_row = 0;
                self.cursor_col = 0;
            }
            // Insert n blank characters
            '@' => {
                let n = p(0, 1) as usize;
                let row = &mut self.cells[self.cursor_row];
                let col = self.cursor_col;
                for i in (col..row.len().saturating_sub(n)).rev() {
                    row[i + n] = row[i];
                }
                for i in col..col + n {
                    if i < row.len() {
                        row[i] = Cell::default();
                    }
                }
            }
            'm' => self.handle_sgr(params),
            _ => {}
        }
    }

    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.len() >= 2 && (params[0] == b"0" || params[0] == b"2") {
            if let Ok(title) = std::str::from_utf8(params[1]) {
                self.osc_title = Some(title.to_string());
            }
        }
    }
    fn esc_dispatch(&mut self, _: &[u8], _: bool, byte: u8) {
        match byte {
            b'M' => {
                // Reverse index — scroll down if at top of scroll region, else move cursor up.
                if self.cursor_row == self.scroll_top {
                    self.scroll_down();
                } else if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                }
            }
            b'7' => {
                // Save cursor position (DECSC).
                self.saved_cursor_row = self.cursor_row;
                self.saved_cursor_col = self.cursor_col;
            }
            b'8' => {
                // Restore cursor position (DECRC).
                self.cursor_row = self.saved_cursor_row.min(self.rows.saturating_sub(1));
                self.cursor_col = self.saved_cursor_col.min(self.cols.saturating_sub(1));
            }
            _ => {}
        }
    }
}

// ── TermProcessor — owns both parser and grid cleanly ─────────────────────────

pub struct TermProcessor {
    parser: Parser,
    pub grid: TermGrid,
}

impl TermProcessor {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self::with_scrollback(cols, rows, HISTORY_MAX)
    }

    pub fn with_scrollback(cols: usize, rows: usize, history_max: usize) -> Self {
        Self {
            parser: Parser::new(),
            grid: TermGrid::with_scrollback(cols, rows, history_max),
        }
    }

    pub fn process(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.parser.advance(&mut self.grid, b);
        }
    }

    pub fn clear_all(&mut self) {
        self.grid.clear_all();
    }

    pub fn visible_text(&self) -> String {
        self.grid.visible_text()
    }

    pub fn match_count(&self, query: &str) -> usize {
        self.grid.match_count(query)
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.grid.resize(cols, rows);
    }

    pub fn begin_selection(&mut self, row: usize, col: usize) {
        self.grid.begin_selection(row, col);
    }

    pub fn update_selection(&mut self, row: usize, col: usize) {
        self.grid.update_selection(row, col);
    }

    pub fn clear_selection(&mut self) {
        self.grid.clear_selection();
    }

    pub fn has_selection(&self) -> bool {
        self.grid.has_selection()
    }

    pub fn selected_text(&self) -> Option<String> {
        self.grid.selected_text()
    }
}

fn rows_to_text<'a>(rows: impl Iterator<Item = &'a Vec<Cell>>) -> String {
    rows.map(|row| row_to_text(row))
        .collect::<Vec<_>>()
        .join("\n")
}

fn row_to_text(row: &[Cell]) -> String {
    row.iter()
        .map(|cell| cell.ch)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn count_text_matches(text: &str, query: &str) -> usize {
    let query = query.trim();
    if query.is_empty() {
        return 0;
    }
    text.to_lowercase().matches(&query.to_lowercase()).count()
}

// ── Colour helpers ───────────────────────────────────────────────────────────

fn ansi_color(idx: u16, bright: bool) -> TermColor {
    color256(if bright { idx as u8 + 8 } else { idx as u8 })
}

pub fn color256(n: u8) -> TermColor {
    const C16: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    if (n as usize) < C16.len() {
        let (r, g, b) = C16[n as usize];
        return TermColor::from_rgb(r, g, b);
    }
    if n >= 232 {
        let v = 8 + (n - 232) * 10;
        return TermColor::from_rgb(v, v, v);
    }
    let n = n - 16;
    let b = n % 6;
    let g = (n / 6) % 6;
    let r = n / 36;
    let s = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
    TermColor::from_rgb(s(r), s(g), s(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_all_resets_cells_history_and_cursor() {
        let mut term = TermProcessor::new(4, 2);
        term.process(b"abcd\nefgh\nijkl");
        assert!(!term.grid.history.is_empty());

        term.clear_all();

        assert!(term.grid.history.is_empty());
        assert_eq!(term.grid.cursor_row, 0);
        assert_eq!(term.grid.cursor_col, 0);
        assert_eq!(term.visible_text(), "\n");
    }

    #[test]
    fn visible_text_trims_line_end_padding() {
        let mut term = TermProcessor::new(8, 2);
        term.process(b"hi\r\nthere");

        assert_eq!(term.visible_text(), "hi\nthere");
    }

    #[test]
    fn match_count_searches_scrollback_and_live_rows_case_insensitively() {
        let mut term = TermProcessor::new(8, 2);
        term.process(b"Alpha\r\nBeta\r\nalpha");

        assert_eq!(term.match_count("alpha"), 2);
        assert_eq!(term.match_count("BETA"), 1);
        assert_eq!(term.match_count(""), 0);
    }

    #[test]
    fn scroll_by_moves_into_history_and_back_to_live() {
        let mut term = TermProcessor::new(8, 2);
        term.process(b"one\r\ntwo\r\nthree\r\nfour");
        assert!(term.grid.history.len() >= 2);
        assert_eq!(term.grid.scroll_offset, 0);

        assert!(term.grid.scroll_by(1));
        assert_eq!(term.grid.scroll_offset, 1);
        assert!(
            term.visible_text().contains("three"),
            "visible text was {:?}",
            term.visible_text()
        );

        assert!(term.grid.scroll_by(-1));
        assert_eq!(term.grid.scroll_offset, 0);
    }

    #[test]
    fn scroll_by_reports_false_at_scroll_limits() {
        let mut term = TermProcessor::new(8, 2);
        term.process(b"one\r\ntwo\r\nthree");

        assert!(!term.grid.scroll_by(-1));
        let max = term.grid.history.len();
        assert!(term.grid.scroll_by(max as isize + 100));
        assert_eq!(term.grid.scroll_offset, max);
        assert!(!term.grid.scroll_by(1));
    }

    #[test]
    fn snapshot_replayed_then_resized_keeps_full_history_scrollable() {
        // Mirrors the attach sequence: a large scrollback snapshot is replayed
        // into a default-sized grid, then the view resizes to the real window.
        // History must remain fully scrollable afterwards (regression for the
        // "can only scroll new data" report). (#2)
        let mut term = TermProcessor::new(80, 24);
        for i in 0..500 {
            term.process(format!("line {i}\r\n").as_bytes());
        }
        let history_before = term.grid.history.len();
        assert!(history_before >= 400, "history was {history_before}");

        // Resize as the canvas reports real bounds.
        term.resize(120, 40);
        // The whole history is still reachable by scrolling up.
        assert!(term.grid.scroll_by(history_before as isize));
        assert_eq!(term.grid.scroll_offset, term.grid.history.len());
        // Earliest retained content is visible at the top of the viewport.
        assert!(
            term.visible_text().contains("line "),
            "expected history text, got {:?}",
            term.visible_text()
        );
    }

    #[test]
    fn alt_screen_switch_isolates_content() {
        let mut term = TermProcessor::new(10, 4);
        term.process(b"main");
        // switch to alt screen
        term.process(b"\x1b[?1049h");
        assert!(term.grid.using_alt_screen);
        // alt screen should be blank
        assert_eq!(term.visible_text().trim(), "");
        // write to alt screen
        term.process(b"alt");
        assert_eq!(term.visible_text().trim(), "alt");
        // switch back to main screen
        term.process(b"\x1b[?1049l");
        assert!(!term.grid.using_alt_screen);
        // main screen content should be restored
        assert_eq!(term.visible_text().trim(), "main");
    }

    #[test]
    fn scrolling_region_restricts_scroll() {
        let mut term = TermProcessor::new(5, 4);
        // Write 4 lines
        term.process(b"top\r\n---\r\na\r\nb");
        // Set scrolling region rows 3-4 (1-based)
        term.process(b"\x1b[3;4r");
        assert_eq!(term.grid.scroll_top, 2);
        assert_eq!(term.grid.scroll_bottom, 3);
        // Cursor should be homed after DECSTBM
        assert_eq!(term.grid.cursor_row, 0);
        // The top row should still be "top" (outside the scroll region)
        assert_eq!(
            term.visible_text().lines().next().unwrap_or("").trim(),
            "top"
        );
    }

    #[test]
    fn cursor_visible_toggled_by_private_mode() {
        let mut term = TermProcessor::new(10, 4);
        assert!(term.grid.cursor_visible);
        term.process(b"\x1b[?25l");
        assert!(!term.grid.cursor_visible);
        term.process(b"\x1b[?25h");
        assert!(term.grid.cursor_visible);
    }

    #[test]
    fn esc_7_8_save_restore_cursor() {
        let mut term = TermProcessor::new(20, 5);
        term.process(b"\x1b[3;5H"); // move to row 3, col 5 (1-based)
        term.process(b"\x1b7"); // save
        term.process(b"\x1b[1;1H"); // home
        assert_eq!(term.grid.cursor_row, 0);
        term.process(b"\x1b8"); // restore
        assert_eq!(term.grid.cursor_row, 2);
        assert_eq!(term.grid.cursor_col, 4);
    }
}
