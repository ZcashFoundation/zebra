//! Mempool transaction storage.
//!
//! The main struct [`Storage`] holds verified and rejected transactions.
//! [`Storage`] is effectively the data structure of the mempool. Convenient methods to
//! manage it are included.
//!
//! [`Storage`] does not expose a service so it can only be used by other code directly.
//! Only code inside the [`crate::components::mempool`] module has access to it.

use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
    sync::Arc,
    time::Duration,
};

use thiserror::Error;

use zebra_chain::{
    block::Height,
    transaction::{self, Hash, Transaction, UnminedTx, UnminedTxId, VerifiedUnminedTx},
    transparent,
};
use zebra_node_services::mempool::TransactionDependencies;

use self::{eviction_list::EvictionList, verified_set::VerifiedSet};
use super::{
    config, downloads::TransactionDownloadVerifyError, pending_outputs::PendingOutputs,
    MempoolError,
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(test)]
pub mod tests;

mod eviction_list;
mod verified_set;

/// The size limit for mempool transaction rejection lists per [ZIP-401].
///
/// > The size of RecentlyEvicted SHOULD never exceed `eviction_memory_entries`
/// > entries, which is the constant 40000.
///
/// We use the specified value for all lists for consistency.
///
/// [ZIP-401]: https://zips.z.cash/zip-0401#specification
pub(crate) const MAX_EVICTION_MEMORY_ENTRIES: usize = 40_000;

/// Transactions rejected based on transaction authorizing data (scripts, proofs, signatures),
/// or lock times. These rejections are only valid for the current tip.
///
/// Each committed block clears these rejections, because new blocks can supply missing inputs.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(dead_code)]
pub enum ExactTipRejectionError {
    #[error("transaction did not pass consensus validation: {0}")]
    FailedVerification(#[from] zebra_consensus::error::TransactionError),
    #[error("transaction did not pass standard validation: {0}")]
    FailedStandard(#[from] NonStandardTransactionError),
}

/// Transactions rejected based only on their effects (spends, outputs, transaction header).
/// These rejections are only valid for the current tip.
///
/// Each committed block clears these rejections, because new blocks can evict other transactions.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(dead_code)]
pub enum SameEffectsTipRejectionError {
    #[error(
        "transaction rejected because another transaction in the mempool has already spent some of \
        its inputs"
    )]
    SpendConflict,

    #[error(
        "transaction rejected because it spends missing outputs from \
        another transaction in the mempool"
    )]
    MissingOutput,
}

/// Transactions rejected based only on their effects (spends, outputs, transaction header).
/// These rejections are valid while the current chain continues to grow.
///
/// Rollbacks and network upgrades clear these rejections, because they can lower the tip height,
/// or change the consensus rules.
#[derive(Error, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(dead_code)]
pub enum SameEffectsChainRejectionError {
    #[error("best chain tip has reached transaction expiry height")]
    Expired,

    #[error("transaction inputs were spent, or nullifiers were revealed, in the best chain")]
    DuplicateSpend,

    #[error("transaction was committed to the best chain")]
    Mined,

    /// Otherwise valid transaction removed from mempool due to [ZIP-401] random
    /// eviction.
    ///
    /// Consensus rule:
    /// > The txid (rather than the wtxid ...) is used even for version 5 transactions
    ///
    /// [ZIP-401]: https://zips.z.cash/zip-0401#specification
    #[error("transaction evicted from the mempool due to ZIP-401 denial of service limits")]
    RandomlyEvicted,
}

