use std::collections::{HashMap, HashSet};

use thiserror::Error;

use zebra_chain::transaction::{self, UnminedTx, UnminedTxId};

use self::verified_set::VerifiedSet;
use super::{downloads::TransactionDownloadVerifyError, MempoolError};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(test)]
pub mod tests;

mod verified_set;

/// The maximum number of verified transactions to store in the mempool.
const MEMPOOL_SIZE: usize = 4;

/// The size limit for mempool transaction rejection lists.
///
/// > The size of RecentlyEvicted SHOULD never exceed `eviction_memory_entries` entries,
/// > which is the constant 40000.
///
/// https://zips.z.cash/zip-0401#specification
///
/// We use the specified value for all lists for consistency.
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

    /// Otherwise valid transaction removed from mempool due to ZIP-401 random eviction.
    ///
    /// Consensus rule:
    /// > The txid (rather than the wtxid ...) is used even for version 5 transactions
    ///
    /// https://zips.z.cash/zip-0401#specification
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

#[derive(Default)]
pub struct Storage {
    /// The set of verified transactions in the mempool. This is a
    /// cache of size [`MEMPOOL_SIZE`].
    verified: VerifiedSet,

    /// The set of transactions rejected due to bad authorizations, or for other reasons,
    /// and their rejection reasons. These rejections only apply to the current tip.
    ///
    /// Only transactions with the exact `UnminedTxId` are invalid.
    tip_rejected_exact: HashMap<UnminedTxId, ExactTipRejectionError>,

    /// A set of transactions rejected for their effects, and their rejection reasons.
    /// These rejections only apply to the current tip.
    ///
    /// Any transaction with the same `transaction::Hash` is invalid.
    tip_rejected_same_effects: HashMap<transaction::Hash, SameEffectsTipRejectionError>,

    /// Sets of transactions rejected for their effects, keyed by rejection reason.
    /// These rejections apply until a rollback or network upgrade.
    ///
    /// Any transaction with the same `transaction::Hash` is invalid.
    chain_rejected_same_effects:
        HashMap<SameEffectsChainRejectionError, HashSet<transaction::Hash>>,
}

impl Storage {
    /// Insert a [`UnminedTx`] into the mempool, caching any rejections.
    ///
    /// Returns an error if the mempool's verified transactions or rejection caches
    /// prevent this transaction from being inserted.
    /// These errors should not be propagated to peers, because the transactions are valid.
    ///
    /// If inserting this transaction evicts other transactions, they will be tracked
    /// as [`StorageRejectionError::RandomlyEvicted`].
    pub fn insert(&mut self, tx: UnminedTx) -> Result<UnminedTxId, MempoolError> {
        // # Security
        //
        // This method must call `reject`, rather than modifying the rejection lists directly.
        let tx_id = tx.id;

        // First, check if we have a cached rejection for this transaction.
        if let Some(error) = self.rejection_error(&tx.id) {
            return Err(error);
        }

        // If `tx` is already in the mempool, we don't change anything.
        //
        // Security: transactions must not get refreshed by new queries,
        // because that allows malicious peers to keep transactions live forever.
        if self.verified.contains(&tx.id) {
            return Err(MempoolError::InMempool);
        }

        // Then, we try to insert into the pool. If this fails the transaction is rejected.
        let mut result = Ok(tx_id);
        if let Err(rejection_error) = self.verified.insert(tx) {
            // We could return here, but we still want to check the mempool size
            self.reject(tx_id, rejection_error.clone().into());
            result = Err(rejection_error.into());
        }

        // Once inserted, we evict transactions over the pool size limit.
        while self.verified.transaction_count() > MEMPOOL_SIZE {
            let evicted_tx = self
                .verified
                .evict_one()
                .expect("mempool is empty, but was expected to be full");

            self.reject(
                evicted_tx.id,
                SameEffectsChainRejectionError::RandomlyEvicted.into(),
            );

            // If this transaction gets evicted, set its result to the same error
            // (we could return here, but we still want to check the mempool size)
            if evicted_tx.id == tx_id {
                result = Err(SameEffectsChainRejectionError::RandomlyEvicted.into());
            }
        }

        assert!(self.verified.transaction_count() <= MEMPOOL_SIZE);

        result
    }

