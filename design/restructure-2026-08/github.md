repo: TheDancingDeveloper-org/vogt
branch: main
path: web/src

## Last sync
date: 2026-08-17T03:59:44Z

### Updated in this project
- Mobile: Sessions made the phone's first-class surface (list, attach, approval)
- Desktop shell wiring Board / Backlog / Inbox under one rail
- Inbox unified from GitHub, drift, CI and agent events
- Board and Backlog rebuilt with content-sized rows instead of fixed heights

## Sync history
- 2026-08-17T03:46:41Z — Inbox surface, rail with sessions + file tree
- 2026-08-17T03:41:18Z — first read of web/src (shell, board, backlog, audit, styles)

## Screen map
| Project screen | Built from |
| --- | --- |
| Vogt Restructure Wireframes.dc.html — turn 1 shells | web/src/App.tsx, web/src/layout.ts, web/src/styles.css |
| Turn 1 board options (1a, 1c, 1e) | web/src/Board.tsx, web/src/styles.css (.vogt-surface.board) |
| Turn 2 backlog (2a) | web/src/Backlog.tsx, web/src/styles.css (.vogt-backlog) |
| Turn 2 inbox (2b) | web/src/AuditBrowser.tsx (view=inbox), web/src/Projects.tsx (drift inbox) |
| Vogt Inbox.dc.html | web/src/AuditBrowser.tsx, web/src/App.tsx (drawer, session rows), web/src/FileTree.tsx, web/src/styles.css |
| Vogt.dc.html (shell) + BoardSurface / BacklogSurface / InboxSurface | web/src/App.tsx, web/src/Board.tsx, web/src/Backlog.tsx, web/src/AuditBrowser.tsx |
| Vogt Mobile.dc.html turn 4 (sessions-first) | web/src/ModKeyRow.tsx, web/src/styles.css (.modkey-row, .terminal-composer), web/src/App.tsx (continuity/activity) |
| Vogt Mobile.dc.html turn 3 (surfaces on phone) | web/src/styles.css (max-width:768px board/backlog rules) |
