use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, LazyLock},
};

use futures::{future::BoxFuture, FutureExt};
use tower::{service_fn, util::ServiceFn, Service};
use tracing::Instrument;

use zebra_script::CachedFfiTransaction;

use crate::{
    primitives::{
        cache::{CacheKey, Cached, CachedItem, CACHE_CAPACITY},
        spawn_fifo_and_convert,
    },
    BoxError,
};

#[cfg(test)]
mod tests;

/// Asynchronous script verification.
///
/// The verifier asynchronously requests the UTXO a transaction attempts
/// to use as an input, and verifies the script as soon as it becomes
/// available.  This allows script verification to be performed
/// asynchronously, rather than requiring that the entire chain up to
/// the previous block is ready.
///
/// Successful verifications are remembered in a bounded cache shared by every `Verifier`, so an
/// input already verified with the same transaction, consensus branch and spent outputs, usually
/// when its transaction entered the mempool, is not verified again. Failures are never remembered.
///
/// The asynchronous script verification design is documented in [RFC4].
///
/// [RFC4]: https://zebra.zfnd.org/dev/rfcs/0004-asynchronous-script-verification.html
#[derive(Debug, Clone, Default, Copy, PartialEq, Eq)]
pub struct Verifier;

/// A script verification request.
#[derive(Debug)]
pub struct Request {
    /// A cached transaction, in the format required by the script verifier FFI interface.
    pub cached_ffi_transaction: Arc<CachedFfiTransaction>,
    /// The index of an input in `cached_ffi_transaction`, used for verifying this request
    ///
    /// Coinbase inputs are rejected by the script verifier, because they do not spend a UTXO.
    pub input_index: usize,
}

impl CachedItem for Request {
    /// Returns the input index under the digest of everything else the script verification reads.
    fn cache_key(&self) -> Option<CacheKey> {
        self.cached_ffi_transaction
            .context_digest()
            .map(|context| CacheKey::Transparent {
                context,
                input_index: self.input_index,
            })
    }
}

/// The uncached script verification function.
type VerifyFn = fn(Request) -> BoxFuture<'static, Result<(), BoxError>>;

/// The bounded cache of successful script verifications, shared by every [`Verifier`].
static VERIFIER: LazyLock<Cached<ServiceFn<VerifyFn>>> = LazyLock::new(|| {
    let uncached: VerifyFn = verify;
    Cached::new(service_fn(uncached), CACHE_CAPACITY, "transparent_script")
});

/// Verifies one input's script on the Rayon thread pool.
fn verify(req: Request) -> BoxFuture<'static, Result<(), BoxError>> {
    let Request {
        cached_ffi_transaction,
        input_index,
    } = req;

    let span = tracing::trace_span!("script");
    async move {
        // Script verification is CPU-bound so run in Rayon thread
        spawn_fifo_and_convert(move || cached_ffi_transaction.is_valid(input_index)).await?;
        tracing::trace!(input_index, "script verification succeeded");

        Ok(())
    }
    .instrument(span)
    .boxed()
}

impl Service<Request> for Verifier {
    type Response = ();
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        VERIFIER.clone().call(req)
    }
}
