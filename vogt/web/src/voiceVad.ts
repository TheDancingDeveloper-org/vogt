// Barge-in voice-activity detection (WI-174 v2).
//
// v1 hands-free is half-duplex: the mic is closed while a reply plays. v2 lets
// the speaker cut in — start talking over the reply and the loop stops speaking
// and listens. That needs a capture running *during* playback that can tell the
// speaker's voice from the reply's own audio. This module is that capture, and
// only the onset decision it makes; the loop (`voiceTurn.ts`) owns what to do
// with an onset (`speechDetected()` → halt + listen, gated on
// `interrupt_response`).
//
// The onset decision is split out as a pure `OnsetDetector` so it is unit-
// tested off a list of frame energies rather than a microphone. The capture
// wrapper below is a thin Web Audio shell around it — echo cancellation on, so
// the reply the device is playing is largely absent from what the mic hears,
// and a leaky accumulator so a confident, sustained onset (not a transient
// click or a dip in the reply's AEC residual) is what fires. A stronger model
// (Silero via a WASM VAD) can replace the shell later behind the same
// `startBargeInDetection` seam without touching the loop.
//
// Everything degrades to a no-op: no `getUserMedia`, no `AudioContext`, or a
// capture that throws, simply means no barge-in — v1 half-duplex, which is a
// working state.

/** Onset tuning, read from the shared `vogt.assistant.voice.*` namespace. */
export interface OnsetConfig {
  /** RMS (0–1) a frame must exceed to count as loud. Over the echo-cancelled
   *  signal, the reply's residual sits below this and the speaker's voice above. */
  vad_threshold: number;
  /** Sustained loud time that fires an onset. The design's "≥ 500 ms". */
  vad_onset_ms: number;
}

export const ONSET_DEFAULTS: OnsetConfig = {
  vad_threshold: 0.045,
  vad_onset_ms: 500,
};

const VOICE_CFG_PREFIX = "vogt.assistant.voice.";

export function readOnsetConfig(
  read: (key: string) => string | null = (key) => {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  },
): OnsetConfig {
  const num = (key: keyof OnsetConfig, fallback: number): number => {
    const raw = read(VOICE_CFG_PREFIX + key);
    if (raw === null) return fallback;
    const n = Number(raw);
    return Number.isFinite(n) && n >= 0 ? n : fallback;
  };
  return {
    vad_threshold: num("vad_threshold", ONSET_DEFAULTS.vad_threshold),
    vad_onset_ms: num("vad_onset_ms", ONSET_DEFAULTS.vad_onset_ms),
  };
}

/**
 * A leaky-accumulator onset detector. Each loud frame adds its duration to an
 * accumulator; each quiet frame subtracts it (floored at zero). An onset fires
 * — once — when the accumulator crosses `vad_onset_ms`, so a brief click or a
 * momentary AEC residual does not trigger, but a half-second of real speech
 * does, even through the small dips of natural speech.
 */
export class OnsetDetector {
  private above = 0;
  private fired = false;

  constructor(
    private readonly cfg: OnsetConfig,
    private readonly onOnset: () => void,
  ) {}

  /** Feed one analysis frame: `rms` in [0, 1], `dtMs` its duration. */
  push(rms: number, dtMs: number): void {
    if (rms >= this.cfg.vad_threshold) this.above += dtMs;
    else this.above = Math.max(0, this.above - dtMs);
    if (!this.fired && this.above >= this.cfg.vad_onset_ms) {
      this.fired = true;
      this.onOnset();
    }
  }

  /** Reset for a fresh listening window. */
  reset(): void {
    this.above = 0;
    this.fired = false;
  }
}

/** Root-mean-square of a time-domain frame, in [0, 1]. */
export function frameRms(samples: Float32Array): number {
  if (samples.length === 0) return 0;
  let sum = 0;
  for (let i = 0; i < samples.length; i += 1) {
    const s = samples[i] ?? 0;
    sum += s * s;
  }
  return Math.sqrt(sum / samples.length);
}

/** A handle that stops the capture and frees the mic. Idempotent. */
export type StopBargeIn = () => void;

interface WindowAudio {
  AudioContext?: typeof AudioContext;
  webkitAudioContext?: typeof AudioContext;
}

/**
 * Open an echo-cancelled capture and fire `onOnset` on a sustained speaker
 * onset. Returns a stop handle; a no-op stop when the platform cannot capture
 * (older WebView, denied permission, no Web Audio), which leaves the loop
 * half-duplex. `now`/`schedule` are injected only so this is drivable in a
 * test; production uses the real clock and `setInterval`.
 */
