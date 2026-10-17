//! Block-local completion of crypto queueing, independent of verification results.
//!
//! Unlike a yielded notifier, these guards wait for actual queue admission, including cache
//! misses delayed by backpressure.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

use parking_lot::Mutex;

use super::{ed25519, halo2, sapling};

#[cfg(test)]
pub(crate) mod tests;

type Pending = Arc<Mutex<Progress>>;

static BLOCKS: LazyLock<Mutex<HashMap<usize, Pending>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

tokio::task_local! {
    static CURRENT: Pending;
}

/// The batch workers used by block verification. Sapling and Halo2 include their signatures.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Verifier {
    Ed25519,
    Sapling,
    Halo2PreNu6_2,
    Halo2Nu6_2,
    Halo2Nu6_3,
}

#[derive(Debug)]
struct Progress {
    pending: usize,
    queued: u8,
}

/// Registration held until the block's transaction futures have been drained or cancelled.
#[derive(Debug)]
pub(crate) struct BlockFlush {
    key: usize,
}

impl BlockFlush {
    /// Registers only blocks containing shielded data, using their existing shared allocation.
    ///
    /// Count transactions before dispatch: a Tower buffer may delay their service calls. Each
    /// transaction replaces its slot with request guards before releasing it.
    pub(crate) fn register<T>(context: &Arc<T>, shielded_transactions: usize) -> Self {
        #[cfg(test)]
        tests::record_registry_access();
        let key = key(context);
        let pending = Arc::new(Mutex::new(Progress {
            pending: shielded_transactions,
            queued: 0,
        }));
        BLOCKS.lock().insert(key, pending);
        Self { key }
    }
}

impl Drop for BlockFlush {
    fn drop(&mut self) {
        BLOCKS.lock().remove(&self.key);
    }
}

fn key<T>(context: &Arc<T>) -> usize {
    // The allocation address is an opaque identity, never dereferenced or used for arithmetic.
    Arc::as_ptr(context) as usize
}

/// One transaction or shielded request that has not finished queueing its batch work.
#[derive(Debug)]
pub(crate) struct Queueing(Pending);

impl Queueing {
    fn new(pending: Pending) -> Self {
        pending.lock().pending += 1;
        Self(pending)
    }

    /// Looks up a registered shielded transaction before its future is constructed.
    pub(crate) fn for_transaction<T>(context: &Arc<T>) -> Option<Self> {
        #[cfg(test)]
        tests::record_registry_access();
        BLOCKS.lock().get(&key(context)).cloned().map(Self)
    }

    /// Attaches child request guards during synchronous construction, without yielding.
    pub(crate) fn scope<T>(&self, f: impl FnOnce() -> T) -> T {
        CURRENT.sync_scope(self.0.clone(), f)
    }

    /// Tracks a shielded request only when constructed by a registered block transaction.
    pub(crate) fn for_request() -> Option<Self> {
        CURRENT.try_with(|pending| Self::new(pending.clone())).ok()
    }

    /// Records admission to the batch queue, not construction of a verification future.
    pub(crate) fn enqueued(self, verifier: Verifier) {
        // There are only five verifier kinds, so every discriminant fits the bit mask.
        self.0.lock().queued |= 1 << verifier as u8;
    }
}

impl Drop for Queueing {
    fn drop(&mut self) {
        let queued = {
            let mut progress = self.0.lock();
            progress.pending -= 1;
            if progress.pending != 0 {
                return;
            }
            progress.queued
        };

        // Never hold the progress lock while acquiring service queue capacity.
        for (verifier, flush) in [
            (Verifier::Ed25519, flush_ed25519 as fn() -> _),
            (Verifier::Sapling, sapling::try_flush),
            (Verifier::Halo2PreNu6_2, flush_halo2_pre_nu6_2),
            (Verifier::Halo2Nu6_2, flush_halo2_nu6_2),
            (Verifier::Halo2Nu6_3, flush_halo2_nu6_3),
        ] {
            // Every discriminant fits the five-bit mask above.
            if queued & (1 << verifier as u8) != 0 {
                #[cfg(not(test))]
                let result = flush();
                #[cfg(test)]
                let result = tests::FLUSH
                    .try_with(|flush| flush(verifier))
                    .unwrap_or_else(|_| flush());
                if let Err(error) = result {
                    tracing::trace!(?verifier, ?error, "could not queue block crypto flush");
                }
            }
        }
    }
}

fn flush_ed25519() -> Result<bool, crate::BoxError> {
    ed25519::VERIFIER.primary().clone().try_flush()
}

fn flush_halo2_pre_nu6_2() -> Result<bool, crate::BoxError> {
    halo2::VERIFIER_PRE_NU6_2.primary().clone().try_flush()
}

fn flush_halo2_nu6_2() -> Result<bool, crate::BoxError> {
    halo2::VERIFIER_NU6_2.primary().clone().try_flush()
}

fn flush_halo2_nu6_3() -> Result<bool, crate::BoxError> {
    halo2::VERIFIER_NU6_3_ONWARD.primary().clone().try_flush()
}
