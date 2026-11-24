// The hands-free loop, proven without a recogniser, a synth, or a DOM.
//
// `voiceTurn.ts` is a pure state machine on purpose: every effect is a port,
// and time is an injected clock. So these tests drive the exact interleavings a
// device is worst at showing — a release mid-arming, a final result after the
// stop, an idle session, an approval that must never be spoken past — and read
// them off a list of port calls rather than off a microphone.

import { describe, expect, it } from "vitest";

import {
  VOICE_CONFIG_DEFAULTS,
  VoiceConversation,
  readVoiceConfig,
  voiceStatusLabel,
  type Scheduler,
  type VoiceConfig,
  type VoicePorts,
  type VoiceState,
} from "../voiceTurn";

/** A manual clock: timers fire only when the test advances time, earliest
 *  first, and a timer that schedules another is honoured in the same advance. */
function makeClock() {
  let now = 0;
  let nextId = 1;
  let timers: { id: number; fn: () => void; at: number }[] = [];
  const scheduler: Scheduler = {
    set(fn, ms) {
      const id = nextId++;
      timers.push({ id, fn, at: now + ms });
      return id;
    },
    clear(id) {
      timers = timers.filter((t) => t.id !== id);
    },
  };
  return {
    scheduler,
    advance(ms: number) {
      const target = now + ms;
      for (;;) {
        const due = timers
          .filter((t) => t.at <= target)
          .sort((a, b) => a.at - b.at)[0];
        if (!due) break;
        now = due.at;
        timers = timers.filter((t) => t.id !== due.id);
        due.fn();
      }
      now = target;
    },
  };
}

function makePorts() {
  const events: string[] = [];
  const states: VoiceState[] = [];
  const ports: VoicePorts = {
    openMic: () => events.push("openMic"),
    closeMic: () => events.push("closeMic"),
    sendTurn: (t) => events.push(`sendTurn:${t}`),
    speak: (t) => events.push(`speak:${t}`),
    stopSpeaking: () => events.push("stopSpeaking"),
    onChange: (s) => states.push(s),
    onEnded: (r) => events.push(`ended:${r}`),
    onRecognizerRestart: (n) => events.push(`restart:${n}`),
  };
  const count = (prefix: string) =>
    events.filter((e) => e === prefix || e.startsWith(`${prefix}:`)).length;
  return { ports, events, states, count };
}

function cfg(over: Partial<VoiceConfig> = {}): VoiceConfig {
  // The older cases assume an immediate re-open and no watchdog; the cases
  // for those two features set them explicitly.
  return { ...VOICE_CONFIG_DEFAULTS, reopen_delay_ms: 0, mic_watchdog_ms: 100_000, ...over };
}

/** Begin and reach `listening` — the common preamble. */
function live(over: Partial<VoiceConfig> = {}) {
  const clock = makeClock();
  const p = makePorts();
  const vc = new VoiceConversation(p.ports, cfg(over), clock.scheduler);
  vc.begin();
  vc.micReady();
  return { clock, vc, ...p };
}