/// Storage error that combines all other specific error types.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(dead_code)]
pub enum RejectionError {
    #[error(transparent)]
    ExactTip(#[from] ExactTipRejectionError),
    #[error(transparent)]
    SameEffectsTip(#[from] SameEffectsTipRejectionError),
    #[error(transparent)]
    SameEffectsChain(#[from] SameEffectsChainRejectionError),
    #[error(transparent)]
    NonStandardTransaction(#[from] NonStandardTransactionError),
}

/// Non-standard transaction error.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum NonStandardTransactionError {
    #[error("transaction is dust")]
    IsDust,
}

/// Represents a set of transactions that have been removed from the mempool, either because
/// they were mined, or because they were invalidated by another transaction that was mined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovedTransactionIds {
    /// A list of ids for transactions that were removed mined onto the best chain.
    pub mined: HashSet<UnminedTxId>,
    /// A list of ids for transactions that were invalidated by other transactions
    /// that were mined onto the best chain.
    pub invalidated: HashSet<UnminedTxId>,
}

impl RemovedTransactionIds {
    /// Returns the total number of transactions that were removed from the mempool.
    pub fn total_len(&self) -> usize {
        self.mined.len() + self.invalidated.len()
    }
}

/// Hold mempool verified and rejected mempool transactions.
pub struct Storage {
    /// The set of verified transactions in the mempool.
    verified: VerifiedSet,

    /// The set of outpoints with pending requests for their associated transparent::Output.
    pub(super) pending_outputs: PendingOutputs,

    /// The set of transactions rejected due to bad authorizations, or for other
    /// reasons, and their rejection reasons. These rejections only apply to the
    /// current tip.
    ///
    /// Only transactions with the exact [`UnminedTxId`] are invalid.
    tip_rejected_exact: HashMap<UnminedTxId, ExactTipRejectionError>,

    /// A set of transactions rejected for their effects, and their rejection
    /// reasons. These rejections only apply to the current tip.
    ///
    /// Any transaction with the same [`transaction::Hash`] is invalid.
    tip_rejected_same_effects: HashMap<transaction::Hash, SameEffectsTipRejectionError>,

    /// Sets of transactions rejected for their effects, keyed by rejection reason.
    /// These rejections apply until a rollback or network upgrade.
    ///
    /// Any transaction with the same [`transaction::Hash`] is invalid.
    ///
    /// An [`EvictionList`] is used for both randomly evicted and expired
    /// transactions, even if it is only needed for the evicted ones. This was
    /// done just to simplify the existing code; there is no harm in having a
    /// timeout for expired transactions too since re-checking expired
    /// transactions is cheap.
    // If this code is ever refactored and the lists are split in different
    // fields, then we can use an `EvictionList` just for the evicted list.
    chain_rejected_same_effects: HashMap<SameEffectsChainRejectionError, EvictionList>,

    /// The mempool transaction eviction age limit.
    /// Same as [`config::Config::eviction_memory_time`].
    eviction_memory_time: Duration,

    /// Max total cost of the verified mempool set, beyond which transactions
    /// are evicted to make room.
    tx_cost_limit: u64,
}

impl Drop for Storage {
    fn drop(&mut self) {
        self.clear();
    }
}

impl Storage {
    #[allow(clippy::field_reassign_with_default)]
    pub(crate) fn new(config: &config::Config) -> Self {
        Self {
            tx_cost_limit: config.tx_cost_limit,
            eviction_memory_time: config.eviction_memory_time,
            verified: Default::default(),
            pending_outputs: Default::default(),
            tip_rejected_exact: Default::default(),
            tip_rejected_same_effects: Default::default(),
            chain_rejected_same_effects: Default::default(),
        }
    }

    /// Check whether a transaction is standard.
    ///
    /// Zcashd defines non-consensus standard transaction checks in
    /// https://github.com/zcash/zcash/blob/v6.10.0/src/policy/policy.cpp#L58-L135
    ///
    /// This checks are applied before inserting a transaction in `AcceptToMemoryPool`:
    /// https://github.com/zcash/zcash/blob/v6.10.0/src/main.cpp#L1819
    ///
    /// Currently, we only implement the dust output check.
    fn is_standard_tx(&mut self, tx: &VerifiedUnminedTx) -> Result<(), MempoolError> {
        // TODO: implement other standard transaction checks from zcashd.

        if tx.height.unwrap_or_default() > Height::MIN {
            // Check for dust outputs.
            for output in tx.transaction.transaction.outputs() {
                if output.is_dust() {
                    let rejection_error = NonStandardTransactionError::IsDust;
                    self.reject(tx.transaction.id, rejection_error.clone().into());

                    return Err(MempoolError::NonStandardTransaction(rejection_error));
                }
            }
        }

        Ok(())
    }

    /// Insert a [`VerifiedUnminedTx`] into the mempool, caching any rejections.
    ///
    /// Accepts the [`VerifiedUnminedTx`] being inserted and `spent_mempool_outpoints`,
    /// a list of transparent inputs of the provided [`VerifiedUnminedTx`] that were found
    /// as newly created transparent outputs in the mempool during transaction verification.
    ///
    /// Returns an error if the mempool's verified transactions or rejection caches
    /// prevent this transaction from being inserted.
    /// These errors should not be propagated to peers, because the transactions are valid.
    ///
    /// If inserting this transaction evicts other transactions, they will be tracked
    /// as [`SameEffectsChainRejectionError::RandomlyEvicted`].
    #[allow(clippy::unwrap_in_result)]
    pub fn insert(
        &mut self,
        tx: VerifiedUnminedTx,
        spent_mempool_outpoints: Vec<transparent::OutPoint>,
        height: Option<Height>,
    ) -> Result<UnminedTxId, MempoolError> {
        // Check that the transaction is standard.
        self.is_standard_tx(&tx)?;

        // # Security
        //
        // This method must call `reject`, rather than modifying the rejection lists directly.
        let unmined_tx_id = tx.transaction.id;
        let tx_id = unmined_tx_id.mined_id();

        // First, check if we have a cached rejection for this transaction.
        if let Some(error) = self.rejection_error(&unmined_tx_id) {
            tracing::trace!(
                ?tx_id,
                ?error,
                stored_transaction_count = ?self.verified.transaction_count(),
                "returning cached error for transaction",
            );

            return Err(error);
        }

        // If `tx` is already in the mempool, we don't change anything.
        //
        // Security: transactions must not get refreshed by new queries,
        // because that allows malicious peers to keep transactions live forever.
        if self.verified.contains(&tx_id) {
            tracing::trace!(
                ?tx_id,
                stored_transaction_count = ?self.verified.transaction_count(),
                "returning InMempool error for transaction that is already in the mempool",
            );

            return Err(MempoolError::InMempool);
        }

        // Then, we try to insert into the pool. If this fails the transaction is rejected.
        let mut result = Ok(unmined_tx_id);
        if let Err(rejection_error) = self.verified.insert(
            tx,
            spent_mempool_outpoints,
            &mut self.pending_outputs,
            height,
        ) {
            tracing::debug!(
                ?tx_id,
                ?rejection_error,
                stored_transaction_count = ?self.verified.transaction_count(),
                "insertion error for transaction",
            );

            // We could return here, but we still want to check the mempool size
            self.reject(unmined_tx_id, rejection_error.clone().into());
            result = Err(rejection_error.into());
        }

        // Once inserted, we evict transactions over the pool size limit per [ZIP-401];
        //
        // > On receiving a transaction: (...)
        // > Calculate its cost. If the total cost of transactions in the mempool including this
        // > one would `exceed mempooltxcostlimit`, then the node MUST repeatedly call
        // > EvictTransaction (with the new transaction included as a candidate to evict) until the
        // > total cost does not exceed `mempooltxcostlimit`.
        //
        // 'EvictTransaction' is equivalent to [`VerifiedSet::evict_one()`] in
        // our implementation.
        //
        // [ZIP-401]: https://zips.z.cash/zip-0401
        while self.verified.total_cost() > self.tx_cost_limit {
            // > EvictTransaction MUST do the following:
            // > Select a random transaction to evict, with probability in direct proportion to
            // > eviction weight. (...) Remove it from the mempool.
            let victim_tx = self
                .verified
                .evict_one()
                .expect("mempool is empty, but was expected to be full");

            // > Add the txid and the current time to RecentlyEvicted, dropping the oldest entry in
            // > RecentlyEvicted if necessary to keep it to at most `eviction_memory_entries entries`.
            self.reject(
                victim_tx.transaction.id,
                SameEffectsChainRejectionError::RandomlyEvicted.into(),
            );

            // If this transaction gets evicted, set its result to the same error
            if victim_tx.transaction.id == unmined_tx_id {
                result = Err(SameEffectsChainRejectionError::RandomlyEvicted.into());
            }
        }

        result
    }

    /// Remove transactions from the mempool via exact [`UnminedTxId`].
    ///
    /// For v5 transactions, transactions are matched by WTXID, using both the:
    /// - non-malleable transaction ID, and
    /// - authorizing data hash.
    ///
    /// This matches the exact transaction, with identical blockchain effects, signatures, and proofs.
    ///
    /// Returns the number of transactions which were removed.
    ///
    /// Removes from the 'verified' set, if present.
    /// Maintains the order in which the other unmined transactions have been inserted into the mempool.
    ///
    /// Does not add or remove from the 'rejected' tracking set.
    #[allow(dead_code)]
    pub fn remove_exact(&mut self, exact_wtxids: &HashSet<UnminedTxId>) -> usize {
        self.verified
            .remove_all_that(|tx| exact_wtxids.contains(&tx.transaction.id))
            .len()
    }

    /// Clears a list of mined transaction ids from the verified set's tracked transaction dependencies.
    pub fn clear_mined_dependencies(&mut self, mined_ids: &HashSet<transaction::Hash>) {
        self.verified.clear_mined_dependencies(mined_ids);
    }

    /// Reject and remove transactions from the mempool via non-malleable [`transaction::Hash`].
    /// - For v5 transactions, transactions are matched by TXID,
    ///   using only the non-malleable transaction ID.
    ///   This matches any transaction with the same effect on the blockchain state,
    ///   even if its signatures and proofs are different.
    /// - Returns the number of transactions which were removed.
    /// - Removes from the 'verified' set, if present.
    ///   Maintains the order in which the other unmined transactions have been inserted into the mempool.
    /// - Prunes `pending_outputs` of any closed channels.
    ///
    /// Reject and remove transactions from the mempool that contain any spent outpoints or revealed
    /// nullifiers from the passed in `transactions`.
    ///
    /// Returns the number of transactions that were removed.
    pub fn reject_and_remove_same_effects(
        &mut self,
        mined_ids: &HashSet<transaction::Hash>,
        transactions: Vec<Arc<Transaction>>,
    ) -> RemovedTransactionIds {
        let removed_mined = self
            .verified
            .remove_all_that(|tx| mined_ids.contains(&tx.transaction.id.mined_id()));

        let spent_outpoints: HashSet<_> = transactions
            .iter()
            .flat_map(|tx| tx.spent_outpoints())
            .collect();
        let sprout_nullifiers: HashSet<_> = transactions
            .iter()
            .flat_map(|transaction| transaction.sprout_nullifiers())
            .collect();
        let sapling_nullifiers: HashSet<_> = transactions
            .iter()
            .flat_map(|transaction| transaction.sapling_nullifiers())
            .collect();
        let orchard_nullifiers: HashSet<_> = transactions
            .iter()
            .flat_map(|transaction| transaction.orchard_nullifiers())
            .collect();

        let duplicate_spend_ids: HashSet<_> = self
            .verified
            .transactions()
            .values()
            .map(|tx| (tx.transaction.id, &tx.transaction.transaction))
            .filter_map(|(tx_id, tx)| {
                (tx.spent_outpoints()
                    .any(|outpoint| spent_outpoints.contains(&outpoint))
                    || tx
                        .sprout_nullifiers()
                        .any(|nullifier| sprout_nullifiers.contains(nullifier))
                    || tx
                        .sapling_nullifiers()
                        .any(|nullifier| sapling_nullifiers.contains(nullifier))
                    || tx
                        .orchard_nullifiers()
                        .any(|nullifier| orchard_nullifiers.contains(nullifier)))
                .then_some(tx_id)
            })
            .collect();

        let removed_duplicate_spend = self
            .verified
            .remove_all_that(|tx| duplicate_spend_ids.contains(&tx.transaction.id));

        for &mined_id in mined_ids {
            self.reject(
                // the reject and rejection_error fns that store and check `SameEffectsChainRejectionError`s
                // only use the mined id, so using `Legacy` ids will apply to v5 transactions as well.
                UnminedTxId::Legacy(mined_id),
                SameEffectsChainRejectionError::Mined.into(),
            );
        }

        for duplicate_spend_id in duplicate_spend_ids {
            self.reject(
                duplicate_spend_id,
                SameEffectsChainRejectionError::DuplicateSpend.into(),
            );
        }

        self.pending_outputs.prune();

        RemovedTransactionIds {
            mined: removed_mined,
            invalidated: removed_duplicate_spend,
        }
    }

    /// Clears the whole mempool storage.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.verified.clear();
        self.tip_rejected_exact.clear();
        self.pending_outputs.clear();
        self.tip_rejected_same_effects.clear();
        self.chain_rejected_same_effects.clear();
        self.update_rejected_metrics();
    }

    /// Clears rejections that only apply to the current tip.
    pub fn clear_tip_rejections(&mut self) {
        self.tip_rejected_exact.clear();
        self.tip_rejected_same_effects.clear();
        self.update_rejected_metrics();
    }

    /// Clears rejections that only apply to the current tip.
    ///
    /// # Security
    ///
    /// This method must be called at the end of every method that adds rejections.
    /// Otherwise, peers could make our reject lists use a lot of RAM.
    fn limit_rejection_list_memory(&mut self) {
        // These lists are an optimisation - it's ok to totally clear them as needed.
        if self.tip_rejected_exact.len() > MAX_EVICTION_MEMORY_ENTRIES {
            self.tip_rejected_exact.clear();
        }
        if self.tip_rejected_same_effects.len() > MAX_EVICTION_MEMORY_ENTRIES {
            self.tip_rejected_same_effects.clear();
        }
        // `chain_rejected_same_effects` limits its size by itself
        self.update_rejected_metrics();
    }

    /// Returns the set of [`UnminedTxId`]s in the mempool.
    pub fn tx_ids(&self) -> impl Iterator<Item = UnminedTxId> + '_ {
        self.transactions().values().map(|tx| tx.transaction.id)
    }

