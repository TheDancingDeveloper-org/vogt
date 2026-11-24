// Which size a terminal pane draws at, and when it asks the PTY to change.
//
// One PTY can have several viewers — a desktop pane at 178×32 and a phone at
// 66×40 — but only one size. The program paints for that size: a diff-painting
// TUI (Bubble Tea, Ink) repaints its frame with relative cursor moves, so a
// viewer that draws the stream at any other width wraps each line and the
// moves land on the wrong rows, leaving a stack of ghost frames (WI-1089).
//
// So every viewer draws at the PTY's size, as the engine announces it on
// `snapshot-start` and in `resize` frames, whatever its own pane fits. A pane
// asks for its fitted size only while it owns the size: from mount, and again
// whenever the person acts in it (types, focuses it, resizes it, or taps "fit
// here"). Another viewer's resize takes ownership away, so an idle pane that
// reconnects or refits never takes the size back from the one in use.
//
// An engine that announces no size leaves `pty` unset: the pane then draws at
// its fitted size and always asks for it, as before.

export interface TermSize {
  cols: number;
  rows: number;
}

/** Requests remembered as ours, so a late echo of one is not read as a takeover. */
const PENDING_REQUESTS = 4;

export const sameSize = (a: TermSize | null, b: TermSize | null): boolean =>
  a !== null && b !== null && a.cols === b.cols && a.rows === b.rows;

export class TerminalSizeTracker {
  /** Whether this pane may ask the PTY for its fitted size. */
  owner = true;
  /** The PTY's size as the engine last announced it. */
  pty: TermSize | null = null;
  /** The size this pane's host fits. */
  fitted: TermSize | null = null;
  private requested: TermSize[] = [];

  /**
   * Record the host's fitted size. Returns true when the host changed size
   * since the last fit — the person resized the window, split or keyboard — as
   * opposed to the first fit or a refit at the same size.
   */
  fit(size: TermSize): boolean {
    const changed = this.fitted !== null && !sameSize(this.fitted, size);
    this.fitted = size;
    return changed;
  }

  /** The person acted in this pane: it takes the size. */
  claim(): void {
    this.owner = true;
  }

  /** The size the local terminal should draw at. */
  target(): TermSize | null {
    return this.pty ?? this.fitted;
  }

  /**
   * The size to ask the PTY for now, or null when this pane does not own the
   * size or already has what it fits.
   */
  request(): TermSize | null {
    if (!this.owner || this.fitted === null) return null;
    if (sameSize(this.pty, this.fitted)) return null;
    const size = this.fitted;
    if (!this.requested.some((r) => sameSize(r, size))) {
      this.requested = [...this.requested, size].slice(-PENDING_REQUESTS);
    }
    return size;
  }

  /**
   * The engine announced the PTY's size. A size this pane asked for confirms
   * it owns the size. A `resize` frame it did not ask for means another viewer
   * took the size. So does a snapshot (a reattach) whose size moved from the
   * one this pane last knew: the other viewer resized while this pane was
   * away, and the `resize` frame went to nobody. The first snapshot a pane
   * sees takes nothing away — it is drawn before the pane's own request.
   */
  announce(size: TermSize, fromSnapshot: boolean): void {
    const previous = this.pty;
    this.pty = size;
    const confirmed = this.requested.findIndex((r) => sameSize(r, size));
    if (confirmed >= 0) {
      // Requests made after this one are still in flight.
      this.owner = true;
      this.requested = this.requested.slice(confirmed + 1);
      return;
    }
    if (fromSnapshot && (previous === null || sameSize(previous, size))) return;
    this.owner = false;
  }

  /** The PTY's size when another viewer set it and this pane fits otherwise. */
  foreign(): TermSize | null {
    if (this.owner || this.pty === null || this.fitted === null) return null;
    return sameSize(this.pty, this.fitted) ? null : this.pty;
  }
}
