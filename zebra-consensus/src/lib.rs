//! Implementation of Zcash consensus checks.
//!
//! More specifically, this crate implements *semantic* validity checks,
//! as defined below.
//!
//! ## Verification levels.
//!
//! Zebra's implementation of the Zcash consensus rules is oriented
//! around three telescoping notions of validity:
//!
//! 1. *Structural Validity*, or whether the format and structure of the
//!    object are valid.  For instance, Sprout-on-BCTV14 proofs are not
//!    allowed in version 4 transactions, and a transaction with a spend
//!    or output description must include a binding signature.
//!
//! 2. *Semantic Validity*, or whether the object could potentially be
//!    valid, depending on the chain state.  For instance, a transaction
//!    that spends a UTXO must supply a valid unlock script; a shielded
//!    transaction must have valid proofs, etc.
//!
//! 3. *Contextual Validity*, or whether a semantically valid
//!    transaction is actually valid in the context of a particular
//!    chain state.  For instance, a transaction that spends a
//!    UTXO is only valid if the UTXO remains unspent; a
//!    shielded transaction spending some note must reveal a nullifier
//!    not already in the nullifier set, etc.
//!
//! *Structural validity* is enforced by the definitions of data
//! structures in `zebra-chain`.  *Semantic validity* is enforced by the
//! code in this crate.  *Contextual validity* is enforced in
//! `zebra-state` when objects are committed to the chain state.
//!
//! ## Service initialization
//!
//! [`init`] builds the block verification stack: the
//! [`SemanticBlockVerifier`](block::SemanticBlockVerifier) behind the
//! [`CheckpointGateLayer`](checkpoints::CheckpointGateLayer), which rejects
//! commits at or below the network's mandatory checkpoint height — Zebra
//! cannot fully validate blocks in that range, so they are only committed by
//! checkpoint-verified sync (the known-hash IBD engine; see
//! `docs/design/known-hash-ibd.md` and the [`checkpoints`] module docs).
//!
//! # Correctness
//!
//! Block and transaction verification requests should be wrapped in a
//! timeout, because full block and transaction verification wait for UTXOs
//! from previous blocks. Otherwise, verification of out-of-order and invalid
//! blocks and transactions can hang indefinitely.

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_consensus")]

mod block;
mod primitives;
mod script;

pub mod checkpoints;
pub mod config;
pub mod error;
pub mod service_trait;
pub mod transaction;

#[cfg(test)]
mod tests;

use tokio::{sync::oneshot, task::JoinHandle};
use tower::{buffer::Buffer, util::BoxService, Layer, Service, ServiceExt};
use tracing::{instrument, Instrument, Span};

use zebra_chain::{
    block::{self as chain_block, Height},
    parameters::Network,
};

use zebra_node_services::mempool;
use zebra_state as zs;

use crate::{
    block::SemanticBlockVerifier, checkpoints::CheckpointGateLayer, error::TransactionError,
};

#[cfg(any(test, feature = "proptest-impl"))]
pub use block::check::difficulty_is_valid;

pub use block::check::merkle_root_validity;
pub use block::{subsidy::funding_stream_address, Request, VerifyBlockError, MAX_BLOCK_SIGOPS};
pub use config::Config;
pub use error::BlockError;
pub use primitives::{ed25519, groth16, halo2, redjubjub, redpallas, spawn_fifo};

/// A boxed [`std::error::Error`].
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// The bound for the chain verifier and transaction verifier buffers.
///
/// We choose the verifier buffer bound based on the maximum number of
/// concurrent verifier users, to avoid contention:
///   - the `ChainSync` block download and verify stream
///   - the `Inbound` block download and verify stream
///   - the `Mempool` transaction download and verify stream
///   - a block miner component, which we might add in future, and
///   - 1 extra slot to avoid contention.
///
/// We deliberately add extra slots, because they only cost a small amount of
/// memory, but missing slots can significantly slow down Zebra.
const VERIFIER_BUFFER_BOUND: usize = 5;

