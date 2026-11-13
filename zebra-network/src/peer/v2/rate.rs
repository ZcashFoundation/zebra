//! Rate limits for the version 2 protocol that never make their caller wait.

use std::time::Duration;

use tokio::time::Instant;

/// A token bucket: up to `burst` tokens, refilled at one per `interval`.
///
/// Taking a token never waits, so a caller that is over its rate decides at
/// once what to do instead: refuse, drop, or stop reading until the next
/// token.
#[derive(Clone, Debug)]
pub struct TokenBucket {
    /// The time one token takes to refill.
    interval: Duration,
    /// The most tokens the bucket holds.
    burst: u32,
    /// The tokens in the bucket as of `refilled_at`.
    tokens: u32,
    /// The time the tokens were last counted: when the bucket is not full,
    /// the time the next token started to refill.
    refilled_at: Instant,
}

impl TokenBucket {
    /// Returns a full bucket of `burst` tokens, at least one, that refills
    /// one token per `interval`.
    pub fn new(interval: Duration, burst: u32, now: Instant) -> Self {
        let burst = burst.max(1);
        TokenBucket {
            interval,
            burst,
            tokens: burst,
            refilled_at: now,
        }
    }

    /// Adds the tokens that refilled up to `now`.
    fn refill(&mut self, now: Instant) {
        let missing = self.burst - self.tokens;
        // A zero interval never limits: the division below would be by zero.
        let refilled = match self.interval.as_nanos() {
            0 => missing,
            interval => {
                let elapsed = now.saturating_duration_since(self.refilled_at).as_nanos();
                u32::try_from(elapsed / interval)
                    .unwrap_or(u32::MAX)
                    .min(missing)
            }
        };
        self.tokens += refilled;
        if self.tokens == self.burst {
            // A full bucket earns nothing, so the next token refills from when one is taken.
            self.refilled_at = now;
        } else {
            // `refilled` is less than `missing` here, so the elapsed time covers the product.
            self.refilled_at += self.interval.saturating_mul(refilled);
        }
    }

    /// Returns `true` if a token can be taken at `now`.
    pub fn is_available(&mut self, now: Instant) -> bool {
        self.refill(now);
        self.tokens > 0
    }

    /// Takes a token at `now`, and returns `false` if there is none.
    pub fn try_take(&mut self, now: Instant) -> bool {
        if !self.is_available(now) {
            return false;
        }
        self.tokens -= 1;
        true
    }
}
