//! Batched, non-publishing proposal checks before verified-set admission.

use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
    task::{Context, Poll},
};

use futures::{future::BoxFuture, FutureExt};
use tokio::sync::oneshot;
use tower::ServiceExt;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, MAX_BLOCK_BYTES},
    ironwood, orchard,
    parameters::Network,
    sapling,
    serialization::ZcashSerialize,
    sprout,
    transaction::{Hash, UnminedTxId, VerifiedUnminedTx},
    transparent::OutPoint,
};
use zebra_rpc::{
    config::mining::{self, default_miner_address, MinerAddressType},
    proposal_block_from_template, BlockTemplateResponse, CoinbaseCache, LongPollInput, MinerParams,
    TransactionTemplate,
};
use zebra_state::{ReadRequest, ReadResponse};

use super::{
    block_template::{BlockVerifier, ReadState},
    downloads::TRANSACTION_VERIFY_TIMEOUT,
    storage::ExactTipRejectionError,
    BoxError, Storage,
};

/// Mempool policy, not consensus: at most 100 unique transactions including the candidate.
/// Matches zcashd's `DEFAULT_ANCESTOR_LIMIT`; blocks may contain larger packages.
pub(super) const MAX_PACKAGE_COUNT: usize = 100;

/// A deterministic failure of the bounded, mandatory transaction package.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct PackageRejection(&'static str);

/// A semantically verified transaction, owned by admission until its proposal check completes.
pub(super) struct Candidate {
    /// The individually verified transaction, still invisible to mempool consumers.
    pub tx: VerifiedUnminedTx,
    /// Its direct mempool spends, rechecked by final storage insertion.
    pub spent: Vec<OutPoint>,
    /// The committed parent its semantic verification used.
    pub parent: block::Hash,
    /// The original queue caller, held through stale retries.
    pub response: Option<oneshot::Sender<Result<(), BoxError>>>,
    stale: bool,
}

impl Candidate {
    pub fn new(
        tx: VerifiedUnminedTx,
        spent: Vec<OutPoint>,
        parent: block::Hash,
        response: Option<oneshot::Sender<Result<(), BoxError>>>,
    ) -> Self {
        Self {
            tx,
            spent,
            parent,
            response,
            stale: false,
        }
    }
}

/// One proposal over a batch of candidates and the union of their mempool ancestors.
struct Check {
    candidates: Vec<Candidate>,
    ancestors: Vec<UnminedTxId>,
    parent: block::Hash,
    stale: bool,
    work: BoxFuture<'static, Result<bool, BoxError>>,
}

/// The completed admission of one candidate: `Ok(true)` passed its proposal, `Ok(false)` needs
/// re-verification against the current tip, and an error rejects it.
pub(super) type Outcome = (Candidate, Result<bool, BoxError>);

/// One validation context, independent of the configured mining payout and template cache.
///
/// Candidates are checked in batches: one proposal holds every waiting candidate that fits in a
/// block without conflicting with an earlier one. A failed batch is split in half until each
/// failure is attributed to a single candidate, so only single-candidate results are reported or
/// cached as rejections.
pub(super) struct Admission {
    network: Network,
    read_state: ReadState,
    verifier: BlockVerifier,
    miner_params: MinerParams,
    coinbase_cache: CoinbaseCache,
    /// Candidates in arrival order, waiting for a batch.
    waiting: VecDeque<Candidate>,
    /// Halves of failed batches, checked depth-first before any new batch.
    splits: Vec<Vec<Candidate>>,
    /// At most one proposal, retained until its blocking verification work has finished.
    check: Option<Check>,
}

impl Admission {
    /// Create the private transparent validation context, even on non-mining nodes.
    pub fn new(network: Network, read_state: ReadState, verifier: BlockVerifier) -> Self {
        let miner_params = MinerParams::new(
            &network,
            mining::Config {
                miner_address: Some(
                    default_miner_address(network.kind(), &MinerAddressType::Transparent)
                        .parse()
                        .expect("the built-in validation address is valid"),
                ),
                ..Default::default()
            },
        )
        .expect("the transparent validation payout matches the network");
        Self {
            network,
            read_state,
            verifier,
            miner_params,
            coinbase_cache: CoinbaseCache::default(),
            waiting: VecDeque::new(),
            splits: Vec::new(),
            check: None,
        }
    }