/// Initialize block and transaction verification services.
///
/// Returns a block verifier, transaction verifier,
/// a [`BackgroundTaskHandles`] with the state checkpoint verify task,
/// and the maximum configured checkpoint verification height.
///
/// The consensus configuration is specified by `config`, and the Zcash network
/// to verify blocks for is specified by `network`.
///
/// The block verification service asynchronously performs semantic verification
/// checks. Blocks that pass semantic verification are submitted to the supplied
/// `state_service` for contextual verification before being committed to the chain.
/// Blocks at or below the network's mandatory checkpoint height are rejected by
/// the [`CheckpointGateLayer`] before they reach the verifier (see the
/// [`checkpoints`] module docs).
///
/// The transaction verification service asynchronously performs semantic verification
/// checks. Transactions that pass semantic verification return an `Ok` result to the caller.
///
/// This function should only be called once for a particular state service.
///
/// Dropped requests are cancelled on a best-effort basis, but may continue to be processed.
///
/// # Correctness
///
/// Block and transaction verification requests should be wrapped in a timeout,
/// so that out-of-order and invalid requests do not hang indefinitely.
/// See the [crate](self) documentation for details.
#[instrument(skip(state_service, mempool))]
pub async fn init<S, Mempool>(
    config: Config,
    network: &Network,
    state_service: S,
    mempool: oneshot::Receiver<Mempool>,
) -> (
    Buffer<BoxService<Request, chain_block::Hash, VerifyBlockError>, Request>,
    Buffer<
        BoxService<transaction::Request, transaction::Response, TransactionError>,
        transaction::Request,
    >,
    BackgroundTaskHandles,
    Height,
)
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    Mempool::Future: Send + 'static,
{
    // Give other tasks priority before spawning the checkpoint task.
    tokio::task::yield_now().await;

    // Make sure the state contains the known best chain checkpoints, in a separate thread.

    let checkpoint_state_service = state_service.clone();
    let checkpoint_network = network.clone();

    let state_checkpoint_verify_handle = tokio::task::spawn(
        async move {
            tracing::info!("starting state checkpoint validation");

            // # Consensus
            //
            // > A network upgrade is settled on a given network when there is a social
            // > consensus that it has activated with a given activation block hash.
            // > A full validator that potentially risks Mainnet funds or displays Mainnet
            // > transaction information to a user MUST do so only for a block chain that
            // > includes the activation block of the most recent settled network upgrade,
            // > with the corresponding activation block hash.
            //
            // <https://zips.z.cash/protocol/protocol.pdf#blockchain>
            //
            // An O(1) spot-check preserves the wrong-chain-on-restart
            // detection the old full-list walk provided, without issuing one
            // serial state request per checkpoint (design doc §7.2): the
            // highest checkpoint at or below the state tip pins the whole
            // chain below it by construction. The spot-check always runs (it
            // is a cheap consistency assertion), independent of the
            // `checkpoint_sync` config.
            let checkpoints = checkpoint_network.checkpoint_list();

            let tip = match checkpoint_state_service
                .clone()
                .oneshot(zebra_state::Request::Tip)
                .await
            {
                Ok(zebra_state::Response::Tip(tip)) => tip,
                Ok(response) => {
                    unreachable!("unexpected response type: {response:?} from state request")
                }
                Err(e) => {
                    tracing::warn!(
                        "unexpected error: {e:?} in state tip request while verifying previous \
                         state checkpoints. Is Zebra shutting down?"
                    );
                    return;
                }
            };

            let Some((tip_height, _)) = tip else {
                tracing::info!("state is empty, no previous chain to spot-check");
                return;
            };

            let Some(check_height) = checkpoints.max_height_in_range(..=tip_height) else {
                tracing::info!("no checkpoint at or below the state tip to spot-check");
                return;
            };

            let checkpoint_hash = checkpoints
                .hash(check_height)
                .expect("max_height_in_range returns a height in the list");

            match checkpoint_state_service
                .oneshot(zebra_state::Request::BestChainBlockHash(check_height))
                .await
            {
                Ok(zebra_state::Response::BlockHash(Some(state_hash))) => assert_eq!(
                    checkpoint_hash, state_hash,
                    "invalid block in state: a previous Zebra instance followed an \
                     incorrect chain. Delete and re-sync your state to use the best chain"
                ),
                Ok(zebra_state::Response::BlockHash(None)) => {
                    // The `Tip` and `BestChainBlockHash` reads are not atomic.
                    // The best chain can shrink below the spot-checked height
                    // between them — e.g. an `invalidateblock` RPC or a reorg
                    // to a shorter chain — leaving no block at that height. That
                    // is a benign race, not a wrong-chain indictment, so skip
                    // the spot-check this startup rather than panicking the
                    // consensus task (which would exit the node).
                    tracing::info!(
                        ?check_height,
                        "best chain changed during the startup spot-check; \
                         skipping it this run"
                    );
                }
                Ok(response) => {
                    unreachable!("unexpected response type: {response:?} from state request")
                }
                Err(e) => {
                    tracing::warn!(
                        "unexpected error: {e:?} in state request while verifying previous \
                         state checkpoints. Is Zebra shutting down?"
                    );
                }
            }

            tracing::info!("finished state checkpoint validation");
        }
        .instrument(Span::current()),
    );

    // transaction verification

    let transaction = transaction::Verifier::new(network, state_service.clone(), mempool);
    let transaction = Buffer::new(BoxService::new(transaction), VERIFIER_BUFFER_BOUND);

    // block verification
    let max_checkpoint_height = checkpoints::max_checkpoint_height(config, network);

    tracing::info!(?max_checkpoint_height, "initializing block verifier");

    let block = SemanticBlockVerifier::new(network, state_service.clone(), transaction.clone());
    let block = CheckpointGateLayer::new(network).layer(block);

    let block = Buffer::new(BoxService::new(block), VERIFIER_BUFFER_BOUND);

    let task_handles = BackgroundTaskHandles {
        state_checkpoint_verify_handle,
    };

    (block, transaction, task_handles, max_checkpoint_height)
}

/// The background task handles for `zebra-consensus` verifier initialization.
#[derive(Debug)]
pub struct BackgroundTaskHandles {
    /// A handle to the state checkpoint verify task.
    /// Finishes when all the checkpoints are verified, or when the state tip is reached.
    pub state_checkpoint_verify_handle: JoinHandle<()>,
}

/// Calls [`init`] with a closed mempool setup channel for conciseness in tests.
///
/// See [`init`] for more details.
#[cfg(any(test, feature = "proptest-impl"))]
pub async fn init_test<S>(
    config: Config,
    network: &Network,
    state_service: S,
) -> (
    Buffer<BoxService<Request, chain_block::Hash, VerifyBlockError>, Request>,
    Buffer<
        BoxService<transaction::Request, transaction::Response, TransactionError>,
        transaction::Request,
    >,
    BackgroundTaskHandles,
    Height,
)
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    init(
        config.clone(),
        network,
        state_service.clone(),
        oneshot::channel::<
            Buffer<BoxService<mempool::Request, mempool::Response, BoxError>, mempool::Request>,
        >()
        .1,
    )
    .await
}
