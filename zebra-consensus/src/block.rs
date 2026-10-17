//! Consensus-based block verification.
//!
//! In contrast to checkpoint verification, which only checks hardcoded
//! hashes, block verification checks all Zcash consensus rules.
//!
//! The block verifier performs all of the semantic validation checks.
//! If accepted, the block is sent to the state service for contextual
//! verification, where it may be accepted or rejected.

use std::{
    collections::VecDeque,
    future::Future,
    io::{self, Write},
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
    block,
    parameters::{subsidy::SubsidyError, Network},
    serialization::{sha256d, SerializationError, ZcashDeserialize, ZcashSerialize},
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
    /// Successful proposals, newest first; shared by all outstanding verification requests.
    proposals: Arc<watch::Sender<ProposalCache>>,
}

type ProposalCache = VecDeque<PreparedProposal>;

#[derive(Debug)]
struct PreparedProposal {
    key: [u8; 32],
    transaction_bytes: usize,
    block: Arc<block::Block>,
    contextual: Arc<zs::ContextuallyVerifiedBlock>,
    retained_bytes: usize,
}

/// Bound retained proposal data, including unusually large spent-output scripts.
///
/// The shared conservative charge, not measured heap usage, is 66,198,006 bytes for a MAX_BLOCK_BYTES
/// proposal with 19,215 transactions using 104-byte one-input/one-P2PKH-output payments. Eight such
/// ordinary transparent proposals need about 505 MiB; 8 * 72 MiB leaves about 14% layout headroom.
/// Smaller consensus-valid transactions can pack more decoded structs into a block and retain fewer
/// entries. Eviction only loses cache hits, never validation, as for large spent-output scripts.
const MAX_PROPOSAL_BYTES: usize = 8 * 72 * 1024 * 1024;

/// Charge once on a blocking worker, before taking the shared cache lock.
fn cache_proposal(
    proposals: &watch::Sender<ProposalCache>,
    key: [u8; 32],
    block: Arc<block::Block>,
    transaction_bytes: usize,
    contextual: Box<zs::ContextuallyVerifiedBlock>,
) {
    let retained_bytes = contextual.retained_bytes();
    proposals.send_modify(|proposals| {
        proposals.retain(|proposal| proposal.key != key);
        proposals.push_front(PreparedProposal {
            key,
            transaction_bytes,
            block,
            contextual: Arc::from(contextual),
            retained_bytes,
        });
        let mut retained_bytes = 0usize;
        let retained = proposals
            .iter()
            .take(8)
            .take_while(|proposal| {
                retained_bytes = retained_bytes.saturating_add(proposal.retained_bytes);
                retained_bytes <= MAX_PROPOSAL_BYTES
            })
            .count();
        proposals.truncate(retained);
    });
}

/// Read-only access to successfully validated proposals for decoding mined submissions.
///
/// This shares the verifier's bounded cache, not its validation authority: decoded blocks must
/// still pass the normal verifier, including checks of the submitted proof of work and live state.
#[derive(Clone, Debug)]
pub struct PreparedBlockCache(Arc<watch::Sender<ProposalCache>>);

impl PreparedBlockCache {
    /// Decodes a block, reusing parsed transactions only when their complete wire bytes match.
    ///
    /// Transaction-section length and the header with only nonce and solution normalized select a
    /// candidate. Exact streaming byte comparison permits reuse; the returned block keeps the
    /// submitted header. Misses, evictions and malformed input use ordinary deserialization.
    /// No full-body hash pass or transaction-byte allocation is needed. Call from a blocking worker.
    pub fn deserialize(&self, bytes: &[u8]) -> Result<Arc<block::Block>, SerializationError> {
        self.reuse(bytes)
            .map(Ok)
            .unwrap_or_else(|| block::Block::zcash_deserialize(bytes).map(Arc::new))
    }

    fn reuse(&self, bytes: &[u8]) -> Option<Arc<block::Block>> {
        if bytes.len() > usize::try_from(block::MAX_BLOCK_BYTES).expect("block limit fits usize")
            || self.0.borrow().is_empty()
        {
            return None;
        }
        let mut transactions = bytes;
        let header = block::Header::zcash_deserialize(&mut transactions).ok()?;
        let normalized_header = normalized_proposal_header(&header);
        let block = self
            .0
            .borrow()
            .iter()
            .find(|proposal| {
                proposal.transaction_bytes == transactions.len()
                    && normalized_proposal_header(&proposal.block.header) == normalized_header
            })?
            .block
            .clone();

        // Candidate selection is not authority to substitute different transaction bytes.
        struct Compare<'a>(&'a [u8]);
        impl Write for Compare<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0 = self
                    .0
                    .strip_prefix(bytes)
                    .ok_or(io::ErrorKind::InvalidData)?;
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut comparison = Compare(transactions);
        block.transactions.zcash_serialize(&mut comparison).ok()?;
        if !comparison.0.is_empty() {
            return None;
        }
        if *block.header == header {
            return Some(block);
        }
        Some(Arc::new(block::Block {
            header: Arc::new(header),
            transactions: block.transactions.clone(),
        }))
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

