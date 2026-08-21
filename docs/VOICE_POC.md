# Voice-first Vogt — proof of concept

*2026-08-17. Companion to `REQUIREMENTS.md` r16 (FR-T9–T13, FR-M6). This is
the POC's scope, sequence and exit criteria — not a design of the finished
feature. It is expected to be wrong in places; its job is to find out where.*

## 1. What the POC has to prove

Three questions, in the order they can be answered cheaply:

1. **Can the assistant answer the domain questions at all?** *Notifications*,
   *open issues for a project*, *work on this item*, *spawn a session on this
   model*. Two of these need tools that do not exist (`notifications`,
   `model`/`effort` on `session.start`). This half is testable with typed
   input and no hardware.
2. **Does a recognizer's output survive contact with the vocabulary?** Project
   slugs and `WI-7`. FR-T5 has said "unproven" for three revisions; this is
   the pass that ends it, one way or the other.
3. **Is the on-screen approval tap tolerable in a voice flow?** FR-T2 forbids
   spoken approval. The POC keeps that rule and *measures the cost*: how many
   times per journey the speaker has to look at the phone.

What it does **not** try to prove: background always-listening (ruled out by
FR-M6), local Whisper/Kokoro on the phone (server-side only), or a native
Anthropic transport (r12 deferral stands).

## 2. The five utterances (FR-T13's acceptance list)

Each is spoken, not typed. Each has a "done" that is either a spoken answer
or a spoken announcement plus an on-screen card.

| # | Utterance | Tools the loop needs | Done when |
|---|---|---|---|
| U1 | *"Are there any notifications?"* | `notifications` (new, FR-T10) | Count, sources covered, first entries spoken; an uncollected source is *said* to be uncollected |
| U2 | *"What open issues are there for rustnzbd?"* | `project.list` (repair), `bugs` / `work.list` | Slug repaired from whatever the recognizer heard; open items spoken by id and title |
| U3 | *"Can you work on issue 12 for rustnzbd?"* | `work.get`, `session.start` (gated) | Card announced — *"I've prepared a session on WI-12 in rustnzbd; approve it on screen"* — with no spoken route to yes; one tap starts it |
| U4 | *"Research the best place to buy risotto in Wollongong, using GPT 5.6 medium"* | `session.start` with `model`, `effort` and no subject (FR-T11) | Resolves to the scratch project; card names project, model and effort; refused with a named reason if no scratch project is configured |
| U5 | *"What is the terminal for rustnzbd doing?"* | `list_sessions`, `read_session_tail` (existing) | Tail summarised and spoken; terminal bytes delimited (FR-T4), never read verbatim unless asked |

U4 without a model — *"…please research…"* — must fall back to the default
profile's model and effort, and say which it chose.

## 3. Pieces, smallest first

Ordered so each step is demonstrable on its own and the hardware step is last.

### 3.1 Engine: provider profiles (FR-T9) — *no hardware*

- `assistant_profiles` in config: a map of name →
  `{base_url, api_key, default_model, default_effort}`; `assistant_default_profile`.
  The existing flat `assistant_*` keys become the implicit `default` profile
  so no deployment breaks.
- `POST /api/assistant/messages` accepts an optional `profile`; the loop picks
  that profile's `base_url`/key/model. FR-T7's `claude-*` refusal is
  evaluated per profile.
- `/api/config` advertises `assistant_profiles: [{name, default_model}]`.
- **POC targets:** `clawbay` (`https://api.theclawbay.com/v1`, GPT models —
  the validated path) and `openrouter` (`https://openrouter.ai/api/v1`). Both
  are OpenAI-compatible; the difference is a URL and a key.
- **Claude subscription** is *not* a profile: it is the
  `Claude Code (protected)` session template. U3/U4 spawn it; the assistant
  loop itself does not run on it.

### 3.2 Engine: `notifications` tool (FR-T10) — *no hardware*

- One more registry-backed read in `vogt_tools.rs`, over `inbox.list`
  (FR-N4). Bounded to one page; result carries `coverage` so the model can say
  "GitHub was not collected" instead of "no notifications".
