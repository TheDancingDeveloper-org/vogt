# Conversational Assistant

The assistant is a server-side supervisor with read access to every terminal
session and confirmation-gated keystroke injection, designed to be driven by
voice from the mobile app (on-device STT in, `speechSynthesis` out) or by
typed messages from any browser.

## Architecture

- `engine/server/src/assistant.rs` — runtime: in-memory conversation, OpenAI-compatible
  tool-use loop against the configured backend, tool dispatch, pending-action
  gate.
- `engine/server/src/assistant_api.rs` — HTTP surface (see `docs/engine/API_CONTRACT.md`).
- `web/src/Assistant.tsx` — PWA tab: transcript, composer, mic (APK only),
  TTS toggle, approve/deny cards.

The runtime only exists when `assistant_api_key` is configured; otherwise the
routes 404 and the PWA hides the tab (`assistant_enabled` in `GET /api/config`).

## Configuration

| Key (TOML) | Env | Default |
|---|---|---|
| `assistant_api_key` | `MYDEVENV2_ASSISTANT_API_KEY` | unset (feature off) |
| `assistant_base_url` | `MYDEVENV2_ASSISTANT_BASE_URL` | `https://api.theclawbay.com/v1` |
| `assistant_model` | `MYDEVENV2_ASSISTANT_MODEL` | `gpt-5.4-mini` |
| `assistant_auto_type` | `MYDEVENV2_ASSISTANT_AUTO_TYPE` | `false` |
| `assistant_max_tool_calls` | `MYDEVENV2_ASSISTANT_MAX_TOOL_CALLS` | `8` |
| `assistant_reasoning_effort` | `MYDEVENV2_ASSISTANT_REASONING_EFFORT` | unset |

The backend must be OpenAI-compatible (`POST {base_url}/chat/completions`
with `tools` / `tool_calls`). Notes from validation against The Claw Bay
(August 2026): GPT models respond quickly with correct tool calls; the
`claude-*` proxy routes hung and should be avoided until understood.

## Tools

| Tool | Effect |
|---|---|
| `list_sessions` | id, name, command, activity state, exit code, cwd, created_at for every session |
| `read_session_tail` | last N bytes of a session's scrollback (default 4 KiB, max 16 KiB), ANSI-stripped by default |
| `send_input` | type text (max 4 KiB) into a session's PTY, optional Enter |

## Threat model

Extends the rule in `docs/engine/AGENT_TASKS.md`: external content must never become
instructions.

- **Terminal output is untrusted.** Sessions run arbitrary programs, including
  other AI agents consuming untrusted web content. Anything those programs
  print can reach the assistant's context via `read_session_tail`. Tool
  results are wrapped in `<terminal-output>` delimiters and the system prompt
  tells the model to treat embedded instructions as data — but that is
  defense-in-depth, not the guarantee.
- **The guarantee is structural.** `send_input` is intercepted in the tool
  dispatcher: with `assistant_auto_type` off (the default), the loop pauses
  and returns a `PendingAction` carrying the exact bytes and target session.
  Nothing reaches a PTY until `POST /api/assistant/actions/:id` approves it.
  Actions expire after 120 s; one may be pending at a time; a new user
  message auto-denies it. No model output can bypass this, because the model
  never holds the write handle.
- **No other effectors.** The assistant has no file, network, git, or config
  tools. Its blast radius is: read scrollback, and (after approval) type into
  a PTY.
- **The UI never auto-approves by voice.** Approval is an on-screen tap so a
  misheard utterance can't authorize an injection.
- **Privacy:** `read_session_tail` output — which may include secrets printed
  in a terminal — is sent to the configured LLM provider and kept in the
  in-memory transcript (never persisted to disk; `POST /api/assistant/reset`
  or a restart clears it). Don't provision the assistant against a provider
  you wouldn't show your terminals to.
- **Token scoping:** mutating assistant routes require the `assistant`
  capability. A token holding it transitively gains type-into-any-session
  power *subject to on-screen approval*; `assistant_auto_type=true` removes
  that approval and should only be set where the token boundary is already
  trusted.

## Voice

- **STT** — `@capacitor-community/speech-recognition` (on-device Android
  recognizer), only inside the APK; browsers fall back to typing. Push-to-talk
  via the mic button, partial results land in the composer, final result
  auto-sends. `RECORD_AUDIO` is declared in the manifest; the plugin prompts
  at first use.
- **TTS** — Web Speech `speechSynthesis`, sentence-chunked, toggle persisted
  in localStorage. The synth is primed on the toggle gesture because the
  Android WebView requires a user gesture before the first utterance.
