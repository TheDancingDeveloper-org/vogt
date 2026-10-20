//! Reconnect delays for the event stream: doubling from a floor to a cap,
//! reset once a connection opens.

use std::time::Duration;

/// Exponential backoff state.
#[derive(Clone, Debug)]
pub struct Backoff {
    floor: Duration,
    cap: Duration,
    attempt: u32,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(500), Duration::from_secs(30))
    }
}

impl Backoff {
    /// Backoff from `floor`, never longer than `cap`.
    pub fn new(floor: Duration, cap: Duration) -> Self {
        Self { floor, cap, attempt: 0 }
    }

    /// The delay before the next try, and count the try.
    pub fn next_delay(&mut self) -> Duration {
        let factor = 1u32.checked_shl(self.attempt.min(16)).unwrap_or(u32::MAX);
        self.attempt = self.attempt.saturating_add(1);
        self.floor.saturating_mul(factor).min(self.cap)
    }

    /// Tries since the last reset.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// A connection opened: start again from the floor.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_caps_and_resets() {
        let mut b = Backoff::new(Duration::from_millis(500), Duration::from_secs(4));
        let delays: Vec<_> = (0..6).map(|_| b.next_delay().as_millis()).collect();
        assert_eq!(delays, vec![500, 1000, 2000, 4000, 4000, 4000]);
        assert_eq!(b.attempt(), 6);
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_millis(500));
    }

    #[test]
    fn huge_attempt_counts_do_not_overflow() {
        let mut b = Backoff::default();
        for _ in 0..100 {
            b.next_delay();
        }
        assert_eq!(b.next_delay(), Duration::from_secs(30));
    }
}
