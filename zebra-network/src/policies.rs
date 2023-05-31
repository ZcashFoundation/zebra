use std::pin::Pin;

use futures::{Future, FutureExt};
use tower::retry::Policy;

/// A very basic retry policy with a limited number of retry attempts.
///
/// TODO: Remove this when <https://github.com/tower-rs/tower/pull/414> lands.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct RetryLimit {
    remaining_tries: usize,
}

impl RetryLimit {
    /// Create a policy with the given number of retry attempts.
    pub fn new(retry_attempts: usize) -> Self {
        RetryLimit {
            remaining_tries: retry_attempts,
        }
    }
}

impl<Req: Clone + std::fmt::Debug, Res, E: std::fmt::Debug> Policy<Req, Res, E> for RetryLimit {
    type Future = Pin<Box<dyn Future<Output = Self> + Send + Sync + 'static>>;

    fn retry(&self, req: &Req, result: Result<&Res, &E>) -> Option<Self::Future> {
        if let Err(e) = result {
            if self.remaining_tries > 0 {
                tracing::debug!(?req, ?e, remaining_tries = self.remaining_tries, "retrying");

                let remaining_tries = self.remaining_tries - 1;
                let retry_outcome = RetryLimit { remaining_tries };

                Some(
                    // Let other tasks run, so we're more likely to choose a different peer,
                    // and so that any notfound inv entries win the race to the PeerSet.
                    //
                    // # Security
                    //
                    // We want to choose different peers for retries, so we have a better chance of getting each block.
                    // This is implemented by the connection state machine sending synthetic `notfound`s to the
                    // `InventoryRegistry`, as well as forwarding actual `notfound`s from peers.
                    Box::pin(tokio::task::yield_now().map(move |()| retry_outcome)),
                )
            } else {
                None
            }
        } else {
            None
        }
    }

    fn clone_request(&self, req: &Req) -> Option<Req> {
        Some(req.clone())
    }
}
