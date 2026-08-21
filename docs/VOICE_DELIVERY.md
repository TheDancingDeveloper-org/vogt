# Voice assistant — delivery status and remaining scope

*2026-08-21. Companion to `VOICE_POC.md` and `REQUIREMENTS.md` r16/r18
(FR-T5–T14, FR-M4, FR-M6). This file records what is delivered, what is
enabled where, and what remains — each remainder tracked as a GitHub issue
under the tracker [#188](https://github.com/TheDancingDeveloper-org/vogt/issues/188).*

## 1. What is delivered

**Checkpoint A of the POC is built, tested and merged to `dev`**
(`VOICE_POC.md` §6 is the evidence, with per-piece test counts):

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

**Voice input today is mobile-only.** The microphone is gated on the
Capacitor `SpeechRecognition` native plugin; a desktop browser gets typed
input plus spoken replies (Web Speech synthesis). This is why the *website*
shows no mic even with the assistant enabled — the desktop STT path is
Checkpoint B, not a regression.

**[voicemode](https://github.com/mbailey/voicemode) is a pattern, not a
dependency.** FR-T12 adopts its architecture — STT/TTS behind
OpenAI-compatible audio endpoints so cloud and local Whisper.cpp + Kokoro
are interchangeable by configuration. None of that pipeline is built yet.

## 2. What is enabled where

Enabled on **vogt-dev (Node B)** on 2026-08-21. The Komodo stack environment
gained, per `VOICE_POC.md` §3.1 and the r20 key-destination rule:

| Variable | Value |
|---|---|
| `MYDEVENV2_ASSISTANT_API_KEY` | Infisical `apps/prod` `OpenRouter_Token` |
| `MYDEVENV2_ASSISTANT_BASE_URL` | `https://openrouter.ai/api/v1` |
| `MYDEVENV2_ASSISTANT_MODEL` | `openai/gpt-5.4-mini` |

OpenRouter is the chosen default provider. The stack was redeployed and
`https://vogt-dev.sprooty.com/api/config` answers `assistant_enabled: true`,
so the PWA renders the tab. Rollback is removing the three lines and
redeploying. Two caveats, both carried by
[#194](https://github.com/TheDancingDeveloper-org/vogt/issues/194): the
end-to-end GUI round trip is **not yet validated** (a CLI probe with the
engine token 401s — a capability question, not proof of breakage), and the
key is a shared token with a **$5 limit**.

## 3. What remains

| Issue | Scope | Design source | Blocked by |
|---|---|---|---|
| [#189](https://github.com/TheDancingDeveloper-org/vogt/issues/189) | Checkpoint B: desktop mic via Web Speech `webkitSpeechRecognition`, then the spoken five-utterance pass with findings into `VOICE_POC.md` §6 | `VOICE_POC.md` §3.4, §4–5; FR-T13 | — |
| [#190](https://github.com/TheDancingDeveloper-org/vogt/issues/190) | Checkpoint C: engine `POST /api/assistant/stt` / `tts` proxying OpenAI-compatible audio endpoints; MediaRecorder capture and `<audio>` playback; 404-and-fall-back when unconfigured | `VOICE_POC.md` §3.5; FR-T12; voicemode | — |
| [#191](https://github.com/TheDancingDeveloper-org/vogt/issues/191) | FR-M4: FCM client entry for the dev application id, so a dev APK installs beside prod and registers for push | REQUIREMENTS FR-M4, §7.2 | — |
| [#192](https://github.com/TheDancingDeveloper-org/vogt/issues/192) | Checkpoint D: foreground service for active conversations, speak-the-push, the battery number, and the phone half of FR-T13 (first hardware validation of mobile voice at all) | `VOICE_POC.md` §3.6; FR-M6 | #191 |
| [#193](https://github.com/TheDancingDeveloper-org/vogt/issues/193) | FR-T14: durable, attributable, both-directions interaction log — today only a capped in-memory transcript in `engine/server/src/assistant.rs` | REQUIREMENTS FR-T14 (r18) | — |
| [#194](https://github.com/TheDancingDeveloper-org/vogt/issues/194) | Ops: validate the enabled assistant end-to-end in the GUI; dedicated/raised-limit key as its own Infisical secret; declare the env facts | this file §2 | — |

The only hard edge is #191 → #192. Everything else is independently
pickable. The POC's exit criteria (`VOICE_POC.md` §5) close when #189 and
#192 both land their findings; r17 of the requirements then either closes
FR-T13 or reopens FR-T2 with the measured tap counts.