    /// Returns a reference to the [`HashMap`] of [`VerifiedUnminedTx`]s in the verified set.
    ///
    /// Each [`VerifiedUnminedTx`] contains an [`UnminedTx`],
    /// and adds extra fields from the transaction verifier result.
    pub fn transactions(&self) -> &HashMap<transaction::Hash, VerifiedUnminedTx> {
        self.verified.transactions()
    }

    /// Returns a reference to the [`TransactionDependencies`] in the verified set.
    pub fn transaction_dependencies(&self) -> &TransactionDependencies {
        self.verified.transaction_dependencies()
    }

    /// Returns a [`transparent::Output`] created by a mempool transaction for the provided
    /// [`transparent::OutPoint`] if one exists, or None otherwise.
    pub fn created_output(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Output> {
        self.verified.created_output(outpoint)
    }

    /// Returns the number of transactions in the mempool.
    #[allow(dead_code)]
    pub fn transaction_count(&self) -> usize {
        self.verified.transaction_count()
    }

    /// Returns the cost of the transactions in the mempool, according to ZIP-401.
    #[allow(dead_code)]
    pub fn total_cost(&self) -> u64 {
        self.verified.total_cost()
    }

    /// Returns the total serialized size of the verified transactions in the set.
    ///
    /// See [`VerifiedSet::total_serialized_size()`] for details.
    pub fn total_serialized_size(&self) -> usize {
        self.verified.total_serialized_size()
    }

    /// Returns the set of [`UnminedTx`]es with exactly matching `tx_ids` in the
    /// mempool.
    ///
    /// This matches the exact transaction, with identical blockchain effects,
    /// signatures, and proofs.
    pub fn transactions_exact(
        &self,
        tx_ids: HashSet<UnminedTxId>,
    ) -> impl Iterator<Item = &UnminedTx> {
        tx_ids.into_iter().filter_map(|tx_id| {
            self.transactions()
                .get(&tx_id.mined_id())
                .map(|tx| &tx.transaction)
        })
    }

    /// Returns the set of [`UnminedTx`]es with matching [`transaction::Hash`]es
    /// in the mempool.
    ///
    /// This matches transactions with the same effects, regardless of
    /// [`transaction::AuthDigest`].
    pub fn transactions_same_effects(
        &self,
        tx_ids: HashSet<Hash>,
    ) -> impl Iterator<Item = &UnminedTx> {
        self.verified
            .transactions()
            .iter()
            .filter(move |(tx_id, _)| tx_ids.contains(tx_id))
            .map(|(_, tx)| &tx.transaction)
    }

    /// Returns a transaction and the transaction ids of its dependencies, if it is in the verified set.
    pub fn transaction_with_deps(
        &self,
        tx_id: transaction::Hash,
    ) -> Option<(VerifiedUnminedTx, HashSet<transaction::Hash>)> {
        let tx = self.verified.transactions().get(&tx_id).cloned()?;
        let deps = self
            .verified
            .transaction_dependencies()
            .dependencies()
            .get(&tx_id)
            .cloned()
            .unwrap_or_default();

        Some((tx, deps))
    }

    /// Returns `true` if a transaction exactly matching an [`UnminedTxId`] is in
    /// the mempool.
    ///
    /// This matches the exact transaction, with identical blockchain effects,
    /// signatures, and proofs.
    pub fn contains_transaction_exact(&self, tx_id: &transaction::Hash) -> bool {
        self.verified.contains(tx_id)
    }

    /// Returns the number of rejected [`UnminedTxId`]s or [`transaction::Hash`]es.
    ///
    /// Transactions on multiple rejected lists are counted multiple times.
    #[allow(dead_code)]
    pub fn rejected_transaction_count(&mut self) -> usize {
        self.tip_rejected_exact.len()
            + self.tip_rejected_same_effects.len()
            + self
                .chain_rejected_same_effects
                .iter_mut()
                .map(|(_, map)| map.len())
                .sum::<usize>()
    }

    /// Add a transaction to the rejected list for the given reason.
    pub fn reject(&mut self, tx_id: UnminedTxId, reason: RejectionError) {
        match reason {
            RejectionError::ExactTip(e) => {
                self.tip_rejected_exact.insert(tx_id, e);
            }
            RejectionError::SameEffectsTip(e) => {
                self.tip_rejected_same_effects.insert(tx_id.mined_id(), e);
            }
            RejectionError::SameEffectsChain(e) => {
                let eviction_memory_time = self.eviction_memory_time;
                self.chain_rejected_same_effects
                    .entry(e)
                    .or_insert_with(|| {
                        EvictionList::new(MAX_EVICTION_MEMORY_ENTRIES, eviction_memory_time)
                    })
                    .insert(tx_id.mined_id());
            }
            RejectionError::NonStandardTransaction(e) => {
                // Non-standard transactions are rejected based on their exact
                // transaction data.
                self.tip_rejected_exact
                    .insert(tx_id, ExactTipRejectionError::from(e));
            }
        }
        self.limit_rejection_list_memory();
    }

    /// Returns the rejection error if a transaction matching an [`UnminedTxId`]
    /// is in any mempool rejected list.
    ///
    /// This matches transactions based on each rejection list's matching rule.
    ///
    /// Returns an arbitrary error if the transaction is in multiple lists.
    pub fn rejection_error(&self, txid: &UnminedTxId) -> Option<MempoolError> {
        if let Some(error) = self.tip_rejected_exact.get(txid) {
            return Some(error.clone().into());
        }

        if let Some(error) = self.tip_rejected_same_effects.get(&txid.mined_id()) {
            return Some(error.clone().into());
        }

        for (error, set) in self.chain_rejected_same_effects.iter() {
            if set.contains_key(&txid.mined_id()) {
                return Some(error.clone().into());
            }
        }

        None
    }

    /// Returns the set of [`UnminedTxId`]s matching `tx_ids` in the rejected list.
    ///
    /// This matches transactions based on each rejection list's matching rule.
    pub fn rejected_transactions(
        &self,
        tx_ids: HashSet<UnminedTxId>,
    ) -> impl Iterator<Item = UnminedTxId> + '_ {
        tx_ids
            .into_iter()
            .filter(move |txid| self.contains_rejected(txid))
    }

