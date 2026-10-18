# Composer attachments (WI-1829)

Status: design, no implementation. 2026-10-18.

## Problem

To hand an agent a screenshot or file from the phone today you open the Files
panel, upload into some folder, then type the path into the composer by hand.
The goal is one gesture from any composer (chat, assistant, terminal session)
or from the Android share sheet. That gesture puts the file somewhere the agent
can read it and inserts the reference for you.

## What exists

| Piece | Where | Reused how |
|---|---|---|
| Streaming upload `PUT /api/files/upload?path=&create_parents=` | `engine/server/src/files.rs` `upload_file` | The write path stays as it is: spool to a temp file in the target dir, sha256, atomic rename. Cap is `MAX_UPLOAD_BYTES` = 10 GiB. |
| Capability gate | `engine/server/src/auth.rs` (`FilesystemWrite` on the upload route, WI-1020) | Attachments need the same capability. No new grant. |
| Client `api.uploadFile(path, blob, create_parents)` | `web/src/api.ts` | Called as is by the new composer helper. |
| Chat composer (`textarea.chat-composer`) | `web/src/Chat.tsx` | Gets the attach button, paste and drop. |
| Assistant composer | `web/src/Assistant.tsx` | Same. |
| Terminal composer / ModKeyRow, xterm paste | `web/src/Terminal.tsx`, `ModKeyRow.tsx`, `terminalInput.ts` | Same, plus a paste-image hook on xterm. |
| Chat turn | `engine/server/src/chat_codex.rs` `turn_start` sends `input: [{type:"text"}]` to the Codex app-server | Gains `{type:"localImage", path}` items. |
| Session input `POST /api/sessions/{id}/input {text, submit}` | `engine/server/src/api.rs` | No change. The path goes in as text. |
| Worktree cleanup | `engine/server/src/worktree_clean.rs` (WI-1304/WI-1750) | Extended to remove the attachments dir when the session stops. |
| Android wrapper | `mobile/android` (Capacitor 8). `ServerActivity` has a LAUNCHER filter only. | Gets an `ACTION_SEND` / `SEND_MULTIPLE` intent filter. |

## Recommendation

### 1. One engine route: `POST /api/attachments`

This is a thin wrapper over the upload code, not a new write path. The client
does not pick the path. The engine does, so a composer can never write into the
repo or anywhere else in the tree.

```
POST /api/attachments?target=session:<uuid>|chat:<uuid>|assistant&name=<orig>
Content-Type: <mime>           (raw body, streamed like /api/files/upload)
→ 201 { id, path, rel_path, name, mime, bytes, sha256, width?, height?, stripped_exif: bool }
GET    /api/attachments/{id}/thumb   (≤256px JPEG/WebP, for the chip and transcript)
DELETE /api/attachments/{id}         (remove a chip before sending)
```

- **Auth:** it takes `FilesystemWrite`, the same as `/api/files/upload`. The
  caller must also be able to see the target: the same `may_show` policy that
  gates session and chat reads (WI-1005/WI-1020). An agent token that can write
  files can attach, as it could upload already. That is not a new power.
- **Where the file lands:** in the target's working directory, at
  `<session cwd>/.vogt/attachments/<yyyymmdd-hhmmss>-<slug>.<ext>`.
  - The session cwd is the linked worktree for worker sessions and the chosen
    cwd otherwise. A chat uses its own cwd, and the assistant uses
    `<engine state>/assistant/attachments/`.
  - It is never written at the repo root. `.vogt/` is git-ignored in two ways:
    the engine writes `.vogt/.gitignore` containing `*` when it creates the
    dir, so it never shows up in `git status` even for repos that don't list
    it.
  - The file is in the agent's own sandbox root, so every CLI can read it
    without an extra permission grant.
- **Names:** the engine slugifies the name, rejects separators and dotfiles,
  and appends `-2`, `-3`, … on collision. Original names are kept only in the
  response.
- **Limits:**
  - Per-file cap `ENGINE_ATTACHMENT_MAX_BYTES`, default **25 MiB**. A phone
    photo is 3–8 MB, and anything larger belongs in the Files panel.
  - Per-target cap of 200 MiB or 100 files. Over either cap the request gets
    `413` with a message pointing at Files.
  - A MIME allow-list is not enforced, because any file is useful. Only
    images get thumbnails and image items.
