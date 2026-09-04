import { Show, type Component } from "solid-js";
import { createNarrow } from "./narrow";
import { useShellActions } from "./shellActions";

/**
 * The phone's inline "Go to…" control (command palette opener).
 *
 * Renders only on a narrow client that is inside the shell: a desk has the
 * Places rail's own Go to… button, and a surface rendered outside `App.tsx`
 * has no palette to open. It is a compact icon so it can share a header row
 * with a title and that surface's primary action instead of taking a row of
 * its own above every screen.
 */
export const GoToButton: Component<{ class?: string }> = (props) => {
  const narrow = createNarrow();
  const shell = useShellActions();
  return (
    <Show when={narrow() && shell}>
      {(actions) => (
        <button
          type="button"
          class={`go-to-inline${props.class ? ` ${props.class}` : ""}`}
          aria-label="Go to… (command palette)"
          title="Go to… (Ctrl/Cmd+K)"
          onClick={() => actions().openCommandPalette()}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="12" cy="12" r="8.5" />
            <path d="M15.2 8.8l-2.1 5.3-4.3 1.1 2.1-5.3z" />
          </svg>
        </button>
      )}
    </Show>
  );
};

export default GoToButton;
