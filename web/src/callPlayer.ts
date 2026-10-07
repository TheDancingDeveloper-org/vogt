// Playing a live call's reply: clips arrive one sentence at a time and play
// back to back, in order, through Web Audio (the only path the Android WebView
// plays — see audioPlayback.ts).
//
// Each clip is decoded as it arrives and scheduled to start where the one
// before it ends, so consecutive sentences play without a gap even though
// each was synthesized separately. The player reports when each clip
// *starts* (the server reckons how much of an interrupted reply was heard
// from these) and when the queue runs dry, and `clear()` stops everything at
// once — the barge-in path, where the speaker must go quiet the moment the
// user talks.

/** The slice of `AudioContext` the player uses; a test supplies a fake. */
export interface PlayerContext {
  readonly currentTime: number;
  readonly state: string;
  resume(): Promise<void>;
  decodeAudioData(data: ArrayBuffer): Promise<AudioBuffer>;
  createBufferSource(): AudioBufferSourceNode;
  readonly destination: AudioNode;
}

export interface CallPlayerEvents {
  /** Clip `index` of `responseId` began to play. */
  onStarted?: (responseId: string, index: number) => void;
  /** Nothing of `responseId` is playing or queued any more. */
  onIdle?: (responseId: string) => void;
  /** The first clip of `responseId` reached the speaker. */
  onFirstAudio?: (responseId: string) => void;
  /** A clip could not be decoded; it is skipped. */
  onError?: (error: unknown) => void;
}

interface Scheduled {
  responseId: string;
  source: AudioBufferSourceNode;
  timer: ReturnType<typeof setTimeout> | null;
}

/** A small lead so the first clip is never scheduled in the past. */
const LEAD_S = 0.02;

export class CallPlayer {
  private nextStart = 0;
  private active: Scheduled[] = [];
  /** Clips handed in but not yet decoded and scheduled, per response. */
  private decoding = new Map<string, number>();
  /** Bumped by `clear()`: a decode that finishes afterwards is dropped. */
  private generation = 0;
  private chain: Promise<void> = Promise.resolve();
  private heard = new Set<string>();

  constructor(
    private readonly ctx: PlayerContext,
    private readonly events: CallPlayerEvents = {},
  ) {}

  /** Queue clip `index` of `responseId`. Clips play in the order queued. */
  enqueue(responseId: string, index: number, bytes: ArrayBuffer): void {
    const generation = this.generation;
    this.decoding.set(responseId, (this.decoding.get(responseId) ?? 0) + 1);
    // Decoding is serialized so clips are scheduled in arrival order even
    // when a short one would decode before a long one ahead of it.
    this.chain = this.chain.then(async () => {
      let buffer: AudioBuffer | null = null;
      try {
        if (this.ctx.state === "suspended") await this.ctx.resume();
        buffer = await this.ctx.decodeAudioData(bytes.slice(0));
      } catch (error) {
        this.events.onError?.(error);
      }
      if (generation !== this.generation) return;
      this.decoding.set(responseId, (this.decoding.get(responseId) ?? 1) - 1);
      if (buffer) this.schedule(responseId, index, buffer);
      else this.maybeIdle(responseId);
    });
  }

  private schedule(responseId: string, index: number, buffer: AudioBuffer): void {
    const source = this.ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(this.ctx.destination);
    const now = this.ctx.currentTime;
    const startAt = Math.max(now + LEAD_S, this.nextStart);
    this.nextStart = startAt + buffer.duration;
    const entry: Scheduled = { responseId, source, timer: null };
    const announce = () => {
      entry.timer = null;
      if (!this.heard.has(responseId)) {
        this.heard.add(responseId);
        this.events.onFirstAudio?.(responseId);
      }
      this.events.onStarted?.(responseId, index);
    };
    entry.timer = setTimeout(announce, Math.max(0, (startAt - now) * 1000));
    source.onended = () => {
      this.active = this.active.filter((e) => e !== entry);
      this.maybeIdle(responseId);
    };
    this.active.push(entry);
    source.start(startAt);
  }

  private maybeIdle(responseId: string): void {
    const queued = this.decoding.get(responseId) ?? 0;
    const playing = this.active.some((e) => e.responseId === responseId);
    if (queued === 0 && !playing) {
      this.decoding.delete(responseId);
      this.events.onIdle?.(responseId);
    }
  }

  /** Whether anything is playing or waiting to. */
  busy(): boolean {
    return this.active.length > 0 || [...this.decoding.values()].some((n) => n > 0);
  }

  /** Stop everything now and drop what is queued. Reports nothing. */
  clear(): void {
    this.generation += 1;
    const active = this.active;
    this.active = [];
    this.decoding.clear();
    this.nextStart = 0;
    for (const entry of active) {
      if (entry.timer) clearTimeout(entry.timer);
      entry.source.onended = null;
      try {
        entry.source.stop();
      } catch {
        /* never started, or already stopped */
      }
    }
  }
}
