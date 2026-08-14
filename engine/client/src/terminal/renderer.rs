//! Monospace terminal rasterizer: [`TermGrid`] → a BGRA pixel buffer.
//!
//! GPUI-free on purpose — it emits a raw [`TermFrame`] (BGRA, premultiplied
//! over the dark background) that the GPUI view wraps in a `RenderImage` and
//! blits with `window.paint_image`. Glyphs are rasterized with `fontdue` and
//! cached per `char`. Logic ported from `rdpapp`'s `GpuiTermRenderer`.

use std::collections::HashMap;

use super::grid::TermGrid;

static TERM_FONT_BYTES: &[u8] = include_bytes!("../../assets/NotoMono-Regular.ttf");
static TERM_FONT_FALLBACK_BYTES: &[u8] = include_bytes!("../../assets/DejaVuSansMono.ttf");

/// Terminal background (#1e1e2e — Catppuccin-ish dark).
const BG: (u8, u8, u8) = (0x1e, 0x1e, 0x2e);
/// Selection highlight background.
const SEL: (u8, u8, u8) = (0x33, 0x47, 0x7a);
/// Block-cursor colour.
const CURSOR: (u8, u8, u8) = (0xcc, 0xcc, 0xcc);

/// Minimum and maximum font sizes the user can zoom to (Ctrl+wheel / buttons).
pub const MIN_FONT_SIZE: f32 = 7.0;
pub const MAX_FONT_SIZE: f32 = 40.0;
/// Default terminal font size used on first launch.
pub const DEFAULT_FONT_SIZE: f32 = 15.0;

