import { Component, For } from "solid-js";
import {
  altArmed,
  applyStickyMods,
  armAlt,
  armCtrl,
  ctrlArmed,
} from "./terminalModifiers";

interface Props {
  /** Send raw bytes/text into the active terminal's PTY stdin. */
  send: (data: string) => void;
  /** Copy current xterm selection (if any) to clipboard. */
  onCopy?: () => void;
  /** Paste clipboard into the active terminal. */
  onPaste?: () => void;
  /** Select terminal content. */
  onSelectAll?: () => void;
  /** Focus the mobile command composer. */
  onFocusComposer?: () => void;
}

/**
 * Mobile modifier-key row. Visible only on coarse-pointer narrow screens
 * (see styles.css media query).
 *
 * The sticky `Ctrl`/`Alt` keys arm a shared store (terminalModifiers) rather
 * than a local signal, so the chord lands on the next character *from the soft
 * keyboard* too — the terminal input path consumes the same armed state
 * (#236). Tap `Ctrl`, then `r` → `^R`; tap `Alt`, then `f` → `ESC f`.
 */
const ModKeyRow: Component<Props> = (props) => {
  // Route a modkey button's bytes through the same sticky-modifier consumer the
  // soft keyboard uses, so arming Ctrl then tapping `/` chords too. Multi-byte
  // sequences (arrows, Home/End) pass straight through.
  const emit = (data: string) => {
    const next = applyStickyMods(data);
    props.send(typeof next === "string" ? next : data);
  };

  interface Key {
    label: string;
    onPress: () => void;
    armed?: () => boolean;
    /** Marks the extra second-tier keys, for a subtle visual grouping. */
    extra?: boolean;
  }

  const keys: Array<Key> = [
    { label: "Esc", onPress: () => emit("\x1b") },
    { label: "Tab", onPress: () => emit("\t") },
    {
      label: "Ctrl",
      onPress: () => armCtrl(),
      armed: () => ctrlArmed(),
    },
    {
      label: "Alt",
      onPress: () => armAlt(),
      armed: () => altArmed(),
    },
    { label: "^C", onPress: () => props.send("\x03") },
    { label: "^D", onPress: () => props.send("\x04") },
    { label: "^L", onPress: () => props.send("\x0c") },
    { label: "Bksp", onPress: () => props.send("\x7f") },
    { label: "←", onPress: () => emit("\x1b[D") },
    { label: "↑", onPress: () => emit("\x1b[A") },
    { label: "↓", onPress: () => emit("\x1b[B") },
    { label: "→", onPress: () => emit("\x1b[C") },
    { label: "Home", onPress: () => emit("\x1b[H"), extra: true },
    { label: "End", onPress: () => emit("\x1b[F"), extra: true },
    { label: "PgUp", onPress: () => emit("\x1b[5~"), extra: true },
    { label: "PgDn", onPress: () => emit("\x1b[6~"), extra: true },
    { label: "/", onPress: () => emit("/") },
    { label: "|", onPress: () => emit("|") },
    { label: "~", onPress: () => emit("~") },
    { label: "Enter", onPress: () => emit("\r") },
    { label: "Type", onPress: () => props.onFocusComposer?.() },
    { label: "Sel", onPress: () => props.onSelectAll?.() },
    { label: "Copy", onPress: () => props.onCopy?.() },
    { label: "Paste", onPress: () => props.onPaste?.() },
  ];

  return (
    <div class="modkey-row" role="toolbar" aria-label="Terminal modifier keys">
      <For each={keys}>
        {(k) => (
          <button
            type="button"
            class={[k.armed?.() ? "armed" : "", k.extra ? "modkey-extra" : ""]
              .filter(Boolean)
              .join(" ") || undefined}
            aria-pressed={k.armed ? k.armed() : undefined}
            onClick={() => k.onPress()}
          >
            {k.label}
          </button>
        )}
      </For>
    </div>
  );
};

export default ModKeyRow;
