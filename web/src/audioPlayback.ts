// Playing a fetched audio clip where the Android WebView can actually hear it.
//
// The obvious way to play server-synthesised speech — `new Audio(URL.create-
// ObjectURL(blob)).play()` — works in every desktop browser and fails on the
// Android WebView with `MEDIA_ELEMENT_ERROR: Media load rejected by URL safety
// check` (surfacing as NotSupportedError, "no supported source"). The WebView
// hands `<audio>` playback to a native loader that cannot read a renderer-side
// `blob:` URL, so *every* clip is refused regardless of format — the client
// diagnostics showed a perfectly typed `audio/wav` Blob rejected at load. Piper
// mp3 and Kokoro wav failed identically; the container was never the problem.
//
// Web Audio sidesteps it: decode the bytes in the renderer and play them
// through an `AudioBufferSourceNode`. That works on the WebView, gives an exact
// end event (the hands-free loop re-opens the mic on it), and is the natural
// base for sentence-by-sentence streaming later. The element path is kept only
// as a fallback for a browser with no `AudioContext` at all.
//
// One rule matters for the hands-free loop: `onEnded` fires on a *natural* end
// only. A clip halted by `stop()` (the next turn starting, mute, leaving) must
// not report itself finished, or the loop would re-open the mic on top of the
// take that halted it.

export interface Playback {
  /** Seconds of audio, once decoded. */
  readonly duration: number;
  /** Whether it is still playing. */
  playing(): boolean;
  /** Halt now. `onEnded` is not called. Idempotent. */
  stop(): void;
}

export interface PlayOptions {
  /** Abort while still decoding: nothing starts. */
  signal?: AbortSignal;
  /** Playback has begun. */
  onPlaying?: (duration: number) => void;
  /** Reached the natural end of the clip (never after `stop()`). */
  onEnded?: () => void;
}

interface AudioWindow {
  AudioContext?: typeof AudioContext;
  webkitAudioContext?: typeof AudioContext;
}

let context: AudioContext | null = null;

function contextCtor(): (typeof AudioContext) | null {
  const w = window as unknown as AudioWindow;
  return w.AudioContext ?? w.webkitAudioContext ?? null;
}

/** Whether Web Audio is available here at all. */
export function audioContextAvailable(): boolean {
  return contextCtor() !== null;
}

function getContext(): AudioContext {
  if (context === null) {
    const Ctor = contextCtor();
    if (!Ctor) throw new Error("no AudioContext");
    context = new Ctor();
  }
  return context;
}

/**
 * Create and resume the context inside a user gesture. Autoplay policy lets a
 * context start only after the page has been interacted with; the toggles and
 * the mic press are that interaction, so they prime it — the same reason the
 * speech synth is primed on the toggle gesture.
 */
export function primeAudio(): void {
  if (!audioContextAvailable()) return;
  try {
    const ctx = getContext();
    if (ctx.state === "suspended") void ctx.resume().catch(() => {});
  } catch {
    /* no audio here; the fallback path will say so when it matters */
  }
}

/** Decode `blob` and play it. Resolves once playback has started. */
export async function playAudioBlob(blob: Blob, opts: PlayOptions = {}): Promise<Playback> {
  const bytes = await blob.arrayBuffer();
  if (opts.signal?.aborted) throw new DOMException("aborted", "AbortError");
  const ctx = getContext();
  if (ctx.state === "suspended") await ctx.resume();
  // `decodeAudioData` detaches its input; hand it a copy so a caller's Blob
  // bytes stay usable (diagnostics read the size after).
  const decoded = await ctx.decodeAudioData(bytes.slice(0));
  if (opts.signal?.aborted) throw new DOMException("aborted", "AbortError");

  const source = ctx.createBufferSource();
  source.buffer = decoded;
  source.connect(ctx.destination);
  let done = false;
  let live = true;
  source.onended = () => {
    live = false;
    if (done) return; // halted, not finished — say nothing
    done = true;
    opts.onEnded?.();
  };
  source.start();
  opts.onPlaying?.(decoded.duration);
  return {
    duration: decoded.duration,
    playing: () => live && !done,
    stop: () => {
      if (done) return;
      done = true;
      live = false;
      try {
        source.stop();
      } catch {
        /* already stopped */
      }
    },
  };
}

/**
 * Release the output stream. Called when a reply ends or is halted, *before*
 * the microphone re-opens: Android is touchy about starting speech capture
 * while the app still holds an active playback stream — the recogniser can
 * come up silently dead. The next `playAudioBlob` resumes the context.
 */
export function suspendAudio(): void {
  if (context !== null && context.state === "running") {
    void context.suspend().catch(() => {});
  }
}

/** Test seam: forget the shared context. */
export function resetAudioForTests(): void {
  context = null;
}