    #[error("block exceeds the maximum serialized size")]
    OversizedBlock,

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

/// Converts a semantic, contextual, or mined state commit error into a [`VerifyBlockError`].
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
            proposals: Arc::new(watch::channel(VecDeque::new()).0),
        }
    }

    /// Returns a read-only decoder sharing this verifier's successful proposal cache.
    pub fn prepared_blocks(&self) -> PreparedBlockCache {
        PreparedBlockCache(self.proposals.clone())
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

        let mut state_service = self.state_service.clone();
        let mut transaction_verifier = self.transaction_verifier.clone();
        let network = self.network.clone();
        let proposals = self.proposals.clone();

        let block = request.block();

        // We don't include the block hash, because it's likely already in a parent span
        let span = tracing::debug_span!("block", height = ?block.coinbase_height());

        async move {
            let phase_start = Instant::now();
            let hash = block.hash();
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
                    return Err(BlockError::AlreadyInChain(hash, location).into())
                }
                zs::Response::KnownBlock(None) => {}
                _ => unreachable!("wrong response to Request::KnownBlock"),
            }

            tracing::trace!("performing block checks");
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

            // Hash only when populating the cache or when there is something to reuse.
            let proposal_key = if request.is_proposal() || !proposals.borrow().is_empty() {
                let block = block.clone();
                let (key, size) =
                    tokio::task::spawn_blocking(move || proposal_key_and_size(&block))
                        .await
                        .expect("proposal hashing task must not panic");
                if u64::try_from(size).expect("block sizes fit u64") > block::MAX_BLOCK_BYTES {
                    return Err(VerifyBlockError::OversizedBlock);
                }
                Some((key, size))
            } else {
                None
            };
            let cached = if let Some((key, _)) = proposal_key.filter(|_| !request.is_proposal()) {
                proposals
                    .borrow()
                    .iter()
                    .find(|proposal| {
                        proposal.key == key
                            && proposal.block.header.previous_block_hash
                                == block.header.previous_block_hash
                    })
                    .map(|proposal| proposal.contextual.clone())
            } else {
                None
            };

            // Cache hits already bind the Merkle root through the verified proposal key.
            // On misses, bind the body before announcing early work and reuse these hashes below.
            let transaction_hashes = if cached.is_none() {
                let hashes: Arc<[_]> = block.transactions.iter().map(|t| t.hash()).collect();
                check::merkle_root_validity(&network, &block, &hashes)?;
                Some(hashes)
            } else {
                None
            };

            let now = Utc::now();
            check::time_is_valid_at(&block.header, now, &height, &hash)
                .map_err(VerifyBlockError::Time)?;

            // Keep early work alive while the remaining checks and state commit run.
            let _mining_candidate = if request.is_proposal() {
                None
            } else {
                state_service
                    .clone()
                    .oneshot(zs::Request::MiningCandidate(block.clone()))
                    .await
                    .ok()
            };

            if let Some(cached) = cached {
                // Cache hits skip transaction verification, so record their checks and commit
                // under the same phases as other blocks.
                metrics::histogram!(
                    "zebra.consensus.block.phase.duration_seconds",
                    "phase" => "block_checks"
                )
                .record(phase_start.elapsed().as_secs_f64());
                let phase_start = Instant::now();
                let mut contextual = (*cached).clone();
                contextual.rebind(block, received_time);
                let commit = match request {
                    Request::CommitMined(_, notifications) => {
                        contextual.into_mined_request(notifications)
                    }
                    Request::Commit(_) => {
                        zs::Request::CommitContextuallyVerifiedBlock(Box::new(contextual))
                    }
                    Request::CheckProposal(_) => unreachable!("proposals never reuse the cache"),
                };
                let result = state_service.oneshot(commit).await;
                metrics::histogram!(
                    "zebra.consensus.block.phase.duration_seconds",
                    "phase" => "state_commit"
                )
                .record(phase_start.elapsed().as_secs_f64());
                return match result {
                    Ok(zs::Response::Committed(committed_hash)) => {
                        assert_eq!(committed_hash, hash, "state must commit correct hash");
                        Ok(hash)
                    }
                    Err(source) => Err(map_commit_error(source, hash)),
                    _ => unreachable!("wrong response for contextual block commit"),
                };
            }

            let transaction_hashes =
                transaction_hashes.expect("cache hits return after committing the block");

            // Since errors cause an early exit, try to do the
            // quick checks first.

            // Quick field validity and structure checks
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

            // Bound shielded work before output recovery or proof verification.
            check::shielded_action_limits_are_valid(&block, &network, height, hash)?;

            // Now do the slower checks

            // Check compatibility with ZIP-212 shielded Sapling and Orchard coinbase output decryption
            tx::check::coinbase_outputs_are_decryptable(&coinbase_tx, &network, height)?;

            if !request.is_proposal() {
                metrics::histogram!(
                    "zebra.consensus.block.phase.duration_seconds",
                    "phase" => "block_checks"
                )
                .record(phase_start.elapsed().as_secs_f64());
            }
            let phase_start = Instant::now();

            // Send transactions to the transaction verifier to be checked
            let mut async_checks = FuturesUnordered::new();

            let known_utxos = Arc::new(transparent::new_ordered_outputs(
                &block,
                &transaction_hashes,
            ));

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
                        height,
                        time: block.header.time,
                    });
                async_checks.push(rsp);
            }
            tracing::trace!(len = async_checks.len(), "built async tx checks");

            // Get the transaction results back from the transaction verifier.

            // Sum up some block totals from the transaction responses.
            let mut sigops = 0;
            let mut block_miner_fees = Ok(Amount::zero());
            let mut transaction_error = None;

            use futures::StreamExt;
            while let Some(result) = async_checks.next().await {
                tracing::trace!(?result, remaining = async_checks.len());
                let response = match result {
                    Ok(response) if transaction_error.is_none() => response,
                    Ok(_) => continue,
                    Err(error) => {
                        // Dropping sibling futures would not stop their dispatched CPU work.
                        // Keep the first error and drain the existing checks before returning it.
                        if transaction_error.is_none() {
                            transaction_error = Some(error);
                        }
                        continue;
                    }
                };

                sigops += response.sigops;

                // Coinbase transactions consume the miner fee,
                // so they don't add any value to the block's total miner fee.
                if let Some(miner_fee) = response.miner_fee {
                    block_miner_fees += miner_fee;
                }
            }
            if let Some(error) = transaction_error {
                return Err(VerifyBlockError::Transaction(error.into()));
            }

            if !request.is_proposal() {
                metrics::histogram!(
                    "zebra.consensus.block.phase.duration_seconds",
                    "phase" => "transaction_verification"
                )
                .record(phase_start.elapsed().as_secs_f64());
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
                let block = prepared_block.block.clone();
                return match state_service
                    .ready()
                    .await
                    .map_err(VerifyBlockError::ValidateProposal)?
                    .call(zs::Request::CheckBlockProposalValidity(prepared_block))
                    .await
                    .map_err(VerifyBlockError::ValidateProposal)?
                {
                    zs::Response::ValidBlockProposal(contextual) => {
                        let (key, size) = proposal_key.expect("proposals always have a cache key");
                        let transaction_bytes = size - block.header.zcash_serialized_size();
                        tokio::task::spawn_blocking(move || {
                            cache_proposal(&proposals, key, block, transaction_bytes, contextual);
                        })
                        .await
                        .expect("proposal retention accounting must not panic");
                        Ok(hash)
                    }
                    _ => unreachable!("wrong response for CheckBlockProposalValidity"),
                };
            }
            let commit = match request {
                Request::CommitMined(_, notifications) => zs::Request::CommitMinedBlock {
                    block: prepared_block,
                    contextual: None,
                    notifications,
                },
                Request::Commit(_) => zs::Request::CommitSemanticallyVerifiedBlock(prepared_block),
                Request::CheckProposal(_) => unreachable!("proposals return after validation"),
            };

            let phase_start = Instant::now();
            let result = state_service
                .ready()
                .await
                .map_err(|source| VerifyBlockError::StateService { source, hash })?
                .call(commit)
                .await;
            metrics::histogram!(
                "zebra.consensus.block.phase.duration_seconds",
                "phase" => "state_commit"
            )
            .record(phase_start.elapsed().as_secs_f64());

            match result {
                Ok(zs::Response::Committed(committed_hash)) => {
                    assert_eq!(committed_hash, hash, "state must commit correct hash");
                    Ok(hash)
                }

                Err(source) => Err(map_commit_error(source, hash)),

                _ => unreachable!("wrong response for semantic block commit"),
            }
        }
        .instrument(span)
        .boxed()
    }
}

