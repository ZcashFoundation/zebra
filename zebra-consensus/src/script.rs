use std::{future::Future, pin::Pin, sync::Arc};

use tracing::Instrument;

use zebra_chain::{parameters::ConsensusBranchId, transaction::Transaction, transparent};

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
#[derive(Debug, Clone)]
pub struct Verifier<ZS> {
    state: ZS,
    branch: ConsensusBranchId,
}

impl<ZS> Verifier<ZS> {
    pub fn new(state: ZS, branch: ConsensusBranchId) -> Self {
        Self { state, branch }
    }
}

/// A script verification request.
///
/// Ideally, this would supply only an `Outpoint` and the unlock script,
/// rather than the entire `Transaction`, but we call a C++
/// implementation, and its FFI requires the entire transaction.
/// At some future point, we could investigate reducing the size of the
/// request.
#[derive(Debug)]
pub struct Request {
    pub transaction: Arc<Transaction>,
    pub input_index: usize,
}

impl<ZS> tower::Service<Request> for Verifier<ZS>
where
    ZS: tower::Service<zebra_state::Request, Response = zebra_state::Response, Error = BoxError>,
    ZS::Future: Send + 'static,
{
    type Response = ();
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.state.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        use futures_util::FutureExt;

        let input = &req.transaction.inputs()[req.input_index];

        match input {
            transparent::Input::PrevOut { outpoint, .. } => {
                let outpoint = *outpoint;
                let transaction = req.transaction;
                let branch_id = self.branch;
                let input_index = req.input_index;

                let span = tracing::trace_span!("script", ?outpoint);
                let output =
                    span.in_scope(|| self.state.call(zebra_state::Request::AwaitUtxo(outpoint)));

                async move {
                    tracing::trace!("awaiting outpoint lookup");
                    let previous_output = match output.await? {
                        zebra_state::Response::Utxo(output) => output,
                        _ => unreachable!("AwaitUtxo always responds with Utxo"),
                    };
                    tracing::trace!(?previous_output, "got UTXO");

                    zebra_script::is_valid(
                        transaction,
                        branch_id,
                        (input_index as u32, previous_output),
                    )?;
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