    /// Remove [`UnminedTx`]es from the mempool via exact [`UnminedTxId`].
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
            .remove_all_that(|tx| exact_wtxids.contains(&tx.id))
    }

    /// Remove [`UnminedTx`]es from the mempool via non-malleable [`transaction::Hash`].
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
            .remove_all_that(|tx| mined_ids.contains(&tx.id.mined_id()))
    }

    /// Clears the whole mempool storage.
    pub fn clear(&mut self) {
        self.verified.clear();
        self.tip_rejected_exact.clear();
        self.tip_rejected_same_effects.clear();
        self.chain_rejected_same_effects.clear();
    }

    /// Clears rejections that only apply to the current tip.
    pub fn clear_tip_rejections(&mut self) {
        self.tip_rejected_exact.clear();
        self.tip_rejected_same_effects.clear();
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
        for (_, map) in self.chain_rejected_same_effects.iter_mut() {
            if map.len() > MAX_EVICTION_MEMORY_ENTRIES {
                map.clear();
            }
        }
    }

    /// Returns the set of [`UnminedTxId`]s in the mempool.
    pub fn tx_ids(&self) -> impl Iterator<Item = UnminedTxId> + '_ {
        self.verified.transactions().map(|tx| tx.id)
    }

    /// Returns the set of [`Transaction`][transaction::Transaction]s in the mempool.
    pub fn transactions(&self) -> impl Iterator<Item = &UnminedTx> {
        self.verified.transactions()
    }

    /// Returns the number of transactions in the mempool.
    #[allow(dead_code)]
    pub fn transaction_count(&self) -> usize {
        self.verified.transaction_count()
    }

    /// Returns the set of [`Transaction`][transaction::Transaction]s with exactly matching
    /// `tx_ids` in the mempool.
    ///
    /// This matches the exact transaction, with identical blockchain effects, signatures, and proofs.
    pub fn transactions_exact(
        &self,
        tx_ids: HashSet<UnminedTxId>,
    ) -> impl Iterator<Item = &UnminedTx> {
        self.verified
            .transactions()
            .filter(move |tx| tx_ids.contains(&tx.id))
    }

    /// Returns `true` if a [`UnminedTx`] exactly matching an [`UnminedTxId`] is in
    /// the mempool.
    ///
    /// This matches the exact transaction, with identical blockchain effects, signatures, and proofs.
    pub fn contains_transaction_exact(&self, txid: &UnminedTxId) -> bool {
        self.verified.transactions().any(|tx| &tx.id == txid)
    }

    /// Returns the number of rejected [`UnminedTxId`]s or [`transaction::Hash`]es.
    ///
    /// Transactions on multiple rejected lists are counted multiple times.
    #[allow(dead_code)]
    pub fn rejected_transaction_count(&self) -> usize {
        self.tip_rejected_exact.len()
            + self.tip_rejected_same_effects.len()
            + self
                .chain_rejected_same_effects
                .iter()
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
                self.chain_rejected_same_effects
                    .entry(e)
                    .or_default()
                    .insert(txid.mined_id());
            }
        }
        self.limit_rejection_list_memory();
    }

    /// Returns the rejection error if a [`UnminedTx`] matching an [`UnminedTxId`]
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
            if set.contains(&txid.mined_id()) {
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

    /// Returns `true` if a [`UnminedTx`] matching the supplied [`UnminedTxId`] is in
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
            // Sync has just started. Mempool shouldn't even be enabled, so will not
            // happen in practice.
            TransactionDownloadVerifyError::NoTip |
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
    /// specified height.
    ///
    /// https://zips.z.cash/zip-0203#specification
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
}
