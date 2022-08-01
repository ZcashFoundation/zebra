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
    time::Duration,
};

use thiserror::Error;

use zebra_chain::transaction::{self, Hash, UnminedTx, UnminedTxId, VerifiedUnminedTx};

use self::{eviction_list::EvictionList, verified_set::VerifiedSet};
use super::{config, downloads::TransactionDownloadVerifyError, MempoolError};

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
/// These rejections are only valid for the current tip.
///
/// Each committed block clears these rejections, because new blocks can supply missing inputs.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(dead_code)]
pub enum ExactTipRejectionError {
    #[error("transaction did not pass consensus validation")]
    FailedVerification(#[from] zebra_consensus::error::TransactionError),
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
}

/// Hold mempool verified and rejected mempool transactions.
pub struct Storage {
    /// The set of verified transactions in the mempool.
    verified: VerifiedSet,

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
            tip_rejected_exact: Default::default(),
            tip_rejected_same_effects: Default::default(),
            chain_rejected_same_effects: Default::default(),
        }
    }

    /// Insert a [`VerifiedUnminedTx`] into the mempool, caching any rejections.
    ///
    /// Returns an error if the mempool's verified transactions or rejection caches
    /// prevent this transaction from being inserted.
    /// These errors should not be propagated to peers, because the transactions are valid.
    ///
    /// If inserting this transaction evicts other transactions, they will be tracked
    /// as [`SameEffectsChainRejectionError::RandomlyEvicted`].
    #[allow(clippy::unwrap_in_result)]
    pub fn insert(&mut self, tx: VerifiedUnminedTx) -> Result<UnminedTxId, MempoolError> {
        // # Security
        //
        // This method must call `reject`, rather than modifying the rejection lists directly.
        let tx_id = tx.transaction.id;

        // First, check if we have a cached rejection for this transaction.
        if let Some(error) = self.rejection_error(&tx_id) {
            return Err(error);
        }

        // If `tx` is already in the mempool, we don't change anything.
        //
        // Security: transactions must not get refreshed by new queries,
        // because that allows malicious peers to keep transactions live forever.
        if self.verified.contains(&tx_id) {
            return Err(MempoolError::InMempool);
        }

        // Then, we try to insert into the pool. If this fails the transaction is rejected.
        let mut result = Ok(tx_id);
        if let Err(rejection_error) = self.verified.insert(tx) {
            // We could return here, but we still want to check the mempool size
            self.reject(tx_id, rejection_error.clone().into());
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
            // (we could return here, but we still want to check the mempool size)
            if victim_tx.transaction.id == tx_id {
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
    }

    /// Remove transactions from the mempool via non-malleable [`transaction::Hash`].
    ///
    /// For v5 transactions, transactions are matched by TXID,
    /// using only the non-malleable transaction ID.
    /// This matches any transaction with the same effect on the blockchain state,
    /// even if its signatures and proofs are different.
    ///
    /// Returns the number of transactions which were removed.
    ///
    /// Removes from the 'verified' set, if present.
    /// Maintains the order in which the other unmined transactions have been inserted into the mempool.
    ///
    /// Does not add or remove from the 'rejected' tracking set.
    pub fn remove_same_effects(&mut self, mined_ids: &HashSet<transaction::Hash>) -> usize {
        self.verified
            .remove_all_that(|tx| mined_ids.contains(&tx.transaction.id.mined_id()))
    }

    /// Clears the whole mempool storage.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.verified.clear();
        self.tip_rejected_exact.clear();
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
        self.verified.transactions().map(|tx| tx.id)
    }

    /// Returns the set of [`UnminedTx`]s in the mempool.
    pub fn transactions(&self) -> impl Iterator<Item = &UnminedTx> {
        self.verified.transactions()
    }

    /// Returns the number of transactions in the mempool.
    #[allow(dead_code)]
    pub fn transaction_count(&self) -> usize {
        self.verified.transaction_count()
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
        self.verified
            .transactions()
            .filter(move |tx| tx_ids.contains(&tx.id))
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
            .filter(move |tx| tx_ids.contains(&tx.id.mined_id()))
    }

    /// Returns `true` if a transaction exactly matching an [`UnminedTxId`] is in
    /// the mempool.
    ///
    /// This matches the exact transaction, with identical blockchain effects,
    /// signatures, and proofs.
    pub fn contains_transaction_exact(&self, txid: &UnminedTxId) -> bool {
        self.verified.transactions().any(|tx| &tx.id == txid)
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
    pub fn reject(&mut self, txid: UnminedTxId, reason: RejectionError) {
        match reason {
            RejectionError::ExactTip(e) => {
                self.tip_rejected_exact.insert(txid, e);
            }
            RejectionError::SameEffectsTip(e) => {
                self.tip_rejected_same_effects.insert(txid.mined_id(), e);
            }
            RejectionError::SameEffectsChain(e) => {
                let eviction_memory_time = self.eviction_memory_time;
                self.chain_rejected_same_effects
                    .entry(e)
                    .or_insert_with(|| {
                        EvictionList::new(MAX_EVICTION_MEMORY_ENTRIES, eviction_memory_time)
                    })
                    .insert(txid.mined_id());
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
    pub fn reject_if_needed(&mut self, txid: UnminedTxId, e: TransactionDownloadVerifyError) {
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
            TransactionDownloadVerifyError::Invalid(e) => {
                self.reject(txid, ExactTipRejectionError::FailedVerification(e).into())
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
        let mut txid_set = HashSet::new();
        // we need a separate set, since reject() takes the original unmined ID,
        // then extracts the mined ID out of it
        let mut unmined_id_set = HashSet::new();

        for t in self.transactions() {
            if let Some(expiry_height) = t.transaction.expiry_height() {
                if tip_height >= expiry_height {
                    txid_set.insert(t.id.mined_id());
                    unmined_id_set.insert(t.id);
                }
            }
        }

        // expiry height is effecting data, so we match by non-malleable TXID
        self.remove_same_effects(&txid_set);

        // also reject it
        for id in unmined_id_set.iter() {
            self.reject(*id, SameEffectsChainRejectionError::Expired.into());
        }

        unmined_id_set
    }

    /// Check if transaction should be downloaded and/or verified.
    ///
    /// If it is already in the mempool (or in its rejected list)
    /// then it shouldn't be downloaded/verified.
    pub fn should_download_or_verify(&mut self, txid: UnminedTxId) -> Result<(), MempoolError> {
        // Check if the transaction is already in the mempool.
        if self.contains_transaction_exact(&txid) {
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
        metrics::gauge!(
            "mempool.rejected.transaction.ids",
            self.rejected_transaction_count() as f64,
        );
        // This is just an approximation.
        // TODO: make it more accurate #2869
        let item_size = size_of::<(transaction::Hash, SameEffectsTipRejectionError)>();
        metrics::gauge!(
            "mempool.rejected.transaction.ids.bytes",
            (self.rejected_transaction_count() * item_size) as f64,
        );
    }
}
