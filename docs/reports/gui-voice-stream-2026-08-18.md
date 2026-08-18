# Combined GUI-drift + voice workstream for `vogt-dev`

**Prepared:** 2026-08-18  
**Target branch/stream:** `dev` (the repository branch that builds the `dev`/`dev-<sha>` image consumed by the `vogt-dev` stack).  
**Starting GUI baseline:** `373b742` (`design/restructure-wireframes`).  
**Starting voice checkpoint:** `feat/voice-poc` at `5320200` (rebased on `373b742`; Checkpoint A complete).  
**Intent:** combine the two workstreams without losing either the GUI truthfulness fixes or the voice POC's already-tested changes.

## Executive decision

Use one integration stream, but keep the work in two reviewable lanes:

- **Lane G — GUI drift:** repair the deployed PWA's visibly broken/legacy-first experience, starting with the Files sidebar and phone/Board/Backlog/Sessions composition.
- **Lane V — Voice:** merge the recovered Checkpoint A code, validate desktop speech, then add server-side STT/TTS and only then pursue phone background speech.

Do **not** merge the two historical branches blindly. `feat/voice-poc` is a large, tested change set (2,747 lines over 33 files) and the current working branch already contains untracked documentation/reports. Create a fresh integration branch from `dev`, cherry-pick the two voice commits, then resolve/test GUI changes on top. Keep each lane's commits separate so a deployment can stop after a GUI repair or after voice Checkpoint A.

## What the recovery handover says

The authoritative recovery handover is:

`/home/sprooty/Working/temp/vokt2-recovery-2026-08-17/HANDOVER.md`

It says:

- the dead Claude session must not be resumed;
- the recovered worktree is `.claude/worktrees/voice-poc`, branch `feat/voice-poc`;
- Checkpoint A is complete and green (238 Rust, 817 Python, 262 web reported by that handover);
- the authoritative voice document is the worktree copy of `docs/VOICE_POC.md`, not the stale untracked copy in the main checkout;
- next voice checkpoint is desktop microphone validation; server STT/TTS is Checkpoint C; phone work is blocked by FR-M4/dev APK and device evidence.

The current `feat/voice-poc` tip is `5320200`, with parent `f7e7789`; both are descendants of `373b742`. The current main checkout is on `design/restructure-wireframes` and has not incorporated those commits.

## GUI drift baseline to carry forward

The live review at `docs/reports/restructure-live-2026-08-17/REPORT.md` and its screenshots establish these defects:

1. **Critical — Files sidebar visibly broken.** `desktop-sessions.png` shows repeated blank/broken glyph boxes and ellipsized `…` instead of a readable tree. This is a functional Sessions defect, not only a styling concern.
2. **High — phone Board violates one-state-at-a-time.** It stacks desktop filters and all workflow states instead of a state pill row.
3. **High — Board/Backlog retain legacy select-grid/table interaction.** The design calls for chips, `+ filter`, named lenses, content-sized rows/cards and in-place expansion.
4. **High — Sessions is only a route shell.** The observed live page lacks the designed attention-sorted session cards/list, pane workspace and pending-action composition.
5. **High — Inbox is contractually strong but visually an operations form.** Coverage/evidence/reasons are truthful; phone row actions are not in bottom sheets and useful entries are pushed below the fold.
6. **Medium-high — phone bottom nav has labels but no live counts.**
7. **Medium — desktop rail/file region is overly pictographic and action-dense.**

These findings should be treated as acceptance evidence, not as a request to redesign unrelated inner surfaces.

## Integration rules

1. **Branch base:** start from `dev`, not the stale local `design/restructure-wireframes` tip.
2. **Voice import:** cherry-pick `f7e7789` then `5320200` (or cherry-pick the range `f7e7789..5320200` with the first commit included explicitly). Verify migration is `0010_session_model.sql`; do not resurrect `0009`.
3. **GUI changes:** commit in small lane-G commits. Do not mix Files sidebar repair with voice engine changes.
4. **Requirements/docs:** preserve the current r16 FR-T9–T13/FR-M6 rows and the GUI gap register. Reconcile `docs/VOICE_POC.md` from the authoritative voice worktree copy before committing; do not overwrite its §6 findings with the stale main copy.
5. **No deployment by SSH or ad hoc Compose.** `dev` push builds the signed dev image; Komodo/`personal/vogt-dev` is the deployment path. Publish and deploy remain separate.
6. **Every browser claim needs a real CSS/browser capture.** Keep the live screenshot baseline and add post-change captures at 1440×1000 and 390×664.

## Ordered stream of work

### Phase 0 — Integration preparation (no product behavior change)

**Goal:** make the combined branch reproducible.

