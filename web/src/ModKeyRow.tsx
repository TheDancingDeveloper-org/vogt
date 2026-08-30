import { Component, For, createSignal } from "solid-js";
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
  const [expanded, setExpanded] = createSignal(false);
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
    { label: "^D", onPress: () => props.send("\x04"), extra: true },
    { label: "^L", onPress: () => props.send("\x0c"), extra: true },
    { label: "Bksp", onPress: () => props.send("\x7f"), extra: true },
    { label: "←", onPress: () => emit("\x1b[D"), extra: true },
    { label: "↑", onPress: () => emit("\x1b[A") },
    { label: "↓", onPress: () => emit("\x1b[B") },
    { label: "→", onPress: () => emit("\x1b[C"), extra: true },
    { label: "Home", onPress: () => emit("\x1b[H"), extra: true },
    { label: "End", onPress: () => emit("\x1b[F"), extra: true },
    { label: "PgUp", onPress: () => emit("\x1b[5~"), extra: true },
    { label: "PgDn", onPress: () => emit("\x1b[6~"), extra: true },
    { label: "/", onPress: () => emit("/"), extra: true },
    { label: "|", onPress: () => emit("|"), extra: true },
    { label: "~", onPress: () => emit("~"), extra: true },
    { label: "Enter", onPress: () => emit("\r"), extra: true },
    { label: "Type", onPress: () => props.onFocusComposer?.(), extra: true },
    { label: "Sel", onPress: () => props.onSelectAll?.(), extra: true },
    { label: "Copy", onPress: () => props.onCopy?.(), extra: true },
    { label: "Paste", onPress: () => props.onPaste?.(), extra: true },
  ];

  return (
    <div class={`modkey-row${expanded() ? " modkey-row--expanded" : ""}`} role="toolbar" aria-label="Terminal modifier keys">
      <For each={keys.filter((key) => !key.extra)}>
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
      <button
        type="button"
        class="modkey-more"
        aria-label="More terminal keys"
        aria-expanded={expanded()}
        onClick={() => setExpanded((value) => !value)}
      >
        ⋯
      </button>
      <For each={keys.filter((key) => key.extra)}>
        {(k) => (
          <button
            type="button"
            class="modkey-extra"
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
