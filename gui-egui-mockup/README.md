# gui-egui-mockup: a static mockup of the Vogt GUI

A **static** mockup of the whole Vogt GUI on [egui] and [fluent2-egui], for
desktop and the browser (WI-2162). It shows every main screen of today's web
GUI (`web/`) with realistic fake data so the look can be judged. There is no
backend: nothing is read from or sent to a server, and every action only
changes in-memory state (or shows a "mockup" toast).

It is separate from the live proof of concept in `gui-egui/` (WI-1806, branch
`wi-1806-vogt-gui-slice1`), so neither blocks the other. Like that crate, it is
a standalone Cargo workspace, so the engine's lockfile and Rust floor are
untouched. fluent2-egui needs Rust 1.95. The code is safe Rust only
(`unsafe_code = "forbid"`).

## Screens

| Screen | What it shows |
|---|---|
| Login | engine address, username, password, "keep me signed in"; any credentials work |
| Shell | top bar, navigation pane, status bar, command palette (**Ctrl+K**), shortcut overlay (**?**) |
| Sessions | the session rail (filter, tabs, state, role and model badges, activity sparklines) and the open session: a terminal grid with a fake Claude Code transcript, the brief, and status |
| Oversight | summary tiles, pending decisions with the recommended answer first, a card per worker |
| Backlog | a filter bar (search, project, priority, state, kind, hide done) over a sortable DataGrid; Enter or double-click opens the item |
| Board | a kanban column per state; cards drag between columns (in memory only) |
| Work item | breadcrumb, markdown body, the hand-off fields, comments with a composer, relations, branches, linked sessions |
| Git | a branch's changed files and a read-only diff, inline or side by side |
| Inbox | notifications from sessions, CI, drift and deploys; archive, snooze, mark read |
| Projects | the project list and the selected project's work by state, commits, deploy lanes and policy |
| Settings | theme (light, dark, high contrast), density (compact, medium, touch), terminal, notifications, account |

Screenshots of every screen in both themes are in [`docs/screenshots/`](docs/screenshots/),
for example `sessions-light.png`, `backlog-dark.png`, `palette-light.png`.

## Layout

| path | what |
|---|---|
| `src/fixtures.rs` | the fake deployment: sessions, decisions, work items, comments, inbox, projects, a diff |
| `src/app.rs` | the shell: sign-in gate, top bar, navigation, palette, shortcut overlay, theme |
| `src/screens/` | one module per screen |
| `tests/smoke.rs` | kittest smoke tests (no GPU) |
| `tests/screenshots.rs` | the screenshot run (feature `snapshots`, needs a wgpu adapter) |
| `scripts/screenshots.sh` | runs the screenshots in a container with Mesa lavapipe |

## Run it

Native (from this directory):

```sh
cargo run -p vogt-gui-mockup
```

In the browser with [trunk] (`cargo install --locked trunk`, and the
`wasm32-unknown-unknown` target):

```sh
trunk serve            # http://127.0.0.1:8080, rebuilds on change
trunk build --release  # static files in dist/, serve them with any web server
```

## Checks

```sh
export CARGO_BUILD_JOBS=4
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32-unknown-unknown -- -D warnings
cargo test
trunk build --release
scripts/screenshots.sh   # needs Docker; rewrites docs/screenshots/
```

[egui]: https://github.com/emilk/egui
[fluent2-egui]: https://github.com/TheDancingDeveloper-org/fluent2-egui
[trunk]: https://trunkrs.dev
