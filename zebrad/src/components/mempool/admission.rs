//! Batched, non-publishing proposal checks before verified-set admission.

use std::{
    collections::HashSet,
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

/// Room left in a batch for the admission proposal's coinbase: a transparent payout to the
/// built-in validation address, plus any funding stream outputs.
pub(super) const COINBASE_RESERVE_BYTES: usize = 1_000;

/// Signature operations left in a batch for the admission proposal's coinbase.
pub(super) const COINBASE_RESERVE_SIGOPS: u32 = 20;

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
    /// The committed parent its semantic verification used, if the state had a tip.
    pub parent: Option<block::Hash>,
    /// The original queue caller, held through stale retries.
    pub response: Option<oneshot::Sender<Result<(), BoxError>>>,
    stale: bool,
}

impl Candidate {
    pub fn new(
        tx: VerifiedUnminedTx,
        spent: Vec<OutPoint>,
        parent: Option<block::Hash>,
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

    /// Whether this candidate's semantic verification and direct mempool spends still apply.
    fn is_current(&self, storage: &Storage, parent: block::Hash) -> bool {
        !self.stale
            && self.parent == Some(parent)
            && required_outputs_available(storage, &self.spent)
    }
}

/// One proposal over a batch of candidates and the union of their mempool ancestors.
struct Check {
    candidates: Vec<Candidate>,
    ancestors: Vec<UnminedTxId>,
    parent: block::Hash,
    work: BoxFuture<'static, Result<bool, BoxError>>,
}

/// The result of a candidate's admission.
pub(super) enum Verdict {
    /// Its proposal passed, so it can be inserted into storage.
    Passed,
    /// Its semantic verification no longer applies to the committed tip.
    Retry,
    /// Its proposal failed on its own.
    Rejected(BoxError),
}

/// One validation context, independent of the configured mining payout and template cache.
///
/// Candidates are checked in batches: one proposal holds every waiting candidate that fits in a
/// block without conflicting with an earlier one. A failed batch is split into up to
/// `split_width` pieces, which are checked concurrently, until each failure is attributed to a
/// single candidate, so only single-candidate results are reported or cached as rejections.
///
/// A new batch starts only after every piece of the previous one has finished, so the candidates
/// in flight never exceed one block's worth. Each piece also carries its own copy of any unmined
/// ancestors, which pieces may share.
pub(super) struct Admission {
    network: Network,
    read_state: ReadState,
    verifier: BlockVerifier,
    miner_params: MinerParams,
    coinbase_cache: CoinbaseCache,
    /// Candidates in arrival order, waiting for a batch.
    waiting: Vec<Candidate>,
    /// Pieces of failed or outdated batches, checked depth-first before any new batch.
    splits: Vec<Vec<Candidate>>,
    /// Proposals retained until their blocking verification work has finished: one batch, or
    /// up to `split_width` pieces of one.
    checks: Vec<Check>,
    /// The number of pieces a failed batch is split into, and checked at once.
    split_width: usize,
}

impl Admission {
    /// Create the private transparent validation context, even on non-mining nodes.
    pub fn new(
        network: Network,
        read_state: ReadState,
        verifier: BlockVerifier,
        split_width: usize,
    ) -> Self {
        let miner_params = validation_miner_params(&network);
        Self {
            network,
            read_state,
            verifier,
            miner_params,
            coinbase_cache: CoinbaseCache::default(),
            waiting: Vec::new(),
            splits: Vec::new(),
            checks: Vec::new(),
            split_width: split_width.max(2),
        }
    }

    /// The number of candidates waiting for a batch or a split recheck.
    #[cfg(test)]
    pub fn queued(&self) -> usize {
        self.waiting.len() + self.splits.iter().map(Vec::len).sum::<usize>()
    }

    /// The number of proposals still retained for their verification work.
    #[cfg(test)]
    pub fn checks_in_flight(&self) -> usize {
        self.checks.len()
    }

