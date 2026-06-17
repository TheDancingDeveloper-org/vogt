//! Keystroke → terminal byte-sequence mapping.
//!
//! Pure (no GPUI types) so it is unit-testable. The GPUI terminal view adapts
//! `gpui::Keystroke` into a [`KeyInput`] and forwards the resulting bytes to the
//! PTY over the attach WebSocket. Logic ported from `rdpapp::term_key_to_bytes`.

/// A platform-neutral view of a key press.
pub struct KeyInput<'a> {
    /// Canonical key name, e.g. "a", "up", "enter", "f1".
    pub key: &'a str,
    /// Resolved character for printable keys (handles shift/compose), if any.
    pub key_char: Option<&'a str>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Super/Windows/Cmd key — never produces terminal bytes here.
    pub platform: bool,
}

/// Translate a key press into the bytes to write to the PTY, or `None` if the
/// key produces no input (pure modifier, reserved app shortcut, etc.).
pub fn key_to_bytes(k: &KeyInput<'_>) -> Option<Vec<u8>> {
    let key = k.key;

    // Reserved client shortcuts (Ctrl+Shift+C/V/F) are handled by the UI and
    // must NOT be sent to the PTY.
    if k.ctrl && k.shift && !k.alt && !k.platform && matches!(key, "c" | "v" | "f") {
        return None;
    }

    // Special keys → VT100 escape sequences.
    let special: Option<&[u8]> = match key {
        "up" => Some(b"\x1b[A"),
        "down" => Some(b"\x1b[B"),
        "right" => Some(b"\x1b[C"),
        "left" => Some(b"\x1b[D"),
        "home" => Some(b"\x1b[H"),
        "end" => Some(b"\x1b[F"),
        "insert" => Some(b"\x1b[2~"),
        "delete" => Some(b"\x1b[3~"),
        "pageup" => Some(b"\x1b[5~"),
        "pagedown" => Some(b"\x1b[6~"),
        "f1" => Some(b"\x1bOP"),
        "f2" => Some(b"\x1bOQ"),
        "f3" => Some(b"\x1bOR"),
        "f4" => Some(b"\x1bOS"),
        "f5" => Some(b"\x1b[15~"),
        "f6" => Some(b"\x1b[17~"),
        "f7" => Some(b"\x1b[18~"),
        "f8" => Some(b"\x1b[19~"),
        "f9" => Some(b"\x1b[20~"),
        "f10" => Some(b"\x1b[21~"),
        "f11" => Some(b"\x1b[23~"),
        "f12" => Some(b"\x1b[24~"),
        "return" | "enter" => Some(b"\r"),
        "backspace" => Some(b"\x7f"),
        "tab" => Some(b"\t"),
        "escape" => Some(b"\x1b"),
        _ => None,
    };
    if let Some(seq) = special {
        return Some(seq.to_vec());
    }

    // Ctrl+Space → NUL. GPUI names the space key "space" on Windows (and " " on
    // some platforms), so handle both spellings before the len==1 check below.
    if k.ctrl && (key == "space" || key == " ") {
        return Some(vec![0]);
    }

    // Ctrl+letter / Ctrl+symbol → control byte (0x01-0x1f).
    if k.ctrl && key.len() == 1 {
        let c = key.chars().next()?.to_ascii_lowercase();
        match c {
            'a'..='z' => return Some(vec![c as u8 - b'a' + 1]),
            '[' => return Some(b"\x1b".to_vec()),
            '\\' => return Some(b"\x1c".to_vec()),
            ']' => return Some(b"\x1d".to_vec()),
            ' ' => return Some(vec![0]), // Ctrl+Space → NUL
            _ => {}
        }
    }

    // Skip pure modifiers.
    if matches!(
        key,
        "shift" | "control" | "ctrl" | "alt" | "platform" | "super" | "function" | "capslock"
    ) {
        return None;
    }

    // Printable: prefer key_char (handles shift/compose), fall back to key.
    if !k.ctrl && !k.platform {
        if let Some(key_char) = k.key_char {
            if !key_char.is_empty() {
                return Some(prefix_alt(k.alt, key_char.as_bytes()));
            }
        }
        // The Windows GPUI backend names the spacebar "space" and does not
        // populate `key_char` for it, so the len==1 fallback below would miss
        // it. Handle the named spelling explicitly so plain space still types.
        if key == "space" {
            return Some(prefix_alt(k.alt, b" "));
        }
        if key.len() == 1 {
            let c = key.chars().next()?;
            if c.is_ascii_graphic() || c == ' ' {
                let byte = if k.shift {
                    c.to_ascii_uppercase() as u8
                } else {
                    c as u8
                };
                return Some(prefix_alt(k.alt, &[byte]));
            }
        }
    }

    None
}

