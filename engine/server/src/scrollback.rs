use bytes::Bytes;

/// Byte-oriented ring buffer for PTY scrollback.
///
/// Lines are intentionally not tracked — terminal output is full of partial
/// lines, escape sequences, and binary payloads. Replaying raw bytes into
/// xterm.js is the cleanest path to a faithful redraw on reattach.
///
/// The one boundary the ring *does* respect is where it drops its oldest
/// bytes on overflow. A naive byte-exact cut can slice through the middle of
/// an ANSI/CSI escape sequence or a UTF-8 multibyte character; when the
/// resulting snapshot is replayed into a freshly-reset xterm.js the truncated
/// leading sequence is misparsed — a chopped `\x1b[32m` left as `\x1b[s...`
/// is read as "save cursor" and eats the following characters, so a line like
/// `scope` renders as `cope` (issue #366). To avoid that, overflow trimming
/// advances the cut forward to just past the next newline, so the retained
/// buffer always begins in the terminal's ground state: a line feed never
/// appears inside a CSI/OSC sequence and `0x0A` is never a UTF-8 continuation
/// byte, so a replay that starts there is always safe.
pub struct Scrollback {
    capacity: usize,
    buf: Vec<u8>,
    /// Wall-clock-ish counter of total bytes ever written. Useful for clients
    /// that want a monotonic position cursor.
    total_written: u64,
}

/// Given a minimum number of leading bytes to drop from `buf`, return the
/// index the retained region should start at so that it begins in the
/// terminal's ground state.
///
/// We must drop *at least* `at_least` bytes (the ring-buffer overflow). We
/// then extend the cut forward to just past the next newline, because the byte
/// after a `\n` is the only position we can prove is ground state without
/// replaying the entire prior history: a line feed never appears inside a
/// CSI/OSC escape sequence, and `0x0A` is never a UTF-8 continuation byte, so
/// no multibyte character straddles it. When `at_least` is 0 nothing is being
/// cut off the front (the retained region already starts at a natural stream
/// boundary), so we align nothing. When no newline exists at or after the cut
/// (a single very long line with no line breaks) we fall back to the raw
/// `at_least` cut rather than dropping the whole buffer.
fn newline_aligned_start(buf: &[u8], at_least: usize) -> usize {
    if at_least == 0 {
        return 0;
    }
    if at_least >= buf.len() {
        return buf.len();
    }
    // The newline must sit at index >= at_least - 1 so that dropping through it
    // (its index + 1) still removes at least `at_least` bytes.
    let from = at_least - 1;
    match buf[from..].iter().position(|&b| b == b'\n') {
        Some(rel) => from + rel + 1,
        None => at_least,
    }
}

/// Advance `from` forward to the next UTF-8 sequence start — the first byte at
/// or after `from` that is not a continuation byte (`0b10xx_xxxx`). Used as the
/// fallback boundary when no newline seam exists, so a tail snapshot never
/// begins in the middle of a multibyte character (which would render as
/// mojibake). At most three continuation bytes are ever skipped.
fn utf8_aligned_start(buf: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < buf.len() && (buf[i] & 0xC0) == 0x80 {
        i += 1;
    }
    i
}

