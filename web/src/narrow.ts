// Whether this client is a narrow one, as a signal (FR-M3, RESTRUCTURE
// Stage 9–10).
//
// The breakpoint is the shell's own, and it is asked of `matchMedia` rather
// than of `innerWidth` so a rotation or a resized window changes the answer
// without anything polling. Every surface that composes differently on a
// phone reads it from here, so there is one breakpoint in the product rather
// than one per file that happened to need it.

import { createSignal, onCleanup, type Accessor } from "solid-js";

export const NARROW_QUERY = "(max-width: 768px)";

/** A signal that is true while the viewport matches `query`. */
export function createNarrow(query: string = NARROW_QUERY): Accessor<boolean> {
  const [narrow, setNarrow] = createSignal(false);

  // `matchMedia` is absent in jsdom unless a test stubs it; a surface that
  // cannot ask is a wide one, which is what the tests that do not stub it
  // are testing.
  const media = typeof window !== "undefined" && window.matchMedia
    ? window.matchMedia(query)
    : null;
  if (media) {
    setNarrow(media.matches);
    const update = () => setNarrow(media.matches);
    media.addEventListener("change", update);
    onCleanup(() => media.removeEventListener("change", update));
  }

  return narrow;
}
