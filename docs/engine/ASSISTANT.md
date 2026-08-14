# Conversational Assistant

The assistant is a server-side supervisor with read access to every terminal
session and to a curated read-only slice of Vogt, and confirmation-gated
effectors on both — keystroke injection into a PTY, and mutating Vogt
operations. It is designed to be driven by voice from the mobile app
(on-device STT in, `speechSynthesis` out) or by typed messages from any
browser.

## Architecture

- `engine/server/src/assistant.rs` — runtime: in-memory conversation, OpenAI-compatible
  tool-use loop against the configured backend, tool dispatch, pending-action
  gate.
- `engine/server/src/vogt_tools.rs` — the Vogt toolbox: `tools/list` fetched
  from vogt-core's MCP surface and converted to OpenAI function shape, the
  curated slice, credential resolution, `tools/call`, delimiting.
- `engine/server/src/assistant_api.rs` — HTTP surface (see `docs/engine/API_CONTRACT.md`).
- `web/src/Assistant.tsx` — PWA tab: transcript, composer, mic (APK only),
  TTS toggle, approve/deny cards.

The runtime only exists when `assistant_api_key` is configured; otherwise the
routes 404 and the PWA hides the tab (`assistant_enabled` in `GET /api/config`).
The Vogt half is independently absent: with no `vogt_core_url`, or with a core
that is not answering, the `vogt_*` tools are simply not offered that turn and
the terminal half works exactly as before (FR-T6, FR-E9).

## Configuration

| Key (TOML) | Env | Default |
|---|---|---|
| `assistant_api_key` | `MYDEVENV2_ASSISTANT_API_KEY` | unset (feature off) |
| `assistant_base_url` | `MYDEVENV2_ASSISTANT_BASE_URL` | `https://api.theclawbay.com/v1` |
| `assistant_model` | `MYDEVENV2_ASSISTANT_MODEL` | `gpt-5.4-mini` |
| `assistant_auto_type` | `MYDEVENV2_ASSISTANT_AUTO_TYPE` | `false` |
| `assistant_max_tool_calls` | `MYDEVENV2_ASSISTANT_MAX_TOOL_CALLS` | `8` |
| `assistant_reasoning_effort` | `MYDEVENV2_ASSISTANT_REASONING_EFFORT` | unset |

The Vogt half needs no assistant-specific key. It uses `vogt_core_url` — the
same core the front door proxies — and the `vogt_core_token_file` pairing on
each front-door token's `extra_tokens` entry. A deployment with only the
shared `vogt_core_token` gets Vogt *reads* in the assistant and a named
refusal on writes; pairing a token is how it opts that token in.

The backend must be OpenAI-compatible (`POST {base_url}/chat/completions`
with `tools` / `tool_calls`). Notes from validation against The Claw Bay
(August 2026): GPT models respond quickly with correct tool calls; the
`claude-*` proxy routes hung and should be avoided until understood.

## Tools

The engine's own three are literals in `assistant.rs` — they are this
process's surface onto its own PTYs:

| Tool | Effect |
|---|---|
| `list_sessions` | id, name, command, activity state, exit code, cwd, created_at for every session |
| `read_session_tail` | last N bytes of a session's scrollback (default 4 KiB, max 16 KiB), ANSI-stripped by default |
| `send_input` | type text (max 4 KiB) into a session's PTY, optional Enter |

### The Vogt tools are fetched, not written (FR-T1)

Vogt's operation registry generates its own MCP tool schemas, and the core
serves them at `/mcp`. At the start of every turn the assistant POSTs
`tools/list` to the core and converts each `Tool` into an OpenAI function:
an MCP `inputSchema` is already JSON Schema, so it is forwarded **verbatim**
rather than restated here. A hand-written copy would be correct exactly once,
and would then drift silently as the registry changed.

- **Naming.** `work.get` → MCP `work_get` → function `vogt_work_get`. The
  `vogt_` prefix keeps the engine's `list_sessions` and Vogt's `session_list`
  from ever being confused, by the model or by the dispatcher.
- **Curation.** The slice is named in `vogt_tools.rs` and intersected with
  what the core actually serves. Reads: `backlog`, `bugs`, `why`,
  `project.brief`, `project.list`, `work.get`, `work.list`, `compliance`.
  Writes: `work.create`, `work.transition`, `work.comment`, `session.start`.
  A curated name the core does not serve — a rename, or a scope this caller
  lacks — is logged at info and skipped. It is never fabricated: a fabricated
  schema is a tool call that fails at the far end for a reason nobody can read.
- **Which is a write is decided here, not there.** The gate must not depend on
  a remote answer, so `mutating` comes from the curated write set rather than
  from anything the core said.
- **Caching.** One entry per credential, keyed by a SHA-256 digest of the core
  token (never the token), expiring after 5 minutes — because `tools/list` is
  scope-filtered at the core and two tokens may legitimately see two different
  lists. `POST /api/assistant/reset` also clears the cache, which is how an
  operator who has just changed a token's scopes sees the effect without a
  restart.
- **A fetch failure is not an error.** No core configured, core down, or an
  unreadable answer, and the Vogt tools are absent for that turn. The
  assistant still watches terminals.
- **Results are capped** at 16 KiB per call and truncated with a marker.

### Which credential a Vogt call uses (FR-T3)

The assistant runs server-side but is always *called* by an authenticated
user. `require_bearer` leaves an `AuthorizedIdentity` in the request
extensions carrying the front-door token's name and the vogt-core token paired
with it (FR-S9); `assistant_api.rs` turns that into a `Caller` and hands it to
the runtime. There is no other credential in reach of the tool loop.

