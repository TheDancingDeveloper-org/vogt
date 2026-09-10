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
