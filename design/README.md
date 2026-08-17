# design/

Diagrams, mockups and exploratory notes. **This directory may be messy**, and
that is the point of having it: `docs/DESIGN.md` describes what exists and is
held to it, so a drawing of something that does not exist yet cannot live
there. It lives here.

Two rules for anything filed in this directory:

- **Nothing here is a specification.** A wireframe is not a requirement. What
  is owed lives in `docs/REQUIREMENTS.md` — its numbered tables for what is
  agreed, §7 for what was designed and not built. If a mockup here shows a
  behaviour nobody has written down, it is an idea, not a plan.
- **Nothing here is a source of truth about the built product.** The screens
  below were drawn against a commit and will drift from it. Where a document
  here and the code disagree, the code is right and the document is old.

## `restructure-2026-08/` — the desktop and mobile restructure

An export from the design tool that produced it, imported verbatim on
2026-08-17. It restructures the PWA around board / backlog / inbox under one
rail, and makes sessions the phone's first-class surface.

| File | What it shows |
|---|---|
| `Vogt.dc.html` | The desktop shell — rail, surfaces, drawer |
| `Vogt Restructure Wireframes.dc.html` | The turn-by-turn exploration the rest came out of |
| `BoardSurface.dc.html` | Board, rebuilt with content-sized rows rather than fixed heights |
| `BacklogSurface.dc.html` | Ranked backlog |
| `InboxSurface.dc.html` | One inbox over GitHub, drift, CI and agent events |
| `SessionsSurface.dc.html` | Sessions list, attach and approval |
| `Vogt Mobile.dc.html` | The phone, sessions-first |
| `Vogt Phone.dc.html` | Phone frames for the same surfaces |
| `Vogt Design Guardrails.dc.html` | The rules the rest is drawn to — principles, colour tokens, type, density |
| `android-frame.jsx` | The device frame the phone mockups render inside |
| `support.js`, `doc-page.js` | The export's own runtime; every `.dc.html` loads `./support.js` from beside it |
| `github.md` | The tool's sync record: what it read out of `web/src`, when, and which screen came from which file |

**Opening one**: they are self-contained HTML — open the file in a browser.
They pull React and a webfont from public CDNs, so a machine with no outbound
network renders them unstyled rather than not at all. Keep each `.dc.html`
beside `support.js`; nothing else links between them.

**Why the guardrails document is the one to read first.** It is the only part
of this export that is not a drawing: it states the constraints the screens
obey, and every one of them is lifted from the product's own principles rather
than invented — provenance and freshness on every answer, declared and observed
visually distinct, reporting never enforcing, "nothing to say" and "not
collected" given different copy, every write taking a reason, agents named as
the actors they are. Those are FR-O4, FR-R4, FR-G13, FR-S1 and FR-E4 wearing
colour and spacing. A screen that breaks one of them is wrong here for the same
reason the equivalent code would be wrong.

**What was deliberately not imported.** The export also carried a copy of
`web/src/styles.css`. It is byte-identical to the repository's own file at the
commit the tool read, so it is a *snapshot the tool took*, not a change it
proposes — and a second copy of a live source file sitting in `design/` is the
kind of thing that gets edited by mistake and then diverges silently. The
colour tokens the guardrails document quotes are the ones in
`web/src/styles.css`; read them there.
