//! The budget for the address records a version 2 peer announces (draft,
//! "Misbehavior and Banning": rate-limit address records).
//!
//! The connection task owns one [`AddrTokenBucket`] and applies it to every
//! announced record, whichever of the peer's address announcement streams
//! carried it: the draft lets a peer have two such streams read at once, so
//! the budget can't live in a stream reader.

use tokio::time::Instant;

/// Address records a peer may announce per second, once its burst is spent
/// (zcashd and Bitcoin Core: one record every 10 seconds).
pub(super) const ADDR_TOKENS_PER_SECOND: f64 = 0.1;

/// The most address records a peer may announce in a burst (zcashd and
/// Bitcoin Core: one maximum-size `addr` message).
pub(super) const MAX_ADDR_TOKENS: f64 = 1000.0;

/// The tokens a new connection starts with: a peer can announce its own
/// address at once, but nothing more (zcashd and Bitcoin Core).
pub(super) const INITIAL_ADDR_TOKENS: f64 = 1.0;

/// A token bucket for the address records one peer announces. Records over
/// the budget are dropped without blaming the peer, since a conforming peer
/// with a different budget can exceed it.
#[derive(Clone, Debug)]
pub(super) struct AddrTokenBucket {
    /// The records the peer may announce now.
    tokens: f64,
    /// When `tokens` was last refilled.
    refilled_at: Instant,
}

impl AddrTokenBucket {
    /// A bucket with the initial tokens, refilled from `now`.
    pub(super) fn new(now: Instant) -> Self {
        AddrTokenBucket {
            tokens: INITIAL_ADDR_TOKENS,
            refilled_at: now,
        }
    }

    /// Takes one token for a record announced at `now`, returning whether
    /// the record is within the budget.
    pub(super) fn try_take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.refilled_at);
        self.refilled_at = self.refilled_at.max(now);
        self.tokens =
            (self.tokens + elapsed.as_secs_f64() * ADDR_TOKENS_PER_SECOND).min(MAX_ADDR_TOKENS);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// The tokens available after the last refill.
    #[cfg(test)]
    pub(super) fn tokens(&self) -> f64 {
        self.tokens
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// One record is allowed at once, then one every 10 seconds.
    #[tokio::test(start_paused = true)]
    async fn bucket_allows_a_burst_of_one_then_refills_slowly() {
        let mut bucket = AddrTokenBucket::new(Instant::now());
        assert!(
            bucket.try_take(Instant::now()),
            "the first record is allowed"
        );
        assert!(
            !bucket.try_take(Instant::now()),
            "the second is over budget"
        );

        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(!bucket.try_take(Instant::now()), "9 s refill less than one");
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(bucket.try_take(Instant::now()), "10 s refill one token");
        assert!(!bucket.try_take(Instant::now()));

        tokio::time::advance(Duration::from_secs(50)).await;
        let allowed = (0..10).filter(|_| bucket.try_take(Instant::now())).count();
        assert_eq!(allowed, 5, "50 s refill five tokens");
    }

    /// An idle peer's budget is capped at one maximum-size burst.
    #[tokio::test(start_paused = true)]
    async fn bucket_is_capped() {
        let mut bucket = AddrTokenBucket::new(Instant::now());
        tokio::time::advance(Duration::from_secs(1_000_000)).await;

        let allowed = (0..2000)
            .filter(|_| bucket.try_take(Instant::now()))
            .count();
        // The cap is a whole number of tokens, far below `usize::MAX`.
        assert_eq!(allowed, MAX_ADDR_TOKENS as usize);
        assert!(bucket.tokens() < 1.0);
    }
}
