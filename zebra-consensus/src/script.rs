use std::{future::Future, pin::Pin, sync::Arc};

use tracing::Instrument;

use zebra_chain::{parameters::NetworkUpgrade, transparent};
use zebra_script::CachedFfiTransaction;

use crate::BoxError;

/// Asynchronous script verification.
///
/// The verifier asynchronously requests the UTXO a transaction attempts
/// to use as an input, and verifies the script as soon as it becomes
/// available.  This allows script verification to be performed
/// asynchronously, rather than requiring that the entire chain up to
/// the previous block is ready.
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
    /// The network upgrade active in the context of this verification request.
    ///
    /// Because the consensus branch ID changes with each network upgrade,
    /// it has to be specified on a per-request basis.
    pub upgrade: NetworkUpgrade,
}

impl tower::Service<Request> for Verifier {
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
        use futures_util::FutureExt;

        let Request {
            cached_ffi_transaction,
            input_index,
            upgrade,
        } = req;
        let input = &cached_ffi_transaction.inputs()[input_index];
        match input {
            transparent::Input::PrevOut { outpoint, .. } => {
                let outpoint = *outpoint;

                // Avoid calling the state service if the utxo is already known
                let span = tracing::trace_span!("script", ?outpoint);

                async move {
                    cached_ffi_transaction.is_valid(upgrade, input_index)?;
                    tracing::trace!("script verification succeeded");

                    Ok(())
                }
                .instrument(span)
                .boxed()
            }
            transparent::Input::Coinbase { .. } => {
                async { Err("unexpected coinbase input".into()) }.boxed()
            }
        }
    }
}
