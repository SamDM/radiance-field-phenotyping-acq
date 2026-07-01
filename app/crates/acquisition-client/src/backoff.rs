use std::time::Duration;

const INITIAL_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(5);

/// Tracks reconnect attempt count and computes exponential-backoff delays,
/// doubling from 1s and plateauing at 5s. Resets after a successful connection.
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    pub fn new() -> Self {
        Self { attempt: 0 }
    }

    /// Delay to use for the upcoming wait (1s, 2s, 4s, then plateaus at 5s).
    pub fn current_delay(&self) -> Duration {
        INITIAL_DELAY
            .checked_mul(1u32.checked_shl(self.attempt).unwrap_or(u32::MAX))
            .unwrap_or(MAX_DELAY)
            .min(MAX_DELAY)
    }

    /// True once the delay has plateaued at `MAX_DELAY` — used to stop
    /// logging every failed attempt once the backoff is no longer changing.
    pub fn is_at_cap(&self) -> bool {
        self.current_delay() >= MAX_DELAY
    }

    /// 1-based attempt number, for display.
    pub fn attempt_number(&self) -> u32 {
        self.attempt + 1
    }

    /// Call after a failed connect or a dropped connection.
    pub fn record_failure(&mut self) {
        self.attempt = self.attempt.saturating_add(1);
    }

    /// Call after a connection is successfully established.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}