    /// Queue a candidate for the next batch, without inserting or publishing it.
    pub fn push(&mut self, candidate: Candidate) {
        self.waiting.push(candidate);
    }

    /// Mark all work stale, but retain it until all proposal/proof work actually finishes.
    pub fn reset(&mut self) {
        for candidate in self
            .waiting
            .iter_mut()
            .chain(self.splits.iter_mut().flatten())
            .chain(
                self.checks
                    .iter_mut()
                    .flat_map(|check| &mut check.candidates),
            )
        {
            candidate.stale = true;
        }
    }

    /// Carry candidates verified at `previous` onto its child `tip`, and return the ones `tip`
    /// mined.
    ///
    /// Stored mempool transactions stay across a new block without semantic re-verification,
    /// and each carried candidate's next proposal checks it against the new block. In-flight
    /// checks for `previous` still finish, then their carried candidates are rebuilt.
    pub fn grow(
        &mut self,
        previous: block::Hash,
        tip: block::Hash,
        mined_ids: &HashSet<Hash>,
    ) -> Vec<Candidate> {
        let mut mined = Vec::new();
        let mut carry = |candidates: &mut Vec<Candidate>| {
            let (was_mined, kept): (Vec<_>, Vec<_>) = std::mem::take(candidates)
                .into_iter()
                .partition(|candidate| mined_ids.contains(&candidate.tx.transaction.id.mined_id()));
            mined.extend(was_mined);
            *candidates = kept;
            for candidate in candidates.iter_mut() {
                if candidate.parent == Some(previous) {
                    candidate.parent = Some(tip);
                    // Outputs of mined parents are now chain outputs, checked by the proposal.
                    candidate
                        .spent
                        .retain(|outpoint| !mined_ids.contains(&outpoint.hash));
                }
            }
        };
        carry(&mut self.waiting);
        self.splits.iter_mut().for_each(&mut carry);
        self.checks
            .iter_mut()
            .for_each(|check| carry(&mut check.candidates));
        self.splits.retain(|split| !split.is_empty());
        mined
    }

    /// Return the verdicts of the next completed proposal, or stale candidates needing a retry.
    ///
    /// Callers must apply each verdict to storage before polling again, because later batches
    /// snapshot their ancestor packages from it.
    pub fn poll(
        &mut self,
        cx: &mut Context<'_>,
        storage: &mut Storage,
        parent: block::Hash,
    ) -> Poll<Vec<(Candidate, Verdict)>> {
        loop {
            let retries = self.start_ready(storage, parent);
            if !retries.is_empty() {
                return Poll::Ready(retries);
            }
            let Some((index, result)) =
                self.checks
                    .iter_mut()
                    .enumerate()
                    .find_map(|(index, check)| match check.work.as_mut().poll(cx) {
                        Poll::Ready(result) => Some((index, result)),
                        Poll::Pending => None,
                    })
            else {
                return Poll::Pending;
            };
            let check = self.checks.swap_remove(index);
            // Requeued candidates are started before the other checks are polled again.
            if let Some(verdicts) = self.finish(check, result, storage, parent) {
                return Poll::Ready(verdicts);
            }
        }
    }