    /// The number of candidates waiting for a batch or a split recheck.
    #[cfg(test)]
    pub fn queued(&self) -> usize {
        self.waiting.len() + self.splits.iter().map(Vec::len).sum::<usize>()
    }

    /// Queue a candidate for the next batch, without inserting or publishing it.
    pub fn push(&mut self, candidate: Candidate) {
        self.waiting.push_back(candidate);
    }

    /// Mark all work stale, but retain it until all proposal/proof work actually finishes.
    pub fn reset(&mut self) {
        if let Some(check) = &mut self.check {
            check.stale = true;
        }
        for candidate in self
            .waiting
            .iter_mut()
            .chain(self.splits.iter_mut().flatten())
        {
            candidate.stale = true;
        }
    }

    /// Return the outcomes of the next completed proposal, or stale candidates needing a retry.
    ///
    /// Callers must apply each result to storage before polling again, because later batches
    /// snapshot their ancestor packages from it.
    pub fn poll(
        &mut self,
        cx: &mut Context<'_>,
        storage: &mut Storage,
        parent: block::Hash,
    ) -> Poll<Vec<Outcome>> {
        loop {
            if self.check.is_none() {
                let retries = self.start(storage, parent);
                if !retries.is_empty() {
                    return Poll::Ready(retries);
                }
            }
            let Some(check) = &mut self.check else {
                return Poll::Pending;
            };
            let Poll::Ready(mut result) = check.work.as_mut().poll(cx) else {
                return Poll::Pending;
            };
            let mut check = self.check.take().expect("the completed check is present");
            if check.stale
                || check.parent != parent
                || check.ancestors.iter().any(|id| {
                    storage
                        .transactions()
                        .get(&id.mined_id())
                        .map(|tx| tx.transaction.id)
                        != Some(*id)
                })
            {
                result = Ok(false);
            }
            match result {
                // Any failure of a batch may belong to one of its candidates, including errors
                // that are not cached, so each half is checked again without its sibling.
                Err(_) if check.candidates.len() > 1 => {
                    let second = check.candidates.split_off(check.candidates.len() / 2);
                    self.splits.push(second);
                    self.splits.push(check.candidates);
                }
                Err(error) => {
                    let candidate = check
                        .candidates
                        .pop()
                        .expect("a failed check has one candidate");
                    if deterministic_rejection(error.as_ref()) {
                        storage.reject(
                            candidate.tx.transaction.id,
                            ExactTipRejectionError::FailedProposal {
                                reason: error.to_string(),
                                ancestors: check.ancestors.into(),
                            }
                            .into(),
                        );
                    }
                    return Poll::Ready(vec![(candidate, Err(error))]);
                }
                Ok(passed) => {
                    return Poll::Ready(
                        check
                            .candidates
                            .into_iter()
                            .map(|candidate| (candidate, Ok(passed)))
                            .collect(),
                    );
                }
            }
        }
    }

    /// Start the next split or batch, returning the candidates that are stale for `parent`.
    ///
    /// A split is a subset of a batch that already fit in a block without conflicts, so its
    /// candidates are only deferred if their ancestor packages changed since.
    fn start(&mut self, storage: &Storage, parent: block::Hash) -> Vec<Outcome> {
        let split = self.splits.pop();
        let is_split = split.is_some();
        let mut queue = split.map_or_else(|| std::mem::take(&mut self.waiting), VecDeque::from);
        let mut retries = Vec::new();
        let mut batch = Batch::default();
        let mut candidates = Vec::new();
        let mut deferred = VecDeque::new();
        let mut package = None;
        while let Some(candidate) = queue.pop_front() {
            if !candidate.is_current(storage, parent) {
                retries.push((candidate, Ok(false)));
                continue;
            }
            match batch.add(storage, &candidate) {
                Ok(true) => candidates.push(candidate),
                // Its own package failed, so check it alone to report the failure.
                Err(error) if candidates.is_empty() => {
                    package = Some(Err(error));
                    candidates.push(candidate);
                    break;
                }
                Ok(false) | Err(_) => deferred.push_back(candidate),
            }
        }
        deferred.append(&mut queue);
        if is_split {
            while let Some(candidate) = deferred.pop_back() {
                self.waiting.push_front(candidate);
            }
        } else {
            self.waiting = deferred;
        }
        if candidates.is_empty() {
            return retries;
        }
        let package = package.unwrap_or_else(|| Ok(batch.package(storage, &candidates)));
        let work = verify_package(
            self.network.clone(),
            self.read_state.clone(),
            self.verifier.clone(),
            self.miner_params.clone(),
            self.coinbase_cache.clone(),
            package,
            parent,
        )
        .boxed();
        self.check = Some(Check {
            candidates,
            ancestors: batch.ancestors,
            parent,
            stale: false,
            work,
        });
        retries
    }
}