/// Hash wire bytes with normalized nonce and solution, counting the actual block size in one pass.
fn proposal_key_and_size(block: &block::Block) -> ([u8; 32], usize) {
    struct Writer(sha256d::Writer, usize);
    impl Write for Writer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let len = self.0.write(buf)?;
            self.1 += len;
            Ok(len)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    let header = normalized_proposal_header(&block.header);
    let mut writer = Writer(sha256d::Writer::default(), 0);
    header
        .zcash_serialize(&mut writer)
        .expect("hash writers cannot fail");
    block
        .transactions
        .zcash_serialize(&mut writer)
        .expect("hash writers cannot fail");
    // The key always hashes a Common solution, but Regtest's actual wire solution is shorter.
    if matches!(block.header.solution, equihash::Solution::Regtest(_)) {
        writer.1 -=
            equihash::Solution::SERIALIZED_SIZE - equihash::Solution::REGTEST_SERIALIZED_SIZE;
    }
    (writer.0.finish(), writer.1)
}

/// Keep byte-based decoding and parsed proposal lookup on the same normalized header.
fn normalized_proposal_header(header: &block::Header) -> block::Header {
    block::Header {
        nonce: [0; 32].into(),
        solution: equihash::Solution::Common([0; 1344]),
        ..*header
    }
}
