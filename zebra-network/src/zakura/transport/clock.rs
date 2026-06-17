//! Clock abstraction used by Zakura transport rate-limit logic.

use tokio::time::Instant;

/// Clock used by Zakura rate-limit logic.
pub trait Clock: Clone + Send + Sync + 'static {
    /// Return the current monotonic instant.
    fn now(&self) -> Instant;
}

/// Production clock backed by [`Instant::now`].
#[derive(Copy, Clone, Debug, Default)]
pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
