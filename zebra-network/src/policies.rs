use std::pin::Pin;

use futures::{Future, FutureExt};
use tower::retry::Policy;

/// A very basic retry policy with a limited number of retry attempts.
///
/// XXX Remove this when https://github.com/tower-rs/tower/pull/414 lands.
#[derive(Clone, Debug)]
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
    type Future = Pin<Box<dyn Future<Output = Self> + Send + 'static>>;

    fn retry(&self, req: &Req, result: Result<&Res, &E>) -> Option<Self::Future> {
        if let Err(e) = result {
            if self.remaining_tries > 0 {
                tracing::debug!(?req, ?e, remaining_tries = self.remaining_tries, "retrying");
                let remaining_tries = self.remaining_tries - 1;

                Some(
                    async move {
                        // Let other tasks run, so we're more likely to choose a different peer,
                        // and so that any notfound inv entries win the race to the PeerSet.
                        //
                        // TODO: move syncer retries into the PeerSet,
                        //       so we always choose different peers (#3235)
                        tokio::task::yield_now().await;
                        RetryLimit { remaining_tries }
                    }
                    .boxed(),
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
