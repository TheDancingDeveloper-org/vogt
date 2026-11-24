// The live call's microphone: echo-cancelled capture, cut into 20 ms frames
// of 16 kHz PCM16 by `public/call-capture-worklet.js`.
//
// Echo cancellation, noise suppression and gain control are asked of the
// platform because the engine's voice detector (an energy detector) does
// best on exactly that kind of signal — and because, on a phone speaker, the
// reply's own echo is what would otherwise interrupt it. On the Android app
// the native shell also switches the device to its communication audio mode
// for the call (voiceService `startCallAudio`), which is what brings the
// hardware echo canceller in.

export interface Capture {
  /** Stop the microphone and release the device. Idempotent. */
  stop(): void;
}

export interface CaptureOptions {
  /** One 20 ms frame: 320 samples of little-endian PCM16. */
  onFrame: (frame: ArrayBuffer) => void;
}

interface CaptureWindow {
  AudioContext?: typeof AudioContext;
  webkitAudioContext?: typeof AudioContext;
}

/** Whether this browser can capture for a call at all. */
export function callCaptureSupported(): boolean {
  const w = window as unknown as CaptureWindow;
  const Ctor = w.AudioContext ?? w.webkitAudioContext;
  return (
    typeof navigator !== "undefined" &&
    typeof navigator.mediaDevices?.getUserMedia === "function" &&
    Ctor !== undefined &&
    typeof Ctor.prototype !== "undefined" &&
    "audioWorklet" in Ctor.prototype
  );
}

/**
 * Open the microphone and start framing it. Call from a user gesture: the
 * permission prompt and the audio context both want one.
 */
export async function startCapture(options: CaptureOptions): Promise<Capture> {
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: {
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
      channelCount: 1,
    },
  });
  const w = window as unknown as CaptureWindow;
  const Ctor = (w.AudioContext ?? w.webkitAudioContext) as typeof AudioContext;
  const ctx = new Ctor();
  let stopped = false;
  const stop = () => {
    if (stopped) return;
    stopped = true;
    for (const track of stream.getTracks()) track.stop();
    void ctx.close().catch(() => {});
  };
  try {
    if (ctx.state === "suspended") await ctx.resume();
    await ctx.audioWorklet.addModule(new URL("call-capture-worklet.js", document.baseURI).href);
    const source = ctx.createMediaStreamSource(stream);
    const node = new AudioWorkletNode(ctx, "vogt-call-capture");
    node.port.onmessage = (event: MessageEvent<ArrayBuffer>) => {
      if (!stopped) options.onFrame(event.data);
    };
    source.connect(node);
    // Connected so the graph pulls it (Chrome renders only what reaches the
    // destination); the processor writes no output, so nothing is heard.
    node.connect(ctx.destination);
  } catch (error) {
    stop();
    throw error;
  }
  return { stop };
}