/// Alt/Meta prefixes the sequence with ESC (xterm meta-sends-escape).
fn prefix_alt(alt: bool, bytes: &[u8]) -> Vec<u8> {
    if alt {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        out.push(0x1b);
        out.extend_from_slice(bytes);
        out
    } else {
        bytes.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k<'a>(key: &'a str, key_char: Option<&'a str>) -> KeyInput<'a> {
        KeyInput {
            key,
            key_char,
            ctrl: false,
            alt: false,
            shift: false,
            platform: false,
        }
    }

    #[test]
    fn arrows_map_to_csi() {
        assert_eq!(key_to_bytes(&k("up", None)).unwrap(), b"\x1b[A");
        assert_eq!(key_to_bytes(&k("left", None)).unwrap(), b"\x1b[D");
    }

    #[test]
    fn enter_and_backspace() {
        assert_eq!(key_to_bytes(&k("enter", None)).unwrap(), b"\r");
        assert_eq!(key_to_bytes(&k("backspace", None)).unwrap(), b"\x7f");
    }

    #[test]
    fn ctrl_c_is_etx() {
        let mut ki = k("c", None);
        ki.ctrl = true;
        assert_eq!(key_to_bytes(&ki).unwrap(), vec![3]);
    }

    #[test]
    fn ctrl_shift_c_is_swallowed() {
        let mut ki = k("c", None);
        ki.ctrl = true;
        ki.shift = true;
        assert_eq!(key_to_bytes(&ki), None);
    }

    #[test]
    fn printable_prefers_key_char() {
        assert_eq!(key_to_bytes(&k("a", Some("A"))).unwrap(), b"A");
    }

    #[test]
    fn alt_prefixes_escape() {
        let mut ki = k("b", Some("b"));
        ki.alt = true;
        assert_eq!(key_to_bytes(&ki).unwrap(), vec![0x1b, b'b']);
    }

    #[test]
    fn pure_modifier_is_none() {
        assert_eq!(key_to_bytes(&k("shift", None)), None);
    }

    #[test]
    fn space_types_via_named_key_without_key_char() {
        // Windows GPUI backend: key == "space", key_char == None.
        assert_eq!(key_to_bytes(&k("space", None)).unwrap(), b" ");
        // Some backends supply the literal char as key_char.
        assert_eq!(key_to_bytes(&k("space", Some(" "))).unwrap(), b" ");
        // Other backends name the key " " directly.
        assert_eq!(key_to_bytes(&k(" ", Some(" "))).unwrap(), b" ");
    }

    #[test]
    fn alt_space_prefixes_escape() {
        let mut ki = k("space", None);
        ki.alt = true;
        assert_eq!(key_to_bytes(&ki).unwrap(), vec![0x1b, b' ']);
    }

    #[test]
    fn ctrl_space_still_nul_for_named_key() {
        let mut ki = k("space", None);
        ki.ctrl = true;
        assert_eq!(key_to_bytes(&ki).unwrap(), vec![0]);
    }
}
