//! GPUI terminal view: attaches to a server PTY session over WebSocket and
//! renders the live grid.
//!
//! Pattern mirrors `rdpapp`'s `LocalTermView`: a `fontdue`-rasterized BGRA
//! frame painted onto a `canvas`, `request_animation_frame` to keep the live
//! view ticking, and `on_key_down`/`on_scroll_wheel`/mouse listeners for input.
//! Server output arrives on the background tokio runtime and is fed into the
//! parser from a GPUI async task via `weak.update`.

use std::sync::{Arc, Mutex};

use gpui::{
    canvas, div, prelude::*, px, Bounds, ClipboardItem, Context, Corners, FocusHandle, FontWeight,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderImage,
    ScrollDelta, ScrollWheelEvent, Window,
};
use image::{Frame, ImageBuffer, Rgba};
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

use mydevenv2_client::{
    bridge,
    terminal::{key_to_bytes, KeyInput, TermProcessor, TermRenderer, DEFAULT_COLS, DEFAULT_ROWS},
    ws::{self, AttachEvent, AttachInput},
};

/// Default pixel size used for the very first frame before layout reports the
/// real canvas bounds.
const DEFAULT_PX_W: u32 = 960;
const DEFAULT_PX_H: u32 = 600;
const PAGE_SCROLL_SENTINEL: f32 = u32::MAX as f32;

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Connecting,
    Live,
    Closed,
}

pub struct TerminalView {
    ws_url: String,
    token: String,
    term: TermProcessor,
    renderer: TermRenderer,
    last_frame: Option<Arc<RenderImage>>,
    focus_handle: FocusHandle,
    bounds_arc: Arc<Mutex<Option<Bounds<Pixels>>>>,
    last_size_px: (u32, u32),
    input: Option<UnboundedSender<AttachInput>>,
    status: Status,
    error: Option<String>,
    search_query: String,
    search_count: usize,
    wheel_remainder_lines: f32,
}

impl TerminalView {
    /// Attach to `session_id` on the given server and start streaming.
    pub fn new(ws_url: String, token: String, _session_id: Uuid, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            ws_url,
            token,
            term: TermProcessor::new(DEFAULT_COLS, DEFAULT_ROWS),
            renderer: TermRenderer::new(15.0),
            last_frame: None,
            focus_handle: cx.focus_handle(),
            bounds_arc: Arc::new(Mutex::new(None)),
            last_size_px: (0, 0),
            input: None,
            status: Status::Connecting,
            error: None,
            search_query: String::new(),
            search_count: 0,
            wheel_remainder_lines: 0.0,
        };