| | Credential | If the caller has no pairing |
|---|---|---|
| Read | the caller's pairing, else the deployment-wide `vogt_core_token` | falls back, because a read attributes nothing |
| Write | the **approving** caller's pairing, and nothing else | refused, with the token's name and what to configure |

The asymmetry is the whole of FR-T3. A write filed under a shared token names
the wrong actor in an audit row somebody reads months later, and a wrong answer
there is worse than a refusal a user can act on. The credential is taken from
the request that *approved* the action, not from the one that sent the message
that proposed it — when those differ, FR-T3 is about the second.

Front-door capabilities gate reaching the assistant at all (`assistant` on
every mutating assistant route). What a given write is *allowed* to do in Vogt
is enforced at the core against the approver's own core token (FR-S4), which
is the same check any other client of the core gets.

## Threat model

Extends the rule in `docs/engine/AGENT_TASKS.md`: external content must never become
instructions.

- **Terminal output is untrusted.** Sessions run arbitrary programs, including
  other AI agents consuming untrusted web content. Anything those programs
  print can reach the assistant's context via `read_session_tail`. Tool
  results are wrapped in `<terminal-output>` delimiters and the system prompt
  tells the model to treat embedded instructions as data — but that is
  defense-in-depth, not the guarantee.
- **Vogt content is untrusted too, by the same rule (FR-T4).** A work item's
  title and body are typed by people; an imported GitHub issue's body is typed
  by strangers on a forge; marker text and drift detail are quoted from files
  nobody reviewed for this purpose. Stored data is external content exactly as
  program output is, and it reaches the model's context the moment the
  assistant reads a backlog. So **every** Vogt result — reads, and the core's
  answer to an approved write — is wrapped in
  `<vogt-data operation="…">` delimiters, with the same framing in the system
  prompt: what is inside is data to report on, never instructions to follow.
  The rule is about where the text came from, not about which tool fetched it.
- **The guarantee is structural.** `send_input` is intercepted in the tool
  dispatcher: with `assistant_auto_type` off (the default), the loop pauses
  and returns a `PendingAction` carrying the exact bytes and target session.
  Nothing reaches a PTY until `POST /api/assistant/actions/:id` approves it.
  Actions expire after 120 s; one may be pending at a time; a new user
  message auto-denies it. No model output can bypass this, because the model
  never holds the write handle.
- **Vogt writes use the same gate, with no auto-type escape (FR-T2).** A
  mutating `vogt_*` tool is intercepted in the same place and pauses the same
  loop; the pending action carries the operation, a one-line target, the
  exact arguments pretty-printed, and the `reason` that will be written to
  Vogt's audit log. Nothing reaches the core until the action is approved.
  `assistant_auto_type` is a setting about typing into a terminal and
  deliberately does not extend to Vogt: there is no configuration that lets a
  model's output become a Vogt write unattended. A write proposed without a
  reason is refused before it becomes a card at all — a card that cannot say
  what will be recorded is not an approval anyone can meaningfully give — and
  the dispatcher refuses a mutating Vogt tool outright if it ever reaches it,
  so a future edit that loses the interception fails closed instead of
  writing.
- **No other effectors.** The assistant has no file, network, git, or config
  tools, and its Vogt reach is the curated slice — no `token.issue`, no
  `restore`, no `import`, whatever else the core may serve. Its blast radius
  is: read scrollback, read that slice of Vogt, and (after approval) type into
  a PTY or make one of four Vogt writes as the approving user.
- **The UI never auto-approves by voice.** Approval is an on-screen tap so a
  misheard utterance can't authorize an injection or a write. The spoken
  announcement says what is being asked and that it must be approved on
  screen; it offers no spoken way to answer.
- **Privacy:** `read_session_tail` output — which may include secrets printed
  in a terminal — and every Vogt read are sent to the configured LLM provider
  and kept in the in-memory transcript (never persisted to disk;
  `POST /api/assistant/reset` or a restart clears it). Don't provision the
  assistant against a provider you wouldn't show your terminals *and your
  work tracker* to.
- **Token scoping:** mutating assistant routes require the `assistant`
  capability. A token holding it transitively gains type-into-any-session
  power *subject to on-screen approval*; `assistant_auto_type=true` removes
  that approval and should only be set where the token boundary is already
  trusted. It does **not** transitively gain Vogt write power: that needs a
  paired core token whose own scopes the core enforces.

## What is not built here

- **FR-T5 (voice validation)** — the mic and the synth are as they were.
  Nothing in this stage put them through a domain-vocabulary pass against
  project names and words like "backlog", and "it has a mic" is not evidence.
  The system prompt now tells the model that work items are referred to as
  `WI-7` and projects by slug, which is what a recognizer's output will have
  to survive.
- **FR-T7 (provider portability)** — the loop is still OpenAI-compatible only,
  and the `claude-*` proxy-route hang recorded above is still unexplained. No
  native Anthropic backend exists yet, and no route is refused with a named
  reason.

## Voice

- **STT** — `@capacitor-community/speech-recognition` (on-device Android
  recognizer), only inside the APK; browsers fall back to typing. Push-to-talk
  via the mic button, partial results land in the composer, final result
  auto-sends. `RECORD_AUDIO` is declared in the manifest; the plugin prompts
  at first use.
- **TTS** — Web Speech `speechSynthesis`, sentence-chunked, toggle persisted
  in localStorage. The synth is primed on the toggle gesture because the
  Android WebView requires a user gesture before the first utterance.
