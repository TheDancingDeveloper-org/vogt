repo: TheDancingDeveloper-org/vogt
branch: dev
path: web/

## Last sync
date: 2026-08-20T00:18:30Z
commit: 095ddf9d4bf9

### Updated in this project
- Full drift audit `main`@1026d2f → `dev`@095ddf9: 82 files changed under `web/src/`, histories diverged. Findings recorded in `rail-spec.md` §0.
- Superseding files identified: `SurfaceHeader.tsx` (+test), `placeMetrics.ts`, `WaitingSession.tsx`, `narrow.ts`, `identity.ts`.
- Turn 5 restates the header and rail work as deltas against dev; withdrawn items listed with reasons (including my count-source table and the invented instance/actor line).
- `rail-spec.md` and `rail.css` rewritten as append-only deltas; handoff bundle regenerated against dev.

## Screen map
| Project screen | Repo files |
| --- | --- |
| Vogt Surface Header.dc.html — 5a header delta | web/src/SurfaceHeader.tsx, web/src/__tests__/surfaceHeader.test.tsx, web/src/styles.css (`.surface-header*` ~549–760, ~8320–8400), web/src/Board.tsx (~1859), web/src/AuditBrowser.tsx (~982), web/src/Inbox.tsx |
| Vogt Surface Header.dc.html — 5b rail delta | web/src/styles.css (`.places-rail` ~269–500, `.place-count` ~404, `.session-row` ~1230–1275, `.phone-bottom-nav` ~1153–1335, `.places-rail > .file-tree` ~2113), web/src/App.tsx (rail + `.phone-bottom-nav` ~1494), web/src/Sessions.tsx |
| rail-spec.md / rail.css | delta over the files above; counts from web/src/placeMetrics.ts; waiting-session card from web/src/WaitingSession.tsx; breakpoint from web/src/narrow.ts |
| Turns 1–4 (history, recreated from `main`) | web/src/styles.css @main (`.board-header`, `.vogt-backlog-header`, `.vab-header`, `.place-header` — all now dead), web/src/App.tsx, web/src/FileTree.tsx, web/src/Projects.tsx, web/src/WorkItemDetail.tsx |

## Sync history
- 2026-08-19T21:36:00Z — branch `main`. Recreated surface headers and the places rail; produced turns 1–4 and the first `rail-spec.md`/`rail.css`. Reference predated dev's SurfaceHeader work.