        view.start_attach(cx);
        view
    }

    pub fn reattach(&mut self, cx: &mut Context<Self>) {
        if let Some(tx) = &self.input {
            let _ = tx.send(AttachInput::Close);
        }
        self.input = None;
        self.term.clear_all();
        self.status = Status::Connecting;
        self.error = None;
        self.last_frame = None;
        self.start_attach(cx);
        cx.notify();
    }

    pub fn close(&self) {
        if let Some(tx) = &self.input {
            let _ = tx.send(AttachInput::Close);
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.term.clear_all();
        self.last_frame = None;
        self.search_count = 0;
        cx.notify();
    }

    pub fn set_search_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.search_query = query;
        self.search_count = self.term.match_count(&self.search_query);
        self.last_frame = None;
        cx.notify();
    }

    fn scroll_terminal_by_wheel(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let lines = wheel_delta_to_lines(ev.delta, self.renderer.cell_h(), self.term.grid.rows);
        if !lines.is_finite() || lines == 0.0 {
            return;
        }
        cx.stop_propagation();

        self.wheel_remainder_lines += lines;
        let whole_lines = self.wheel_remainder_lines.trunc() as isize;
        if whole_lines == 0 {
            return;
        }
        self.wheel_remainder_lines -= whole_lines as f32;

        if self.term.grid.scroll_by(whole_lines) {
            self.last_frame = None;
            cx.notify();
        }
    }

    fn start_attach(&mut self, cx: &mut Context<Self>) {
        // Spawn the attach on the tokio runtime (its internal `tokio::spawn`
        // needs a runtime context), then pump its events into the parser from a
        // GPUI async task so all terminal mutation stays on the foreground
        // thread.
        let handle = bridge::handle();
        let ws_url = self.ws_url.clone();
        let token = self.token.clone();

        cx.spawn(
            move |weak: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut cx = cx.clone();
                async move {
                    // Establish the attach inside the tokio runtime.
                    let attach = handle
                        .spawn(async move { ws::spawn_attach(ws_url, token) })
                        .await;
                    let mut attach = match attach {
                        Ok(a) => a,
                        Err(e) => {
                            let _ = weak.update(&mut cx, |v, cx| {
                                v.status = Status::Closed;
                                v.error = Some(format!("attach task failed: {e}"));
                                cx.notify();
                            });
                            return;
                        }
                    };
                    // Hand the input channel to the view so keystrokes can be sent.
                    let input_tx = attach.input_tx.clone();
                    let _ = weak.update(&mut cx, |v, cx| {
                        v.input = Some(input_tx);
                        cx.notify();
                    });

                    // Stream events; drain greedily to batch bursts.
                    while let Some(ev) = attach.event_rx.recv().await {
                        let mut batch = vec![ev];
                        while let Ok(next) = attach.event_rx.try_recv() {
                            batch.push(next);
                        }
                        let stop = weak
                            .update(&mut cx, |v, cx| {
                                let mut closed = false;
                                for ev in batch {
                                    match ev {
                                        AttachEvent::Output(bytes) => {
                                            v.term.process(&bytes);
                                            if !v.search_query.trim().is_empty() {
                                                v.search_count =
                                                    v.term.match_count(&v.search_query);
                                            }
                                        }
                                        AttachEvent::SnapshotReady => v.status = Status::Live,
                                        AttachEvent::Lag(note) => {
                                            v.error = Some(format!("lagged: {note}"));
                                        }
                                        AttachEvent::Closed => closed = true,
                                        AttachEvent::Error(e) => v.error = Some(e),
                                    }
                                }
                                if closed {
                                    v.status = Status::Closed;
                                }
                                v.last_frame = None;
                                cx.notify();
                                closed
                            })
                            .unwrap_or(true);
                        if stop {
                            break;
                        }
                    }
                }
            },
        )
        .detach();
    }

    fn send_input(&self, bytes: Vec<u8>) {
        if let Some(tx) = &self.input {
            let _ = tx.send(AttachInput::Data(bytes));
        }
    }

    fn cell_at(&self, pos: Point<Pixels>) -> Option<(usize, usize)> {
        let slot = self.bounds_arc.lock().ok()?;
        let bounds = (*slot)?;
        let x = f32::from(pos.x - bounds.origin.x);
        let y = f32::from(pos.y - bounds.origin.y);
        let (row, col) = self.renderer.cell_at(x, y);
        Some((
            row.min(self.term.grid.rows.saturating_sub(1)),
            col.min(self.term.grid.cols.saturating_sub(1)),
        ))
    }

    /// Recompute size, resize the PTY if needed, and re-rasterize if dirty.
    fn refresh_frame(&mut self) {
        if let Ok(slot) = self.bounds_arc.lock() {
            if let Some(bounds) = *slot {
                let pw = u32::from(bounds.size.width);
                let ph = u32::from(bounds.size.height);
                if (pw, ph) != self.last_size_px && pw > 0 && ph > 0 {
                    self.last_size_px = (pw, ph);
                    let (cols, rows) = self.renderer.cols_rows_for(pw, ph);
                    self.term.resize(cols, rows);
                    if let Some(tx) = &self.input {
                        let _ = tx.send(AttachInput::Resize {
                            cols: cols as u16,
                            rows: rows as u16,
                        });
                    }
                    self.last_frame = None;
                }
            }
        }
        let (pw, ph) = if self.last_size_px.0 > 0 && self.last_size_px.1 > 0 {
            self.last_size_px
        } else {
            (DEFAULT_PX_W, DEFAULT_PX_H)
        };
        if self.last_frame.is_none() {
            let frame = self.renderer.render(&self.term.grid, pw, ph);
            self.last_frame = Some(to_render_image(frame));
        }
    }
}

fn wheel_delta_to_lines(delta: ScrollDelta, cell_h: usize, rows: usize) -> f32 {
    let lines = match delta {
        ScrollDelta::Lines(p) => dominant_axis(p.x, p.y),
        ScrollDelta::Pixels(p) => {
            let cell_h = cell_h.max(1) as f32;
            dominant_axis(f32::from(p.x), f32::from(p.y)) / cell_h
        }
    };
    normalize_wheel_lines(lines, rows)
}

fn dominant_axis(x: f32, y: f32) -> f32 {
    if x.abs() > y.abs() {
        x
    } else {
        y
    }
}

fn normalize_wheel_lines(lines: f32, rows: usize) -> f32 {
    if !lines.is_finite() || lines == 0.0 {
        return 0.0;
    }

    let page = rows.saturating_sub(1).max(1) as f32;
    if lines.abs() >= PAGE_SCROLL_SENTINEL / 2.0 {
        return lines.signum() * page;
    }

    lines.clamp(-page, page)
}

