// The barge-in onset decision, proven off a list of frame energies.
//
// The capture shell (getUserMedia + Web Audio) is a thin, jsdom-hostile wrapper
// and is left to the browser round trip; what is worth pinning here is the
// decision it feeds — that a sustained onset fires once, a click does not, and
// natural dips are tolerated — because that is what makes barge-in usable
// rather than trigger-happy.

import { describe, expect, it } from "vitest";

import {
  ONSET_DEFAULTS,
  OnsetDetector,
  SilenceEndpointer,
  frameRms,
  readOnsetConfig,
  type OnsetConfig,
} from "../voiceVad";

const cfg: OnsetConfig = { vad_threshold: 0.1, vad_onset_ms: 500 };

/** Push `n` frames of a constant level, 50 ms each. */
function push(det: OnsetDetector, level: number, n: number, dt = 50): void {
  for (let i = 0; i < n; i += 1) det.push(level, dt);
}

describe("frameRms", () => {
  it("is zero for silence and the amplitude for a constant frame", () => {
    expect(frameRms(new Float32Array([0, 0, 0]))).toBe(0);
    expect(frameRms(new Float32Array([0.5, 0.5, 0.5]))).toBeCloseTo(0.5, 6);
    expect(frameRms(new Float32Array(0))).toBe(0);
  });
});

describe("OnsetDetector", () => {
  it("fires once after a sustained loud stretch", () => {
    let onsets = 0;
    const det = new OnsetDetector(cfg, () => (onsets += 1));
    push(det, 0.3, 9); // 450 ms — not yet
    expect(onsets).toBe(0);
    push(det, 0.3, 1); // crosses 500 ms
    expect(onsets).toBe(1);
    push(det, 0.3, 20); // stays loud — but it only fires once
    expect(onsets).toBe(1);
  });

  it("does not fire on a quiet signal or a brief click", () => {
    let onsets = 0;
    const det = new OnsetDetector(cfg, () => (onsets += 1));
    push(det, 0.02, 40); // well below threshold for 2 s
    push(det, 0.3, 2); // a 100 ms click
    push(det, 0.02, 40);
    expect(onsets).toBe(0);
  });

  it("tolerates the natural dips in speech but resets on real silence", () => {
    let onsets = 0;
    const det = new OnsetDetector(cfg, () => (onsets += 1));
    // Loud with a one-frame dip every third frame: the accumulator still climbs.
    for (let i = 0; i < 30 && onsets === 0; i += 1) {
      det.push(i % 3 === 2 ? 0.02 : 0.3, 50);
    }
    expect(onsets).toBe(1);

    // A fresh detector that goes quiet before reaching the threshold never fires.
    const det2 = new OnsetDetector(cfg, () => (onsets += 1));
    push(det2, 0.3, 8); // 400 ms up
    push(det2, 0.02, 8); // 400 ms down — back to zero
    push(det2, 0.3, 8); // 400 ms up again, still short of 500
    expect(onsets).toBe(1); // unchanged
  });

  it("re-arms after reset", () => {
    let onsets = 0;
    const det = new OnsetDetector(cfg, () => (onsets += 1));
    push(det, 0.3, 10);
    expect(onsets).toBe(1);
    det.reset();
    push(det, 0.3, 10);
    expect(onsets).toBe(2);
  });
});

describe("readOnsetConfig", () => {
  it("is the defaults when nothing is stored", () => {
    expect(readOnsetConfig(() => null)).toEqual(ONSET_DEFAULTS);
  });

  it("takes overrides and ignores malformed values", () => {
    const store: Record<string, string> = {
      "vogt.assistant.voice.vad_threshold": "0.08",
      "vogt.assistant.voice.vad_onset_ms": "nope",
    };
    const c = readOnsetConfig((k) => store[k] ?? null);
    expect(c.vad_threshold).toBe(0.08);
    expect(c.vad_onset_ms).toBe(ONSET_DEFAULTS.vad_onset_ms);
  });
});

// The server-STT tap take has no partial transcripts to rearm a silence clock
// on, so its end of turn is read off the capture's own energy (WI-959).
describe("SilenceEndpointer", () => {
  const ecfg = { vad_threshold: 0.1, vad_onset_ms: 200, silence_duration_ms: 1000 };
  function feed(ep: SilenceEndpointer, level: number, n: number): void {
    for (let i = 0; i < n; i += 1) ep.push(level, 50);
  }

  it("ends the turn once, after speech and then the silence window", () => {
    const events: string[] = [];
    const ep = new SilenceEndpointer(ecfg, {
      onSpeech: () => events.push("speech"),
      onSilence: () => events.push("silence"),
    });
    feed(ep, 0.3, 10); // 500 ms of speech: the onset fires at 200 ms
    expect(events).toEqual(["speech"]);
    feed(ep, 0.01, 19); // 950 ms quiet: not yet
    expect(events).toEqual(["speech"]);
    feed(ep, 0.01, 1); // 1000 ms quiet: the turn ends
    expect(events).toEqual(["speech", "silence"]);
    feed(ep, 0.3, 10);
    feed(ep, 0.01, 40);
    expect(events).toEqual(["speech", "silence"]);
  });

  it("restarts the silence window when the speaker pauses and carries on", () => {
    const events: string[] = [];
    const ep = new SilenceEndpointer(ecfg, {
      onSpeech: () => events.push("speech"),
      onSilence: () => events.push("silence"),
    });
    feed(ep, 0.3, 10);
    feed(ep, 0.01, 15); // a 750 ms pause mid-sentence
    feed(ep, 0.3, 4);
    feed(ep, 0.01, 15);
    expect(events).toEqual(["speech"]);
    feed(ep, 0.01, 5);
    expect(events).toEqual(["speech", "silence"]);
  });

  it("never ends a capture that heard no speech — a click is not a turn", () => {
    const events: string[] = [];
    const ep = new SilenceEndpointer(ecfg, {
      onSpeech: () => events.push("speech"),
      onSilence: () => events.push("silence"),
    });
    feed(ep, 0.01, 40);
    feed(ep, 0.5, 2); // a 100 ms click
    feed(ep, 0.01, 60);
    expect(events).toEqual([]);
  });
});
