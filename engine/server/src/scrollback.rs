use bytes::Bytes;

/// Byte-oriented ring buffer for PTY scrollback.
///
/// Lines are intentionally not tracked — terminal output is full of partial
/// lines, escape sequences, and binary payloads. Replaying raw bytes into
/// xterm.js is the cleanest path to a faithful redraw on reattach.
pub struct Scrollback {
    capacity: usize,
    buf: Vec<u8>,
    /// Wall-clock-ish counter of total bytes ever written. Useful for clients
    /// that want a monotonic position cursor.
    total_written: u64,
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
            // Single chunk bigger than capacity: keep only the tail.
            let start = chunk.len() - self.capacity;
            self.buf.clear();
            self.buf.extend_from_slice(&chunk[start..]);
            return;
        }
        let overflow = (self.buf.len() + chunk.len()).saturating_sub(self.capacity);
        if overflow > 0 {
            // Drop the oldest `overflow` bytes.
            self.buf.drain(..overflow);
        }
        self.buf.extend_from_slice(chunk);
    }

    /// Snapshot the entire scrollback as a single `Bytes` (cheap-ish clone).
    pub fn snapshot(&self) -> Bytes {
        Bytes::copy_from_slice(&self.buf)
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
}
