//! Deterministic clock helpers for Zakura limit tests.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

pub use crate::zakura::{Clock, RealClock};

use tokio::time::Instant;

/// Manually advanced test clock.
#[derive(Clone, Debug)]
pub struct TestClock {
    now: Arc<Mutex<Instant>>,
}

impl TestClock {
    /// Create a test clock initialized to `Instant::now()`.
    pub fn new() -> Self {
        Self {
            now: Arc::new(Mutex::new(Instant::now())),
        }
    }

    /// Advance the clock by `duration`.
    pub fn advance(&self, duration: Duration) {
        let mut now = self
            .now
            .lock()
            .expect("test clock mutex should not be poisoned");
        *now += duration;
    }
}

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self
            .now
            .lock()
            .expect("test clock mutex should not be poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_advances_deterministically() {
        let clock = TestClock::new();
        let start = clock.now();

        clock.advance(Duration::from_secs(2));

        assert_eq!(clock.now().duration_since(start), Duration::from_secs(2));
    }
}