/// Perceptual luminance (0–255) of an RGB triple, Rec. 601 weights.
fn luminance(r: u8, g: u8, b: u8) -> u32 {
    (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000
}

/// Lift a foreground colour that is too close to the dark terminal background
/// up to a readable floor, so "black on dark" text never disappears (#6). Only
/// near-black foregrounds are touched; everything else is returned unchanged.
fn readable_fg(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    // Floor matched roughly to the background luminance plus headroom.
    const FLOOR: u32 = 96;
    let lum = luminance(r, g, b);
    if lum >= FLOOR {
        return (r, g, b);
    }
    // Pure (or near) black → neutral mid-grey that reads on the dark bg.
    if r as u32 + g as u32 + b as u32 <= 24 {
        return (0x8a, 0x8a, 0x8a);
    }
    // Otherwise scale the existing hue up to the contrast floor.
    let scale = (FLOOR as f32 / lum.max(1) as f32).min(4.0);
    let lift = |c: u8| ((c as f32 * scale).round() as u32).min(255) as u8;
    (lift(r), lift(g), lift(b))
}

/// A finished frame: `width * height` BGRA pixels, row-major, top-left origin.
pub struct TermFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub struct TermRenderer {
    font: fontdue::Font,
    fallback_font: fontdue::Font,
    font_size: f32,
    cell_w: usize,
    cell_h: usize,
    ascent: isize,
    cache: HashMap<char, (fontdue::Metrics, Vec<u8>)>,
}

impl TermRenderer {
    pub fn new(font_size: f32) -> Self {
        let font = fontdue::Font::from_bytes(TERM_FONT_BYTES, fontdue::FontSettings::default())
            .expect("embedded terminal font failed to load");
        let fallback_font =
            fontdue::Font::from_bytes(TERM_FONT_FALLBACK_BYTES, fontdue::FontSettings::default())
                .expect("embedded terminal fallback font failed to load");
        let lm = font
            .horizontal_line_metrics(font_size)
            .expect("font has no horizontal metrics");
        let cell_h = lm.new_line_size.ceil() as usize;
        let ascent = lm.ascent.ceil() as isize;
        let (m, _) = font.rasterize('M', font_size);
        let cell_w = m.advance_width.ceil().max(1.0) as usize;
        Self {
            font,
            fallback_font,
            font_size,
            cell_w: cell_w.max(1),
            cell_h: cell_h.max(1),
            ascent,
            cache: HashMap::new(),
        }
    }

    pub fn cell_w(&self) -> usize {
        self.cell_w
    }

    pub fn cell_h(&self) -> usize {
        self.cell_h
    }

    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Change the rasterization font size (clamped), recomputing cell metrics
    /// and dropping the glyph cache. Returns true if the size actually changed.
    pub fn set_font_size(&mut self, size: f32) -> bool {
        let size = size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if (size - self.font_size).abs() < 0.01 {
            return false;
        }
        self.font_size = size;
        let lm = self
            .font
            .horizontal_line_metrics(size)
            .expect("font has no horizontal metrics");
        self.cell_h = (lm.new_line_size.ceil() as usize).max(1);
        self.ascent = lm.ascent.ceil() as isize;
        let (m, _) = self.font.rasterize('M', size);
        self.cell_w = (m.advance_width.ceil().max(1.0) as usize).max(1);
        self.cache.clear();
        true
    }

    /// Grid dimensions (cols, rows) that fit a pixel viewport, each at least 1.
    pub fn cols_rows_for(&self, pixel_w: u32, pixel_h: u32) -> (usize, usize) {
        (
            (pixel_w as usize / self.cell_w).max(1),
            (pixel_h as usize / self.cell_h).max(1),
        )
    }

    /// Viewport cell under a pixel coordinate (for mouse selection).
    pub fn cell_at(&self, x: f32, y: f32) -> (usize, usize) {
        let col = (x.max(0.0) as usize) / self.cell_w;
        let row = (y.max(0.0) as usize) / self.cell_h;
        (row, col)
    }

    fn ensure_glyph(&mut self, ch: char) {
        if ch <= ' ' || self.cache.contains_key(&ch) {
            return;
        }
        let entry = if self.font.lookup_glyph_index(ch) != 0 {
            self.font.rasterize(ch, self.font_size)
        } else {
            self.fallback_font.rasterize(ch, self.font_size)
        };
        self.cache.insert(ch, entry);
    }

    pub fn render(&mut self, grid: &TermGrid, pixel_w: u32, pixel_h: u32) -> TermFrame {
        let w = pixel_w.max(1) as usize;
        let h = pixel_h.max(1) as usize;

        let visible_rows = grid.view_rows();
        // Warm the glyph cache for everything visible.
        for row in &visible_rows {
            for cell in *row {
                self.ensure_glyph(cell.ch);
            }
        }

        // Fill background.
        let mut bgra = vec![0u8; w * h * 4];
        for chunk in bgra.chunks_exact_mut(4) {
            chunk[0] = BG.2; // B
            chunk[1] = BG.1; // G
            chunk[2] = BG.0; // R
            chunk[3] = 0xff; // A
        }

        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let ascent = self.ascent;
        let cache = &self.cache;

        for (row_idx, row) in visible_rows.iter().enumerate() {
            let y_base = row_idx * cell_h;
            if y_base >= h {
                break;
            }
            let baseline = y_base as isize + ascent;

            for (col_idx, cell) in row.iter().enumerate() {
                let x_base = col_idx * cell_w;
                if x_base >= w {
                    break;
                }

                // Cell background — selection overrides explicit bg.
                if grid.is_cell_selected(row_idx, col_idx) {
                    fill_bgra(&mut bgra, w, x_base, y_base, cell_w, cell_h, h, SEL);
                } else if !cell.bg.is_default {
                    fill_bgra(
                        &mut bgra,
                        w,
                        x_base,
                        y_base,
                        cell_w,
                        cell_h,
                        h,
                        (cell.bg.r, cell.bg.g, cell.bg.b),
                    );
                }

                // Block cursor (live view only).
                let is_cursor = grid.cursor_visible
                    && grid.scroll_offset == 0
                    && row_idx == grid.cursor_row
                    && col_idx == grid.cursor_col;
                if is_cursor {
                    fill_bgra(&mut bgra, w, x_base, y_base, cell_w, cell_h, h, CURSOR);
                }

                if cell.ch > ' ' {
                    // Glyph colour: invert under the cursor.
                    let (fr, fg_, fb) = if is_cursor {
                        if cell.bg.is_default {
                            BG
                        } else {
                            (cell.bg.r, cell.bg.g, cell.bg.b)
                        }
                    } else if cell.bg.is_default {
                        // Only lift low-contrast glyphs against the terminal's
                        // own dark background; an explicit cell bg (e.g. a
                        // coloured block) keeps the author's exact fg.
                        readable_fg(cell.fg.r, cell.fg.g, cell.fg.b)
                    } else {
                        (cell.fg.r, cell.fg.g, cell.fg.b)
                    };

                    if let Some((metrics, bitmap)) = cache.get(&cell.ch) {
                        let glyph_top =
                            baseline - (metrics.ymin as isize + metrics.height as isize);
                        let glyph_left = x_base as isize + metrics.xmin as isize;
                        for (i, &alpha) in bitmap.iter().enumerate() {
                            if alpha == 0 || metrics.width == 0 {
                                continue;
                            }
                            let gx = glyph_left + (i % metrics.width) as isize;
                            let gy = glyph_top + (i / metrics.width) as isize;
                            if gx < 0 || gy < 0 {
                                continue;
                            }
                            let (gx, gy) = (gx as usize, gy as usize);
                            if gx >= w || gy >= h {
                                continue;
                            }
                            let off = (gy * w + gx) * 4;
                            let a = alpha as u32;
                            let na = 255 - a;
                            // Stored BGRA: blend fg over existing pixel.
                            bgra[off] = ((fb as u32 * a + bgra[off] as u32 * na) / 255) as u8;
                            bgra[off + 1] =
                                ((fg_ as u32 * a + bgra[off + 1] as u32 * na) / 255) as u8;
                            bgra[off + 2] =
                                ((fr as u32 * a + bgra[off + 2] as u32 * na) / 255) as u8;
                            bgra[off + 3] = 0xff;
                        }
                    }
                }
            }
        }

        TermFrame {
            width: pixel_w.max(1),
            height: pixel_h.max(1),
            bgra,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_bgra(
    buf: &mut [u8],
    w: usize,
    x: usize,
    y: usize,
    cw: usize,
    ch: usize,
    h: usize,
    rgb: (u8, u8, u8),
) {
    for dy in 0..ch {
        let py = y + dy;
        if py >= h {
            break;
        }
        for dx in 0..cw {
            let px = x + dx;
            if px >= w {
                break;
            }
            let off = (py * w + px) * 4;
            buf[off] = rgb.2; // B
            buf[off + 1] = rgb.1; // G
            buf[off + 2] = rgb.0; // R
            buf[off + 3] = 0xff;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::grid::TermProcessor;

    #[test]
    fn renderer_has_positive_cell_metrics() {
        let r = TermRenderer::new(15.0);
        assert!(r.cell_w() >= 1);
        assert!(r.cell_h() >= 1);
    }

    #[test]
    fn cols_rows_fit_viewport() {
        let r = TermRenderer::new(15.0);
        let (cols, rows) = r.cols_rows_for(800, 600);
        assert!(cols >= 1 && rows >= 1);
        assert!(cols * r.cell_w() <= 800 + r.cell_w());
    }

    #[test]
    fn set_font_size_changes_cell_metrics() {
        let mut r = TermRenderer::new(15.0);
        let (w0, h0) = (r.cell_w(), r.cell_h());
        assert!(r.set_font_size(28.0));
        assert!(r.cell_w() > w0 && r.cell_h() > h0);
        // Idempotent within clamp + epsilon.
        assert!(!r.set_font_size(28.0));
        // Clamped to bounds.
        r.set_font_size(1000.0);
        assert_eq!(r.font_size(), MAX_FONT_SIZE);
        r.set_font_size(0.0);
        assert_eq!(r.font_size(), MIN_FONT_SIZE);
    }

    #[test]
    fn readable_fg_lifts_pure_black_only_on_default_bg() {
        // Pure black is lifted to a readable grey.
        let lifted = readable_fg(0, 0, 0);
        assert!(luminance(lifted.0, lifted.1, lifted.2) >= 96);
        // A bright colour is untouched.
        assert_eq!(readable_fg(0xcc, 0xcc, 0xcc), (0xcc, 0xcc, 0xcc));
    }

    #[test]
    fn render_produces_correctly_sized_buffer() {
        let mut r = TermRenderer::new(15.0);
        let mut term = TermProcessor::new(20, 5);
        term.process(b"hello world");
        let frame = r.render(&term.grid, 200, 80);
        assert_eq!(frame.width, 200);
        assert_eq!(frame.height, 80);
        assert_eq!(frame.bgra.len(), 200 * 80 * 4);
    }
}
