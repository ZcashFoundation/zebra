use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use tracing::Instrument;

use zebra_chain::{parameters::ConsensusBranchId, transaction::Transaction, transparent};
use zebra_state::Utxo;

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
    pub known_utxos: Arc<HashMap<transparent::OutPoint, Utxo>>,
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

        let Request {
            transaction,
            input_index,
            known_utxos,
        } = req;
        let input = &transaction.inputs()[input_index];

        match input {
            transparent::Input::PrevOut { outpoint, .. } => {
                let outpoint = *outpoint;
                let branch_id = self.branch;

                let span = tracing::trace_span!("script", ?outpoint);
                let query =
                    span.in_scope(|| self.state.call(zebra_state::Request::AwaitUtxo(outpoint)));

                async move {
                    tracing::trace!("awaiting outpoint lookup");
                    let utxo = if let Some(output) = known_utxos.get(&outpoint) {
                        tracing::trace!("UXTO in known_utxos, discarding query");
                        output.clone()
                    } else if let zebra_state::Response::Utxo(utxo) = query.await? {
                        utxo
                    } else {
                        unreachable!("AwaitUtxo always responds with Utxo")
                    };
                    tracing::trace!(?utxo, "got UTXO");

                    if transaction.inputs().len() < 20 {
                        zebra_script::is_valid(
                            transaction,
                            branch_id,
                            (input_index as u32, utxo.output),
                        )?;
                        tracing::trace!("script verification succeeded");
                    } else {
                        tracing::debug!(
                            inputs.len = transaction.inputs().len(),
                            "skipping verification of script with many inputs to avoid quadratic work until we fix zebra_script/zcash_script interface"
                        );
                    }

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