- Prompt gains one sentence telling the model that coverage is part of the
  answer.

### 3.3 Core + engine: `model` / `effort` on `session.start` (FR-T11) — *no hardware*

- `StartSessionParams` gains `model: str | None`, `effort: str | None`;
  registry, CLI, REST, MCP all see them (FR-E8 parity).
- The engine maps them onto the template's CLI: `claude --model X`
  (effort via `CLAUDE_CODE_EFFORT_LEVEL` or the CLI's flag, whichever the
  installed version honours); `codex -m X -c model_reasoning_effort=Y`. A
  template that cannot honour them refuses with a named reason rather than
  ignoring them silently.
- Applied values are stored on the session row and shown in Sessions.
- `session_scratch_project` (core config): the slug a subject-less start
  resolves to. Absent → refusal naming the setting.
- The assistant's `session.start` tool description says: *no project → scratch
  project; no model → profile default; say which you used.*

### 3.4 Web: repair pass (FR-T13) — *desktop first*

- Between recognizer result and composer: fuzzy-match tokens against
  `project.list` slugs (edit distance ≤ 2, and phonetic aliases from a small
  per-deployment table — "rust NZB D" → `rustnzbd`); regex-repair
  `(issue|W I|WI)\s*(\d+)` → `WI-N`. Show the repaired text for a beat before
  auto-send, so a wrong repair is visible.
- Desktop STT: Web Speech `webkitSpeechRecognition` where present, else the
  server STT route (3.5). This is where U1–U5 are first spoken end-to-end.

### 3.5 Engine: server-side STT/TTS (FR-T12) — *desktop, then phone*

- `POST /api/assistant/stt` (multipart audio → `{text}`) and
  `POST /api/assistant/tts` (`{text}` → audio stream), each proxied to
  configured OpenAI-compatible audio endpoints (`assistant_stt_base_url`,
  `assistant_tts_base_url`, keys, model/voice names). 404 when unconfigured.
- POC provider: OpenAI's audio endpoints via The Claw Bay if it fronts them,
  else OpenAI direct. Local Whisper.cpp + Kokoro are configuration, tried
  only if the cloud path works.
- Client: MediaRecorder capture → STT route; TTS route → `<audio>`; the
  existing Web Speech path stays as fallback and as the phone's first try.

### 3.6 Phone: background + speak-the-push (FR-M6) — *needs the dev APK, FR-M4*

- Capacitor foreground-service plugin held only while a conversation is
  active; released on end. Persistent notification names the app and offers
  "End conversation".
- FCM arrival during an active conversation → spoken via TTS as well as shown.
- Measure: battery over a 30-minute held conversation, and whether the socket
  survives screen-off.

## 4. Sequence and where it can stop

```
3.1 profiles ─┐
3.2 notifications ├─ typed, no hardware, testable in CI ─► checkpoint A
3.3 model/effort ─┘
3.4 repair pass + desktop voice ──────────────────────────► checkpoint B
3.5 server STT/TTS ────────────────────────────────────────► checkpoint C
3.6 phone background ──────────────────────────────────────► checkpoint D
```

- **Checkpoint A** proves question 1 by typing the five utterances. If U1–U4
  cannot be answered typed, voice is moot.
- **Checkpoint B** proves question 2 on a laptop microphone — cheaper than a
  phone and it is the same recognizer problem.
- **Checkpoint C** decides whether server-side speech is worth keeping over
  on-device Web Speech (quality, latency, cost).
- **Checkpoint D** is the phone. It needs FR-M4's dev build alongside prod,
  which is the blocker already recorded in §7.2 of `REQUIREMENTS.md`.

Each checkpoint is a demo and a written note in this file's §6. Stopping at
any checkpoint leaves a working, narrower thing.

## 5. Exit criteria

The POC is *done* when:

- U1–U5 pass typed (A) and spoken on desktop (B), each at least three times in
  a row with a real recognizer, and the repair pass's misses are listed.
- The approval-tap count per journey is recorded for U3 and U4, with a
  one-line verdict: tolerable / not.