fn to_render_image(frame: mydevenv2_client::terminal::TermFrame) -> Arc<RenderImage> {
    let buf = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(frame.width, frame.height, frame.bgra)
        .expect("terminal image buffer dimensions match");
    Arc::new(RenderImage::new(vec![Frame::new(buf)]))
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_frame();

        // Keep the live view ticking while connected.
        if self.status != Status::Closed {
            window.request_animation_frame();
        }

        let last_frame = self.last_frame.clone();
        let bounds_arc = Arc::clone(&self.bounds_arc);
        let err_banner = self.error.clone();
        let status = self.status;
        let search_query = self.search_query.clone();
        let search_count = self.search_count;

        let canvas_el = canvas(
            move |bounds, _window, _cx| {
                if let Ok(mut slot) = bounds_arc.lock() {
                    *slot = Some(bounds);
                }
                last_frame
            },
            |bounds, frame, window, _cx| {
                if let Some(frame) = frame {
                    window
                        .paint_image(bounds, Corners::default(), frame, 0, false)
                        .ok();
                }
            },
        )
        .size_full();

        let mut root = div()
            .id("mydevenv2-terminal")
            .size_full()
            .overflow_hidden()
            .relative()
            .bg(gpui::rgb(0x1e1e2e))
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, ev: &MouseDownEvent, window, cx| {
                    window.focus(&view.focus_handle);
                    if let Some((row, col)) = view.cell_at(ev.position) {
                        view.term.grid.begin_selection(row, col);
                        view.last_frame = None;
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|view, ev: &MouseMoveEvent, _window, cx| {
                if ev.pressed_button == Some(MouseButton::Left) {
                    if let Some((row, col)) = view.cell_at(ev.position) {
                        view.term.grid.update_selection(row, col);
                        view.last_frame = None;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _ev: &MouseUpEvent, _window, cx| {
                    view.last_frame = None;
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|view, ev: &gpui::KeyDownEvent, _, cx| {
                let ks = &ev.keystroke;
                let m = &ks.modifiers;
                let key = ks.key.as_str();

                // Ctrl+Shift+C → copy the current selection to the clipboard.
                // Must run BEFORE we clear the selection below.
                if m.control && m.shift && !m.alt && !m.platform && key == "c" {
                    if let Some(text) = view.term.selected_text() {
                        if !text.is_empty() {
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                    }
                    return;
                }
                // Ctrl+Shift+V → paste clipboard text into the PTY.
                if m.control && m.shift && !m.alt && !m.platform && key == "v" {
                    if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                        if !text.is_empty() {
                            view.send_input(text.into_bytes());
                        }
                    }
                    return;
                }

                // Any other key returns to the live tail and drops selection.
                view.term.grid.scroll_offset = 0;
                view.term.clear_selection();
                let ki = KeyInput {
                    key,
                    key_char: ks.key_char.as_deref(),
                    ctrl: m.control,
                    alt: m.alt,
                    shift: m.shift,
                    platform: m.platform,
                };
                if let Some(bytes) = key_to_bytes(&ki) {
                    view.send_input(bytes);
                }
                view.last_frame = None;
                cx.notify();
            }))
            .on_scroll_wheel(cx.listener(|view, ev: &ScrollWheelEvent, _, cx| {
                view.scroll_terminal_by_wheel(ev, cx);
            }))
            .child(canvas_el);

        if status == Status::Connecting {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .px(px(8.0))
                    .py(px(4.0))
                    .bg(gpui::rgb(0x223355))
                    .text_color(gpui::rgb(0xc8ddff))
                    .font_weight(FontWeight::MEDIUM)
                    .child("Connecting..."),
            );
        }

        if !search_query.trim().is_empty() {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .px(px(8.0))
                    .py(px(4.0))
                    .bg(gpui::rgb(0x25311f))
                    .text_color(gpui::rgb(0xcde8ba))
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("{search_count} matches")),
            );
        }

        if let Some(err) = err_banner {
            root = root.child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .px(px(8.0))
                    .py(px(4.0))
                    .bg(gpui::rgb(0x55_2222))
                    .text_color(gpui::rgb(0xff_aaaa))
                    .font_weight(FontWeight::MEDIUM)
                    .child(err),
            );
        }

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    #[test]
    fn wheel_lines_use_vertical_delta() {
        assert_eq!(
            wheel_delta_to_lines(ScrollDelta::Lines(point(0.0, 3.0)), 18, 50),
            3.0
        );
    }

    #[test]
    fn wheel_lines_fall_back_to_horizontal_delta() {
        assert_eq!(
            wheel_delta_to_lines(ScrollDelta::Lines(point(-3.0, 0.0)), 18, 50),
            -3.0
        );
    }

    #[test]
    fn wheel_pixels_convert_by_cell_height() {
        assert_eq!(
            wheel_delta_to_lines(ScrollDelta::Pixels(point(px(0.0), px(36.0))), 18, 50),
            2.0
        );
    }

    #[test]
    fn wheel_page_sentinel_maps_to_one_viewport() {
        assert_eq!(
            wheel_delta_to_lines(ScrollDelta::Lines(point(0.0, u32::MAX as f32)), 18, 50),
            49.0
        );
        assert_eq!(
            wheel_delta_to_lines(ScrollDelta::Lines(point(0.0, -(u32::MAX as f32))), 18, 50),
            -49.0
        );
    }

    #[test]
    fn wheel_delta_is_clamped_to_one_viewport() {
        assert_eq!(
            wheel_delta_to_lines(ScrollDelta::Lines(point(0.0, 500.0)), 18, 50),
            49.0
        );
    }
}