    /// Return a completed check's verdicts, or requeue its candidates without verdicts if its
    /// packages changed or its failure is not yet attributed to one candidate.
    fn finish(
        &mut self,
        mut check: Check,
        result: Result<bool, BoxError>,
        storage: &mut Storage,
        parent: block::Hash,
    ) -> Option<Vec<(Candidate, Verdict)>> {
        // `grow` removes mined candidates from in-flight checks, which are kept until their
        // work finishes, so a check can end with no candidates left.
        if check.candidates.is_empty() {
            return None;
        }
        // Rebuild outdated proposals: `start` retries the candidates whose semantic
        // verification no longer applies, and checks the rest against the current parent.
        if check.parent != parent
            || check.candidates.iter().any(|candidate| candidate.stale)
            || !storage.contains_exact_ancestors(&check.ancestors)
        {
            if !check.candidates.is_empty() {
                self.splits.push(check.candidates);
            }
            return None;
        }
        // The committed tip moved before this mempool saw it.
        if matches!(result, Ok(false)) {
            return Some(
                check
                    .candidates
                    .into_iter()
                    .map(|candidate| (candidate, Verdict::Retry))
                    .collect(),
            );
        }
        match result {
            Ok(_) => Some(
                check
                    .candidates
                    .into_iter()
                    .map(|candidate| (candidate, Verdict::Passed))
                    .collect(),
            ),
            // Any failure of a batch may belong to one of its candidates, including errors that
            // are not cached, so each piece is checked again without its siblings.
            Err(_) if check.candidates.len() > 1 => {
                let size = check.candidates.len().div_ceil(self.split_width);
                let mut rest = check.candidates;
                while !rest.is_empty() {
                    // Push the last piece first, so pieces pop in arrival order.
                    self.splits
                        .push(rest.split_off((rest.len() - 1) / size * size));
                }
                None
            }
            Err(error) => {
                let candidate = check.candidates.pop().expect("already checked nonempty");
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
                Some(vec![(candidate, Verdict::Rejected(error))])
            }
        }
    }

    /// Start pending pieces up to the split width, then a new batch once all pieces have
    /// finished, returning the candidates that are stale for `parent`.
    fn start_ready(&mut self, storage: &Storage, parent: block::Hash) -> Vec<(Candidate, Verdict)> {
        let mut retries = Vec::new();
        while self.checks.len() < self.split_width {
            let Some(split) = self.splits.pop() else {
                break;
            };
            let deferred = self.start(split, storage, parent, &mut retries);
            // A split is a subset of a batch, so its candidates are only deferred if their
            // ancestor packages changed since. The first candidate always starts.
            if !deferred.is_empty() {
                self.splits.push(deferred);
            }
        }
        if self.splits.is_empty() && self.checks.is_empty() {
            let waiting = std::mem::take(&mut self.waiting);
            self.waiting = self.start(waiting, storage, parent, &mut retries);
        }
        retries
    }

    /// Start one batch proposal from `queue` in arrival order, plus one proposal for each
    /// candidate whose own package failed, returning the candidates that did not fit.
    fn start(
        &mut self,
        queue: Vec<Candidate>,
        storage: &Storage,
        parent: block::Hash,
        retries: &mut Vec<(Candidate, Verdict)>,
    ) -> Vec<Candidate> {
        let mut batch = Batch::new(&self.network);
        let mut candidates = Vec::new();
        let mut deferred = Vec::new();
        for candidate in queue {
            if !candidate.is_current(storage, parent) {
                retries.push((candidate, Verdict::Retry));
                continue;
            }
            match batch.add(storage, &candidate) {
                Fit::Added => candidates.push(candidate),
                Fit::Deferred => deferred.push(candidate),
                // Check it alone, so its failure is reported against the committed parent.
                Fit::Invalid(error, ancestors) => {
                    self.spawn_check(vec![candidate], ancestors, Err(error), parent)
                }
            }
        }
        if !candidates.is_empty() {
            let package = Ok(batch.package(storage, &candidates));
            self.spawn_check(candidates, batch.ancestors, package, parent);
        }
        deferred
    }

