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
  type AssistantTranscriptEntry,
} from "./api";

const TTS_PREF_KEY = "mydevenv2.assistant.tts";

interface AssistantProps {
  onError: (message: string) => void;
}

function speakSentences(text: string) {
  if (!("speechSynthesis" in window) || !text.trim()) return;
  // Sentence-level chunks keep the synth responsive and interruptible.
  const sentences = text.match(/[^.!?\n]+[.!?]?/g) ?? [text];
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

export default function Assistant(props: AssistantProps) {
  const [transcript, setTranscript] = createSignal<AssistantTranscriptEntry[]>([]);
  const [pending, setPending] = createSignal<AssistantPendingAction | null>(null);
  const [busy, setBusy] = createSignal(false);
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
    if (listening()) void stopListening();
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
      speakSentences(
        `I'd like to type into session ${reply.pending_action.session_name}. Approve on screen.`,
      );
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

  const stopListening = async () => {
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

  const startListening = async () => {
    if (listening()) {
      await stopListening();
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
      stopSpeaking();
      setListening(true);
      await SpeechRecognition.removeAllListeners();
      await SpeechRecognition.addListener("partialResults", (data) => {
        const best = data.matches?.[0];
        if (best) setDraft(best);
      });
      await SpeechRecognition.addListener("listeningState", (data) => {
        if (data.status === "stopped") {
          setListening(false);
          // Auto-send whatever the recognizer settled on.
          const text = draft().trim();
          if (text) void send(text);
        }
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
            “is any agent stuck?”, “answer yes to the prompt in session two”.
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
              <div style={{ "font-size": "13px" }}>
                Type into <strong>{action().session_name}</strong>
                {action().submit ? " and press Enter" : ""}:
              </div>
              <code
                style={{
                  "font-family": "monospace",
                  "white-space": "pre-wrap",
                  "word-break": "break-all",
                  padding: "6px 8px",
                  background: "rgba(0,0,0,0.25)",
                  "border-radius": "6px",
                }}
              >
                {visibleText(action().text)}
              </code>
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
          placeholder={listening() ? "listening…" : "Ask about your sessions"}
          style={{ flex: 1 }}
          onInput={(e) => setDraft(e.currentTarget.value)}
        />
        <Show when={sttAvailable()}>
          <button
            type="button"
            title={listening() ? "Stop listening" : "Speak"}
            style={listening() ? { background: "#da3633", color: "#fff" } : {}}
            onClick={() => void startListening()}
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
