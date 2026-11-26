//! Consensus-based block verification.
//!
//! In contrast to checkpoint verification, which only checks hardcoded
//! hashes, block verification checks all Zcash consensus rules.
//!
//! The block verifier performs all of the semantic validation checks.
//! If accepted, the block is sent to the state service for contextual
//! verification, where it may be accepted or rejected.

use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};

use chrono::Utc;
use futures::stream::FuturesUnordered;
use futures_util::FutureExt;
use thiserror::Error;
use tokio::sync::watch;
use tower::{Service, ServiceExt};
use tracing::Instrument;

use zebra_chain::{
    amount::Amount,
    block::{self, Block},
    parameters::{subsidy::SubsidyError, Network},
    transparent,
    work::equihash,
};
use zebra_state as zs;

use crate::{error::*, transaction as tx, BoxError};

pub mod check;
pub mod request;

pub use request::Request;

#[cfg(test)]
mod tests;

/// Asynchronous semantic block verification.
#[derive(Debug)]
pub struct SemanticBlockVerifier<S, V> {
    /// The network to be verified.
    network: Network,
    state_service: S,
    transaction_verifier: V,
    /// The block proposals that this verifier has validated, shared by all its requests.
    validated_proposals: Arc<watch::Sender<ValidatedProposals>>,
}

/// Block proposals that passed validation, with the state's validation results, newest first.
///
/// A miner that solves a validated proposal submits the same block with a nonce and an Equihash
/// solution. That block only needs its proof of work checked: the rest of its validation is the
/// proposal's.
type ValidatedProposals = VecDeque<(Arc<block::Block>, Box<zs::ContextuallyVerifiedBlock>)>;

/// The number of validated proposals the verifier keeps.
///
/// Mining nodes validate each new block template as a proposal, so this covers the templates
/// that miners can still be working on. Each proposal keeps its decoded block and the outputs it
/// creates and spends, so the limit also bounds that memory.
const MAX_VALIDATED_PROPOSALS: usize = 8;

/// Returns `true` if `block` has the header of a proposal that has a validation result, with a
/// different nonce and Equihash solution.
///
/// This doesn't compare the transactions, which would hash their authorizing data, so it can run
/// before the proof of work check. [`take_solved_proposal`] compares them.
fn header_solves_validated_proposal(
    validated_proposals: &watch::Sender<ValidatedProposals>,
    block: &block::Block,
) -> bool {
    validated_proposals
        .borrow()
        .iter()
        .any(|(proposal, _)| is_solved_proposal_header(proposal, block))
}

/// Removes and returns the validation result of the proposal that `block` solves, if there is one.
fn take_solved_proposal(
    validated_proposals: &watch::Sender<ValidatedProposals>,
    block: &block::Block,
) -> Option<Box<zs::ContextuallyVerifiedBlock>> {
    let mut solved = None;

    validated_proposals.send_if_modified(|proposals| {
        if let Some(index) = proposals
            .iter()
            .position(|(proposal, _)| is_solved_proposal(proposal, block))
        {
            solved = proposals.remove(index).map(|(_, contextual)| contextual);
        }

        // Nothing waits for changes to the proposals.
        false
    });

    solved
}

/// Returns `true` if `block` is `proposal` with a different nonce and Equihash solution.
///
/// `block`'s transactions haven't been checked against its header, so an equal header doesn't
/// mean equal transactions. They are compared by [`UnminedTxId`], because [`Transaction`]
/// equality only compares transaction IDs, which don't cover the input scripts, signatures and
/// proofs of a v5 or later transaction.
///
/// [`UnminedTxId`]: zebra_chain::transaction::UnminedTxId
/// [`Transaction`]: zebra_chain::transaction::Transaction
fn is_solved_proposal(proposal: &block::Block, block: &block::Block) -> bool {
    is_solved_proposal_header(proposal, block)
        && block.transactions.len() == proposal.transactions.len()
        && block
            .transactions
            .iter()
            .zip(&proposal.transactions)
            .all(|(transaction, validated)| {
                // `submitblock` builds a solved proposal from the proposal's own transactions,
                // which skips hashing their authorizing data here.
                Arc::ptr_eq(transaction, validated)
                    || transaction.unmined_id() == validated.unmined_id()
            })
}

