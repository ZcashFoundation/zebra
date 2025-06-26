use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use futures::FutureExt;
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, tests::generate, Block},
    parameters::{Network, NetworkUpgrade},
    transaction::{Transaction, UnminedTx, VerifiedUnminedTx},
    transparent,
};
use zebra_node_services::mempool;
use zebra_script::CachedFfiTransaction;
use zebra_state as zs;

use crate::{
    error::TransactionError,
    transaction::{check, POLL_MEMPOOL_DELAY},
    BoxError,
};

/// A request to verify additional constraints applied to mempool transactions that
/// are not applied to block transactions.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Request {
    /// The transaction itself.
    transaction: UnminedTx,

    /// The height of the next block.
    ///
    /// The next block is the first block that could possibly contain a
    /// mempool transaction.
    proposed_height: block::Height,
}

struct Response {
    /// The full content of the verified mempool transaction.
    /// Also contains the transaction fee and other associated fields.
    ///
    /// Mempool transactions always have a transaction fee,
    /// because coinbase transactions are rejected from the mempool.
    ///
    /// [`Response::Mempool`] responses are uniquely identified by the
    /// [`UnminedTxId`] variant for their transaction version.
    transaction: VerifiedUnminedTx,

    /// The block proposal to be verified for the transaction to be verified
    block_proposal: Block,

    /// Outputs spent by this transaction which are available in the state
    /// at the time of verification.
    // TODO: Add this to the block router verifier's block proposal request as an optional field which, if Some,
    //       indicates that the block proposal is being verified for a transaction in the mempool, and that some of the
    //       checks in the transaction verifier can be skipped.
    spent_state_outputs: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,

    /// A list of spent [`transparent::OutPoint`]s that were found in
    /// the mempool's list of `created_outputs`.
    ///
    /// Used by the mempool to determine dependencies between transactions
    /// in the mempool and to avoid adding transactions with missing spends
    /// to its verified set.
    spent_mempool_outpoints: Vec<transparent::OutPoint>,
}

