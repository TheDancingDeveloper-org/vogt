import { Component, createSignal, For } from "solid-js";

interface Props {
  /** Send raw bytes/text into the active terminal's PTY stdin. */
  send: (data: string) => void;
}

/**
 * Mobile modifier-key row. Visible only on coarse-pointer narrow screens
 * (see styles.css media query). Tapping a "sticky" key like Ctrl arms it for
 * the next character: tap Ctrl, then C → ^C is sent.
 */
const ModKeyRow: Component<Props> = (props) => {
  const [ctrlArmed, setCtrlArmed] = createSignal(false);

  const send = (s: string) => {
    if (ctrlArmed()) {
      // Convert next letter into ASCII control code 1..26 (Ctrl-A..Ctrl-Z).
      // For non-letters, fall back to sending as-is preceded by Ctrl semantics
      // (only letters are meaningfully handled here).
      const ch = s.length === 1 ? s.toLowerCase() : "";
      const code = ch.charCodeAt(0);
      if (code >= 97 && code <= 122) {
        props.send(String.fromCharCode(code - 96));
      } else {
        // Send literally if Ctrl+X doesn't map to a control character.
        props.send(s);
      }
      setCtrlArmed(false);
      return;
    }
    props.send(s);
  };

  const keys: Array<{ label: string; send?: () => void; armed?: () => boolean }> = [
    { label: "Esc", send: () => send("\x1b") },
    { label: "Tab", send: () => send("\t") },
    {
      label: "Ctrl",
      send: () => setCtrlArmed((v) => !v),
      armed: () => ctrlArmed(),
    },
    { label: "←", send: () => send("\x1b[D") },
    { label: "↑", send: () => send("\x1b[A") },
    { label: "↓", send: () => send("\x1b[B") },
    { label: "→", send: () => send("\x1b[C") },
    { label: "/", send: () => send("/") },
    { label: "|", send: () => send("|") },
    { label: "~", send: () => send("~") },
    { label: "Enter", send: () => send("\r") },
  ];

  // After a non-Ctrl key tap, if Ctrl was armed we already consumed it.
  // For Ctrl+letter, the actual letter still comes from the soft keyboard —
  // so when armed, we hook the next single keystroke at the App level too
  // (parent passes us send(), and the App should intercept terminal data).
  // For Phase 2 we accept the limitation that Ctrl only chords with the
  // explicit modkey buttons; chording with the soft keyboard requires deeper
  // input interception we'll add later.

  return (
    <div class="modkey-row" role="toolbar" aria-label="Terminal modifier keys">
      <For each={keys}>
        {(k) => (
          <button
            type="button"
            class={k.armed?.() ? "armed" : undefined}
            onClick={() => k.send?.()}
          >
            {k.label}
          </button>
        )}
      </For>
    </div>
  );
};

export default ModKeyRow;