    /// Start verifying `package` as a proposal for `candidates` on top of `parent`.
    fn spawn_check(
        &mut self,
        candidates: Vec<Candidate>,
        ancestors: Vec<UnminedTxId>,
        package: Result<Vec<VerifiedUnminedTx>, BoxError>,
        parent: block::Hash,
    ) {
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
        self.checks.push(Check {
            candidates,
            ancestors,
            parent,
            work,
        });
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

/// Whether a candidate's package joined a batch.
enum Fit {
    Added,
    /// It conflicts with the batch or would exceed block limits, so it waits for a later one.
    Deferred,
    /// Its own package is invalid, with the ancestors witnessed before it failed.
    Invalid(BoxError, Vec<UnminedTxId>),
}

/// The union of candidate packages that fits in one block without conflicting spends.
struct Batch {
    /// Mempool ancestors in topological order, witnessed for staleness and rejection expiry.
    ancestors: Vec<UnminedTxId>,
    included: HashSet<Hash>,
    spends: HashSet<Spend>,
    /// Block bytes used, starting with the header, transaction count and coinbase reserve.
    bytes: usize,
    /// Block sigops used, starting with the coinbase reserve.
    sigops: u32,
}

impl Batch {
    fn new(network: &Network) -> Self {
        Self {
            ancestors: Vec::new(),
            included: HashSet::new(),
            spends: HashSet::new(),
            // A CompactSize transaction count takes at most 5 bytes for any block-sized count.
            bytes: block::Header::serialized_size(network) + 5 + COINBASE_RESERVE_BYTES,
            sigops: COINBASE_RESERVE_SIGOPS,
        }
    }

    /// Whether these block totals leave room for the header and coinbase reserve.
    ///
    /// The reserve over-estimates the coinbase, so the first candidate is only held to its own
    /// package limits, and its proposal decides whether the real block fits. A batch always
    /// starts.
    fn fits(&self, bytes: usize, sigops: u32) -> bool {
        self.included.is_empty() || check_limits(bytes, sigops).is_ok()
    }

    /// Add a candidate and its missing ancestors, leaving the batch unchanged unless it fits.
    fn add(&mut self, storage: &Storage, candidate: &Candidate) -> Fit {
        let tx = &candidate.tx;
        // Skip the ancestor walk when the candidate alone cannot fit.
        if self.included.contains(&tx.transaction.id.mined_id())
            || !self.fits(
                self.bytes.saturating_add(tx.transaction.size),
                self.sigops.saturating_add(tx.block_sigop_count()),
            )
        {
            return Fit::Deferred;
        }
        let mut ancestors = Vec::new();
        let package = match required_package(storage, tx, &candidate.spent, &mut ancestors) {
            Ok(package) => package,
            Err(error) => return Fit::Invalid(error, ancestors),
        };
        let added: Vec<_> = package
            .into_iter()
            .filter(|tx| !self.included.contains(&tx.transaction.id.mined_id()))
            .collect();
        let mut spends = Vec::new();
        let mut bytes = self.bytes;
        let mut sigops = self.sigops;
        for tx in &added {
            // Only conflicts with the rest of the batch are deferred. A package that conflicts
            // with itself fails its proposal, so it is isolated and rejected like any other.
            for spend in spends_of(tx) {
                if self.spends.contains(&spend) {
                    return Fit::Deferred;
                }
                spends.push(spend);
            }
            bytes = bytes.saturating_add(tx.transaction.size);
            sigops = sigops.saturating_add(tx.block_sigop_count());
        }
        if !self.fits(bytes, sigops) {
            return Fit::Deferred;
        }
        self.spends.extend(spends);
        self.bytes = bytes;
        self.sigops = sigops;
        for added in added {
            let id = added.transaction.id;
            self.included.insert(id.mined_id());
            if id != tx.transaction.id {
                self.ancestors.push(id);
            }
        }
        Fit::Added
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

/// The transparent payout used by every admission proposal, even on non-mining nodes.
pub(super) fn validation_miner_params(network: &Network) -> MinerParams {
    MinerParams::new(
        network,
        mining::Config {
            miner_address: Some(
                default_miner_address(network.kind(), &MinerAddressType::Transparent)
                    .parse()
                    .expect("the built-in validation address is valid"),
            ),
            ..Default::default()
        },
    )
    .expect("the transparent validation payout matches the network")
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
pub(super) fn required_package<'a>(
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

        // Shielded proofs and signatures that passed mempool verification are skipped through
        // the consensus verifier's bundle cache; scripts and contextual checks run again.
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
