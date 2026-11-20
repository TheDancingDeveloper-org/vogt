// The soft-keyboard inset, measured from the visual viewport.
//
// `--keyboard-inset` is what lifts the phone's bottom bar and the terminal's
// input dock above the keyboard. It used to be `innerHeight - vv.height -
// vv.offsetTop`, which is right at scale 1 and wrong the moment the reader
// pinch-zooms: a zoomed visual viewport is a *crop* of the layout viewport,
// so its height is smaller and the difference read as a keyboard that was
// not there — a 6% accidental zoom left a permanent ~55px band under the
// terminal composer on the 0.5.3 phone. The keyboard is the part of the
// layout viewport the visual viewport can no longer *cover*, which is
// `innerHeight - vv.height * vv.scale`: zoom changes height and scale in
// inverse proportion and cancels out; a keyboard shrinks the product.

export interface VisualViewportLike {
  height: number;
  scale: number;
}

/** The soft keyboard's height in layout CSS px, 0 when there is none. */
export function keyboardInsetFor(
  innerHeight: number,
  viewport: VisualViewportLike | null | undefined,
): number {
  if (!viewport) return 0;
  const scale = viewport.scale > 0 ? viewport.scale : 1;
  return Math.max(0, Math.round(innerHeight - viewport.height * scale));
}

/**
 * A visualViewport `scroll` fires on every keyboard nudge and pan, usually
 * with the height unchanged. Refit the terminal only when the visible height
 * moved by at least one terminal row; less cannot change the fitted grid
 * (WI-2706).
 */
export function viewportScrollChangesRows(
  previousHeight: number | null,
  height: number,
  rowPx: number,
): boolean {
  if (previousHeight === null) return true;
  return Math.abs(height - previousHeight) >= Math.max(1, rowPx);
}