- Create `work/gui-voice-dev` from `dev`.
- Cherry-pick the recovered voice Checkpoint A commits.
- Copy the authoritative `docs/VOICE_POC.md` from `.claude/worktrees/voice-poc` into the integration branch.
- Keep the GUI review report and screenshots as evidence.
- Run the smallest gates first: migration numbering check, TypeScript typecheck, Rust format/clippy, Python tests/mypy/ruff.
- Record the exact resulting commit and image source in the handover.

**Exit:** one clean integration branch with voice Checkpoint A present, no duplicate migration number, and documentation that says exactly what is and is not delivered.

### Phase G1 — Files sidebar recovery (critical GUI blocker)

**Goal:** restore a legible, functional desktop file tree before polishing surfaces.

- Capture the live failure against a local/fixture tree with actual names, not only the remote empty/broken state.
- Inspect the rendered DOM and computed styles for `FileTree.tsx`, `.file-tree`, `.tree-row`, `.tree-icon`, `.tree-term-btn`, and `fileIcons.ts`.
- Replace broken/unreliable icon rendering with stable text/icon treatment that never substitutes filenames with empty glyph boxes. Do not use emoji as navigation semantics; a textual file type or compact neutral icon is acceptable.
- Ensure each row has a readable name/path, expansion affordance, and accessible labels for actions. Keep destructive actions discoverable but not eight competing inline glyphs per row.
- Verify long names, directories, files, loading, empty, error, search results and coarse-pointer behavior.
- Add browser evidence at desktop and narrow widths; the primary acceptance assertion is “a person can identify and open a named file/folder from the rail.”

**Exit:** Files sidebar shows readable names and actions in the live dev build; screenshot no longer contains broken glyph rows or filename-only ellipses.

### Phase G2 — Desktop shell truthfulness pass

**Goal:** make the delivered desktop shell match the selected restructure grammar without redesigning every inner page.

- Keep the 248px Work/Estate/Machine places rail and stable routes.
- Normalize surface headers to title, freshness/honesty, controls, primary action.
- Remove or contain explanatory implementation paragraphs that displace useful content.
- Reduce pictographic rail actions; preserve text-first navigation.
- Keep Sessions as the home for terminal/editor/tools; verify recent places and deep links.
- Add a route matrix browser run for `/`, `/board`, `/backlog`, `/inbox`, `/projects`, `/audit`, `/settings`, `/sessions`, terminal/editor/tool routes.

**Exit:** desktop screenshots show a coherent places shell and no stranded route/capability.

### Phase G3 — Board and Backlog interaction repair

**Goal:** remove the most visible legacy interaction model.

**Board**

- Replace the large select grid with chips, `+ filter`, and named saved lenses while preserving URL round-trip.
- Preserve server-owned columns, drag/keyboard movement, typed reason and refusal rollback.
- Enforce measured content-sized cards: long titles wrap/clamp at three lines and expand in place; no fixed estimate becomes a visual height.
- At phone width, render one workflow state at a time using a labelled pill row. Never stack all columns vertically.

**Backlog**

- Replace the dense legacy table as the primary visual with measured ranked rows showing rank/ref/title/trust/age/score/why/provenance.
- Preserve both bulk transition and bulk label paths with independent typed reasons.
- Keep observed/declaration actions honest (Adopt/Suppress versus Open/Session).
- Ensure first phone viewport contains ranked content, not only filters.

**Exit:** real browser captures demonstrate long content, URL filters, measured expansion, phone one-state Board, and visible Backlog content above the fold.

### Phase G4 — Sessions workspace and phone composition

**Goal:** make Sessions a real steering surface before voice is tested on top of it.

- Render attention-sorted session list: pending action, waiting, errored, running, idle/recent.
- Show readable session name, cwd, activity and continuity/actor context; do not concatenate them into one unreadable control.
- Add internal pane/tool composition for terminal/editor/Git/GUI/History/Tasks/Assistant.
- Render pending terminal and Vogt actions once, with exact payload, model/effort where applicable, expiry/supersession and correct actor wording.
- On phone, show waiting sessions as cards with the visible PTY prompt and labelled `y + Enter` / `Ctrl-C`; other sessions as rows.
- Move Inbox/row actions to bottom sheets (≥52px rows) on coarse clients.
- Add live counts to the four-place phone bar.

**Exit:** Sessions screenshots match the design's steering/workspace hierarchy and a phone user can reach every secondary route through labelled “Go to…”.

### Phase V1 — Voice Checkpoint A integration smoke

**Goal:** prove the recovered typed/tool-loop work is alive on `vogt-dev` without claiming speech is complete.

- Confirm provider profile config is present and keys are never advertised.
- Verify `inbox.list` is the sole unified notification read, with coverage-aware language.
- Verify `session.start` model/effort parity on CLI/REST/MCP and migration `0010`.
- Verify scratch-project resolution and refusal when no scratch project is configured.
- Verify model/effort appear in approval payload/card and unsafe model ids are refused.
- Verify `voiceRepair.ts` repair preview is visible before composer send.
- Run typed U1–U5 and the “yes / approve it / go ahead” no-bypass test.