describe("the hands-free conversation loop", () => {
  it("sends a turn once, when the speaker goes quiet", () => {
    const { clock, vc, events, count } = live({ silence_duration_ms: 1000 });
    vc.partial("what is on top");
    expect(vc.getState()).toBe("listening");
    clock.advance(1000);
    expect(events).toContain("sendTurn:what is on top");
    expect(count("sendTurn")).toBe(1);
    expect(events).toContain("closeMic"); // half-duplex: mic shut while sending
    expect(vc.getState()).toBe("sending");
  });

  it("speaks the reply, then re-opens the mic — the loop's whole point", () => {
    const { clock, vc, events, count } = live({ silence_duration_ms: 1000 });
    vc.partial("what is on top");
    clock.advance(1000);
    vc.replied({ text: "The forge adapter is on top.", hasPendingAction: false });
    expect(vc.getState()).toBe("speaking");
    expect(events).toContain("speak:The forge adapter is on top.");
    const opensBefore = count("openMic");
    vc.speechFinished();
    expect(vc.getState()).toBe("arming");
    expect(count("openMic")).toBe(opensBefore + 1);
    vc.micReady();
    expect(vc.getState()).toBe("listening");
  });

  it("does not tear down a recogniser that is still arming when the user ends", () => {
    const clock = makeClock();
    const { ports, events } = makePorts();
    const vc = new VoiceConversation(ports, cfg(), clock.scheduler);
    vc.begin(); // arming; openMic issued
    vc.end("user"); // lands before micReady
    expect(vc.getState()).toBe("arming"); // deferred, not ended
    expect(events).not.toContain("closeMic");
    vc.micReady(); // recogniser is up now — apply the end
    expect(vc.getState()).toBe("ended");
    expect(events).toContain("closeMic");
    expect(events).toContain("ended:user");
  });

  it("sends the final result that lands in the grace window after the recogniser stops", () => {
    const { clock, vc, events } = live({ final_result_grace_ms: 300, silence_duration_ms: 1000 });
    vc.partial("what is on");
    vc.recognizerStopped(); // endpointing; grace running
    vc.partial("what is on top"); // the late, best result
    expect(vc.getState()).toBe("endpointing");
    clock.advance(300);
    expect(events).toContain("sendTurn:what is on top");
  });

  it("ends the session after too many empty turns in a row", () => {
    const { clock, vc, events } = live({ max_empty_turns: 3, final_result_grace_ms: 100, max_turn_ms: 100_000 });
    for (let turn = 0; turn < 3; turn += 1) {
      vc.recognizerStopped(); // heard nothing
      clock.advance(100);
      if (turn < 2) vc.micReady(); // the loop re-armed the mic
    }
    expect(vc.getState()).toBe("ended");
    expect(events).toContain("ended:empty_turns");
  });

  it("ends the session when it has been idle too long", () => {
    const { clock, vc, events } = live({ idle_timeout_ms: 5000, max_turn_ms: 100_000 });
    clock.advance(5000); // no speech at all
    expect(vc.getState()).toBe("ended");
    expect(events).toContain("ended:idle");
  });

  it("pauses for an on-screen approval and never re-opens the mic to answer it", () => {
    const { clock, vc, count } = live({ silence_duration_ms: 1000 });
    vc.partial("move WI-7 to done");
    clock.advance(1000); // sending
    const opensBeforeAction = count("openMic");
    vc.replied({
      text: "I'd like to make a Vogt change: work.transition on WI-7. Approve on screen.",
      hasPendingAction: true,
    });
    expect(vc.getState()).toBe("paused_for_approval");
    // Announced, but the mic did not re-open — a misheard "yes" authorises nothing.
    expect(count("openMic")).toBe(opensBeforeAction);
    // The user approves on screen; the action's follow-up reply resumes the loop.
    vc.replied({ text: "Done.", hasPendingAction: false });
    expect(vc.getState()).toBe("speaking");
    vc.speechFinished();
    expect(vc.getState()).toBe("arming");
  });

  it("ignores barge-in while a reply plays when interrupt_response is off (the default)", () => {
    const { vc, clock, count } = live({ silence_duration_ms: 100 });
    vc.partial("hello");
    clock.advance(100);
    vc.replied({ text: "a long spoken reply", hasPendingAction: false });
    expect(vc.getState()).toBe("speaking");
    const opensBefore = count("openMic");
    vc.speechDetected();
    expect(vc.getState()).toBe("speaking"); // half-duplex: no interruption
    expect(count("openMic")).toBe(opensBefore);
  });

  it("barges in when interrupt_response is on: halts the reply and re-opens the mic", () => {
    const { vc, clock, events, count } = live({
      silence_duration_ms: 100,
      interrupt_response: true,
    });
    vc.partial("hello");
    clock.advance(100);
    vc.replied({ text: "a long spoken reply", hasPendingAction: false });
    expect(vc.getState()).toBe("speaking");
    const opensBefore = count("openMic");
    vc.speechDetected();
    expect(events).toContain("stopSpeaking");
    expect(vc.getState()).toBe("arming"); // re-opened to capture the interruption
    expect(count("openMic")).toBe(opensBefore + 1);
    // The halted reply's own speechFinished now arrives, and is ignored — we
    // have already left `speaking`.
    vc.speechFinished();
    expect(vc.getState()).toBe("arming");
  });

  it("does not barge in outside playback, even with interrupt_response on", () => {
    const { vc } = live({ interrupt_response: true });
    // In `listening`, a stray VAD onset must not disturb the turn.
    expect(vc.getState()).toBe("listening");
    vc.speechDetected();
    expect(vc.getState()).toBe("listening");
  });

  it("does not strand the session when muted while the mic is still arming", () => {
    // Mute lands between openMic and micReady. The recogniser must not be torn
    // down mid-startup; once it is up it is closed, and unmute re-arms exactly
    // one mic — the case that used to leave a muted-looking session with no
    // capture and no way back.
    const clock = makeClock();
    const { ports, events, count } = makePorts();
    const vc = new VoiceConversation(ports, cfg(), clock.scheduler);
    vc.begin(); // arming, one openMic
    vc.toggleMute(); // during arming
    expect(vc.isMuted()).toBe(true);
    expect(events).not.toContain("closeMic"); // nothing to close yet
    vc.micReady(); // mic is up now → closed, session sits muted in listening
    expect(events).toContain("closeMic");
    expect(vc.getState()).toBe("listening");
    vc.partial("ignored while muted");
    clock.advance(5000);
    expect(count("sendTurn")).toBe(0);
    vc.toggleMute(); // unmute → a fresh turn, one mic
    expect(vc.getState()).toBe("arming");
    expect(count("openMic")).toBe(2);
  });

  it("unmuting during arming opens the mic once, never twice", () => {
    const clock = makeClock();
    const { ports, count } = makePorts();
    const vc = new VoiceConversation(ports, cfg(), clock.scheduler);
    vc.begin(); // openMic #1 in flight
    vc.toggleMute();
    vc.toggleMute(); // unmute while that mic is still coming up
    expect(count("openMic")).toBe(1); // not a second start on a live plugin
    vc.micReady();
    expect(vc.getState()).toBe("listening");
    expect(vc.isMuted()).toBe(false);
  });

  it("waits the settle time before re-opening the mic behind a reply", () => {
    // Android can bring a recogniser up silently dead if capture starts the
    // instant the app's own playback stream closes. So a re-open behind a
    // reply waits `reopen_delay_ms` — and the chip may say Listening… a beat
    // early; that is fine.
    const { vc, clock, count } = live({ silence_duration_ms: 100, reopen_delay_ms: 400 });
    vc.partial("hello");
    clock.advance(100);
    vc.replied({ text: "a reply", hasPendingAction: false });
    const before = count("openMic");
    vc.speechFinished();
    expect(vc.getState()).toBe("arming");
    clock.advance(399);
    expect(count("openMic")).toBe(before);
    clock.advance(1);
    expect(count("openMic")).toBe(before + 1);
  });

  it("a mute during the settle wait cancels the re-open; unmute opens exactly once", () => {
    const { vc, clock, count } = live({ silence_duration_ms: 100, reopen_delay_ms: 400 });
    vc.partial("hello");
    clock.advance(100);
    vc.replied({ text: "a reply", hasPendingAction: false });
    vc.speechFinished(); // arming, re-open pending
    const before = count("openMic");
    vc.toggleMute();
    clock.advance(1000);
    expect(count("openMic")).toBe(before); // never opened while muted
    vc.toggleMute();
    expect(count("openMic")).toBe(before + 1);
  });

  it("ending during the settle wait tears down at once — no mic in flight to protect", () => {
    const { vc, clock, events, count } = live({ silence_duration_ms: 100, reopen_delay_ms: 400 });
    vc.partial("hello");
    clock.advance(100);
    vc.replied({ text: "a reply", hasPendingAction: false });
    vc.speechFinished(); // arming, re-open pending
    const before = count("openMic");
    vc.end("user");
    expect(vc.getState()).toBe("ended");
    expect(events).toContain("ended:user");
    clock.advance(1000);
    expect(count("openMic")).toBe(before); // the pending re-open was cancelled
  });

  it("restarts a silently dead mic when the watchdog fires, bounded by max_mic_restarts", () => {
    // The plugin resolves start() at once and cannot report a native failure,
    // so a mic that reports nothing at all is restarted — a couple of times —
    // then max_turn / idle take over as before.
    const { vc, clock, events, count } = live({
      mic_watchdog_ms: 1000,
      max_mic_restarts: 2,
      max_turn_ms: 100_000,
    });
    expect(count("openMic")).toBe(1);
    clock.advance(1000); // nothing arrived
    expect(events).toContain("restart:1");
    expect(count("closeMic")).toBe(1);
    expect(count("openMic")).toBe(2);
    vc.micReady();
    clock.advance(1000);
    expect(events).toContain("restart:2");
    expect(count("openMic")).toBe(3);
    vc.micReady();
    clock.advance(1000);
    expect(count("openMic")).toBe(3); // capped
    expect(vc.getState()).toBe("listening");
  });

  it("disarms the watchdog as soon as the mic proves alive", () => {
    const a = live({ mic_watchdog_ms: 1000, silence_duration_ms: 5000, max_turn_ms: 100_000 });
    a.vc.recognizerStarted(); // speech began: alive
    a.clock.advance(1000);
    expect(a.count("openMic")).toBe(1);

    const b = live({ mic_watchdog_ms: 1000, silence_duration_ms: 5000, max_turn_ms: 100_000 });
    b.vc.partial("hi"); // something arrived: alive
    b.clock.advance(1000);
    expect(b.count("openMic")).toBe(1);
  });

  it("keeps the session alive when muted, and captures nothing until unmuted", () => {
    const { clock, vc, events, count } = live({ silence_duration_ms: 1000 });
    vc.toggleMute();
    expect(vc.isMuted()).toBe(true);
    expect(vc.isActive()).toBe(true);
    expect(events).toContain("closeMic");
    vc.partial("ignored while muted");
    clock.advance(1000);
    expect(count("sendTurn")).toBe(0);
    vc.toggleMute();
    expect(vc.isMuted()).toBe(false);
    expect(vc.getState()).toBe("arming"); // re-opened for a fresh turn
  });

  it("does not double-send when our own closeMic synchronously stops the recognizer", () => {
    // Web Speech `stop()` fires `onend`, which the host forwards as
    // `recognizerStopped`. A synchronous one lands mid-endpoint, before the
    // turn has left `listening` — the exact shape that used to schedule a
    // second endpoint and send the turn twice.
    const clock = makeClock();
    const events: string[] = [];
    let vc!: VoiceConversation;
    const ports: VoicePorts = {
      openMic: () => events.push("openMic"),
      closeMic: () => {
        events.push("closeMic");
        vc.recognizerStopped(); // the induced, synchronous stop
      },
      sendTurn: (t) => events.push(`sendTurn:${t}`),
      speak: () => {},
      stopSpeaking: () => {},
      onChange: () => {},
      onEnded: () => {},
    };
    vc = new VoiceConversation(
      ports,
      cfg({ silence_duration_ms: 0, final_result_grace_ms: 0 }),
      clock.scheduler,
    );
    vc.begin();
    vc.micReady();
    vc.partial("what is on top");
    clock.advance(0); // silence → endpoint → sending → closeMic (induces stop)
    clock.advance(1000); // let any stray grace timer fire
    expect(events.filter((e) => e.startsWith("sendTurn:")).length).toBe(1);
  });

  it("stops a playing reply and closes the mic when the session ends", () => {
    const { vc, events } = live();
    vc.end("user");
    expect(events).toContain("stopSpeaking");
    expect(events).toContain("closeMic");
    expect(events).toContain("ended:user");
    expect(vc.getState()).toBe("ended");
  });
});

