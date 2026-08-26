# Voice assistant — delivery status and remaining scope

*2026-08-26. Companion to [`VOICE_POC.md`](VOICE_POC.md) (the POC's design
and findings) and [`ENGINE.md`](ENGINE.md) §6 (the assistant's reference,
including **Configuring the assistant provider**). The `FR-Txx` / `FR-Mxx`
ids are stable requirement identifiers; each rule they label is stated in
words. This file records what is delivered, how to enable it, and what
remains — each remainder tracked as a GitHub issue under the tracker
[#188](https://github.com/TheDancingDeveloper-org/vogt/issues/188).*

## 1. What is delivered

**Checkpoint A of the POC is built and tested** (`VOICE_POC.md` §6 is the
evidence, with per-piece test counts):

- Provider profiles (FR-T9): named OpenAI-compatible `{base_url, key, model,
  effort}` sets — `engine/server/src/{config,assistant,assistant_api,gui}.rs`.
- `notifications` tool over the curated `inbox.list` projection (FR-T10), with
  coverage spoken as part of the answer.
- `model` / `effort` on `session.start` with registry parity, the scratch
  project for subject-less requests, and argv validation of model ids
  (FR-T11) — core `models.py`/`sessions.py`, migration `0010`,
  `engine/server/src/agent_cli.rs`.
- The vocabulary repair pass (FR-T13's middle): `web/src/voiceRepair.ts`,
  single-word fuzzy slug match + `WI-\d+` repair, shown before send.
- The Assistant surface: `web/src/Assistant.tsx`, a tab the shell shows only
  when `/api/config` says `assistant_enabled` (FR-T6).
- The FR-T2 gate holds against spoken approval; both mutating journeys cost
  exactly one tap.

**Delivered since** (the implementation work is complete; the remaining
external gate is the real-device coexistence test tracked by #191):

- Desktop microphone via Web Speech where the browser has it (#189).
- Server-side STT/TTS (FR-T12, #190): `POST /api/assistant/stt` / `tts`
  proxied to OpenAI-compatible audio endpoints —
  `engine/server/src/assistant_speech.rs`.
- The durable, attributable interaction log (FR-T14, #193) —
  `engine/server/src/assistant_log.rs`.
- The phone's foreground service and speak-the-push (FR-M6, #192). The
  implementation is delivered; the remaining device validation is the
  dev/prod FCM coexistence test tracked by #191.

**Voice input without any of that is mobile-only.** The on-device microphone
is the Capacitor `SpeechRecognition` plugin; a desktop browser without Web
Speech and without a configured server STT gets typed input plus spoken
replies (Web Speech synthesis).

**[voicemode](https://github.com/mbailey/voicemode) is a pattern, not a
dependency.** FR-T12 adopts its architecture — STT/TTS behind
OpenAI-compatible audio endpoints so cloud and local Whisper.cpp + Kokoro
are interchangeable by configuration.

## 2. Enabling it

The assistant is off until a chat provider is configured, and the PWA hides
its tab until then. Any OpenAI-compatible chat endpoint works; the three
settings that turn it on are:

| Variable | Example |
|---|---|
| `ENGINE_ASSISTANT_BASE_URL` | `https://api.openai.com/v1`, or a local server such as `http://127.0.0.1:11434/v1` |
| `ENGINE_ASSISTANT_API_KEY` | the provider's key (any non-empty value for a local server that needs none) |
| `ENGINE_ASSISTANT_MODEL` | a model id the endpoint serves, e.g. `gpt-5.4-mini` |

Server-side speech is separate and also off by default:
`ENGINE_ASSISTANT_STT_BASE_URLS` / `ENGINE_ASSISTANT_TTS_BASE_URLS` name
ordered lists of OpenAI-compatible audio endpoints (e.g. a local whisper/TTS
server first, a hosted one as fallback), with `_MODEL`, `_VOICE` and `_API_KEY`
beside them. The full table — every variable, its TOML key, default and
semantics, plus named provider profiles — is
[`ENGINE.md` §6, "Configuring the assistant provider"](ENGINE.md#configuring-the-assistant-provider).
Legacy `MYDEVENV2_*` names are accepted as aliases.

Rollback is removing the variables and restarting. Two caveats a deployment
should expect, both carried by
[#194](https://github.com/TheDancingDeveloper-org/vogt/issues/194): a probe
with the *engine's* primary token can `401` on `/api/assistant/*` — that is a
capability question (the `assistant` capability), not proof of breakage — and
a shared provider key with a spend limit should be replaced with a dedicated
one before anyone relies on it.

## 3. What remains

Only [#191](https://github.com/TheDancingDeveloper-org/vogt/issues/191) is
still open in this delivery tracker. It requires an operator to install the
`com.sprooty.vogt.dev` APK beside `com.sprooty.vogt`, confirm both FCM
registrations, and verify that environment-specific pushes reach the intended
installation. The implementation issues #189, #190, #192, #193 and #194 are
closed; their code and validation records remain linked below for context.

| Issue | Scope | Design source | Status |
|---|---|---|---|
| [#189](https://github.com/TheDancingDeveloper-org/vogt/issues/189) | Checkpoint B: desktop mic via Web Speech `webkitSpeechRecognition`, then the spoken five-utterance pass with findings into `VOICE_POC.md` §6 | `VOICE_POC.md` §3.4, §4–5; FR-T13 | Closed |
| [#190](https://github.com/TheDancingDeveloper-org/vogt/issues/190) | Checkpoint C: engine `POST /api/assistant/stt` / `tts` proxying OpenAI-compatible audio endpoints; MediaRecorder capture and `<audio>` playback; 404-and-fall-back when unconfigured | `VOICE_POC.md` §3.5; FR-T12; voicemode | Closed |
| [#191](https://github.com/TheDancingDeveloper-org/vogt/issues/191) | FR-M4: dev/prod package identity, Firebase config handling, side-by-side installation, registration, and correctly routed pushes | FR-M4 | **Open — external device test** |
| [#192](https://github.com/TheDancingDeveloper-org/vogt/issues/192) | Checkpoint D implementation: foreground service for active conversations and speak-the-push | `VOICE_POC.md` §3.6; FR-M6 | Closed |
| [#193](https://github.com/TheDancingDeveloper-org/vogt/issues/193) | FR-T14: durable, attributable, both-directions interaction log | FR-T14 (r18) | Closed |
| [#194](https://github.com/TheDancingDeveloper-org/vogt/issues/194) | Ops: validate the enabled assistant end-to-end in the GUI | this file §2 | Closed |

The remaining edge is the #191 device gate. Once its coexistence and routing
evidence is recorded, #188's residual voice-delivery gate can close. The
closed issues above are not reopened for additional implementation work; any
regression found during the device pass should be filed against the affected
surface.