/// Returns `true` if `block`'s header is `proposal`'s header with a different nonce and Equihash
/// solution.
fn is_solved_proposal_header(proposal: &block::Block, block: &block::Block) -> bool {
    let unsolved_header = block::Header {
        nonce: proposal.header.nonce,
        solution: proposal.header.solution,
        ..*block.header
    };

    unsolved_header == *proposal.header
}

/// Returns an error if the state already has the block with `hash`.
async fn check_block_is_new<S>(
    state_service: &mut S,
    hash: block::Hash,
) -> Result<(), VerifyBlockError>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send,
    S::Future: Send,
{
    // Check that this block is actually a new block.
    tracing::trace!("checking that block is not already in state");
    match state_service
        .ready()
        .await
        .map_err(|source| VerifyBlockError::Depth { source, hash })?
        .call(zs::Request::KnownBlock(hash))
        .await
        .map_err(|source| VerifyBlockError::Depth { source, hash })?
    {
        zs::Response::KnownBlock(Some(location)) => {
            Err(BlockError::AlreadyInChain(hash, location).into())
        }
        zs::Response::KnownBlock(None) => Ok(()),
        _ => unreachable!("wrong response to Request::KnownBlock"),
    }
}

/// Block verification errors.
// TODO: dedupe with crate::error::BlockError
#[non_exhaustive]
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum VerifyBlockError {
    #[error("unable to verify depth for block {hash} from chain state during block verification")]
    Depth { source: BoxError, hash: block::Hash },

    #[error(transparent)]
    Block {
        #[from]
        source: BlockError,
    },

    #[error(transparent)]
    Equihash {
        #[from]
        source: equihash::Error,
    },

    #[error(transparent)]
    Time(zebra_chain::block::BlockTimeError),

    /// Error when attempting to commit a block after semantic verification.
    #[error("unable to commit block after semantic verification: {0}")]
    Commit(#[from] zs::CommitBlockError),

    #[error("unable to validate block proposal: failed semantic verification (proof of work is not checked for proposals): {0}")]
    // TODO: make this into a concrete type (see #5732)
    ValidateProposal(#[source] BoxError),

    #[error("invalid transaction: {0}")]
    Transaction(#[from] TransactionError),

    #[error("invalid block subsidy: {0}")]
    Subsidy(#[from] SubsidyError),

    /// Errors originating from the state service, which may arise from general failures in interacting with the state.
    /// This is for errors that are not specifically related to block depth or commit failures.
    #[error("state service error for block {hash}: {source}")]
    StateService { source: BoxError, hash: block::Hash },
}

impl VerifyBlockError {
    /// Returns `true` if this is definitely a duplicate request.
    /// Some duplicate requests might not be detected, and therefore return `false`.
    pub fn is_duplicate_request(&self) -> bool {
        match self {
            VerifyBlockError::Block { source, .. } => source.is_duplicate_request(),
            VerifyBlockError::Commit(commit_err) => commit_err.is_duplicate_request(),
            _ => false,
        }
    }

    /// Returns `true` if the block's authorizing data doesn't match the commitment in
    /// its header.
    ///
    /// See [`zs::ValidateContextError::is_auth_commitment_mismatch()`].
    pub fn is_auth_commitment_mismatch(&self) -> bool {
        match self {
            VerifyBlockError::Commit(commit_err) => commit_err.is_auth_commitment_mismatch(),
            _ => false,
        }
    }

    /// Returns `true` if the block was rejected because its transactions don't match its header,
    /// in one of the checks that run before its height is used:
    /// - the transaction IDs don't match the header's Merkle root,
    /// - the transactions contain duplicates that keep the Merkle root (CVE-2012-2459), or
    /// - a V5 or later coinbase claims a height other than its expiry height.
    ///
    /// The block hash only commits to the header, so a peer that serves a body for a requested
    /// block hash can still send the wrong transactions. The block itself can still be valid.
    pub fn is_body_header_mismatch(&self) -> bool {
        matches!(
            self,
            VerifyBlockError::Block {
                source: BlockError::BadMerkleRoot { .. }
                    | BlockError::DuplicateTransaction
                    | BlockError::Transaction(TransactionError::CoinbaseExpiryBlockHeight { .. }),
            }
        )
    }

    /// Returns `true` if the block was rejected only because an ancestor's authorizing
    /// data didn't match the commitment in its header.
    ///
    /// See [`zs::ValidateContextError::is_descendant_of_auth_commitment_mismatch()`].
    pub fn is_descendant_of_auth_commitment_mismatch(&self) -> bool {
        match self {
            VerifyBlockError::Commit(commit_err) => {
                commit_err.is_descendant_of_auth_commitment_mismatch()
            }
            _ => false,
        }
    }

    /// Returns a suggested misbehaviour score increment for a certain error.
    pub fn misbehavior_score(&self) -> u32 {
        use VerifyBlockError::*;
        match self {
            Block { source } => source.misbehavior_score(),
            Equihash { .. } | Subsidy(_) => 100,
            Transaction(err) => err.mempool_misbehavior_score(),
            Commit(err) => err.misbehavior_score(),
            _other => 0,
        }
    }
}

/// Records the time since `phase_start` as the block verifier phase `phase`, then restarts the
/// timer for the next phase.
fn record_phase(phase: &'static str, phase_start: &mut Instant) {
    let now = Instant::now();
    metrics::histogram!("zebra.consensus.block.phase.duration_seconds", "phase" => phase)
        .record(now.duration_since(*phase_start).as_secs_f64());
    *phase_start = now;
}

/// Converts an error from a `CommitSemanticallyVerifiedBlock` state request
/// into a [`VerifyBlockError`].
///
/// The state boxes commit errors as [`zs::CommitSemanticallyVerifiedError`], a
/// newtype around [`zs::CommitBlockError`], so the wrapper must be unwrapped
/// here for `is_duplicate_request()` and `misbehavior_score()` to classify
/// duplicate blocks as benign.
fn map_commit_error(source: BoxError, hash: block::Hash) -> VerifyBlockError {
    if let Some(commit_err) = source
        .downcast_ref::<zs::CommitSemanticallyVerifiedError>()
        .map(zs::CommitSemanticallyVerifiedError::inner)
        .or_else(|| source.downcast_ref::<zs::CommitBlockError>())
    {
        return VerifyBlockError::Commit(commit_err.clone());
    }

    VerifyBlockError::StateService { source, hash }
}

/// The maximum number of transparent signature operations allowed in a block.
///
/// # Consensus
///
/// For every block, the sum of legacy and P2SH transparent signature operations across all
/// transactions must not exceed [20_000].
///
/// ## Notes
///
/// This rule is inherited from pre-SegWit Bitcoin, and is not explicitly stated in the Zcash
/// protocol spec. It is covered implicitly in [§7.6], which closes with "Other rules inherited from
/// Bitcoin". The inclusion of this rule is tracked in [`zcash/zips#568`].
///
/// Zebra mirrors `zcashd`'s `ConnectBlock`, which sums `GetLegacySigOpCount()` and
/// `GetP2SHSigOpCount()` per transaction before comparing against this constant.
///
/// [20_000]: <https://github.com/zcash/zcash/blob/bad7f7eadbbb3466bebe3354266c7f69f607fcfd/src/consensus/consensus.h#L30>
/// [`zcash/zips#568`]: <https://github.com/zcash/zips/issues/568>
/// [§7.6]: <https://zips.z.cash/protocol/protocol.pdf#blockheader>
pub const MAX_BLOCK_SIGOPS: u32 = 20_000;

/// The [ZIP 218] per-pool and global shielded action limits, which apply from NU7 activation.
///
/// `GlobalShieldedBudget`, `OrchardProtocolBlockActionLimit`, `SaplingBlockIOLimit` and
/// `SproutBlockJoinSplitLimit` in the ZIP. Orchard and Ironwood share the global budget and
/// have the same per-pool action limit. Sprout JoinSplits are prohibited.
///
/// [ZIP 218]: https://zips.z.cash/zip-0218
pub const GLOBAL_SHIELDED_BUDGET: usize = 330;
/// The per-pool action limit for both Orchard and Ironwood. See [`GLOBAL_SHIELDED_BUDGET`].
pub const ORCHARD_BLOCK_ACTION_LIMIT: usize = 330;
/// See [`GLOBAL_SHIELDED_BUDGET`].
pub const SAPLING_BLOCK_IO_LIMIT: usize = 300;
/// See [`GLOBAL_SHIELDED_BUDGET`].
pub const SPROUT_BLOCK_JOIN_SPLIT_LIMIT: usize = 0;

/// The shielded components that the [ZIP 218] action limits count.
///
/// Shared by the block verifier's limit check and by `getblocktemplate` transaction selection, so
/// that a template can not be built from transactions the verifier would then reject.
///
/// [ZIP 218]: https://zips.z.cash/zip-0218
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShieldedActionCounts {
    /// The number of Orchard actions.
    pub orchard_actions: usize,
    /// The number of Ironwood actions.
    pub ironwood_actions: usize,
    /// The number of Sapling inputs plus outputs.
    pub sapling_io: usize,
    /// The number of Sprout JoinSplits.
    pub joinsplits: usize,
}

impl ShieldedActionCounts {
    /// Returns the counts for `transaction`.
    pub fn from_transaction(transaction: &zebra_chain::transaction::Transaction) -> Self {
        Self {
            orchard_actions: transaction.orchard_actions().count(),
            ironwood_actions: transaction.ironwood_actions().count(),
            sapling_io: transaction
                .sapling_spends_count()
                .saturating_add(transaction.sapling_outputs().count()),
            joinsplits: transaction.joinsplit_count(),
        }
    }

    /// Returns these counts plus `other`.
    ///
    /// Saturating, so that a malformed count can not wrap back below a limit.
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            orchard_actions: self.orchard_actions.saturating_add(other.orchard_actions),
            ironwood_actions: self.ironwood_actions.saturating_add(other.ironwood_actions),
            sapling_io: self.sapling_io.saturating_add(other.sapling_io),
            joinsplits: self.joinsplits.saturating_add(other.joinsplits),
        }
    }

    /// Returns the total shielded cost, which each JoinSplit contributes twice to, because it
    /// produces two shielded outputs.
    pub fn shielded_cost(&self) -> usize {
        self.orchard_actions
            .saturating_add(self.ironwood_actions)
            .saturating_add(self.sapling_io)
            .saturating_add(self.joinsplits.saturating_mul(2))
    }

    /// Returns the first limit these counts exceed, as `(pool, count, limit)`.
    pub fn exceeded_limit(&self) -> Option<(&'static str, usize, usize)> {
        [
            (
                "Orchard actions",
                self.orchard_actions,
                ORCHARD_BLOCK_ACTION_LIMIT,
            ),
            (
                "Ironwood actions",
                self.ironwood_actions,
                ORCHARD_BLOCK_ACTION_LIMIT,
            ),
            (
                "Sapling inputs and outputs",
                self.sapling_io,
                SAPLING_BLOCK_IO_LIMIT,
            ),
            (
                "Sprout JoinSplits",
                self.joinsplits,
                SPROUT_BLOCK_JOIN_SPLIT_LIMIT,
            ),
            (
                "shielded actions across all pools",
                self.shielded_cost(),
                GLOBAL_SHIELDED_BUDGET,
            ),
        ]
        .into_iter()
        .find(|&(_, count, limit)| count > limit)
    }
}

impl<S, V> SemanticBlockVerifier<S, V>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    V: Service<tx::BlockRequest, Response = tx::BlockResponse, Error = BoxError>
        + Send
        + Clone
        + 'static,
    V::Future: Send + 'static,
{
    /// Creates a new SemanticBlockVerifier
    pub fn new(network: &Network, state_service: S, transaction_verifier: V) -> Self {
        Self {
            network: network.clone(),
            state_service,
            transaction_verifier,
            validated_proposals: Arc::new(watch::Sender::new(VecDeque::new())),
        }
    }
}

impl<S, V> Service<Request> for SemanticBlockVerifier<S, V>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    V: Service<tx::BlockRequest, Response = tx::BlockResponse, Error = BoxError>
        + Send
        + Clone
        + 'static,
    V::Future: Send + 'static,
{
    type Response = block::Hash;
    type Error = VerifyBlockError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // We use the state for contextual verification, and we expect those
        // queries to be fast. So we don't need to call
        // `state_service.poll_ready()` here.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        // Stamp the time the verifier received this block: the non-finalized state's
        // `Chain::cmp` uses it to prefer the first-received chain on equal-work ties.
        let received_time = Instant::now();
        let validated_proposals = self.validated_proposals.clone();

        let mut state_service = self.state_service.clone();
        let mut transaction_verifier = self.transaction_verifier.clone();
        let network = self.network.clone();

        let block = request.block();

        // We don't include the block hash, because it's likely already in a parent span
        let span = tracing::debug_span!("block", height = ?block.coinbase_height());

        async move {
            let hash = block.hash();

            // A block that solves a validated proposal is committed below without any other
            // state request. The state rejects a duplicate block when it commits it, so a block
            // with a validated proposal's header skips this check here. Its transactions are
            // only compared with the proposal's after the proof of work check.
            let header_solves_proposal = !request.is_proposal()
                && header_solves_validated_proposal(&validated_proposals, &block);
            if !header_solves_proposal {
                check_block_is_new(&mut state_service, hash).await?;
            }

            tracing::trace!("performing block checks");
            // The template precompute verifies every template as a proposal, so timing proposals
            // would mix template checks into the timings of blocks that are committed.
            let record_phases = !request.is_proposal();
            let mut phase_start = Instant::now();
            let height = block
                .coinbase_height()
                .ok_or(BlockError::MissingHeight(hash))?;

            // Zebra does not support heights greater than
            // [`block::Height::MAX`].
            if height > block::Height::MAX {
                Err(BlockError::MaxHeight(height, hash, block::Height::MAX))?;
            }

            // > The block data MUST be validated and checked against the server's usual
            // > acceptance rules (excluding the check for a valid proof-of-work).
            // <https://en.bitcoin.it/wiki/BIP_0023#Block_Proposal>
            if request.is_proposal() || network.disable_pow() {
                check::difficulty_threshold_is_valid(&block.header, &network, &height, &hash)?;
            } else {
                // Do the difficulty checks first, to raise the threshold for
                // attacks that use any other fields.
                check::difficulty_is_valid(&block.header, &network, &height, &hash)?;
                check::equihash_solution_is_valid(&block.header)?;
            }

            // # Security
            //
            // Check that the header commits to the coinbase height before anything uses it, as the
            // checkpoint verifier does. A peer can rewrite the height of a V5 coinbase without
            // changing the Merkle root. `merkle_root_validity()` uses the height to pick the
            // network upgrade for its consensus branch ID check, and the checks after it use the
            // height before the transaction verifier checks the coinbase expiry height against it.
            // So a rewritten height would get the block checked at that height, or rejected for an
            // error that doesn't show the body was forged (GHSA-g9rc-89v4-hhvf).
            //
            // The height is only authenticated once the Merkle root check also passes, but either
            // failure shows that the body was forged.
            check::coinbase_height_is_committed(&block, height)?;

            // A block that was validated as a proposal, and then solved, only needs the proof of
            // work check above and the time check here. The state commits the proposal's
            // validation result for it.
            let solved_proposal = if request.is_proposal() {
                None
            } else {
                take_solved_proposal(&validated_proposals, &block)
            };
            if let Some(mut contextual) = solved_proposal {
                check::time_is_valid_at(&block.header, Utc::now(), &height, &hash)
                    .map_err(VerifyBlockError::Time)?;

                contextual.rebind(block, received_time);

                // A solved proposal has no transaction verification phase.
                record_phase("block_checks", &mut phase_start);

                return match state_service
                    .ready()
                    .await
                    .map_err(|source| VerifyBlockError::StateService { source, hash })?
                    .call(zs::Request::CommitContextuallyVerifiedBlock(contextual))
                    .await
                {
                    Ok(zs::Response::Committed(committed_hash)) => {
                        assert_eq!(committed_hash, hash, "state must commit correct hash");
                        record_phase("state_commit", &mut phase_start);
                        Ok(hash)
                    }

                    Err(source) => Err(map_commit_error(source, hash)),

                    _ => unreachable!("wrong response for CommitContextuallyVerifiedBlock"),
                };
            }

            // This block has a validated proposal's header, but it didn't get the proposal's
            // validation result: its transactions aren't the proposal's, or a concurrent request
            // for the same block took the result. So it is verified in full, like any other
            // block, starting with the check that it skipped.
            if header_solves_proposal {
                check_block_is_new(&mut state_service, hash).await?;
            }

            // Next, check the Merkle root validity, to ensure that
            // the header binds to the transactions in the blocks.

            // Precomputing this avoids duplicating transaction hash computations.
            let transaction_hashes: Arc<[_]> =
                block.transactions.iter().map(|t| t.hash()).collect();

            check::merkle_root_validity(&network, &block, &transaction_hashes)?;

            // Bound shielded work before output recovery, proof verification, or computing the
            // note commitment trees for early mining work. This runs after the Merkle root check,
            // so a body that doesn't match its header is rejected as that.
            check::shielded_action_limits_are_valid(&block, &network, height, hash)?;

            // Since errors cause an early exit, try to do the
            // quick checks first.

            // Quick field validity and structure checks
            let now = Utc::now();
            check::time_is_valid_at(&block.header, now, &height, &hash)
                .map_err(VerifyBlockError::Time)?;

            let coinbase_tx = check::coinbase_is_first(&block)?;

            // From the NSM reissuance height, the block subsidy depends on the
            // NSM value balance after the parent block, which is not committed yet while
            // blocks are verified concurrently. The state checks the subsidy, funding streams,
            // and miner fees during contextual validation instead, where the parent's chain value
            // pools are known.
            let nsm_reissuance_is_active =
                zebra_chain::parameters::subsidy::nsm_reissuance_is_active(height, &network);

            let expected_subsidy_and_deferred_pool_balance_change = if nsm_reissuance_is_active {
                None
            } else {
                let expected_block_subsidy =
                    zebra_chain::parameters::subsidy::block_subsidy(height, &network)?;

                // See [ZIP-1015](https://zips.z.cash/zip-1015).
                let deferred_pool_balance_change =
                    check::subsidy_is_valid(&block, &network, expected_block_subsidy)?;

                Some((expected_block_subsidy, deferred_pool_balance_change))
            };

            // Let miners build on this block now, rather than when its transactions are verified.
            // Its proof of work is valid, and the Merkle root has bound its transactions to its
            // header, so a changed body under a valid header gets no work. The guard keeps the
            // work available until this block is committed, and withdraws it if the block is
            // rejected.
            let _mining_candidate = if request.is_proposal() {
                None
            } else {
                // Early work is optional, so a state error here doesn't fail verification.
                match state_service
                    .clone()
                    .oneshot(zs::Request::MiningCandidate(block.clone()))
                    .await
                {
                    Ok(zs::Response::MiningCandidate(guard)) => guard,
                    _ => None,
                }
            };

            // Now do the slower checks

            // Check compatibility with ZIP-212 shielded Sapling and Orchard coinbase output decryption
            tx::check::coinbase_outputs_are_decryptable(&coinbase_tx, &network, height)?;

            if record_phases {
                record_phase("block_checks", &mut phase_start);
            }

            let known_utxos = Arc::new(transparent::new_ordered_outputs(
                &block,
                &transaction_hashes,
            ));

            // Read the outputs of earlier blocks that this block spends in one state request,
            // so the transaction verifier doesn't send a request for each input.
            let state_utxos = read_state_utxos(state_service.clone(), &block, &known_utxos).await;

            // Send transactions to the transaction verifier to be checked
            let mut async_checks = FuturesUnordered::new();

            for (&transaction_hash, transaction) in
                transaction_hashes.iter().zip(block.transactions.iter())
            {
                let rsp = transaction_verifier
                    .ready()
                    .await
                    .expect("transaction verifier is always ready")
                    .call(tx::BlockRequest {
                        transaction_hash,
                        transaction: transaction.clone(),
                        known_utxos: known_utxos.clone(),
                        state_utxos: state_utxos.clone(),
                        height,
                        time: block.header.time,
                    });
                async_checks.push(rsp);
            }
            // The transaction requests hold the only other references to these outputs.
            drop(state_utxos);
            tracing::trace!(len = async_checks.len(), "built async tx checks");

            // Get the transaction results back from the transaction verifier.

            // Sum up some block totals from the transaction responses.
            let mut sigops = 0;
            let mut block_miner_fees = Ok(Amount::zero());

            use futures::StreamExt;
            while let Some(result) = async_checks.next().await {
                tracing::trace!(?result, remaining = async_checks.len());
                let response = result
                    .map_err(Into::into)
                    .map_err(VerifyBlockError::Transaction)?;

                sigops += response.sigops;

                // Coinbase transactions consume the miner fee,
                // so they don't add any value to the block's total miner fee.
                if let Some(miner_fee) = response.miner_fee {
                    block_miner_fees += miner_fee;
                }
            }

            // Check the summed block totals

            if sigops > MAX_BLOCK_SIGOPS {
                Err(BlockError::TooManyTransparentSignatureOperations {
                    height,
                    hash,
                    sigops,
                })?;
            }

            let block_miner_fees =
                block_miner_fees.map_err(|amount_error| BlockError::SummingMinerFees {
                    height,
                    hash,
                    source: amount_error,
                })?;

            if let Some((expected_block_subsidy, deferred_pool_balance_change)) =
                expected_subsidy_and_deferred_pool_balance_change
            {
                check::miner_fees_are_valid(
                    &coinbase_tx,
                    height,
                    block_miner_fees,
                    expected_block_subsidy,
                    deferred_pool_balance_change,
                    &network,
                )?;
            }

            if record_phases {
                record_phase("transaction_verification", &mut phase_start);
            }

            // Finally, submit the block for contextual verification.
            let new_outputs = Arc::into_inner(known_utxos)
                .expect("all verification tasks using known_utxos are complete");

            let prepared_block = zs::SemanticallyVerifiedBlock {
                block,
                hash,
                height,
                new_outputs,
                transaction_hashes,
                received_time: Some(received_time),
            };

            // Return early for proposal requests.
            if request.is_proposal() {
                let proposal = prepared_block.block.clone();

                return match state_service
                    .ready()
                    .await
                    .map_err(VerifyBlockError::ValidateProposal)?
                    .call(zs::Request::CheckBlockProposalValidity(prepared_block))
                    .await
                    .map_err(VerifyBlockError::ValidateProposal)?
                {
                    zs::Response::ValidBlockProposal(contextual) => {
                        // Keep the result, so the solved proposal isn't validated again.
                        //
                        // A valid proposal's header commits to its transactions, so proposals
                        // that only differ in their nonce and solution are the same proposal.
                        // Keeping one result for them means that no result is left for a header
                        // once a block has used it, so a later block with that header gets the
                        // known block check first.
                        validated_proposals.send_modify(|proposals| {
                            proposals.retain(|(validated, _)| {
                                !is_solved_proposal_header(validated, &proposal)
                            });
                            proposals.push_front((proposal, contextual));
                            proposals.truncate(MAX_VALIDATED_PROPOSALS);
                        });

                        Ok(hash)
                    }
                    _ => unreachable!("wrong response for CheckBlockProposalValidity"),
                };
            }

            match state_service
                .ready()
                .await
                .map_err(|source| VerifyBlockError::StateService { source, hash })?
                .call(zs::Request::CommitSemanticallyVerifiedBlock(prepared_block))
                .await
            {
                Ok(zs::Response::Committed(committed_hash)) => {
                    assert_eq!(committed_hash, hash, "state must commit correct hash");
                    record_phase("state_commit", &mut phase_start);
                    Ok(hash)
                }

                Err(source) => Err(map_commit_error(source, hash)),

                _ => unreachable!("wrong response for CommitSemanticallyVerifiedBlock"),
            }
        }
        .instrument(span)
        .boxed()
    }
}

/// Returns the outputs that `block` spends, and that the state has in a verified block.
///
/// Leaves out the outputs that the block creates, which are in `block_outputs`, and the
/// outputs that the state doesn't have yet. The transaction verifier waits for those in the
/// state, one request for each input, as it does when the state can't answer this request.
async fn read_state_utxos<S>(
    state_service: S,
    block: &Block,
    block_outputs: &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
) -> Arc<HashMap<transparent::OutPoint, transparent::Utxo>>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    let outpoints: Vec<transparent::OutPoint> = block
        .transactions
        .iter()
        .flat_map(|transaction| transaction.spent_outpoints())
        .filter(|outpoint| !block_outputs.contains_key(outpoint))
        .collect();

    if outpoints.is_empty() {
        return Arc::default();
    }

    match state_service
        .oneshot(zs::Request::AnyChainUtxos(outpoints))
        .await
    {
        Ok(zs::Response::AnyChainUtxos(utxos)) => Arc::new(utxos),
        Ok(_) => unreachable!("wrong response for AnyChainUtxos"),
        Err(error) => {
            tracing::debug!(
                ?error,
                "reading a block's spent outputs from the state failed"
            );

            Arc::default()
        }
    }
}
