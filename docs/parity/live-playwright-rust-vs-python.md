# Live Playwright: Rust core vs Python core (WI-1077)

Recorded 2026-10-18. Both cores ran the same `web/tests/browser` specs
(gui.spec.ts, demo.spec.ts) in the `live` project, each on its own fresh e2e
compose stack (`deploy/vogt.compose.yml` + `engine.overlay.yml` +
`e2e.overlay.yml`, credential-free, no forge fixture token):

- **Rust**: `vogt-core` built from rust-core 345f312d (I1 `rust-core.overlay.yml`).
- **Python**: `origin/main` d867e845 (v0.7.19) core and engine images.

Phone size is the `live-phone` project (iPhone 13 device profile). To
reproduce, bring up the stack as `.github/workflows/e2e.yml` does and run
`pnpm exec playwright test --project=live` and `--project=live-phone` with
`PLAYWRIGHT_LIVE_BASE_URL` and `PLAYWRIGHT_LIVE_TOKEN` set.
No snapshot was refreshed. A test that fails on Rust but passes on Python
would be a parity bug; there are none at either size.

## Desktop

| Core | Passed | Failed | Skipped |
|---|---|---|---|
| Rust | 56 | 90 | 38 |
| Python (main) | 55 | 91 | 38 |

- Fail on Rust only (parity bugs): 0
- Fail on Python only: 1
  - gui.spec.ts › A long session list scrolls inside the session area, not the rail (Test timeout of 30000ms exceeded.)

<details><summary>90 failing on both cores (inherited from main)</summary>