describe("voice config", () => {
  it("is the generic defaults when nothing is stored", () => {
    expect(readVoiceConfig(() => null)).toEqual(VOICE_CONFIG_DEFAULTS);
  });

  it("takes localStorage overrides, and ignores malformed values", () => {
    const store: Record<string, string> = {
      "vogt.assistant.voice.silence_duration_ms": "800",
      "vogt.assistant.voice.interrupt_response": "true",
      "vogt.assistant.voice.idle_timeout_ms": "not-a-number",
    };
    const c = readVoiceConfig((k) => store[k] ?? null);
    expect(c.silence_duration_ms).toBe(800);
    expect(c.interrupt_response).toBe(true);
    expect(c.idle_timeout_ms).toBe(VOICE_CONFIG_DEFAULTS.idle_timeout_ms);
  });
});

describe("the status chip label", () => {
  it("reads Muted over the capture phases, and names the rest", () => {
    expect(voiceStatusLabel("listening", true)).toBe("Muted");
    expect(voiceStatusLabel("listening", false)).toBe("Listening…");
    expect(voiceStatusLabel("sending", false)).toBe("Sending");
    expect(voiceStatusLabel("speaking", true)).toBe("Speaking"); // mute does not hide speaking
    expect(voiceStatusLabel("paused_for_approval", false)).toBe("Paused for approval");
  });
});