- **Privacy:** for `image/jpeg`, `image/heic` and `image/png`, the engine
  strips EXIF, GPS and XMP metadata before the rename. Rust crates
  (`img-parts`) do this without re-encoding. It also applies the orientation
  tag to the pixels, or keeps only Orientation, so photos don't come out
  sideways. `stripped_exif` reports what happened. Stripping happens on the
  server so that every entry point (picker, share sheet, paste) gets it. A
  client-side strip would be a second layer, not a replacement.
- **Retention:** the attachments dir is removed when the session is
  **stopped or removed**. It is not removed on hibernation, because a woken
  session still needs its references. This uses the same hook and the same
  "linked worktree only, never the main checkout" guards as WI-1750. A chat's
  dir goes when the chat is archived. Assistant attachments expire after 7
  days by a sweep. A cwd that is a main checkout (a non-worktree session)
  still has its `.vogt/attachments` cleared on stop, because the dir is
  engine-owned and ignored.
- **Audit:** `vogt::audit event=attachment.create` with the target, bytes,
  mime and principal. The audit never records content or the full original
  name.

### 2. What gets inserted

The inserted text is the **absolute path**, with the **thumbnail chip** shown in
the PWA only. Markdown image syntax is not used: a CLI reads it as plain text,
and it costs tokens for nothing.

| Agent | How it best takes an image | What Vogt sends |
|---|---|---|
| Claude Code (TTY) | It reads an image when the prompt contains a local path (`Read` tool, multimodal). | Text: ` /abs/path.png ` |
| Codex CLI (TTY) | `codex -i` at launch only. In the TUI, a pasted path is read by its file tools. Images are viewed with its `view_image` tool. | Text path (same as above) |
| Chat (Codex app-server) | The `turn/start` input accepts `{type:"localImage", path}` as a real image item. | Text item plus a `localImage` item per image |
| opencode | `@file` references, and image paths in the prompt are attached. | Text: `@/abs/path.png` (the opencode adapter rewrites a bare path) |
| Klaudia | Uses the opencode backend. Same as opencode, to be confirmed in item 6 below. | Same as opencode |
| Assistant (OpenAI-compatible) | `image_url` content parts with a data URI, only when the configured model supports vision. | Path in the text and, if vision is on, a data-URI part. Otherwise the text names the attachment and its path so tools can fetch it. |

The composer keeps the chips separate from the draft text. On send, it appends
one path per line at the end of the text, unless the user has placed a
reference token inline. An agent-CLI adapter in `agent_cli.rs` owns the
per-CLI format (bare path vs `@path`), so the PWA does not hard-code CLI
behaviour.

### 3. Terminal sessions: type into the PTY, no side channel

The path is written through the existing input path: the WS binary frame, or
`session_input` for MCP and driving. It is sent as a **bracketed paste**
(`ESC[200~ … ESC[201~`), so the TUIs treat it as pasted text and don't act on
each character. Bracketed paste is also how Claude Code and Codex detect pasted
image paths. **`submit` is never set**: the user still presses Enter, so an
attachment can't fire a half-written prompt.

A side channel (an engine-held "pending attachments" list that each CLI polls)
was rejected. None of the CLIs have an input for it, and it would split the
prompt across two transports.

`session_input` needs no change. Agents that want to attach for another session
call `POST /api/attachments` and then `session_input` with the path. A
`session_attach` MCP tool can wrap both later; that is optional (item 7).

### 4. UI

The same three entry points on every composer:

```
┌ chat / assistant / terminal composer ───────────────────────────┐
│ [🖼 screenshot-0912.png ×] [📄 trace.log ×]   ← chips, upload %   │
│ ┌──────────────────────────────────────────────────┐ ┌──┐ ┌──┐  │
│ │ why does this crash?                             │ │📎│ │➤ │  │
│ └──────────────────────────────────────────────────┘ └──┘ └──┘  │
└─────────────────────────────────────────────────────────────────┘
📎 → sheet: [ Photo library ] [ Camera ] [ File ]
```

