import { createSignal } from "solid-js";

/**
 * Sticky soft-keyboard modifiers (#236).
 *
 * The phone modkey row can only tap its own buttons; the letters a user wants
 * to chord with come from the OS soft keyboard, which lands in xterm's textarea
 * and never touches the modkey row. So the armed state has to live above both:
 * the modkey row arms it, and the terminal input path (Terminal.dispatchInput)
 * consumes it for the very next printable character. Tap `Ctrl`, then `r` on
 * the soft keyboard → `^R` reaches the PTY.
 *
 * A single module-level store is deliberate: there is one soft keyboard and one
 * focused terminal, and the arm must be readable from a plain function far from
 * any component. Plain signals (no effects) outside a root are safe.
 */

/** How long an armed sticky modifier waits before disarming itself. */
export const STICKY_MOD_TIMEOUT_MS = 5_000;

const [ctrlArmedSignal, setCtrlArmed] = createSignal(false);
const [altArmedSignal, setAltArmed] = createSignal(false);

let ctrlTimer: ReturnType<typeof setTimeout> | null = null;
let altTimer: ReturnType<typeof setTimeout> | null = null;

/** Reactive getter: is sticky Ctrl currently armed? */
export const ctrlArmed = ctrlArmedSignal;
/** Reactive getter: is sticky Alt currently armed? */
export const altArmed = altArmedSignal;

function clearCtrlTimer() {
  if (ctrlTimer !== null) {
    clearTimeout(ctrlTimer);
    ctrlTimer = null;
  }
}

function clearAltTimer() {
  if (altTimer !== null) {
    clearTimeout(altTimer);
    altTimer = null;
  }
}

/** Toggle sticky Ctrl. Arming (re)starts the auto-disarm timeout. */
export function armCtrl() {
  const next = !ctrlArmedSignal();
  setCtrlArmed(next);
  clearCtrlTimer();
  if (next) {
    ctrlTimer = setTimeout(() => {
      ctrlTimer = null;
      setCtrlArmed(false);
    }, STICKY_MOD_TIMEOUT_MS);
  }
}

/** Toggle sticky Alt (ESC prefix). Arming (re)starts the auto-disarm timeout. */
export function armAlt() {
  const next = !altArmedSignal();
  setAltArmed(next);
  clearAltTimer();
  if (next) {
    altTimer = setTimeout(() => {
      altTimer = null;
      setAltArmed(false);
    }, STICKY_MOD_TIMEOUT_MS);
  }
}

/** Clear both sticky modifiers and cancel their timeouts. */
export function disarmMods() {
  clearCtrlTimer();
  clearAltTimer();
  setCtrlArmed(false);
  setAltArmed(false);
}

/**
 * The control byte a printable character maps to under Ctrl, or `null` when
 * Ctrl+char has no meaningful control code (send it literally instead).
 */
export function ctrlByte(ch: string): string | null {
  if (ch.length !== 1) return null;
  const code = ch.charCodeAt(0);
  // Ctrl-A..Ctrl-Z from either case → 0x01..0x1a.
  if (code >= 97 && code <= 122) return String.fromCharCode(code - 96);
  if (code >= 65 && code <= 90) return String.fromCharCode(code - 64);
  // The classic Ctrl-punctuation controls.
  switch (ch) {
    case " ":
    case "@":
      return "\x00";
    case "[":
      return "\x1b";
    case "\\":
      return "\x1c";
    case "]":
      return "\x1d";
    case "^":
      return "\x1e";
    case "_":
      return "\x1f";
    case "?":
      return "\x7f";
    default:
      return null;
  }
}

/**
 * Consume any armed sticky modifier for a single printable input character.
 *
 * Only a lone printable char triggers a chord: multi-byte sequences (arrow
 * keys, pastes) and control bytes pass through untouched and leave the arm in
 * place, so an accidental arm does not eat a paste. On a successful chord both
 * modifiers disarm.
 */
export function applyStickyMods(
  data: string | ArrayBuffer,
): string | ArrayBuffer {
  if (typeof data !== "string") return data;
  const ctrl = ctrlArmedSignal();
  const alt = altArmedSignal();
  if (!ctrl && !alt) return data;
  if (data.length !== 1) return data;
  const code = data.charCodeAt(0);
  // Control characters (Esc, Tab, Enter, DEL) are not chord targets.
  if (code < 0x20 || code === 0x7f) return data;
  let out = data;
  if (ctrl) out = ctrlByte(data) ?? data;
  if (alt) out = `\x1b${out}`;
  disarmMods();
  return out;
}