impl Candidate {
    /// Whether this candidate's semantic verification and direct mempool spends still apply.
    fn is_current(&self, storage: &Storage, parent: block::Hash) -> bool {
        !self.stale && self.parent == parent && required_outputs_available(storage, &self.spent)
    }
}

/// A transparent spend or revealed nullifier: two transactions with the same one conflict.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Spend {
    Transparent(OutPoint),
    Sprout(sprout::Nullifier),
    Sapling(sapling::Nullifier),
    Orchard(orchard::Nullifier),
    Ironwood(ironwood::Nullifier),
}

fn spends_of(tx: &VerifiedUnminedTx) -> impl Iterator<Item = Spend> + '_ {
    let tx = &tx.transaction.transaction;
    tx.spent_outpoints()
        .map(Spend::Transparent)
        .chain(tx.sprout_nullifiers().map(Spend::Sprout))
        .chain(tx.sapling_nullifiers().map(Spend::Sapling))
        .chain(tx.orchard_nullifiers().map(Spend::Orchard))
        .chain(tx.ironwood_nullifiers().map(Spend::Ironwood))
}

/// The union of candidate packages that fits in one block without conflicting spends.
#[derive(Default)]
struct Batch {
    /// Mempool ancestors in topological order, witnessed for staleness and rejection expiry.
    ancestors: Vec<UnminedTxId>,
    included: HashSet<Hash>,
    spends: HashSet<Spend>,
    bytes: usize,
    sigops: u32,
}

impl Batch {
    /// Add a candidate and its missing ancestors, returning `Ok(false)` if they would conflict
    /// with the batch or exceed block limits, and an error if its own package is invalid.
    ///
    /// An empty batch always accepts a valid package, so the first waiting candidate is checked.
    fn add(&mut self, storage: &Storage, candidate: &Candidate) -> Result<bool, BoxError> {
        if self
            .included
            .contains(&candidate.tx.transaction.id.mined_id())
        {
            return Ok(false);
        }
        let mut ancestors = Vec::new();
        let package = package_refs(storage, &candidate.tx, &candidate.spent, &mut ancestors)
            .inspect_err(|_| {
                // A failed package is checked alone, so its witnesses bound its cached rejection.
                if self.included.is_empty() {
                    self.ancestors = ancestors;
                }
            })?;
        let added: Vec<_> = package
            .into_iter()
            .filter(|tx| !self.included.contains(&tx.transaction.id.mined_id()))
            .collect();
        let mut spends = HashSet::new();
        let mut bytes = self.bytes;
        let mut sigops = self.sigops;
        for tx in &added {
            // Only conflicts with the rest of the batch are deferred. A package that conflicts
            // with itself fails its proposal, so it is isolated and rejected like any other.
            for spend in spends_of(tx) {
                if self.spends.contains(&spend) {
                    return Ok(false);
                }
                spends.insert(spend);
            }
            bytes = bytes.saturating_add(tx.transaction.size);
            sigops = sigops.saturating_add(tx.block_sigop_count());
        }
        if check_limits(bytes, sigops).is_err() {
            return Ok(false);
        }
        self.spends.extend(spends);
        self.bytes = bytes;
        self.sigops = sigops;
        for tx in added {
            let id = tx.transaction.id;
            self.included.insert(id.mined_id());
            if id != candidate.tx.transaction.id {
                self.ancestors.push(id);
            }
        }
        Ok(true)
    }

    /// Ancestors first, so every transaction follows the outputs it spends.
    fn package(&self, storage: &Storage, candidates: &[Candidate]) -> Vec<VerifiedUnminedTx> {
        self.ancestors
            .iter()
            .map(|id| storage.transactions()[&id.mined_id()].clone())
            .chain(candidates.iter().map(|candidate| candidate.tx.clone()))
            .collect()
    }
}