impl Scrollback {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            buf: Vec::with_capacity(capacity.min(64 * 1024)),
            total_written: 0,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.total_written = self.total_written.saturating_add(chunk.len() as u64);
        if chunk.len() >= self.capacity {
            // Single chunk bigger than capacity: keep only the tail, aligned
            // forward to a ground-state boundary so replay never starts inside
            // an escape sequence or a UTF-8 char.
            let start = chunk.len() - self.capacity;
            let start = newline_aligned_start(chunk, start);
            self.buf.clear();
            self.buf.extend_from_slice(&chunk[start..]);
            return;
        }
        let overflow = (self.buf.len() + chunk.len()).saturating_sub(self.capacity);
        if overflow > 0 {
            // Drop at least the oldest `overflow` bytes, extending the cut
            // forward to just past the next newline so the retained buffer
            // begins in the terminal's ground state (see the type comment).
            let drop_to = newline_aligned_start(&self.buf, overflow);
            self.buf.drain(..drop_to);
        }
        self.buf.extend_from_slice(chunk);
    }

    /// Snapshot the entire scrollback as a single `Bytes` (cheap-ish clone).
    pub fn snapshot(&self) -> Bytes {
        Bytes::copy_from_slice(&self.buf)
    }

    /// Snapshot at most the last `limit` bytes, trimming the front forward to a
    /// ground-state boundary so replay never begins inside an escape sequence
    /// or a UTF-8 character (#474). A cold-attaching client that sends a tail
    /// hint gets this instead of the whole ring buffer.
    ///
    /// The cut is aligned exactly like the overflow trim in [`Self::push`]:
    /// advance past the next newline when there is one (the only position we
    /// can prove is the terminal's ground state), otherwise fall forward to the
    /// next UTF-8 codepoint start. Both only ever move the cut later, so the
    /// result is always `<= limit`.
    pub fn snapshot_tail(&self, limit: usize) -> Bytes {
        if self.buf.len() <= limit {
            return Bytes::copy_from_slice(&self.buf);
        }
        // Drop at least this many leading bytes so the tail is within `limit`.
        let at_least = self.buf.len() - limit;
        let start = newline_aligned_start(&self.buf, at_least);
        // `newline_aligned_start` may fall back to the raw `at_least` cut when
        // the tail holds no newline; that raw cut can land on a UTF-8
        // continuation byte, so align forward once more. When it already landed
        // just past a newline this is a no-op (that byte is a valid start).
        let start = utf8_aligned_start(&self.buf, start);
        Bytes::copy_from_slice(&self.buf[start..])
    }

    /// Return bytes written after `position` if that absolute cursor is still
    /// represented by the ring buffer.
    pub fn snapshot_since(&self, position: u64) -> Option<Bytes> {
        let retained_start = self.total_written.saturating_sub(self.buf.len() as u64);
        if position < retained_start || position > self.total_written {
            return None;
        }
        let offset = usize::try_from(position - retained_start).ok()?;
        Some(Bytes::copy_from_slice(&self.buf[offset..]))
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn total_written(&self) -> u64 {
        self.total_written
    }

    /// Last `n` bytes (or fewer if buffer smaller). Used by activity heuristics.
    pub fn tail(&self, n: usize) -> &[u8] {
        let start = self.buf.len().saturating_sub(n);
        &self.buf[start..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes_and_snapshots() {
        let mut sb = Scrollback::new(16);
        sb.push(b"hello ");
        sb.push(b"world");
        assert_eq!(&*sb.snapshot(), b"hello world");
        assert_eq!(sb.total_written(), 11);
    }

    #[test]
    fn drops_oldest_on_overflow() {
        let mut sb = Scrollback::new(8);
        sb.push(b"abcdefgh");
        sb.push(b"ij");
        assert_eq!(&*sb.snapshot(), b"cdefghij");
    }

    #[test]
    fn single_chunk_bigger_than_capacity_keeps_tail() {
        let mut sb = Scrollback::new(4);
        sb.push(b"abcdefghij");
        assert_eq!(&*sb.snapshot(), b"ghij");
        assert_eq!(sb.total_written(), 10);
    }

    #[test]
    fn tail_returns_last_n() {
        let mut sb = Scrollback::new(32);
        sb.push(b"the quick brown fox");
        assert_eq!(sb.tail(3), b"fox");
        assert_eq!(sb.tail(100), b"the quick brown fox");
    }

    #[test]
    fn snapshots_only_bytes_after_a_retained_cursor() {
        let mut sb = Scrollback::new(8);
        sb.push(b"abcdefgh");
        sb.push(b"ij");

        assert_eq!(sb.snapshot_since(6).as_deref(), Some(&b"ghij"[..]));
        assert_eq!(sb.snapshot_since(10).as_deref(), Some(&b""[..]));
        assert!(sb.snapshot_since(1).is_none());
        assert!(sb.snapshot_since(11).is_none());
    }

    // ---- cold-attach tail trimming (issue #474) ----

    #[test]
    fn snapshot_tail_returns_whole_buffer_when_within_limit() {
        let mut sb = Scrollback::new(64);
        sb.push(b"short output\n");
        // Limit larger than the buffer: the tail is the whole buffer.
        assert_eq!(&*sb.snapshot_tail(1024), b"short output\n");
    }

    #[test]
    fn snapshot_tail_caps_bytes_at_a_newline_boundary() {
        let mut sb = Scrollback::new(64);
        sb.push(b"line-a\nline-b\nline-c\n"); // 21 bytes
                                              // Keep at most 10 bytes: raw cut drops 11 (index 11 is inside "line-b"),
                                              // so the seam advances forward to just past the next newline (index 14),
                                              // yielding "line-c\n" — <= 10 and starting in ground state.
        let tail = sb.snapshot_tail(10);
        assert_eq!(&*tail, b"line-c\n");
        assert!(tail.len() <= 10);
    }

    #[test]
    fn snapshot_tail_never_splits_a_utf8_codepoint_without_a_newline() {
        let mut sb = Scrollback::new(64);
        // No newline anywhere; 'é' is 0xC3 0xA9. Bytes: a b c d é f g
        sb.push(&[0x61, 0x62, 0x63, 0x64, 0xC3, 0xA9, 0x66, 0x67]); // 8 bytes
                                                                    // Keep at most 4 bytes: raw cut drops 4 -> index 4 = 0xC3 (a lead byte,
                                                                    // already a valid start here). Force a cut onto a continuation byte:
        let tail = sb.snapshot_tail(3);
        // Raw cut drops 5 -> index 5 = 0xA9 (continuation); align forward to 6.
        assert_eq!(&*tail, b"fg");
        assert!(tail.first().map_or(true, |&b| (b & 0xC0) != 0x80));
        assert!(tail.len() <= 3);
    }

    // ---- boundary-safe overflow trimming (issue #366) ----

    #[test]
    fn newline_aligned_start_advances_past_next_newline() {
        // Must drop >= 2 bytes; the next newline is at index 3, so the retained
        // region starts at 4 ("def").
        assert_eq!(newline_aligned_start(b"abc\ndef", 2), 4);
        // Newline exactly at the minimum-drop boundary is honoured, not skipped.
        assert_eq!(newline_aligned_start(b"a\nbcd", 1), 2);
        // No newline at or after the cut: fall back to the raw minimum drop.
        assert_eq!(newline_aligned_start(b"abcdef", 3), 3);
        // Nothing to drop off the front: never advance.
        assert_eq!(newline_aligned_start(b"a\nbc", 0), 0);
        // Dropping everything is clamped to the buffer length.
        assert_eq!(newline_aligned_start(b"ab", 5), 2);
    }

    /// A chopped ANSI escape sequence is exactly issue #366: an overflow cut
    /// that lands inside `\x1b[7m` would leave the buffer starting with the
    /// sequence tail (`7mHI…`), which xterm.js misparses. Alignment must drop
    /// forward past the newline so the retained buffer starts in ground state.
    #[test]
    fn overflow_trim_never_starts_mid_escape_sequence() {
        let mut sb = Scrollback::new(8);
        sb.push(b"\x1b[7mHI\n"); // 7 bytes: inverse-video "HI" then newline
        sb.push(b"XYZ"); // overflow = 2, raw cut would land inside "\x1b[7m"

        let snap = sb.snapshot();
        // The whole chopped-escape line was dropped; replay resumes cleanly.
        assert_eq!(&*snap, b"XYZ");
        // Invariant: the snapshot never begins with the tail of the escape.
        assert_ne!(snap.first(), Some(&b'7'));
        assert_ne!(snap.first(), Some(&0x1b));
    }

    /// The same invariant for the single-chunk-bigger-than-capacity path.
    #[test]
    fn oversized_chunk_trim_starts_after_a_newline() {
        let mut sb = Scrollback::new(6);
        sb.push(b"12345\nABCDE"); // 11 bytes > capacity; raw cut lands at index 5

        // Retained region begins right after the only newline.
        assert_eq!(&*sb.snapshot(), b"ABCDE");
        assert_eq!(sb.total_written(), 11);
    }

    /// A cut through a UTF-8 multibyte character must never leave the snapshot
    /// beginning on a continuation byte (which renders as mojibake).
    #[test]
    fn trim_never_starts_on_a_utf8_continuation_byte() {
        let mut sb = Scrollback::new(3);
        // bytes: 'a' 0xC3 0xA9('é') '\n' 'b'; raw cut at index 2 = the 0xA9
        // continuation byte.
        sb.push(&[0x61, 0xC3, 0xA9, 0x0A, 0x62]);

        let snap = sb.snapshot();
        assert_eq!(&*snap, b"b");
        // Continuation bytes are 0b10xx_xxxx.
        assert!(snap.first().map_or(true, |&b| (b & 0xC0) != 0x80));
    }

    /// Even after a boundary-aligned overflow trim, the retained-cursor
    /// accounting stays exact, so a delta resume neither gaps nor duplicates.
    #[test]
    fn delta_resume_stays_byte_exact_after_aligned_trim() {
        let mut sb = Scrollback::new(8);
        sb.push(b"ab\ncd\n"); // 6 bytes
        sb.push(b"ef\ngh"); // overflow = 3 -> aligned trim drops "ab\n" (3 bytes)

        // Retained buffer starts after the first newline.
        assert_eq!(&*sb.snapshot(), b"cd\nef\ngh");
        assert_eq!(sb.total_written(), 11);

        // A client that had already consumed up to absolute position 6 resumes
        // with exactly the bytes after it, no gap and no repeat.
        assert_eq!(sb.snapshot_since(6).as_deref(), Some(&b"ef\ngh"[..]));
        // A cursor older than what the ring retained forces a full reset.
        assert!(sb.snapshot_since(2).is_none());
    }
}