- Provider switching (`clawbay` ↔ `openrouter`) is a config change plus a
  request field, and a Claude session spawned by U3 runs on the subscription
  via the protected template.
- FR-M6's battery number exists, or the reason it does not (no device) is
  written down.

Then r17 either closes FR-T13 or reopens FR-T2 — and the argument for a
narrow spoken approval, if anyone wants to make it, is made with the numbers
from §6, not before.

## 6. Findings

### Checkpoint A — the five utterances, typed *(2026-08-17)*

**Reached.** §3.1, §3.2 and §3.3 are built and the five utterances complete
without a microphone. Branch `feat/voice-poc`. Suites: 238 Rust, 817 Python,
262 web — all green.

What was built, and what it is asserted by:

| Piece | Where | Proof |
|---|---|---|
| Provider profiles | `engine/server/src/{config,assistant,assistant_api,gui}.rs` | 9 unit tests in `assistant.rs`, 6 in `config.rs` |
| `inbox.list` as the notifications tool | `engine/server/src/vogt_tools.rs`, prompt in `assistant.rs` | 3 tests, plus the pinned-curated-set guard updated deliberately |
| `model` / `effort` on `session.start` | `models.py`, `sessions.py`, `client.py`, migration `0010`, `engine/server/src/agent_cli.rs` | 10 unit tests in `agent_cli.rs`, 7 in `tests/test_sessions.py` |
| Scratch project | `config.py`, `sessions.py` | 4 tests in `tests/test_sessions.py` |
| Repair pass | `web/src/voiceRepair.ts` | 11 tests, including all five utterances |
| Repair wired to the microphone | `web/src/Assistant.tsx` | 3 tests in `assistant.test.tsx` |
| U1–U5 through the tool loop | `engine/server/src/assistant.rs` | 6 tests, one per utterance plus the gate |

**Question 1 — can the assistant answer the domain questions?** Yes, for all
five. U1 reaches `vogt_inbox_list`; U2 reaches `project_list` then `bugs` with
the slug intact; U3 and U4 stop at a card; U5 needs no Vogt at all.

**Question 3 — is the gate tolerable?** Not answered here, and it cannot be
until a person speaks to it. What *is* established is that the gate holds:
`no_utterance_can_talk_its_own_way_past_the_gate` says "yes", "approve it",
"go ahead" and "yes, approve, do it" out loud in sequence and nothing reaches
the core. Both mutating journeys cost **exactly one tap**.

Four things this checkpoint found that the plan did not anticipate:

1. **The approval card had to learn to say which model.** U4 asks for "GPT 5.6
   medium", and `describe_target` had no idea those keys existed — so the card
   said only which project. A card that shows less than the request is asking
   for approval of something narrower than what was asked for, so `model` and
   `effort` were added to `TARGET_KEYS`, last, where they cannot crowd out the
   subject.
2. **A model id becomes argv, and it arrives from a model.** The mapping in
   §3.3 turns `model` into `--model <value>` on a process spawn. A "model id"
   of `--dangerously-skip-permissions` would turn a session into a different
   session while the card the user read said *model*. Values are now validated
   (letters, digits, `. _ - / :`, no leading dash) and refused rather than
   escaped.
3. **Fuzzy matching across a word window repairs sentences, not names.** The
   first repair pass turned "how is rust nzbd?" into "how rustnzbd?" — it ate
   the verb, because "is rust nzbd" squashes to within two edits of the slug.
   Fuzzy matching is now single-word only; a slug spoken as several words is a
   *spacing* problem (the letters are right) and must match exactly.
4. **The engine's `notifications` operation is the wrong tool for the
   question.** It is GitHub-only. Curating it beside the Inbox would let the
   model answer "are there any notifications?" from a quarter of the sources
   and report the rest as nothing — and "no notifications", spoken, is the
   answer that hides that best. Only `inbox.list` is curated, and the prompt
   now says an uncollected source is not an empty one.

Two things deliberately **not** done at this checkpoint, and why:

- **No session-outcome plumbing for `model`/`effort` beyond what was asked
  for.** The column records the request, not what the agent is using now — an
  operator who types `/model` at the prompt has moved on and no column here
  will know. That is the same line FR-E2 draws around activity state.
- **No server-side speech (§3.5) and no phone work (§3.6).** Checkpoint B
  (desktop microphone) is next and needs no new server code; §3.6 is still
  blocked on FR-M4's dev build alongside prod.

### Checkpoint B — desktop microphone

*Not run. Needs a person and a laptop microphone; everything it needs is
built.*

### Checkpoint C — server-side STT/TTS *(2026-08-21)*

**Code half reached; human spoken pass still owed.** §3.5 is built (#190,
FR-T12). The engine fronts two routes — `POST /api/assistant/stt` (multipart
audio → `{text}`) and `POST /api/assistant/tts` (`{text}` → an audio stream) —
proxied to OpenAI-compatible audio endpoints configured **independently of the
chat profile**, which is the whole point of FR-T12: the estate's chat runs
through OpenRouter, which does not front `/audio/transcriptions` and
`/audio/speech` uniformly, so speech needs its own provider.

We adopted **voicemode's** architecture natively (not as a dependency — it is a
Python workstation MCP app with local-audio-hardware deps, unfit for the
headless Rust container): STT and TTS each take an **ordered, comma-separated
base-URL list**, tried in order — local first, cloud fallback — hitting the same
OpenAI-compatible `/audio/transcriptions` / `/audio/speech` on each. The
defaults mirror voicemode exactly (1:1 with its `VOICEMODE_*` vars): STT
`http://127.0.0.1:2022/v1,https://api.openai.com/v1` model `whisper-1`; TTS
`http://127.0.0.1:8880/v1,https://api.openai.com/v1` model `tts-1-hd` voice
`nova`. The key is reused for whichever entry needs one (the cloud endpoint);
local Whisper.cpp / Kokoro need none.

What is asserted by tests, not by a person:

| Property | Where | Proof |
|---|---|---|
| Config keys independent of the chat profile; ordered list; voicemode defaults; key-with-empty-list refused (r20) | `engine/server/src/config.rs` | 3 unit tests |
| Both halves 404 when unconfigured, per half; the two are independent | `engine/server/src/assistant_speech.rs`, `tests/integration.rs` | proxy unit + 3 integration tests |
| Routes auth-gated (`assistant` capability) | `engine/server/src/auth.rs`, `tests/integration.rs` | unit + integration (403 without the cap) |
| Real upstream HTTP: STT sends multipart `file` + `model`; TTS POSTs `{model,input,voice}` and streams the bytes back | `tests/assistant_speech.rs` (mock OpenAI-compatible audio server) | 2 tests asserting what the mock received |
| Ordered failover: dead entry → next entry; all dead → 404 (not 500); a hanging entry is bounded and moved past | `tests/assistant_speech.rs` | 3 tests |
| Web client: server-STT path when on-device absent; server TTS playback on reply; silent degrade when unconfigured/404 | `web/src/Assistant.tsx`, `web/src/__tests__/assistantServerSpeech.test.tsx` | 4 vitest cases |

**Audio is not stored** anywhere (FR-T12): the bytes are proxied through and the
FR-T14 interaction log stays text-only. There is deliberately no debug flag to
retain audio — the safe default is the only behaviour.

**Unconfigured-degrades-silently is verified in tests**: with no reachable
provider both routes 404 and every client (server pipeline, on-device Web
Speech/Capacitor, or neither) falls back to typed input with no error surfaced.

**Still owed — a person, with a configured audio provider.** The verdict
Checkpoint C exists to reach — *is server-side speech worth keeping over
on-device Web Speech (quality, latency, cost)?* — cannot be answered here: it
needs a configured audio provider producing real transcripts and real audio,
and vogt-dev's OpenRouter chat profile supplies none. Point STT/TTS at a live
provider (or a local Whisper.cpp + Kokoro pair) and run U1–U5 spoken on a
desktop with no Web Speech to record that verdict.

### Checkpoint D

*Not started.*