impl<State, Mempool> Service<Request> for super::Verifier<State, Mempool>
where
    State: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    State::Future: Send + 'static,
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    Mempool::Future: Send + 'static,
{
    type Response = Response;

    // TODO: Add a new error type for this service impl.
    type Error = TransactionError;

    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.poll_ready_internal()
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let script_verifier = self.script_verifier;
        let network = self.network.clone();
        let state = self.state.clone();
        let mempool = self.mempool.clone();

        async move {
            tracing::trace!(?req, "got mempool tx verify request");

            // Do quick checks first
            let tx = &req.transaction.transaction;
            check::quick_checks(tx, req.proposed_height, &network)?;
            // Validate the coinbase input consensus rules
            if tx.is_coinbase() {
                return Err(TransactionError::CoinbaseInMempool);
            }

            if !tx.is_valid_non_coinbase() {
                return Err(TransactionError::NonCoinbaseHasCoinbaseInput);
            }

            // Validate `nExpiryHeight` consensus rules
            check::non_coinbase_expiry_height(&req.proposed_height, &tx)?;

            // Consensus rule:
            //
            // > Either v_{pub}^{old} or v_{pub}^{new} MUST be zero.
            //
            // https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
            check::joinsplit_has_vpub_zero(&tx)?;

            // [Canopy onward]: `vpub_old` MUST be zero.
            // https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
            check::disabled_add_to_sprout_pool(&tx, req.proposed_height, &network)?;

            check::spend_conflicts(&tx)?;

            // Skip the state query if we don't need the time for this check.
            let next_median_time_past = if tx.lock_time_is_time() {
                // This state query is much faster than loading UTXOs from the database,
                // so it doesn't need to be executed in parallel
                let state = state.clone();
                Some(
                    Self::mempool_best_chain_next_median_time_past(state)
                        .await?
                        .to_chrono(),
                )
            } else {
                None
            };

            // This consensus check makes sure Zebra produces valid block templates.
            check::lock_time_has_passed(&tx, req.proposed_height, next_median_time_past)?;

            // TODO: Move the this comment and the code it references to a common method.
            // "The consensus rules applied to valueBalance, vShieldedOutput, and bindingSig
            // in non-coinbase transactions MUST also be applied to coinbase transactions."
            //
            // This rule is implicitly implemented during Sapling and Orchard verification,
            // because they do not distinguish between coinbase and non-coinbase transactions.
            //
            // Note: this rule originally applied to Sapling, but we assume it also applies to Orchard.
            //
            // https://zips.z.cash/zip-0213#specification

            // Load spent UTXOs from state.
            // The UTXOs are required for almost all the async checks.
            // TODO: Implement a method for getting spent outputs for mempool transactions
            let (spent_utxos, spent_outputs, spent_mempool_outpoints) = todo!();
            // Self::spent_outputs_for_mempool_tx(
            //     tx.clone(),
            //     req.clone(),
            //     state.clone(),
            //     mempool.clone(),
            // )
            // .await?;

            // WONTFIX: Return an error for Request::Block as well to replace this check in
            //       the state once #2336 has been implemented?
            // TODO: Accept generic param?
            // Self::check_maturity_height(&network, &req, &spent_utxos)?;

            let nu = NetworkUpgrade::current(&network, req.proposed_height);
            let cached_ffi_transaction = Arc::new(
                CachedFfiTransaction::new(tx.clone(), Arc::new(spent_outputs), nu)
                    .map_err(|_| TransactionError::UnsupportedByNetworkUpgrade(tx.version(), nu))?,
            );

            // TODO: Move this logic to a common method.
            // let mut async_checks = match tx.as_ref() {
            //     Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
            //         tracing::debug!(?tx, "got transaction with wrong version");
            //         return Err(TransactionError::WrongVersion);
            //     }
            //     Transaction::V4 {
            //         joinsplit_data,
            //         sapling_shielded_data,
            //         ..
            //     } => Self::verify_v4_transaction(
            //         &req,
            //         &network,
            //         script_verifier,
            //         cached_ffi_transaction.clone(),
            //         joinsplit_data,
            //         sapling_shielded_data,
            //     )?,
            //     Transaction::V5 {
            //         sapling_shielded_data,
            //         orchard_shielded_data,
            //         ..
            //     } => Self::verify_v5_transaction(
            //         &req,
            //         &network,
            //         script_verifier,
            //         cached_ffi_transaction.clone(),
            //         sapling_shielded_data,
            //         orchard_shielded_data,
            //     )?,
            //     #[cfg(feature = "tx_v6")]
            //     Transaction::V6 {
            //         sapling_shielded_data,
            //         orchard_shielded_data,
            //         ..
            //     } => Self::verify_v6_transaction(
            //         &req,
            //         &network,
            //         script_verifier,
            //         cached_ffi_transaction.clone(),
            //         sapling_shielded_data,
            //         orchard_shielded_data,
            //     )?,
            // };

            // let check_anchors_and_revealed_nullifiers_query = state
            //     .clone()
            //     .oneshot(zs::Request::CheckBestChainTipNullifiersAndAnchors(
            //         req.transaction,
            //     ))
            //     .map(|res| {
            //         assert!(
            //             res? == zs::Response::ValidBestChainTipNullifiersAndAnchors,
            //             "unexpected response to CheckBestChainTipNullifiersAndAnchors request"
            //         );
            //         Ok(())
            //     });

            // async_checks.push(check_anchors_and_revealed_nullifiers_query);

            // If the Groth16 parameter download hangs,
            // Zebra will timeout here, waiting for the async checks.
            // async_checks.check().await?;

            // Get the `value_balance` to calculate the transaction fee.
            let value_balance = tx.value_balance(&spent_utxos);

            // Calculate the fee only for non-coinbase transactions.
            let mut miner_fee = None;
            if !tx.is_coinbase() {
                // TODO: deduplicate this code with remaining_transaction_value()?
                miner_fee = match value_balance {
                    Ok(vb) => match vb.remaining_transaction_value() {
                        Ok(tx_rtv) => Some(tx_rtv),
                        Err(_) => return Err(TransactionError::IncorrectFee),
                    },
                    Err(_) => return Err(TransactionError::IncorrectFee),
                };
            }

            let legacy_sigop_count = zebra_script::legacy_sigop_count(&tx)?;

            let transaction = VerifiedUnminedTx::new(
                req.transaction,
                miner_fee.expect("fee should have been checked earlier"),
                legacy_sigop_count,
            )?;

            // TODO: Remove this line from this `Service` impl, condition it on whether a block proposal is being verified for a mempool tx, i.e. call
            //       the mempool before responding to a block tx verify request when the block tx verify request's `spent_state_outputs` field is Some.
            if let Some(mut mempool) = mempool {
                tokio::spawn(async move {
                    // Best-effort poll of the mempool to provide a timely response to
                    // `sendrawtransaction` RPC calls or `AwaitOutput` mempool calls.
                    tokio::time::sleep(POLL_MEMPOOL_DELAY).await;
                    let _ = mempool
                        .ready()
                        .await
                        .expect("mempool poll_ready() method should not return an error")
                        .call(mempool::Request::CheckForVerifiedTransactions)
                        .await;
                });
            }

            Ok(Response {
                transaction,
                spent_mempool_outpoints,
                spent_state_outputs: HashMap::new(),
                block_proposal: generate_block_proposal(state, &req.transaction, todo!(), &network)
                    .await?,
            })
        }
        .boxed()
    }
}

/// Generate a block proposal for the given transaction and its dependencies.
async fn generate_block_proposal<State>(
    state: State,
    transaction: &UnminedTx,
    transaction_dependencies: Vec<Arc<Transaction>>,
    network: &Network,
) -> Result<Block, TransactionError>
where
    State: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    State::Future: Send + 'static,
{
    let transactions = transaction_dependencies
        .into_iter()
        .chain(std::iter::once(transaction.transaction.clone()));

    // TODO: Move block template generation logic to `zebra-consensus`.
    // proposal_block_from_template(block_template(state, transactions)?)
    todo!()
}

// async fn block_template(state, transactions) -> BlockTemplate {
//     todo!()
// }