- **📎 button:** a hidden `<input type="file" multiple>`, with
  `accept="image/*"` + `capture="environment"` for "Camera". In the Android
  WebView, Capacitor's file chooser already returns content URIs for
  `<input type=file>`, so no plugin is needed.
- **Chips:**
  - Each chip uploads on pick and shows progress. A thumbnail comes from a
    local `URL.createObjectURL` until `/thumb` is ready.
  - × calls `DELETE`. Send is disabled while any chip is uploading.
  - On a terminal pane, the chip row is the ModKeyRow tray. Tapping a chip
    inserts the path immediately (see §3), because a PTY has no "draft".
- **Desktop paste:** an `onPaste` handler on each textarea, and an xterm
  `paste` listener on terminals. When `clipboardData.files` holds an image,
  it is attached as `paste-<time>.png`, and plain text pastes are unchanged.
- **Drag and drop:** the composer and the terminal pane accept `Files` drops.
  `terminalDnd.ts` must keep its session-drag MIME check so a file drop does
  not start a split.
- **Transcript:** the chat and assistant transcripts render image attachments
  as thumbnails linking to the file viewer (WI-1005 clickable paths).

### 5. Android share sheet

- **Manifest:** add an `<intent-filter>` with `SEND` and `SEND_MULTIPLE` for
  `image/*`, `text/plain` and `*/*` to `ServerActivity`, the launcher, which
  is `singleTask`. The filter must keep the WI-924 task behaviour.
- **Native side:** a small Capacitor plugin copies each shared `content://`
  stream into app cache. It then raises a `shareReceived` event with
  `{files:[{name, mime, size, cachePath}], text?}`.
- **PWA side:** a **"Send to…" picker** lists running sessions and chats,
  most recent first, from the rail model. The picker also offers the
  assistant and a "New chat" option. Picking a target opens that target's
  composer with the files already attached as chips (upload via
  `POST /api/attachments`) and any shared text in the draft. Nothing sends
  without the user pressing send.
- If the app is not signed in, the share is held until login completes and is
  then shown in the picker.

## Implementation items to split out

1. **Engine: `/api/attachments` route.** Covers the managed path, the
   `.vogt/.gitignore`, the caps, the `FilesystemWrite` + target visibility
   gate, audit, thumbnails, `DELETE`, and an `engine_routes.py` parity entry
   (WI-1094). Tests: path cannot escape, wrong-target refusal, 413, and the
   gitignore is present. Effort M.
2. **Engine: EXIF/GPS strip and orientation** for JPEG/PNG/HEIC, plus tests on
   fixture photos that carry GPS. Effort S.
3. **Engine: retention.** Remove the dir on stop or remove (not hibernate),
   reusing the WI-1750 hook. Archived chats lose theirs, and the assistant dir
   is swept after 7 days. Effort S.
4. **PWA: shared `AttachmentTray` + `useAttachments` helper.** Covers the 📎
   sheet, picker and camera, chips, progress, paste and drop, wired into Chat,
   Assistant and Terminal (bracketed-paste insert, no submit). Vitest coverage
   for paste and drop routing and terminal insertion. Effort M.
5. **Chat: `localImage` items** in `chat_codex::turn_start` and the send
   request (`attachments: [id]`), plus transcript thumbnails. Effort S.
6. **Agent-CLI reference format** in `agent_cli.rs`: bare path (Claude, Codex)
   vs `@path` (opencode). Includes verifying opencode/Klaudia image handling
   and the assistant vision parts gated by model capability. Effort S.
7. **Android share target.** Covers the manifest intent filter, the Capacitor
   plugin that copies content URIs, and the PWA "Send to…" picker. It is
   verified on the remote emulator (sharing from the gallery and from a
   screenshot). Effort M.
8. *(Optional)* **MCP `session_attach`** wraps upload plus input for agents
   driving other sessions. Effort S.

Order: items 1–3 first (the engine is usable alone through the API). Item 4
follows, and items 5–7 can run in parallel after it. Item 8 is last.

## Open questions for the operator

- Is the 25 MiB per-file default right, or should it be larger, for example
  for log bundles?
- Should attachments of a session started without a worktree be kept after
  stop, so the user can look at them later?
- Is an agent token with `FilesystemWrite` allowed to attach to *another*
  session, or should attaching to another session be limited to people
  (WI-983 person gate)?
