//! Classification of a response after its accepted hashes finish verification.

use zebra_network::FindResponseFeedback;

/// Shared classification state for the unique accepted hashes of one response.
#[derive(Clone)]
pub(super) struct FindResponseProgress {
    _feedback: FindResponseFeedback,
}

impl FindResponseProgress {
    /// Retains `feedback` while hash accounting awaits implementation.
    pub(super) fn new(_hash_count: usize, feedback: FindResponseFeedback) -> Self {
        Self {
            _feedback: feedback,
        }
    }

    /// Leaves feedback pending until verified-hash accounting is implemented.
    pub(super) fn record_verified_hash(&self) {}
}

#[cfg(test)]
mod tests;
