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
