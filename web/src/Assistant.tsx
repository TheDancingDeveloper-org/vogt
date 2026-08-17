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
  type AssistantPendingAction,
  type AssistantReply,
  type AssistantSendInputAction,
  type AssistantTranscriptEntry,
  type AssistantVogtWriteAction,
} from "./api";

const TTS_PREF_KEY = "mydevenv2.assistant.tts";

interface AssistantProps {
  onError: (message: string) => void;
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
  const [transcript, setTranscript] = createSignal<AssistantTranscriptEntry[]>([]);
  const [pending, setPending] = createSignal<AssistantPendingAction | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [reasonDraft, setReasonDraft] = createSignal("");
  const [reasonBusy, setReasonBusy] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [ttsOn, setTtsOn] = createSignal(localStorage.getItem(TTS_PREF_KEY) === "1");
  const [listening, setListening] = createSignal(false);
  const [sttAvailable, setSttAvailable] = createSignal(false);

  let scroller: HTMLDivElement | undefined;
  let inputEl: HTMLInputElement | undefined;

  const scrollToEnd = () => {
    queueMicrotask(() => {
      if (scroller) scroller.scrollTop = scroller.scrollHeight;
    });
  };

  createEffect(() => {
    transcript();
    pending();
    scrollToEnd();
  });

  createEffect(() => {
    const action = pending();
    setReasonDraft(action?.kind === "vogt_write" ? action.reason : "");
  });

  onMount(async () => {
    try {
      const history = await api.assistantHistory();
      setTranscript(history.transcript);
      setPending(history.pending_action ?? null);
    } catch (e) {
      props.onError(`assistant history: ${String(e)}`);
    }
    if (Capacitor.isPluginAvailable("SpeechRecognition")) {
      try {
        const { SpeechRecognition } = await import(
          "@capacitor-community/speech-recognition"
        );
        const { available } = await SpeechRecognition.available();
        setSttAvailable(available);
      } catch {
        setSttAvailable(false);
      }
    }
  });

  onCleanup(() => {
    stopSpeaking();
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
      if (ttsOn()) speakSentences(reply.reply);
    }
    setPending(reply.pending_action ?? null);
    if (reply.pending_action && ttsOn()) {
      speakSentences(announce(reply.pending_action));
    }
  };

  const send = async (text: string) => {
    const trimmed = text.trim();
    if (!trimmed || busy()) return;
    setDraft("");
    setBusy(true);
    stopSpeaking();
    setTranscript((cur) => [...cur, { role: "user", text: trimmed }]);
    try {
      applyReply(await api.assistantMessage(trimmed));
    } catch (e) {
      props.onError(`assistant: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const resolve = async (approve: boolean) => {
    const action = pending();
    if (!action || busy()) return;
    setBusy(true);
    setPending(null);
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
      setPending(await api.assistantReplaceReason(action.id, reason));
    } catch (e) {
      props.onError(`assistant reason: ${String(e)}`);
    } finally {
      setReasonBusy(false);
    }
  };

  const reset = async () => {
    stopSpeaking();
    try {
      await api.assistantReset();
      setTranscript([]);
      setPending(null);
    } catch (e) {
      props.onError(`assistant reset: ${String(e)}`);
    }
  };

  const toggleTts = () => {
    const next = !ttsOn();
    setTtsOn(next);
    localStorage.setItem(TTS_PREF_KEY, next ? "1" : "0");
    if (!next) stopSpeaking();
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

  const closeRecognizer = async () => {
    setListening(false);
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
    // Sent here rather than from the recognizer's own "stopped" event,
    // because releasing the button removes that listener and the release is
    // now the ordinary way a take ends. Under the toggle this lived in the
    // listener and worked because the recognizer usually stopped itself.
    const text = draft().trim();
    if (text) void send(text);
  };

  /** End the take and send nothing — for leaving the surface mid-sentence. */
  const abandonTake = async () => {
    takeOpen = false;
    await closeRecognizer();
  };

  // Push-to-talk, and it is held rather than toggled for the reason the name
  // says (FR-T5). A toggle in a room with other people leaves a microphone
  // open until somebody remembers it is open, and the recognizer auto-sends
  // whatever it settled on — so a forgotten toggle does not merely listen, it
  // speaks. Holding makes the open microphone exactly as long as the
  // deliberate act. `docs/ENGINE.md` §6 has called this push-to-talk since
  // before it was.
  const startListening = async () => {
    if (listening()) return;
    try {
      const { SpeechRecognition } = await import(
        "@capacitor-community/speech-recognition"
      );
      const perm = await SpeechRecognition.requestPermissions();
      if (perm.speechRecognition !== "granted") {
        props.onError("microphone permission denied");
        return;
      }
      stopSpeaking();
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
        <Show when={transcript().length === 0 && !pending()}>
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
        <Show when={pending()}>
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