**Exit:** Checkpoint A is reproducible on the integration branch and documented as typed-only.

### Phase V2 — Desktop microphone / Checkpoint B

**Goal:** validate the recognizer against real Vogt vocabulary before adding server speech plumbing.

- Use a real laptop microphone and the desktop PWA.
- Run U1–U5 three times each; record raw recognition text, repaired text, whether repair was correct, latency and approval taps.
- Specifically test `rustnzbd`, `WI-12`, “backlog”, “notifications”, “GPT 5.6 medium”, and a sentence containing a verb adjacent to a multi-word slug (the known fuzzy-repair trap).
- Confirm speech replies and the explicit on-screen approval boundary for U3/U4.
- Update the authoritative `docs/VOICE_POC.md` §6 with results; do not mark FR-T13 complete until the full acceptance list passes.

**Exit:** measured Checkpoint B result: pass with misses listed, or a named blocker with a repair follow-up.

### Phase V3 — Server-side STT/TTS / Checkpoint C

**Goal:** add the independent server speech pipeline only after desktop Web Speech has a measured baseline.

- Implement `/api/assistant/stt` and `/api/assistant/tts` behind independently configured OpenAI-compatible audio endpoints.
- Return 404 when unconfigured; fall back to the existing client path; never store audio unless an explicit debug setting requests it.
- Test provider errors, timeouts, malformed audio, empty text, external-content delimiting and auth boundaries.
- Compare Web Speech versus server STT/TTS on U1–U5 for quality, latency and cost.
- Keep cloud and local Whisper.cpp/Kokoro as configuration alternatives, not separate client implementations.

**Exit:** a measured choice of default speech path; no silent fallback that makes an unavailable provider look empty.

### Phase V4 — Phone voice/background / Checkpoint D

**Goal:** validate background updates and speak-the-push on the installed dev APK.

- Resolve FR-M4 dev-alongside-prod package/FCM registration.
- Deploy the dev image through Komodo; install on a physical device.
- Hold the foreground service only during active voice; release it when conversation ends; no wake word/always-listening mic.
- Verify push wakes the app, opens `/sessions?approval=<id>`, and during an active conversation is spoken as well as shown.
- Measure 30-minute battery impact, screen-off socket survival, push latency and stale approval behavior.
- Record device, APK, deployed digest and result in the voice POC and requirements evidence.

**Exit:** FR-M6 evidence exists or is explicitly blocked by a named hardware/deployment constraint.

### Phase 10 — Combined acceptance and dev deployment

Run the GUI and voice acceptance as one sequence:

1. Build web assets fresh before Cargo/image build.
2. Run Python, web, Rust and browser suites.
3. Capture desktop and phone screenshots for Files, Sessions, Inbox, Board and Backlog.
4. Execute typed U1–U5 and desktop spoken U1–U5 (if Checkpoint B has started).
5. Deploy the resulting signed dev image through Komodo to `vogt-dev`.
6. Re-run the live route sweep and compare screenshots to the baseline.
7. Only then update `docs/DESIGN.md`, `docs/ROADMAP.md` or requirement status to describe what actually exists.

## Suggested commit sequence

1. `chore(integration): import recovered voice checkpoint A`
2. `fix(gui): restore readable files rail`
3. `fix(gui): normalize places shell and route evidence`
4. `fix(gui): make board and backlog content-first on desktop`
5. `fix(gui): make board and actions phone-first`
6. `fix(gui): complete sessions workspace composition`
7. `test(voice): reproduce checkpoint A on dev integration`
8. `feat(voice): record desktop microphone checkpoint B`
9. `feat(voice): add server-side stt and tts checkpoint C`
10. `feat(mobile): validate background voice and push checkpoint D`
11. `docs: close only evidenced GUI/voice requirements`

## Explicit non-goals

- Do not add a second approval store or spoken approval path.
- Do not make the client merge Inbox sources.
- Do not replace the Python operation registry with GUI/engine-specific behavior.
- Do not claim the design export is a specification; use requirements and observed evidence.
- Do not deploy by SSH or hand-edit the running container.
- Do not call typed Checkpoint A “voice complete.”
- Do not start phone background work before the dev APK/FCM prerequisite is actually available.

## Current status at handoff

- **GUI:** shell/Inbox/route skeleton exists, but the live Files sidebar is critically broken and Board/Backlog/Sessions/phone remain materially off-design. See `docs/reports/restructure-live-2026-08-17/REPORT.md`.
- **Voice:** Checkpoint A is implemented and tested on `feat/voice-poc` at `5320200`; desktop microphone Checkpoint B is not run; server STT/TTS and phone background Checkpoint D are not started.
- **Integration:** not yet performed. The current checkout is `design/restructure-wireframes`; this document is the workstream plan, not an assertion that the branches have been combined.
