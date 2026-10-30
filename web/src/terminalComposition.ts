/**
 * IME composition guard for the terminal's soft-keyboard input (WI-2707).
 *
 * An Android keyboard (Gboard) types a word as an IME composition: the
 * textarea holds a provisional string that the keyboard rewrites on every
 * keystroke and commits at `compositionend`. xterm sends the committed text
 * itself, but on a phone its textarea-diff and `input` paths can also fire
 * for the same text, or for provisional text mid-composition, and the PTY
 * gets letters twice. The guard makes the commit the one source of truth:
 * nothing reaches the PTY while composing, the committed text is sent once
 * at `compositionend`, and xterm's own echo of that text is swallowed.
 */

/** How long after a commit xterm's own echo of it is still swallowed (ms). */
export const COMPOSITION_ECHO_WINDOW_MS = 100;

export class TerminalCompositionGuard {
  private composing = false;
  private echo = "";
  private echoDeadline = 0;

  /** True between `compositionstart` and `compositionend`. */
  get isComposing(): boolean {
    return this.composing;
  }

  start(): void {
    this.composing = true;
  }

  /** `compositionupdate` carries only provisional text; nothing to send. */
  update(): void {
    this.composing = true;
  }

  /**
   * Abandon any composition and echo. Android can drop `compositionend`
   * (keyboard switch, app switch, page hide); without this every later
   * keystroke would be swallowed until a reload. Called on textarea blur,
   * page hide, re-attach and paste.
   */
  reset(): void {
    this.composing = false;
    this.echo = "";
    this.echoDeadline = 0;
  }

  /**
   * A real keystroke after a commit: what follows is new input, not xterm's
   * echo, so a fast identical keystroke inside the echo window still lands.
   */
  keydown(): void {
    this.echo = "";
  }

  /** Returns the committed text to send to the PTY (possibly empty). */
  end(committed: string, now: number): string {
    this.composing = false;
    this.echo = committed;
    this.echoDeadline = now + COMPOSITION_ECHO_WINDOW_MS;
    return committed;
  }

  /**
   * Filter one xterm `onData` chunk: null drops it, a string is what to send.
   * Drops data while composing and the part of a chunk that repeats text the
   * guard already sent from a commit.
   */
  filter(data: string, now: number): string | null {
    if (this.composing) return null;
    if (this.echo && now <= this.echoDeadline) {
      if (this.echo.startsWith(data)) {
        this.echo = this.echo.slice(data.length);
        return null;
      }
      if (data.startsWith(this.echo)) {
        const rest = data.slice(this.echo.length);
        this.echo = "";
        return rest || null;
      }
    }
    this.echo = "";
    return data;
  }
}

/** The guard wired to xterm's helper textarea; what Terminal.tsx drives. */
export interface CompositionInput {
  /** Route one xterm `onData` chunk; sends whatever survives the guard. */
  onData(data: string): void;
  /** Drop any stale composition (re-attach). */
  reset(): void;
  /** Programmatic paste: reset the composition, then hand the text to `sink`. */
  paste(text: string, sink: (text: string) => void): void;
  /** Remove the document-level listener. */
  dispose(): void;
}

/**
 * Wire a composition guard to xterm's textarea. `send` receives every byte
 * bound for the PTY; `now` is the clock (performance.now in the app).
 */
export function wireCompositionInput(
  textarea: HTMLTextAreaElement,
  send: (data: string) => void,
  now: () => number = () => performance.now(),
  doc: Document = document,
): CompositionInput {
  const guard = new TerminalCompositionGuard();
  textarea.addEventListener("compositionstart", () => guard.start());
  textarea.addEventListener("compositionupdate", () => guard.update());
  textarea.addEventListener("compositionend", (event) => {
    const committed = guard.end(event.data ?? "", now());
    if (committed) send(committed);
  });
  // Android can drop compositionend; never let a stale composition
  // swallow input past a blur, page hide or paste.
  textarea.addEventListener("blur", () => guard.reset());
  textarea.addEventListener("paste", () => guard.reset(), { capture: true });
  // keyCode 229 is the IME's own keydown inside a composition.
  textarea.addEventListener("keydown", (event) => {
    if (!event.isComposing && event.keyCode !== 229) guard.keydown();
  }, { capture: true });
  const onVisibility = () => {
    if (doc.visibilityState === "hidden") guard.reset();
  };
  doc.addEventListener("visibilitychange", onVisibility);
  return {
    onData(data) {
      const next = guard.filter(data, now());
      if (next) send(next);
    },
    reset: () => guard.reset(),
    paste(text, sink) {
      guard.reset();
      if (text) sink(text);
    },
    dispose: () => doc.removeEventListener("visibilitychange", onVisibility),
  };
}