/// Only cache known transaction/package failures. In particular, `ValidateProposal` also wraps
/// readiness and state-service failures, and transaction services erase unknown errors into
/// `InternalDowncastError`. Neither is evidence that this transaction is invalid.
fn deterministic_rejection(error: &(dyn std::error::Error + 'static)) -> bool {
    use zebra_consensus::{error::TransactionError, BlockError, RouterError, VerifyBlockError};
    use zebra_state::{CommitBlockError, CommitSemanticallyVerifiedError, ValidateContextError};

    if error.is::<PackageRejection>() || error.is::<zebra_chain::amount::Error>() {
        return true;
    }
    if let Some(RouterError::Block { source }) = error.downcast_ref() {
        return deterministic_rejection(source.as_ref());
    }
    if let Some(error) = error.downcast_ref::<VerifyBlockError>() {
        return match error {
            VerifyBlockError::Block { source } => deterministic_rejection(source),
            VerifyBlockError::Transaction(source) => deterministic_rejection(source),
            VerifyBlockError::ValidateProposal(source) => deterministic_rejection(source.as_ref()),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<BlockError>() {
        return match error {
            BlockError::Transaction(source) => deterministic_rejection(source),
            BlockError::DuplicateTransaction
            | BlockError::WrongTransactionConsensusBranchId
            | BlockError::TooManyTransparentSignatureOperations { .. }
            | BlockError::SummingMinerFees { .. } => true,
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<TransactionError>() {
        return match error {
            TransactionError::ValidateContextError(source) => {
                deterministic_rejection(source.as_ref())
            }
            TransactionError::BadBalance
            | TransactionError::IncorrectFee
            | TransactionError::DuplicateTransparentSpend(_)
            | TransactionError::DuplicateSproutNullifier(_)
            | TransactionError::NoInputs
            | TransactionError::NoOutputs
            | TransactionError::BothVPubsNonZero
            | TransactionError::DisabledAddToSproutPool
            | TransactionError::NegativeOrchardValueBalance
            | TransactionError::NotEnoughOrchardFlags
            | TransactionError::NotEnoughIronwoodFlags
            | TransactionError::SmallOrder
            | TransactionError::Amount(_)
            | TransactionError::Balance(_)
            | TransactionError::DuplicateSaplingNullifier(_)
            | TransactionError::DuplicateOrchardNullifier(_)
            | TransactionError::DuplicateIronwoodNullifier(_)
            | TransactionError::ExpiredTransaction { .. }
            | TransactionError::MaximumExpiryHeight { .. }
            | TransactionError::LockedUntilAfterBlockHeight(_)
            | TransactionError::ImmatureTransparentCoinbaseSpend { .. }
            | TransactionError::UnshieldedTransparentCoinbaseSpend { .. }
            | TransactionError::WrongVersion
            | TransactionError::UnsupportedByNetworkUpgrade(..)
            | TransactionError::WrongConsensusBranchId
            | TransactionError::MissingConsensusBranchId
            | TransactionError::Script(_)
            | TransactionError::Groth16(_)
            | TransactionError::MalformedGroth16(_)
            | TransactionError::Ed25519(_)
            | TransactionError::RedJubjub(_)
            | TransactionError::RedPallas(_)
            | TransactionError::SaplingVerificationFailed
            | TransactionError::Halo2VerificationFailed
            | TransactionError::OrchardProofSize
            | TransactionError::IronwoodProofSize => true,
            // Time locks may become valid at the same tip; unknown/service errors are retryable.
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<CommitSemanticallyVerifiedError>() {
        return deterministic_rejection(error.inner());
    }
    if let Some(CommitBlockError::ValidateContextError(source)) = error.downcast_ref() {
        return deterministic_rejection(source.as_ref());
    }
    matches!(
        error.downcast_ref::<ValidateContextError>(),
        Some(
            ValidateContextError::DuplicateTransparentSpend { .. }
                | ValidateContextError::MissingTransparentOutput { .. }
                | ValidateContextError::EarlyTransparentSpend { .. }
                | ValidateContextError::UnshieldedTransparentCoinbaseSpend { .. }
                | ValidateContextError::ImmatureTransparentCoinbaseSpend { .. }
                | ValidateContextError::DuplicateSproutNullifier { .. }
                | ValidateContextError::DuplicateSaplingNullifier { .. }
                | ValidateContextError::DuplicateOrchardNullifier { .. }
                | ValidateContextError::DuplicateIronwoodNullifier { .. }
                | ValidateContextError::NegativeRemainingTransactionValue { .. }
                | ValidateContextError::CalculateRemainingTransactionValue { .. }
                | ValidateContextError::CalculateTransactionValueBalances { .. }
                | ValidateContextError::CalculateBlockChainValueChange { .. }
                | ValidateContextError::AddValuePool { .. }
                | ValidateContextError::UnknownSproutAnchor { .. }
                | ValidateContextError::UnknownSaplingAnchor { .. }
                | ValidateContextError::UnknownOrchardAnchor { .. }
                | ValidateContextError::UnknownIronwoodAnchor { .. }
        )
    )
}

/// Return the complete mandatory ancestor closure followed by the candidate, never a selection.
/// Record exact ancestor witnesses as they are visited, including the bounded prefix proving a
/// limit failure, so rejection caching can expire when that dependency context changes.
fn package_refs<'a>(
    storage: &'a Storage,
    candidate: &'a VerifiedUnminedTx,
    spent: &[OutPoint],
    ancestors: &mut Vec<UnminedTxId>,
) -> Result<Vec<&'a VerifiedUnminedTx>, BoxError> {
    if !required_outputs_available(storage, spent) {
        return Err("a required mempool output is no longer available".into());
    }
    let mut bytes = candidate.transaction.size;
    let mut sigops = candidate.block_sigop_count();
    check_limits(bytes, sigops)?;
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    let mut stack: Vec<(Hash, bool)> = spent
        .iter()
        .map(|outpoint| (outpoint.hash, false))
        .collect();
    let mut package = Vec::new();
    while let Some((hash, expanded)) = stack.pop() {
        if expanded {
            visiting.remove(&hash);
            visited.insert(hash);
            package.push(&storage.transactions()[&hash]);
            continue;
        }
        if visited.contains(&hash) {
            continue;
        }
        if hash == candidate.transaction.id.mined_id() || !visiting.insert(hash) {
            return Err(
                PackageRejection("cyclic transaction dependencies cannot form a block").into(),
            );
        }
        let tx = storage
            .transactions()
            .get(&hash)
            .ok_or("a required ancestor is no longer available")?;
        ancestors.push(tx.transaction.id);
        if ancestors.len() >= MAX_PACKAGE_COUNT {
            return Err(PackageRejection(
                "required transaction package exceeds the mempool ancestor limit",
            )
            .into());
        }
        bytes = bytes
            .checked_add(tx.transaction.size)
            .ok_or(PackageRejection("package byte count overflow"))?;
        sigops = sigops
            .checked_add(tx.block_sigop_count())
            .ok_or(PackageRejection("package sigop count overflow"))?;
        check_limits(bytes, sigops)?;
        stack.push((hash, true));
        if let Some(dependencies) = storage.transaction_dependencies().dependencies().get(&hash) {
            stack.extend(dependencies.iter().map(|hash| (*hash, false)));
        }
    }
    package.push(candidate);
    Ok(package)
}

/// The candidate package as owned transactions.
#[cfg(test)]
pub(super) fn required_package(
    storage: &Storage,
    candidate: &VerifiedUnminedTx,
    spent: &[OutPoint],
    ancestors: &mut Vec<UnminedTxId>,
) -> Result<Vec<VerifiedUnminedTx>, BoxError> {
    Ok(package_refs(storage, candidate, spent, ancestors)?
        .into_iter()
        .cloned()
        .collect())
}

fn required_outputs_available(storage: &Storage, spent: &[OutPoint]) -> bool {
    spent.iter().all(|outpoint| {
        storage
            .transactions()
            .get(&outpoint.hash)
            .is_some_and(|tx| {
                usize::try_from(outpoint.index)
                    .ok()
                    .is_some_and(|index| tx.transaction.transaction.outputs().get(index).is_some())
            })
    })
}

fn check_limits(bytes: usize, sigops: u32) -> Result<(), BoxError> {
    if u64::try_from(bytes)? > MAX_BLOCK_BYTES || sigops > zebra_consensus::MAX_BLOCK_SIGOPS {
        return Err(PackageRejection(
            "required transaction package exceeds block byte or sigop limits",
        )
        .into());
    }
    Ok(())
}

/// Check a package against one committed parent, without publishing or committing it.
async fn verify_package<ReadStateService, Verifier>(
    network: Network,
    read_state: ReadStateService,
    verifier: Verifier,
    miner_params: MinerParams,
    cache: CoinbaseCache,
    package: Result<Vec<VerifiedUnminedTx>, BoxError>,
    parent: block::Hash,
) -> Result<bool, BoxError>
where
    ReadStateService: zebra_state::ReadState,
    Verifier: zebra_consensus::router::service_trait::BlockVerifierService,
{
    let result: Result<bool, BoxError> = async {
        let package = package?;
        let chain_info = match tokio::time::timeout(
            TRANSACTION_VERIFY_TIMEOUT,
            read_state.clone().oneshot(ReadRequest::ChainInfo),
        )
        .await??
        {
            ReadResponse::ChainInfo(info) => info,
            _ => unreachable!("ChainInfo requests return ChainInfo responses"),
        };
        if chain_info.tip_hash != parent {
            return Ok(false);
        }
        let proposal = tokio::task::spawn_blocking(move || -> Result<_, BoxError> {
            let height = chain_info.tip_height.next()?;
            if zebra_chain::parameters::NetworkUpgrade::current(&network, height)
                < zebra_chain::parameters::NetworkUpgrade::Canopy
                || (chain_info.chain_history_root.is_none()
                    && zebra_chain::parameters::NetworkUpgrade::Heartwood
                        .activation_height(&network)
                        != Some(height))
            {
                return Err("proposal admission requires post-Canopy chain information".into());
            }
            let fees = package
                .iter()
                .map(|tx| tx.miner_fee)
                .sum::<zebra_chain::amount::Result<Amount<NonNegative>>>()?;
            // Use the fallible constructor before the infallible template helper, including
            // checked fee/subsidy arithmetic. This cache never contains a mining payout.
            if cache.get(height, fees).is_none() {
                cache.store(
                    height,
                    fees,
                    TransactionTemplate::new_coinbase(&network, height, &miner_params, fees)?,
                );
            }
            let id = LongPollInput::new(
                chain_info.tip_height,
                parent,
                chain_info.max_time,
                package.iter().map(|tx| tx.transaction.id),
            )
            .generate_id();
            let template = BlockTemplateResponse::from_transactions(
                &network,
                &cache,
                &miner_params,
                &chain_info,
                id,
                package,
                None,
            );
            let proposal = proposal_block_from_template(&template, None, &network)?;
            if u64::try_from(proposal.zcash_serialized_size()).expect("block sizes fit u64")
                > MAX_BLOCK_BYTES
            {
                return Err(PackageRejection(
                    "required transaction package exceeds the block byte limit",
                )
                .into());
            }
            Ok(Arc::new(proposal))
        })
        .await??;

        // ponytail: recheck one block-bounded package; reuse proofs only through a future
        // consensus API bound to the exact witnessed transactions and committed parent.
        let check = verifier.oneshot(zebra_consensus::Request::CheckProposal(proposal));
        tokio::pin!(check);
        let result = match tokio::time::timeout(TRANSACTION_VERIFY_TIMEOUT, &mut check).await {
            Ok(result) => result.map(|_| ()),
            Err(error) => {
                // Dropping a verifier future does not stop its blocking cryptographic work.
                // Drain the same request before freeing the single admission slot.
                let _ = check.await;
                Err(error.into())
            }
        };
        result?;
        Ok(true)
    }
    .await;
    // Even a local package-limit failure belongs to one committed parent. A lagging tip
    // notification must not turn obsolete work into a cached deterministic rejection.
    let committed_parent = match tokio::time::timeout(
        TRANSACTION_VERIFY_TIMEOUT,
        read_state.oneshot(ReadRequest::Tip),
    )
    .await??
    {
        ReadResponse::Tip(tip) => tip.map(|(_, hash)| hash),
        _ => unreachable!("Tip requests return Tip responses"),
    };
    if committed_parent != Some(parent) {
        return Ok(false);
    }
    result
}
