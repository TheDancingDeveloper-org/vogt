// Conversational assistant tab: transcript + composer, optional voice.
//
// Voice support is progressive: TTS uses Web Speech `speechSynthesis`
// (available in browsers and the Android WebView); STT uses the
// @capacitor-community/speech-recognition native plugin and therefore only
// appears inside the Capacitor APK. Everything degrades to typed input.
import {
  createEffect,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from "solid-js";
import { Capacitor } from "@capacitor/core";

import {
  api,
  ApiError,
  type AssistantPendingAction,
  type AssistantReply,
  type AssistantSendInputAction,
  type AssistantTranscriptEntry,
  type AssistantVogtWriteAction,
} from "./api";
import { listProjects } from "./vogtApi";
import { describeRepairs, repairUtterance } from "./voiceRepair";
import { readToolDraft, writeToolDraft } from "./toolDrafts";
import { pendingAction, setPendingAction } from "./pendingAction";
import {
  onVoiceServiceEnded,
  registerPushSpeaker,
  startVoiceService,
  stopVoiceService,
} from "./voiceService";

const TTS_PREF_KEY = "mydevenv2.assistant.tts";

// The Web Speech recognizer, read defensively. TypeScript's DOM lib does not
// declare the vendor-prefixed `webkitSpeechRecognition`, and Firefox ships
// neither name — so both are looked up off `window` and the absent case is a
// first-class "no microphone here", not an error (FR-T13, VOICE_POC §3.4).
interface WebSpeechAlternative {
  readonly transcript: string;
}
interface WebSpeechResult {
  readonly length: number;
  readonly [index: number]: WebSpeechAlternative;
}
interface WebSpeechResultList {
  readonly length: number;
  readonly [index: number]: WebSpeechResult;
}
interface WebSpeechRecognitionEvent {
  readonly results: WebSpeechResultList;
}
interface WebSpeechRecognition {
  continuous: boolean;
  interimResults: boolean;
  lang: string;
  onresult: ((event: WebSpeechRecognitionEvent) => void) | null;
  onend: (() => void) | null;
  onerror: ((event: { error: string }) => void) | null;
  start(): void;
  stop(): void;
  abort(): void;
}
type WebSpeechRecognitionCtor = new () => WebSpeechRecognition;

/** The recognizer constructor this browser offers, or null (e.g. Firefox). */
function webSpeechCtor(): WebSpeechRecognitionCtor | null {
  const scope = window as unknown as {
    webkitSpeechRecognition?: WebSpeechRecognitionCtor;
    SpeechRecognition?: WebSpeechRecognitionCtor;
  };
  return scope.webkitSpeechRecognition ?? scope.SpeechRecognition ?? null;
}

/**
 * Whether this browser can capture microphone audio for the server pipeline
 * (FR-T12): a `MediaRecorder` to encode it and `getUserMedia` to open the mic.
 * Both absent — an older WebView, a browser with no recorder — leaves the
 * server STT path unusable, which degrades to typed input (FR-T6).
 */
function mediaRecorderAvailable(): boolean {
  return (
    typeof MediaRecorder !== "undefined" &&
    typeof navigator !== "undefined" &&
    !!navigator.mediaDevices?.getUserMedia
  );
}

interface AssistantDraft {
  text: string;
  profile: string;
}

interface AssistantProps {
  onError: (message: string) => void;
  /** Sessions renders the shared action card when composed in its workspace. */
  pendingHosted?: boolean;
}

function speakSentences(text: string) {
  if (!("speechSynthesis" in window) || !text.trim()) return;
  // Sentence-level chunks keep the synth responsive and interruptible.
  //
  // Split only where a terminator is followed by space or line end, so an
  // identifier keeps its full stop: the old pattern broke `work.transition`
  // into "work." and "transition on WI-7", which a screen reader renders as
  // two sentences and a speaker reads with a pause in the middle of the one
  // word that says what is about to happen.
  const sentences = text.split(/(?<=[.!?])\s+|\n+/).filter((part) => part.trim());
  for (const sentence of sentences) {
    const trimmed = sentence.trim();
    if (!trimmed) continue;
    window.speechSynthesis.speak(new SpeechSynthesisUtterance(trimmed));
  }
}

function stopSpeaking() {
  if ("speechSynthesis" in window) window.speechSynthesis.cancel();
}

/** Escape control characters so the confirm card shows exactly what will be typed. */
function visibleText(text: string): string {
  return text.replace(/[\x00-\x1f\x7f]/g, (c) => {
    if (c === "\n") return "\\n";
    if (c === "\r") return "\\r";
    if (c === "\t") return "\\t";
    return `\\x${c.charCodeAt(0).toString(16).padStart(2, "0")}`;
  });
}

/** Narrowings for the card, so each branch renders its own fields only. */
function asSendInput(
  action: AssistantPendingAction | null | undefined,
): AssistantSendInputAction | undefined {
  return action?.kind === "send_input" ? action : undefined;
}

function asVogtWrite(
  action: AssistantPendingAction | null | undefined,
): AssistantVogtWriteAction | undefined {
  return action?.kind === "vogt_write" ? action : undefined;
}

/**
 * The exact payload, whichever effector it is for. `display: block` and a
 * scroll ceiling because a Vogt payload is several lines and must not push the
 * approve button off screen — an approval you have to hunt for is worse than
 * one you can read.
 */
const payloadStyle = {
  display: "block",
  "font-family": "monospace",
  "white-space": "pre-wrap",
  "word-break": "break-all",
  padding: "6px 8px",
  background: "rgba(0,0,0,0.25)",
  "border-radius": "6px",
  "max-height": "220px",
  "overflow-y": "auto",
} as const;

/**
 * What the assistant is asking permission for, spoken.
 *
 * Says what will happen and to what, and never offers a way to answer it:
 * approval is an on-screen act, so a misheard "yes" authorises nothing
 * (FR-T2).
 */
function announce(action: AssistantPendingAction): string {
  return action.kind === "send_input"
    ? `I'd like to type into session ${action.session_name}. Approve on screen.`
    : `I'd like to make a Vogt change: ${action.operation} on ${action.target}. Approve on screen.`;
}

export default function Assistant(props: AssistantProps) {
  const restored = readToolDraft<AssistantDraft>("assistant", {
    text: "",
    profile: "",
  });
  const [transcript, setTranscript] = createSignal<AssistantTranscriptEntry[]>([]);
  const [busy, setBusy] = createSignal(false);
  const [reasonDraft, setReasonDraft] = createSignal("");
  const [reasonBusy, setReasonBusy] = createSignal(false);
  const [draft, setDraft] = createSignal(restored.text);
  const [ttsOn, setTtsOn] = createSignal(localStorage.getItem(TTS_PREF_KEY) === "1");
  const [listening, setListening] = createSignal(false);
  const [sttAvailable, setSttAvailable] = createSignal(false);
  // Whether the server-side speech pipeline (FR-T12) is configured. Read from
  // `/api/config`, so a client with no on-device recognizer/synthesis picks the
  // server path by capability rather than by provoking a 404. Either can go
  // false at runtime if the route later 404s — a config that changed under a
  // long-lived tab — after which that half degrades silently (FR-T6).
  const [serverSttEnabled, setServerSttEnabled] = createSignal(false);
  const [serverTtsEnabled, setServerTtsEnabled] = createSignal(false);
  // The vocabulary the repair pass matches against (FR-T13). Fetched from
  // `project.list`, not hard-coded: a repair against a list in this file
  // would be a guess wearing a validation pass's clothes. Empty is a working
  // state — the pass repairs work-item refs and leaves names alone.
  const [slugs, setSlugs] = createSignal<string[]>([]);
  const [repaired, setRepaired] = createSignal("");
  // Which backend this conversation runs on (FR-T9). Empty means the
  // deployment's default, which is what a client that never chose sends.
  const [profiles, setProfiles] = createSignal<
    { name: string; model: string; default: boolean }[]
  >([]);
  const [profile, setProfile] = createSignal(restored.profile);

  let scroller: HTMLDivElement | undefined;
  let inputEl: HTMLInputElement | undefined;
  // The server-TTS clip currently playing, if any, so it can be stopped the
  // moment the speaker sends again or leaves (FR-T12). On-device synthesis is
  // stopped through `speechSynthesis.cancel()`; this is its `<audio>` twin.
  let currentAudio: HTMLAudioElement | null = null;
  // Native (Android) voice-conversation plumbing (FR-M6). Both stay undefined
  // on the desktop PWA, where their registrars are no-ops. Cleaned up on leave.
  let voiceEndedCleanup: (() => void) | undefined;
  let pushSpeakerCleanup: (() => void) | undefined;

  /** Stop every speech channel — on-device synthesis and a server clip alike. */
  const haltSpeech = () => {
    stopSpeaking();
    if (currentAudio) {
      try {
        currentAudio.pause();
      } catch {
        /* already stopped */
      }
      currentAudio = null;
    }
  };

  /**
   * Speak a reply, choosing by capability (FR-T12): the browser's own
   * synthesis when it has any, else the server pipeline for a client that has
   * none. A server route that 404s (unconfigured) degrades this half silently
   * — a spoken reply that cannot be spoken is still shown in the transcript.
   */
  const speak = (text: string) => {
    if ("speechSynthesis" in window) {
      speakSentences(text);
      return;
    }
    if (serverTtsEnabled()) void playServerTts(text);
  };

  const playServerTts = async (text: string) => {
    if (!text.trim()) return;
    try {
      const blob = await api.assistantTts(text);
      const url = URL.createObjectURL(blob);
      const audio = new Audio(url);
      audio.addEventListener("ended", () => URL.revokeObjectURL(url), { once: true });
      currentAudio = audio;
      await audio.play();
    } catch (e) {
      // Unconfigured (404): degrade this half silently, exactly as an absent
      // recognizer does for STT (FR-T6). Any other failure is worth surfacing.
      if (e instanceof ApiError && e.status === 404) {
        setServerTtsEnabled(false);
      } else {
        props.onError(`assistant speech: ${String(e)}`);
      }
    }
  };

  const scrollToEnd = () => {
    queueMicrotask(() => {
      if (scroller) scroller.scrollTop = scroller.scrollHeight;
    });
  };

  createEffect(() => {
    transcript();
    pendingAction();
    scrollToEnd();
  });

  createEffect(() => {
    const action = pendingAction();
    setReasonDraft(action?.kind === "vogt_write" ? action.reason : "");
  });

  // A voice conversation being *active* is, on this surface, spoken replies
  // being on: that is the toggle that turns a typed chat into a hands-free one
  // where audio capture and TTS have to survive the phone sleeping. So while it
  // is on, hold the Android foreground service (FR-M6); release it the moment
  // it goes off. Both calls are no-ops on the desktop PWA, so this effect is
  // invisible there — desktop voice is unaffected.
  createEffect(() => {
    if (ttsOn()) startVoiceService();
    else stopVoiceService();
  });

  // The notification's "End conversation" action stops the service natively and
  // dispatches this back to us, so the conversation ends on both sides at once.
  const endFromNotification = () => {
    if (!ttsOn()) return;
    setTtsOn(false);
    localStorage.setItem(TTS_PREF_KEY, "0");
    haltSpeech();
  };

  onMount(async () => {
    voiceEndedCleanup = onVoiceServiceEnded(endFromNotification);
    // Speak-the-push (FR-M6 / FR-M2): an FCM message that arrives while a voice
    // conversation is active is spoken as well as shown. Outside an active
    // conversation the gate is false, so nothing is spoken and FR-M2's shown/
    // handled behaviour is exactly as before. No-op off a native platform.
    pushSpeakerCleanup = await registerPushSpeaker((text) => {
      if (ttsOn()) speak(text);
    });
    try {
      const history = await api.assistantHistory();
      setTranscript(history.transcript);
      setPendingAction(history.pending_action ?? null);
    } catch (e) {
      props.onError(`assistant history: ${String(e)}`);
    }
    try {
      const cfg = await api.publicConfig();
      setProfiles(cfg.assistant_profiles ?? []);
      setServerSttEnabled(cfg.assistant_stt_enabled ?? false);
      setServerTtsEnabled(cfg.assistant_tts_enabled ?? false);
    } catch {
      // No profile list means no choice to offer, not a broken assistant:
      // every request then runs on the deployment's default. Server speech
      // stays off, which is the safe absent state.
    }
    try {
      const listed = await listProjects();
      setSlugs(listed.projects.map((project) => project.slug));
    } catch {
      // A core that cannot be asked costs slug repair and nothing else: the
      // composer, the work-item repair and typed input all keep working.
    }
    // STT backend, in preference order: the Capacitor native plugin inside the
    // APK, then the browser's Web Speech recognizer on the desktop (FR-T13,
    // VOICE_POC §3.4), then the server-side pipeline (FR-T12, §3.5) for a
    // client with neither — a desktop without Web Speech that can still capture
    // audio and let the engine transcribe it. Absent all three — no recognizer,
    // no server route, no MediaRecorder — is a working state that degrades to
    // typed input with no error (FR-T6).
    if (Capacitor.isPluginAvailable("SpeechRecognition")) {
      try {
        const { SpeechRecognition } = await import(
          "@capacitor-community/speech-recognition"
        );
        const { available } = await SpeechRecognition.available();
        if (available) {
          sttBackend = "native";
          setSttAvailable(true);
        }
      } catch {
        setSttAvailable(false);
      }
    } else if (webSpeechCtor()) {
      sttBackend = "web";
      setSttAvailable(true);
    } else if (serverSttEnabled() && mediaRecorderAvailable()) {
      sttBackend = "server";
      setSttAvailable(true);
    }
  });

  onCleanup(() => {
    writeToolDraft<AssistantDraft>("assistant", {
      text: draft(),
      profile: profile(),
    });
    haltSpeech();
    // Leaving the surface ends the conversation, so release the held service
    // and drop the native listeners (all no-ops on the desktop PWA).
    stopVoiceService();
    voiceEndedCleanup?.();
    pushSpeakerCleanup?.();
    // Abandoned, not sent: leaving the surface mid-sentence must not put
    // half an utterance into the conversation on the way out.
    if (listening()) void abandonTake();
  });

  const applyReply = (reply: AssistantReply) => {
    if (reply.reply !== null && reply.reply !== undefined) {
      setTranscript((cur) => [
        ...cur,
        { role: "assistant", text: reply.reply ?? "", tool_trace: reply.tool_trace },
      ]);
      if (ttsOn()) speak(reply.reply);
    }
    setPendingAction(reply.pending_action ?? null);
    if (reply.pending_action && ttsOn()) {
      speak(announce(reply.pending_action));
    }
  };

  const send = async (text: string) => {
    const trimmed = text.trim();
    if (!trimmed || busy()) return;
    setDraft("");
    setBusy(true);
    haltSpeech();
    setTranscript((cur) => [...cur, { role: "user", text: trimmed }]);
    try {
      applyReply(await api.assistantMessage(trimmed, profile() || undefined));
    } catch (e) {
      props.onError(`assistant: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const resolve = async (approve: boolean) => {
    const action = pendingAction();
    if (!action || busy()) return;
    setBusy(true);
    setPendingAction(null);
    try {
      applyReply(await api.assistantAction(action.id, approve));
    } catch (e) {
      props.onError(`assistant action: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const replaceReason = async (action: AssistantVogtWriteAction) => {
    const reason = reasonDraft().trim();
    if (!reason || reasonBusy() || busy()) return;
    setReasonBusy(true);
    try {
      setPendingAction(await api.assistantReplaceReason(action.id, reason));
    } catch (e) {
      props.onError(`assistant reason: ${String(e)}`);
    } finally {
      setReasonBusy(false);
    }
  };

  const reset = async () => {
    haltSpeech();
    try {
      await api.assistantReset();
      setTranscript([]);
      setPendingAction(null);
    } catch (e) {
      props.onError(`assistant reset: ${String(e)}`);
    }
  };

  const toggleTts = () => {
    const next = !ttsOn();
    setTtsOn(next);
    localStorage.setItem(TTS_PREF_KEY, next ? "1" : "0");
    if (!next) haltSpeech();
    else {
      // Prime the synth inside a user gesture — Android WebView requires it.
      window.speechSynthesis?.speak(new SpeechSynthesisUtterance(""));
    }
  };

  // One take: from the press that opens the microphone to the release, or to
  // the recognizer stopping first because the speaker went quiet. Both ends
  // can arrive, and either can arrive first, so ending a take is idempotent —
  // a double send is one thing said once and answered twice.
  let takeOpen = false;
  // Which recognizer this session decided on (onMount), and the live Web
  // Speech instance when that is the one. Native has no handle to hold — the
  // plugin is a module singleton. `server` is the FR-T12 pipeline: capture with
  // `MediaRecorder`, transcribe on the engine.
  let sttBackend: "native" | "web" | "server" | null = null;
  let webRecognition: WebSpeechRecognition | null = null;
  // Server-STT capture state. The take is a single recording: press starts it,
  // release stops it, and the recorder's `stop` posts the audio and sends what
  // came back. `abandonServer` lets leaving the surface mid-take drop the audio
  // instead of sending it.
  let serverRecorder: MediaRecorder | null = null;
  let serverChunks: Blob[] = [];
  let abandonServer = false;

  const closeRecognizer = async () => {
    setListening(false);
    if (sttBackend === "web") {
      const recognition = webRecognition;
      webRecognition = null;
      try {
        recognition?.stop();
      } catch {
        /* already stopped */
      }
      return;
    }
    if (sttBackend === "server") {
      // `stop()` flushes the last chunk and fires `onstop`, which is where the
      // audio is posted (unless the take was abandoned). The tracks are stopped
      // there too, so the browser's recording indicator clears.
      const recorder = serverRecorder;
      serverRecorder = null;
      try {
        if (recorder && recorder.state !== "inactive") recorder.stop();
      } catch {
        /* already stopped */
      }
      return;
    }
    try {
      const { SpeechRecognition } = await import(
        "@capacitor-community/speech-recognition"
      );
      await SpeechRecognition.stop();
      await SpeechRecognition.removeAllListeners();
    } catch {
      /* plugin gone mid-flight — nothing to stop */
    }
  };

  /** End the take and send what was said. */
  const stopListening = async () => {
    if (!takeOpen) return;
    takeOpen = false;
    await closeRecognizer();
    // The server pipeline sends from the recorder's `onstop` once the audio has
    // been transcribed, not from the draft — there is nothing in the composer
    // to send yet — so this path ends here for it.
    if (sttBackend === "server") return;
    // Sent here rather than from the recognizer's own "stopped" event,
    // because releasing the button removes that listener and the release is
    // now the ordinary way a take ends. Under the toggle this lived in the
    // listener and worked because the recognizer usually stopped itself.
    const heard = draft().trim();
    if (!heard) return;
    // FR-T13: the recognizer's best guess is put back into the vocabulary the
    // domain is made of before it is sent. `WI-7` and a project slug are
    // exactly what a recognizer mangles, and they are the subject of the
    // sentence — an utterance that survives everything except its subject is
    // a failed utterance.
    const { text, repairs } = repairUtterance(heard, slugs());
    // Shown, not silently applied: a wrong repair is confidently wrong and is
    // what gets sent, so the speaker gets to see it in the transcript.
    setRepaired(repairs.length ? describeRepairs(repairs) : "");
    setDraft(text);
    void send(text);
  };

  /** End the take and send nothing — for leaving the surface mid-sentence. */
  const abandonTake = async () => {
    takeOpen = false;
    // Tell the server recorder's `onstop` to drop the audio rather than post it.
    abandonServer = true;
    await closeRecognizer();
  };

  // Push-to-talk, and it is held rather than toggled for the reason the name
  // says (FR-T5). A toggle in a room with other people leaves a microphone
  // open until somebody remembers it is open, and the recognizer auto-sends
  // whatever it settled on — so a forgotten toggle does not merely listen, it
  // speaks. Holding makes the open microphone exactly as long as the
  // deliberate act. `docs/ENGINE.md` §6 has called this push-to-talk since
  // before it was.
  // The desktop take, on the browser's own recognizer. The browser asks for
  // the microphone itself (no plugin permission call), the take is held rather
  // than toggled exactly as on the phone, and going quiet fires `onend`, which
  // is the same "recognizer stopped first" end the native `listeningState`
  // handler covers — routed through the one `stopListening` so what was said is
  // sent once (FR-T5).
  const startListeningWeb = () => {
    const Ctor = webSpeechCtor();
    if (!Ctor) return;
    try {
      haltSpeech();
      const recognition = new Ctor();
      recognition.continuous = true;
      recognition.interimResults = true;
      recognition.lang = navigator.language || "en-US";
      recognition.onresult = (event) => {
        let heard = "";
        for (let i = 0; i < event.results.length; i += 1) {
          heard += event.results[i]?.[0]?.transcript ?? "";
        }
        const trimmed = heard.trim();
        if (trimmed) setDraft(trimmed);
      };
      recognition.onend = () => {
        if (takeOpen) void stopListening();
      };
      recognition.onerror = (event) => {
        // A take that simply heard nothing, or was aborted by the release, is
        // an ordinary end — not something to surface as a failure.
        if (event.error !== "no-speech" && event.error !== "aborted") {
          setListening(false);
          props.onError(`speech recognition: ${event.error}`);
        }
      };
      webRecognition = recognition;
      takeOpen = true;
      setListening(true);
      recognition.start();
    } catch (e) {
      setListening(false);
      props.onError(`speech recognition: ${String(e)}`);
    }
  };

  // The desktop take on the server pipeline (FR-T12): capture audio with
  // `MediaRecorder`, and on release post it to the engine's STT route. Held
  // rather than toggled, exactly as the other two paths (FR-T5) — the release
  // is what ends the recording and triggers the transcription-and-send.
  const startListeningServer = async () => {
    try {
      haltSpeech();
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const recorder = new MediaRecorder(stream);
      serverChunks = [];
      abandonServer = false;
      recorder.addEventListener("dataavailable", (event) => {
        if (event.data && event.data.size > 0) serverChunks.push(event.data);
      });
      recorder.addEventListener("stop", () => {
        // Release the microphone so the browser's recording indicator clears.
        stream.getTracks().forEach((track) => track.stop());
        const chunks = serverChunks;
        serverChunks = [];
        if (abandonServer) return;
        const blob = new Blob(chunks, { type: recorder.mimeType || "audio/webm" });
        void transcribeAndSend(blob);
      });
      serverRecorder = recorder;
      takeOpen = true;
      setListening(true);
      recorder.start();
    } catch (e) {
      setListening(false);
      props.onError(`microphone: ${String(e)}`);
    }
  };

  // The server round-trip: audio up, text back, then the same repair pass and
  // send the on-device paths use (FR-T13). A 404 means the route is
  // unconfigured, and the take degrades to typed input with no error (FR-T6).
  const transcribeAndSend = async (blob: Blob) => {
    try {
      const { text } = await api.assistantStt(blob);
      const heard = text.trim();
      if (!heard) return;
      const { text: repairedText, repairs } = repairUtterance(heard, slugs());
      setRepaired(repairs.length ? describeRepairs(repairs) : "");
      setDraft(repairedText);
      void send(repairedText);
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) {
        // Unconfigured: retire the server mic and fall back to typed input,
        // silently — the same degradation an absent recognizer gives.
        setSttAvailable(false);
        sttBackend = null;
      } else {
        props.onError(`speech transcription: ${String(e)}`);
      }
    }
  };

  const startListening = async () => {
    if (listening()) return;
    if (sttBackend === "web") {
      startListeningWeb();
      return;
    }
    if (sttBackend === "server") {
      await startListeningServer();
      return;
    }
    try {
      const { SpeechRecognition } = await import(
        "@capacitor-community/speech-recognition"
      );
      const perm = await SpeechRecognition.requestPermissions();
      if (perm.speechRecognition !== "granted") {
        props.onError("microphone permission denied");
        return;
      }
      haltSpeech();
      takeOpen = true;
      setListening(true);
      await SpeechRecognition.removeAllListeners();
      await SpeechRecognition.addListener("partialResults", (data) => {
        const best = data.matches?.[0];
        if (best) setDraft(best);
      });
      await SpeechRecognition.addListener("listeningState", (data) => {
        // The other end of the take: the recognizer gave up before the
        // button was released, usually because the speaker went quiet. Same
        // path, so what was said is sent once and only once.
        if (data.status === "stopped") void stopListening();
      });
      await SpeechRecognition.start({
        partialResults: true,
        popup: false,
      });
    } catch (e) {
      setListening(false);
      props.onError(`speech recognition: ${String(e)}`);
    }
  };

  return (
    <div
      style={{
        display: "flex",
        "flex-direction": "column",
        flex: 1,
        "min-height": 0,
        padding: "12px",
        gap: "10px",
      }}
    >
      <div style={{ display: "flex", "align-items": "center", gap: "8px" }}>
        <strong style={{ flex: 1 }}>Assistant</strong>
        {/*
          Offered only when there is a choice to make (FR-T9). One configured
          profile is not a decision, and a select with one option is a control
          that asks a question with no answer.
        */}
        <Show when={profiles().length > 1}>
          <select
            data-testid="assistant-profile"
            aria-label="Provider profile"
            value={profile()}
            onChange={(e) => setProfile(e.currentTarget.value)}
          >
            <option value="">
              {`default (${profiles().find((p) => p.default)?.model ?? ""})`}
            </option>
            <For each={profiles()}>
              {(entry) => (
                <option value={entry.name}>{`${entry.name} · ${entry.model}`}</option>
              )}
            </For>
          </select>
        </Show>
        <button
          type="button"
          title={ttsOn() ? "Disable spoken replies" : "Speak replies aloud"}
          onClick={toggleTts}
        >
          {ttsOn() ? "🔊" : "🔇"}
        </button>
        <button type="button" title="Clear the conversation" onClick={() => void reset()}>
          Clear
        </button>
      </div>

      <div
        ref={scroller}
        style={{
          flex: 1,
          "overflow-y": "auto",
          display: "flex",
          "flex-direction": "column",
          gap: "10px",
          padding: "4px",
        }}
      >
        <Show when={transcript().length === 0 && !pendingAction()}>
          <div style={{ opacity: 0.6, "font-size": "13px" }}>
            Ask about your terminal sessions — “what is the build doing?”,
            “is any agent stuck?”, “answer yes to the prompt in session two” —
            or about your work: “what's the top bug?”, “why is WI-7 ranked
            there?”, “start a session on it”.
          </div>
        </Show>
        <For each={transcript()}>
          {(entry) => (
            <div
              style={{
                "align-self": entry.role === "user" ? "flex-end" : "flex-start",
                "max-width": "85%",
                display: "flex",
                "flex-direction": "column",
                gap: "4px",
              }}
            >
              <Show when={entry.role === "assistant" && entry.tool_trace?.length}>
                <div style={{ "font-size": "11px", opacity: 0.55 }}>
                  <For each={entry.tool_trace}>
                    {(step) => <div>· {step}</div>}
                  </For>
                </div>
              </Show>
              <div
                style={{
                  padding: "8px 12px",
                  "border-radius": "12px",
                  background:
                    entry.role === "user" ? "#1f6feb" : "rgba(110, 118, 129, 0.25)",
                  color: entry.role === "user" ? "#fff" : "inherit",
                  "white-space": "pre-wrap",
                  "word-break": "break-word",
                  "font-size": "14px",
                }}
              >
                {entry.text}
              </div>
            </div>
          )}
        </For>
        <Show when={!props.pendingHosted && pendingAction()}>
          {(action) => (
            <div
              style={{
                border: "1px solid #d29922",
                "border-radius": "10px",
                padding: "10px 12px",
                display: "flex",
                "flex-direction": "column",
                gap: "8px",
                background: "rgba(210, 153, 34, 0.08)",
              }}
            >
              <Show when={asSendInput(action())}>
                {(send) => (
                  <>
                    <div style={{ "font-size": "13px" }}>
                      Type into <strong>{send().session_name}</strong>
                      {send().submit ? " and press Enter" : ""}:
                    </div>
                    <code style={payloadStyle}>{visibleText(send().text)}</code>
                  </>
                )}
              </Show>
              <Show when={asVogtWrite(action())}>
                {(write) => (
                  <>
                    <div style={{ "font-size": "13px" }}>
                      Write to Vogt: <strong>{write().operation}</strong> on{" "}
                      <strong>{write().target}</strong>
                    </div>
                    {/*
                      The reason gets its own line above the payload because
                      Vogt stores it and someone reads it back later — it is
                      the part of this card that outlives the approval.
                    */}
                    <div style={{ "font-size": "13px" }}>
                      Recorded reason (editable before approval):
                    </div>
                    <textarea
                      aria-label="Assistant Vogt write reason"
                      value={reasonDraft()}
                      rows={2}
                      disabled={reasonBusy() || busy()}
                      onInput={(event) => setReasonDraft(event.currentTarget.value)}
                    />
                    <button
                      type="button"
                      disabled={reasonBusy() || busy() || !reasonDraft().trim()}
                      onClick={() => void replaceReason(write())}
                    >
                      {reasonBusy() ? "Updating reason…" : "Update reason for review"}
                    </button>
                    <code style={payloadStyle}>{write().payload}</code>
                  </>
                )}
              </Show>
              <div style={{ display: "flex", gap: "8px" }}>
                <button type="button" disabled={busy()} onClick={() => void resolve(true)}>
                  ✓ Approve
                </button>
                <button type="button" disabled={busy()} onClick={() => void resolve(false)}>
                  ✗ Deny
                </button>
              </div>
            </div>
          )}
        </Show>
        <Show when={busy()}>
          <div style={{ opacity: 0.6, "font-size": "13px" }}>thinking…</div>
        </Show>
      </div>

      {/*
        What the repair pass changed on the way from the microphone to the
        composer (FR-T13). Shown because a repair that nobody can see is
        indistinguishable from a recognizer that heard correctly — and when it
        is wrong, this line is the only thing that says why the answer is
        about the wrong item.
      */}
      <Show when={repaired()}>
        <div
          data-testid="voice-repair"
          style={{ opacity: 0.7, "font-size": "12px" }}
        >
          heard differently: {repaired()}
        </div>
      </Show>

      <form
        style={{ display: "flex", gap: "8px" }}
        onSubmit={(e) => {
          e.preventDefault();
          void send(draft());
        }}
      >
        <input
          ref={inputEl}
          type="text"
          value={draft()}
          disabled={busy()}
          placeholder={listening() ? "listening…" : "Ask about sessions or work"}
          style={{ flex: 1 }}
          onInput={(e) => setDraft(e.currentTarget.value)}
        />
        <Show when={sttAvailable()}>
          <button
            type="button"
            data-testid="mic"
            data-listening={listening() ? "yes" : "no"}
            title={listening() ? "Release to send" : "Hold to speak"}
            aria-label="Hold to speak"
            style={{
              // A held button must not also be a drag handle or a scroll
              // start: on a phone the gesture that opens the microphone is
              // the same one that scrolls the transcript.
              "touch-action": "none",
              ...(listening() ? { background: "#da3633", color: "#fff" } : {}),
            }}
            onPointerDown={(e) => {
              // Capture, so releasing off the button still ends the take. A
              // finger that slides while speaking is ordinary; a microphone
              // that stays open because of it is not.
              e.currentTarget.setPointerCapture?.(e.pointerId);
              void startListening();
            }}
            onPointerUp={() => void stopListening()}
            onPointerCancel={() => void stopListening()}
            onLostPointerCapture={() => void stopListening()}
            onKeyDown={(e) => {
              // Hold-to-talk is a pointer idiom, and a button that only
              // answers to pointers is a button some people cannot use. Space
              // and Enter hold; the repeat guard is what stops the key's own
              // auto-repeat from restarting the recognizer every 30ms.
              if ((e.key === " " || e.key === "Enter") && !e.repeat) {
                e.preventDefault();
                void startListening();
              }
            }}
            onKeyUp={(e) => {
              if (e.key === " " || e.key === "Enter") {
                e.preventDefault();
                void stopListening();
              }
            }}
          >
            🎙
          </button>
        </Show>
        <button type="submit" disabled={busy() || !draft().trim()}>
          Send
        </button>
      </form>
    </div>
  );
}
