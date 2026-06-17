# Desktop Client GUI Uplift — June 2026

Handoff notes for the native GPUI desktop client (`client/`). This documents a
pass over a list of GUI complaints (Windows desktop app). Each item below maps
to the original request number. Written for the next agent picking up the work.

## TL;DR

All nine items were addressed in-place on the existing GPUI / FluentGUI stack.
A toolkit migration was considered and **rejected**: the two features assumed to
be blockers (native file picker, ctrl+scroll zoom) already exist in the pinned
`AusAgentSmith-org/fluent-gpui` fork, and a migration would not remove the real
constraint (Windows release binaries must build natively on the `arbit-win`
agent — see `mydevenv2-client-windows-crosscompile` memory / `AGENTS.md` §4).

Verification (all green): `cargo fmt`, `cargo clippy --all-targets -D warnings`
(both `--no-default-features` and full `gui`), `cargo test` (client 42 tests),
and `cargo test -p mydevenv2-server` (incl. new upload integration cases). Full
debug build links on Linux.

## What changed, by request

### 1. Buttons looked out of place / layout
- Terminal toolbar (`ui/mod.rs::terminal_toolbar`) regrouped into labelled
  clusters — **Zoom** (A-/percentage/A+/Reset) · **Connection** (Reconnect,
  Clear) · **Lifecycle** (Duplicate, Kill, Kill & Remove) — separated by thin
  dividers, with rename/search on a second aligned row.
- Sidebar actions split into a primary row (New + Refresh) and a secondary nav
  row (Files / Git / Settings), most now icon+label or icon-only.

### 2. Console scroll only worked for new data
- Not a storage limit: the grid already retains 50 000 lines
  (`terminal/grid.rs` `HISTORY_MAX`) and the server replays its 4 MiB scrollback
  snapshot through the parser on attach. The complaint was really #4 (flaky
  wheel) plus no clear affordance to reach history.
- Added keyboard scrollback nav in `terminal_view.rs`: **Shift+PageUp/PageDown**
  page through history, **Ctrl+Home** jumps to the oldest retained line,
  **Ctrl+End** snaps back to the live tail.
- Added an on-screen scroll-position indicator (`▲ offset/history`) shown while
  scrolled up.
- Regression test `grid.rs::snapshot_replayed_then_resized_keeps_full_history_scrollable`.

### 3. Zoom in/out (ctrl+scroll + buttons)
- `TermRenderer::set_font_size()` recomputes cell metrics and clears the glyph
  cache; `MIN/MAX/DEFAULT_FONT_SIZE` exported from `terminal`.
- `TerminalView`: **Ctrl+MouseWheel** zooms (browser-style), plus **Ctrl+=**,
  **Ctrl+-**, **Ctrl+0**. Zoom reflows cols/rows and resizes the PTY.
- GUI **A- / A+ / Reset** buttons + live percentage in the toolbar.
- Zoom level persists in config (`font_size`); new terminals open at it.

### 4. Mouse scroll buggy
- Ctrl+wheel is now intercepted for zoom before the scroll path, so a held Ctrl
  no longer produces erratic scrolling.
- Existing wheel normalization (`wheel_delta_to_lines`) retained and still
  unit-tested; added `wheel_zoom_direction` + test.

### 5. Files tab rewrite + native Explorer upload
- `Files` panel rebuilt as a real browser: clickable rows (dirs first, with
  folder/document icons), **Up** button, editable path + **Go**, scrollable
  listing, preview, and in-file search.
- **Upload** button opens the native OS file picker via
  `cx.prompt_for_paths(PathPromptOptions{files,multiple,..})` (works on the
  Windows backend — real Explorer dialog) and uploads each selected file into
  the current workspace directory.
- **Server change**: `PUT /api/files` (`server/src/files.rs::write_file`) now
  accepts an optional `content_base64` field so arbitrary **binary** files
  round-trip (the old `content: String` path was UTF-8 only). Malformed base64
  returns 400, not 500. Client uses `ApiClient::upload_file()` which base64s the
  bytes and sets `create_parents: true`. Covered by new integration tests.

### 6. Avoid black-on-dark text
- Renderer: `readable_fg()` lifts near-black glyph colours to a contrast floor,
  but only when the cell uses the **default** dark background (explicit coloured
  cells keep their exact author colours). Tested.
- Panels: git status/log and file preview/listing now use explicit theme
  colours (`on_neutral` / `on_subtle`) instead of relying on inherited defaults.

### 7. Force reconnect after WS IO error (e.g. OS 10054)
- `TerminalView` now **auto-reconnects** with bounded exponential backoff
  (1→15 s) whenever the attach socket drops unexpectedly. A deliberate
  close/kill (`user_closed`) suppresses it. An `attach_gen` generation counter
  prevents stale attach loops from racing a reconnect.
- New `Reconnecting…` / `Disconnected` status pills, and a manual **Reconnect**
  button (always available) that forces a fresh attach.

### 8. Resizable + collapsible left sidebar
- Drag handle between sidebar and content sets the width live (clamped
  180–520 px), persisted to config.
- Collapse toggle shrinks the sidebar to a 56 px **icon rail** showing session
  activity badges (still clickable to attach) + expand / new / settings icons.
  Collapsed state + width persist across launches.

### 9. Holistic review
- Reviewed the custom Windows title bar vs FluentGUI chrome: **kept** — the
  FluentGUI `WindowChrome` only provides edge-resize cursors, not min/max/close
  controls, so `windows_title_bar` is still required (not redundant).
- Status bar in the sidebar given a top border; empty states use theme
  disabled-text colour; consistent rounding/spacing across panels.

### 10. Documentation
- This file. Also see the updated `README.md` status section.

## New persisted config keys (`config.json`)

```jsonc
{
  "server_url": "...",
  "token": "...",
  "font_size": 15.0,          // #3 zoom level
  "sidebar_width": 288.0,     // #8
  "sidebar_collapsed": false  // #8
}
```

All three are `#[serde(default)]` — older config files load unchanged
(tested by `config::tests::config_tolerates_missing_new_fields`).

## Known follow-ups / backlog

- **Tooltips**: icon-only buttons would benefit from hover tooltips, but
  `fluent_primitives::Button` has no `.tooltip()` yet. We own FluentGUI
  (`AusAgentSmith-org/fluent-gpui`) — adding a `tooltip(impl Into<SharedString>)`
  builder there and re-pinning the rev is the clean fix. Left out to avoid a
  cross-repo publish cycle in this pass.
- **Server-side write_file note**: `content` and `content_base64` are mutually
  exclusive; `content_base64` wins if both are present.
- Sidebar drag uses absolute pointer-X as the new width. Correct because the
  sidebar starts at window-left; revisit if the layout ever gains a left gutter.
- File preview is still read-only (no in-app edit/save). Editing remains backlog.

## Build / release reminder (unchanged constraint)

Local Linux `cargo build` validates compile/link only. **Shippable Windows
binaries come only from `.woodpecker/client-windows.yml` on the `arbit-win`
agent** after committing + pushing and tagging `client-v*`. Linux cross-compile
to Windows is a dead end (gpui shader/DirectWrite). See `AGENTS.md` §4 and the
`mydevenv2-client-windows-crosscompile` memory.