export function startBargeInDetection(
  onOnset: () => void,
  cfg: OnsetConfig = readOnsetConfig(),
): StopBargeIn {
  const w = window as unknown as WindowAudio;
  const Ctx = w.AudioContext ?? w.webkitAudioContext;
  const media = navigator.mediaDevices;
  if (!Ctx || !media?.getUserMedia) return () => {};

  const detector = new OnsetDetector(cfg, onOnset);
  let stopped = false;
  let audioCtx: AudioContext | null = null;
  let stream: MediaStream | null = null;
  let timer: ReturnType<typeof setInterval> | null = null;

  const stop: StopBargeIn = () => {
    if (stopped) return;
    stopped = true;
    if (timer !== null) clearInterval(timer);
    timer = null;
    stream?.getTracks().forEach((track) => track.stop());
    stream = null;
    void audioCtx?.close().catch(() => {});
    audioCtx = null;
  };

  void media
    .getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    })
    .then((granted) => {
      if (stopped) {
        granted.getTracks().forEach((track) => track.stop());
        return;
      }
      stream = granted;
      audioCtx = new Ctx();
      const source = audioCtx.createMediaStreamSource(granted);
      const analyser = audioCtx.createAnalyser();
      analyser.fftSize = 1024;
      source.connect(analyser);
      const buffer = new Float32Array(analyser.fftSize);
      const frameMs = 50;
      timer = setInterval(() => {
        analyser.getFloatTimeDomainData(buffer);
        detector.push(frameRms(buffer), frameMs);
      }, frameMs);
    })
    .catch(() => {
      // Denied or unavailable: no barge-in, half-duplex stands.
      stop();
    });

  return stop;
}

// -- end-of-turn on the server-STT take (WI-959) ----------------------------
//
// The on-device recognizers report partial transcripts, and the tap take's
// silence timer is rearmed on each one. The server-STT take has no partials —
// it is a `MediaRecorder` capture that is only transcribed once it ends — so
// it had no way to notice the speaker had gone quiet, and a tap on it could
// only be ended by another tap. This is that missing signal: an energy-based
// endpointer over the same capture stream, so going quiet sends here too.

/** Endpointer tuning; the onset pair is shared with barge-in. */
export interface EndpointConfig extends OnsetConfig {
  /** Quiet time after speech that ends the turn (the tap take's silence window). */
  silence_duration_ms: number;
}

/**
 * Speech-then-silence detection over frame energies. `onSpeech` fires once,
 * when a sustained onset (the barge-in accumulator, so a click does not count)
 * shows the speaker has started; `onSilence` fires once, after speech, when the
 * frames have stayed below the threshold for `silence_duration_ms`. Nothing
 * fires for a capture that never heard speech — the caller owns that timeout.
 */
export class SilenceEndpointer {
  private readonly onset: OnsetDetector;
  private speaking = false;
  private quietMs = 0;
  private ended = false;

  constructor(
    private readonly cfg: EndpointConfig,
    private readonly handlers: { onSpeech: () => void; onSilence: () => void },
  ) {
    this.onset = new OnsetDetector(cfg, () => {
      this.speaking = true;
      this.quietMs = 0;
      this.handlers.onSpeech();
    });
  }

  /** Feed one analysis frame: `rms` in [0, 1], `dtMs` its duration. */
  push(rms: number, dtMs: number): void {
    if (this.ended) return;
    if (!this.speaking) {
      this.onset.push(rms, dtMs);
      return;
    }
    if (rms >= this.cfg.vad_threshold) {
      this.quietMs = 0;
      return;
    }
    this.quietMs += dtMs;
    if (this.quietMs >= this.cfg.silence_duration_ms) {
      this.ended = true;
      this.handlers.onSilence();
    }
  }
}

/** The endpointer's frame period; also its clock. */
const ENDPOINT_FRAME_MS = 50;

/**
 * Watch an already-open capture stream (the server take's own `MediaRecorder`
 * stream — no second microphone) for speech and then silence. Returns a stop
 * handle that frees the analysis graph but leaves the stream's tracks alone:
 * the recorder owns them. Without Web Audio this is a no-op, and the take
 * falls back to ending on a tap or the turn cap.
 */
export function watchStreamForSilence(
  stream: MediaStream,
  cfg: EndpointConfig,
  handlers: { onSpeech: () => void; onSilence: () => void },
): StopBargeIn {
  const w = window as unknown as WindowAudio;
  const Ctx = w.AudioContext ?? w.webkitAudioContext;
  if (!Ctx) return () => {};
  let audioCtx: AudioContext | null = null;
  let timer: ReturnType<typeof setInterval> | null = null;
  let stopped = false;
  const stop: StopBargeIn = () => {
    if (stopped) return;
    stopped = true;
    if (timer !== null) clearInterval(timer);
    timer = null;
    void audioCtx?.close().catch(() => {});
    audioCtx = null;
  };
  try {
    audioCtx = new Ctx();
    const source = audioCtx.createMediaStreamSource(stream);
    const analyser = audioCtx.createAnalyser();
    analyser.fftSize = 1024;
    source.connect(analyser);
    const buffer = new Float32Array(analyser.fftSize);
    const endpointer = new SilenceEndpointer(cfg, handlers);
    timer = setInterval(() => {
      analyser.getFloatTimeDomainData(buffer);
      endpointer.push(frameRms(buffer), ENDPOINT_FRAME_MS);
    }, ENDPOINT_FRAME_MS);
  } catch {
    stop();
  }
  return stop;
}