    /// Returns `true` if a transaction matching the supplied [`UnminedTxId`] is in
    /// the mempool rejected list.
    ///
    /// This matches transactions based on each rejection list's matching rule.
    pub fn contains_rejected(&self, txid: &UnminedTxId) -> bool {
        self.rejection_error(txid).is_some()
    }

    /// Add a transaction that failed download and verification to the rejected list
    /// if needed, depending on the reason for the failure.
    pub fn reject_if_needed(&mut self, tx_id: UnminedTxId, e: TransactionDownloadVerifyError) {
        match e {
            // Rejecting a transaction already in state would speed up further
            // download attempts without checking the state. However it would
            // make the reject list grow forever.
            //
            // TODO: revisit after reviewing the rejected list cleanup criteria?
            // TODO: if we decide to reject it, then we need to pass the block hash
            // to State::Confirmed. This would require the zs::Response::Transaction
            // to include the hash, which would need to be implemented.
            TransactionDownloadVerifyError::InState |
            // An unknown error in the state service, better do nothing
            TransactionDownloadVerifyError::StateError(_) |
            // If download failed, do nothing; the crawler will end up trying to
            // download it again.
            TransactionDownloadVerifyError::DownloadFailed(_) |
            // If it was cancelled then a block was mined, or there was a network
            // upgrade, etc. No reason to reject it.
            TransactionDownloadVerifyError::Cancelled => {}

            // Consensus verification failed. Reject transaction to avoid
            // having to download and verify it again just for it to fail again.
            TransactionDownloadVerifyError::Invalid { error, .. }  => {
                self.reject(tx_id, ExactTipRejectionError::FailedVerification(error).into())
            }
        }
    }

