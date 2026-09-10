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
  };
  const count = (prefix: string) =>
    events.filter((e) => e === prefix || e.startsWith(`${prefix}:`)).length;
  return { ports, events, states, count };
}

function cfg(over: Partial<VoiceConfig> = {}): VoiceConfig {
  return { ...VOICE_CONFIG_DEFAULTS, ...over };
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