- demo.spec.ts › stale provenance cannot activate demo mode: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › A bounded Board cell continues from its cursor rather than re-reading: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › A dialog nobody opened is not downloaded at boot: Test timeout of 30000ms exceeded.
- gui.spec.ts › A resized, collapsed rail does not leak its width into the narrow shell: Test timeout of 30000ms exceeded.
- gui.spec.ts › A spoken reply whose TTS route is unconfigured shows the fallback notice without failing the turn: Test timeout of 30000ms exceeded.
- gui.spec.ts › A voice turn whose STT route is unconfigured retires the mic and shows an actionable notice: Test timeout of 30000ms exceeded.
- gui.spec.ts › Agent Tasks show bindings and findings, relabel Run Now, and survive reload: Error: expect(locator).toHaveValue(expected) failed
- gui.spec.ts › Assistant completes a voice-shaped turn through approval denial and TTS fallback: Test timeout of 30000ms exceeded.
- gui.spec.ts › Assistant hands-free barge-in: the speaker cuts in, the reply is halted, and the mic re-opens: Test timeout of 30000ms exceeded.
- gui.spec.ts › Assistant hands-free conversation: sends on silence, speaks the reply, and re-opens the mic with no touch: Test timeout of 30000ms exceeded.
- gui.spec.ts › Assistant voice turn: capture, STT, the {text, utterance} contract, approval, and a spoken reply: Test timeout of 30000ms exceeded.
- gui.spec.ts › Backlog filters survive a return through the rail, not just browser Back: Test timeout of 30000ms exceeded.
- gui.spec.ts › Backlog ranked rows size to their content and expand in place: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Backlog reaches Start a session… from the collapsed row and counts a page-only State filter: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Board card shows resolved project and assignee names, with the raw refs kept in the title: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Board cards expand measured title and body content in place: Test timeout of 30000ms exceeded.
- gui.spec.ts › Board dragover/drop uses the real browser gesture and keeps its filter on reload: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Board explains a same-column, other-lane drop instead of swallowing it: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Board filters survive a return through the rail, not just browser Back: Test timeout of 30000ms exceeded.
- gui.spec.ts › Board leads with the first card, which opens on a click and on Space: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Board progressive filters survive reload, history, and saved-lens recall: Error: locator.click: Error: strict mode violation: getByRole('group', { name: 'Add Board filters' }
- gui.spec.ts › Chat: ask, switch model, answer an approval, find it again: Test timeout of 30000ms exceeded.
- gui.spec.ts › Command palette lists the machine places, shows shortcuts, and ranks a session above a work item: Error: expect(locator).toContainText(expected) failed
- gui.spec.ts › Crowded desktop rail keeps Files and its footer reachable at 700px: Error: expect(locator).toBeInViewport() failed
- gui.spec.ts › Crowded desktop rail keeps Files and its footer reachable at 900px: Error: expect(locator).toBeInViewport() failed
- gui.spec.ts › Desktop Sessions overview lists the running sessions as links: Error: expect(locator).toContainText(expected) failed
- gui.spec.ts › Desktop Sessions overview points an empty machine at a start: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Desktop session rows open, bookmark, and close entirely from the keyboard: Test timeout of 30000ms exceeded.
- gui.spec.ts › Editor surfaces an on-disk change with Overwrite / Reload: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Editor workspace keeps its file-tree expansion across a tab switch: Test timeout of 30000ms exceeded.
- gui.spec.ts › Enabled GUI capability is reachable and renders its configured stream: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Files rail keeps its hierarchy compact and exposes real modified-file status: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › First run: a reader who already holds a token can reach the gate from the wizard: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › First run: the wizard claims the instance with a password and signs in: Error: expect(received).toEqual(expected) // deep equality
- gui.spec.ts › Git chooser is addressable and a failed panel recovers in place: Test timeout of 30000ms exceeded.
- gui.spec.ts › History distinguishes an archive outage from an empty archive and recovers: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › History lists a live session even when the archive is empty: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Inbox evidence, source filter and batch reason survive a desktop browser: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Inbox is dense, keeps the Source select on screen, links out and sticks its batch bar: Error: expect(locator).toHaveCount(expected) failed
- gui.spec.ts › Inbox puts its first answer before progressive support at every shell width: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Login and authentication errors present Vogt as the only product: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Non-editor routes fetch neither the editor nor the terminal: Test timeout of 30000ms exceeded.
- gui.spec.ts › Places counts distinguish real zero from an unavailable provider: Error: expect(locator).toHaveText(expected) failed
- gui.spec.ts › Places counts expose live workload meaning without overflowing the phone bar: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Projects and Work item headers answer the shared geometry contract: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Projects registry narrows the list to a name/slug search: Error: expect(locator).toHaveCount(expected) failed
- gui.spec.ts › Sessions owns the tool workspace and retains only terminal continuity: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Settings shows the blocked push state and disables Enable when permission is denied: Test timeout of 30000ms exceeded.
- gui.spec.ts › Setup steps: link the forge, import the first project, sweep, and see coverage: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Shift+Enter inserts a newline while Enter sends: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Sign in with a username and password enters the shell with the session: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Stop cancels an assistant send in flight: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › The item body renders Markdown, and the raw toggle shows its source: Error: expect(locator).toHaveText(expected) failed
- gui.spec.ts › The item editor's assignee picker is keyboard-operable: Test timeout of 30000ms exceeded.
- gui.spec.ts › The item page leads with comments and keeps the session form collapsed: Error: expect(received).toBeGreaterThanOrEqual(expected)
- gui.spec.ts › The running session list resizes by drag and remembers the height: Error: expect(received).toBeLessThan(expected)
- gui.spec.ts › The working header puts its controls and its action on one line: Error: /inbox controls sit with the title
- gui.spec.ts › Vogt identity and route-aware titles survive navigation and reload: Error: page.evaluate: TypeError: Cannot read properties of undefined (reading 'ready')
- gui.spec.ts › Work item state is changed from the detail page via Move to: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › a failed assistant send keeps the draft and offers a retry: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › a split composes an existing session, and closing a pane detaches it without killing: Error: expect(locator).toHaveCount(expected) failed
- gui.spec.ts › all six surface headers share the grammar and read the same at both sizes: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/surface-header-board-liv
- gui.spec.ts › an assistant Markdown reply renders a code block: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › app theme dark: backlog renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-dark-backlog-live-
- gui.spec.ts › app theme dark: board renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-dark-board-live-li
- gui.spec.ts › app theme dark: sessions renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-dark-sessions-live
- gui.spec.ts › app theme dark: workitem renders and matches its baseline: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › app theme dim: backlog renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-dim-backlog-live-l
- gui.spec.ts › app theme dim: board renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-dim-board-live-lin
- gui.spec.ts › app theme dim: sessions renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-dim-sessions-live-
- gui.spec.ts › app theme dim: workitem renders and matches its baseline: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › app theme hc-dark: backlog renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-hc-dark-backlog-li
- gui.spec.ts › app theme hc-dark: board renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-hc-dark-board-live
- gui.spec.ts › app theme hc-dark: sessions renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-hc-dark-sessions-l
- gui.spec.ts › app theme hc-dark: workitem renders and matches its baseline: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › app theme hc-light: backlog renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-hc-light-backlog-l
- gui.spec.ts › app theme hc-light: board renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-hc-light-board-liv
- gui.spec.ts › app theme hc-light: sessions renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-hc-light-sessions-
- gui.spec.ts › app theme hc-light: workitem renders and matches its baseline: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › app theme light: backlog renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-light-backlog-live
- gui.spec.ts › app theme light: board renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-light-board-live-l
- gui.spec.ts › app theme light: sessions renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-light-sessions-liv
- gui.spec.ts › app theme light: workitem renders and matches its baseline: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › exited session shows a banner and Remove skips the kill confirm: Error: expect(locator).toContainText(expected) failed
- gui.spec.ts › primary surface headers keep their shared order and geometry across zoom: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › terminal find bar searches the live buffer and navigates matches: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › terminal leaves browser zoom gestures alone and offers explicit font controls: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › terminal split commits atomically, nests, closes and survives reload: Error: expect(locator).toHaveCount(expected) failed
- gui.spec.ts › terminal split deletes the created session if its pane disappears in flight: Test timeout of 30000ms exceeded.
- gui.spec.ts › terminal split retries at the default cwd when the source cwd is outside the workspace: Test timeout of 30000ms exceeded.

</details>

## Phone

| Core | Passed | Failed | Skipped |
|---|---|---|---|
| Rust | 46 | 100 | 38 |
| Python (main) | 45 | 101 | 38 |

- Fail on Rust only (parity bugs): 0
- Fail on Python only: 1
  - gui.spec.ts › A long session list scrolls inside the session area, not the rail (Test timeout of 30000ms exceeded.)

<details><summary>100 failing on both cores (inherited from main)</summary>

- demo.spec.ts › a saved Inbox filter stays applied and the Inbox badge counts under it: Test timeout of 30000ms exceeded.
- demo.spec.ts › canonical terminal links restore split layouts with responsive chrome: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › A bounded Board cell continues from its cursor rather than re-reading: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › A crowded places rail keeps the file tree reachable: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › A dialog nobody opened is not downloaded at boot: Test timeout of 30000ms exceeded.
- gui.spec.ts › A place whose chunk never arrives costs that place, not the shell: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › A resized, collapsed rail does not leak its width into the narrow shell: Test timeout of 30000ms exceeded.
- gui.spec.ts › A spoken reply whose TTS route is unconfigured shows the fallback notice without failing the turn: Test timeout of 30000ms exceeded.
- gui.spec.ts › A voice turn whose STT route is unconfigured retires the mic and shows an actionable notice: Test timeout of 30000ms exceeded.
- gui.spec.ts › Agent Tasks show bindings and findings, relabel Run Now, and survive reload: Error: expect(locator).toHaveValue(expected) failed
- gui.spec.ts › Assistant completes a voice-shaped turn through approval denial and TTS fallback: Test timeout of 30000ms exceeded.
- gui.spec.ts › Assistant hands-free barge-in: the speaker cuts in, the reply is halted, and the mic re-opens: Test timeout of 30000ms exceeded.
- gui.spec.ts › Assistant hands-free conversation: sends on silence, speaks the reply, and re-opens the mic with no touch: Test timeout of 30000ms exceeded.
- gui.spec.ts › Assistant voice turn: capture, STT, the {text, utterance} contract, approval, and a spoken reply: Test timeout of 30000ms exceeded.
- gui.spec.ts › Backlog filters survive a return through the rail, not just browser Back: Test timeout of 30000ms exceeded.
- gui.spec.ts › Backlog ranked rows size to their content and expand in place: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Backlog reaches Start a session… from the collapsed row and counts a page-only State filter: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Board card shows resolved project and assignee names, with the raw refs kept in the title: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Board cards expand measured title and body content in place: Test timeout of 30000ms exceeded.
- gui.spec.ts › Board dragover/drop uses the real browser gesture and keeps its filter on reload: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Board explains a same-column, other-lane drop instead of swallowing it: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Board filters survive a return through the rail, not just browser Back: Test timeout of 30000ms exceeded.
- gui.spec.ts › Board leads with the first card, which opens on a click and on Space: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Board progressive filters survive reload, history, and saved-lens recall: Error: locator.click: Error: strict mode violation: getByRole('group', { name: 'Add Board filters' }
- gui.spec.ts › Chat: ask, switch model, answer an approval, find it again: Test timeout of 30000ms exceeded.
- gui.spec.ts › Command palette lists the machine places, shows shortcuts, and ranks a session above a work item: Error: expect(locator).toContainText(expected) failed
- gui.spec.ts › Crowded desktop rail keeps Files and its footer reachable at 700px: Error: expect(locator).toBeInViewport() failed
- gui.spec.ts › Crowded desktop rail keeps Files and its footer reachable at 900px: Error: expect(locator).toBeInViewport() failed
- gui.spec.ts › Desktop Ctrl+B toggles the Places rail while palette and help stay bound: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Desktop Sessions overview lists the running sessions as links: Error: expect(locator).toContainText(expected) failed
- gui.spec.ts › Desktop Sessions overview points an empty machine at a start: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Desktop session rows open, bookmark, and close entirely from the keyboard: Test timeout of 30000ms exceeded.
- gui.spec.ts › Dialog focus is contained and restored, and feedback matches its live region: Test timeout of 30000ms exceeded.
- gui.spec.ts › Editor surfaces an on-disk change with Overwrite / Reload: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Editor workspace keeps its file-tree expansion across a tab switch: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Enabled GUI capability is reachable and renders its configured stream: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Files rail keeps its hierarchy compact and exposes real modified-file status: Test timeout of 30000ms exceeded.
- gui.spec.ts › First run: a reader who already holds a token can reach the gate from the wizard: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › First run: the wizard claims the instance with a password and signs in: Error: expect(received).toEqual(expected) // deep equality
- gui.spec.ts › Git chooser is addressable and a failed panel recovers in place: Test timeout of 30000ms exceeded.
- gui.spec.ts › History distinguishes an archive outage from an empty archive and recovers: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › History lists a live session even when the archive is empty: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Inbox evidence, source filter and batch reason survive a desktop browser: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Inbox is dense, keeps the Source select on screen, links out and sticks its batch bar: Error: expect(locator).toHaveCount(expected) failed
- gui.spec.ts › Inbox puts its first answer before progressive support at every shell width: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Login and authentication errors present Vogt as the only product: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Non-editor routes fetch neither the editor nor the terminal: Test timeout of 30000ms exceeded.
- gui.spec.ts › Places counts distinguish real zero from an unavailable provider: Error: expect(locator).toHaveText(expected) failed
- gui.spec.ts › Places counts expose live workload meaning without overflowing the phone bar: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Projects and Work item headers answer the shared geometry contract: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Projects registry narrows the list to a name/slug search: Error: expect(locator).toHaveCount(expected) failed
- gui.spec.ts › Recent places dedupes a surface visited under different queries: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Route truth owns unavailable links, current navigation and Settings return: Test timeout of 30000ms exceeded.
- gui.spec.ts › Sessions owns the tool workspace and retains only terminal continuity: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Settings confirms every destructive action before it runs: Test timeout of 30000ms exceeded.
- gui.spec.ts › Settings keeps its Save/Cancel footer on screen at desktop and phone sizes: Test timeout of 30000ms exceeded.
- gui.spec.ts › Settings shows the blocked push state and disables Enable when permission is denied: Test timeout of 30000ms exceeded.
- gui.spec.ts › Setup steps: link the forge, import the first project, sweep, and see coverage: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Shift+Enter inserts a newline while Enter sends: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Sign in with a username and password enters the shell with the session: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › Stop cancels an assistant send in flight: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › The Places rail resizes, collapses, and remembers both across reload: Error: expect(received).toBe(expected) // Object.is equality
- gui.spec.ts › The item body renders Markdown, and the raw toggle shows its source: Error: expect(locator).toHaveText(expected) failed
- gui.spec.ts › The item editor's assignee picker is keyboard-operable: Test timeout of 30000ms exceeded.
- gui.spec.ts › The item page leads with comments and keeps the session form collapsed: Error: expect(received).toBeGreaterThanOrEqual(expected)
- gui.spec.ts › The running session list resizes by drag and remembers the height: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › The working header puts its controls and its action on one line: Error: /board controls and action share a line
- gui.spec.ts › Vogt identity and route-aware titles survive navigation and reload: Error: page.evaluate: TypeError: Cannot read properties of undefined (reading 'ready')
- gui.spec.ts › Work item state is changed from the detail page via Move to: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › a failed assistant send keeps the draft and offers a retry: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › a split composes an existing session, and closing a pane detaches it without killing: Error: expect(locator).toHaveCount(expected) failed
- gui.spec.ts › all six surface headers share the grammar and read the same at both sizes: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/surface-header-board-liv
- gui.spec.ts › an assistant Markdown reply renders a code block: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › app theme dark: backlog renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-dark-backlog-live-
- gui.spec.ts › app theme dark: board renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-dark-board-live-li
- gui.spec.ts › app theme dark: sessions renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-dark-sessions-live
- gui.spec.ts › app theme dark: workitem renders and matches its baseline: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › app theme dim: backlog renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-dim-backlog-live-l
- gui.spec.ts › app theme dim: board renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-dim-board-live-lin
- gui.spec.ts › app theme dim: sessions renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-dim-sessions-live-
- gui.spec.ts › app theme dim: workitem renders and matches its baseline: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › app theme hc-dark: backlog renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-hc-dark-backlog-li
- gui.spec.ts › app theme hc-dark: board renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-hc-dark-board-live
- gui.spec.ts › app theme hc-dark: sessions renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-hc-dark-sessions-l
- gui.spec.ts › app theme hc-dark: workitem renders and matches its baseline: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › app theme hc-light: backlog renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-hc-light-backlog-l
- gui.spec.ts › app theme hc-light: board renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-hc-light-board-liv
- gui.spec.ts › app theme hc-light: sessions renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-hc-light-sessions-
- gui.spec.ts › app theme hc-light: workitem renders and matches its baseline: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › app theme light: backlog renders and matches its baseline: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › app theme light: board renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-light-board-live-l
- gui.spec.ts › app theme light: sessions renders and matches its baseline: Error: A snapshot doesn't exist at /web/tests/browser/gui.spec.ts-snapshots/theme-light-sessions-liv
- gui.spec.ts › app theme light: workitem renders and matches its baseline: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › exited session shows a banner and Remove skips the kill confirm: Error: expect(locator).toContainText(expected) failed
- gui.spec.ts › primary surface headers keep their shared order and geometry across zoom: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › terminal find bar searches the live buffer and navigates matches: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › terminal leaves browser zoom gestures alone and offers explicit font controls: Error: expect(locator).toBeVisible() failed
- gui.spec.ts › terminal split commits atomically, nests, closes and survives reload: Error: expect(locator).toHaveCount(expected) failed
- gui.spec.ts › terminal split deletes the created session if its pane disappears in flight: Test timeout of 30000ms exceeded.
- gui.spec.ts › terminal split retries at the default cwd when the source cwd is outside the workspace: Test timeout of 30000ms exceeded.

</details>

