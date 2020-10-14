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
///
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
            transparent::Input::Coinbase { .. } => unimplemented!(
                "how should we handle verifying coinbase transactions in the script::Verifier?"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use color_eyre::Report;

    use zebra_chain::{
        block::Block, parameters::Network, parameters::NetworkUpgrade,
        serialization::ZcashDeserializeInto, transparent,
    };
    use zebra_test::transcript::{TransError, Transcript};

    #[tokio::test]
    async fn happy_path() -> Result<(), Report> {
        happy_path_test().await
    }

    #[spandoc::spandoc]
    async fn happy_path_test() -> Result<(), Report> {
        zebra_test::init();
        let mut inputs = zebra_test::vectors::MAINNET_BLOCKS
            .range(1..10)
            .flat_map(|(_, block_bytes)| {
                let block: Block = block_bytes.zcash_deserialize_into().unwrap();
                block.transactions.into_iter()
            })
            .map(|transaction| {
                transaction
                    .inputs()
                    .to_vec()
            })
            .flat_map(|inputs| inputs.into_iter());

        assert!(inputs.any(|input| matches!(input, transparent::Input::PrevOut { .. })));

        let state_setup_transcript =
            zebra_test::vectors::MAINNET_BLOCKS
                .range(0..=10)
                .map(|(_, block_bytes)| {
                    let block: Arc<Block> = block_bytes.zcash_deserialize_into().unwrap();
                    let hash = block.hash();

                    let request = zebra_state::Request::CommitFinalizedBlock { block };
                    let response = Ok::<_, TransError>(zebra_state::Response::Committed(hash));

                    (request, response)
                });
        let state_setup_transcript = Transcript::from(state_setup_transcript);

        let config = zebra_state::Config::ephemeral();
        let network = Network::Mainnet;

        let state = zebra_state::init(config, network);

        /// SPANDOC: Setup the state by commiting all the necessary context
        state_setup_transcript.check(state.clone()).await?;

        let branch = NetworkUpgrade::Overwinter.branch_id().unwrap();
        let script_service = super::Verifier::new(state, branch);
        let script_transcript =
            zebra_test::vectors::MAINNET_BLOCKS
                .range(1..10)
                .flat_map(|(_, block_bytes)| {
                    let block: Block = block_bytes.zcash_deserialize_into().unwrap();

                    block
                        .transactions
                        .into_iter()
                        .flat_map(|transaction| {
                            transaction
                            .inputs()
                            .iter()
                            .enumerate()
                            .filter(
                                |(_, input)| !matches!(input, transparent::Input::Coinbase { .. }),
                            )
                            .map(|(ind, _)| ind)
                                .map(move |input_index| (input_index, transaction.clone()))
                        })
                        .map(|(input_index, transaction)| super::Request {
                            input_index,
                            transaction,
                        })
                        .map(|request| (request, Ok::<_, TransError>(())))
                });
        let script_transcript = Transcript::from(script_transcript);
        script_transcript.check(script_service).await?;

        Ok(())
    }
}
