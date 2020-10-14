//! Script verification for Zebra.
//!
//! Verification occurs in multiple stages:
//!   - getting transactions from blocks or the mempool (disk- or network-bound)
//!   - context-free verification of scripts, signatures, and proofs (CPU-bound)
//!   - context-dependent verification of transactions against the chain state
//!     (awaits an up-to-date chain)
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.
//!
//! This is an internal module. Use `verify::BlockVerifier` for blocks and their
//! transactions, or `mempool::MempoolTransactionVerifier` for mempool transactions.

use std::{pin::Pin, sync::Arc};

use std::future::Future;
use zebra_chain::{parameters::ConsensusBranchId, transaction::Transaction, transparent};

use crate::BoxError;

/// Internal script verification service.
///
/// After verification, the script future completes. State changes are handled by
/// `BlockVerifier` or `MempoolTransactionVerifier`.
pub struct Verifier<ZS> {
    state: ZS,
    branch: ConsensusBranchId,
}

impl<ZS> Verifier<ZS> {
    pub fn new(state: ZS, branch: ConsensusBranchId) -> Self {
        Self { state, branch }
    }
}

#[derive(Debug)]
struct Request {
    transaction: Arc<Transaction>,
    input_index: usize,
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
                let output = self.state.call(zebra_state::Request::AwaitUtxo(*outpoint));
                let transaction = req.transaction;
                let branch_id = self.branch;
                let input_index = req.input_index;

                async move {
                    let previous_output = match output.await? {
                        zebra_state::Response::Utxo(output) => output,
                        _ => unreachable!("AwaitUtxo always responds with Utxo"),
                    };

                    zebra_script::is_valid(
                        transaction,
                        branch_id,
                        (input_index as u32, previous_output),
                    )?;

                    Ok(())
                }
                .boxed()
            }
            transparent::Input::Coinbase { .. } => {
                async { Err("unexpected coinbase input".into()) }.boxed()
            }
        }
    }
}