    /// Remove transactions from the mempool if they have not been mined after a
    /// specified height, per [ZIP-203].
    ///
    /// > Transactions will have a new field, nExpiryHeight, which will set the
    /// > block height after which transactions will be removed from the mempool
    /// > if they have not been mined.
    ///
    ///
    /// [ZIP-203]: https://zips.z.cash/zip-0203#specification
    pub fn remove_expired_transactions(
        &mut self,
        tip_height: zebra_chain::block::Height,
    ) -> HashSet<UnminedTxId> {
        let mut tx_ids = HashSet::new();
        let mut unmined_tx_ids = HashSet::new();

        for (&tx_id, tx) in self.transactions() {
            if let Some(expiry_height) = tx.transaction.transaction.expiry_height() {
                if tip_height >= expiry_height {
                    tx_ids.insert(tx_id);
                    unmined_tx_ids.insert(tx.transaction.id);
                }
            }
        }

        // expiry height is effecting data, so we match by non-malleable TXID
        self.verified
            .remove_all_that(|tx| tx_ids.contains(&tx.transaction.id.mined_id()));

        // also reject it
        for id in tx_ids {
            self.reject(
                // It's okay to omit the auth digest here as we know that `reject()` will always
                // use mined ids for `SameEffectsChainRejectionError`s.
                UnminedTxId::Legacy(id),
                SameEffectsChainRejectionError::Expired.into(),
            );
        }

        unmined_tx_ids
    }

    /// Check if transaction should be downloaded and/or verified.
    ///
    /// If it is already in the mempool (or in its rejected list)
    /// then it shouldn't be downloaded/verified.
    pub fn should_download_or_verify(&mut self, txid: UnminedTxId) -> Result<(), MempoolError> {
        // Check if the transaction is already in the mempool.
        if self.contains_transaction_exact(&txid.mined_id()) {
            return Err(MempoolError::InMempool);
        }
        if let Some(error) = self.rejection_error(&txid) {
            return Err(error);
        }
        Ok(())
    }

    /// Update metrics related to the rejected lists.
    ///
    /// Must be called every time the rejected lists change.
    fn update_rejected_metrics(&mut self) {
        metrics::gauge!("mempool.rejected.transaction.ids",)
            .set(self.rejected_transaction_count() as f64);
        // This is just an approximation.
        // TODO: make it more accurate #2869
        let item_size = size_of::<(transaction::Hash, SameEffectsTipRejectionError)>();
        metrics::gauge!("mempool.rejected.transaction.ids.bytes",)
            .set((self.rejected_transaction_count() * item_size) as f64);
    }
}
