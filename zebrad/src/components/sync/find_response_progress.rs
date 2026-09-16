//! Classification of a response after its accepted hashes finish verification.

use std::sync::{Arc, Mutex};

use zebra_network::FindResponseFeedback;

/// Shared classification state for the unique accepted hashes of one response.
#[derive(Clone)]
pub(super) struct FindResponseProgress {
    inner: Arc<Mutex<Inner>>,
}

/// Remaining work and the one-shot feedback owned by a response.
struct Inner {
    remaining: usize,
    feedback: Option<FindResponseFeedback>,
}

impl FindResponseProgress {
    /// Tracks `hash_count` unique hashes using `feedback`.
    pub(super) fn new(hash_count: usize, feedback: FindResponseFeedback) -> Self {
        assert!(
            hash_count > 0,
            "accepted responses contain at least one hash"
        );

        Self {
            inner: Arc::new(Mutex::new(Inner {
                remaining: hash_count,
                feedback: Some(feedback),
            })),
        }
    }

    /// Records a verified hash, crediting the response only after all hashes verify.
    pub(super) fn record_verified_hash(&self) {
        let feedback = {
            let mut inner = self
                .inner
                .lock()
                .expect("progress updates do not panic while locked");

            if inner.feedback.is_none() {
                return;
            }

            inner.remaining -= 1;

            if inner.remaining == 0 {
                inner.feedback.take()
            } else {
                None
            }
        };

        if let Some(feedback) = feedback {
            feedback.mark_useful();
        }
    }

    /// Leaves missing-hash feedback pending until stall reporting is implemented.
    pub(super) fn record_missing_hash(&self) {}
}

#[cfg(test)]
mod tests;
