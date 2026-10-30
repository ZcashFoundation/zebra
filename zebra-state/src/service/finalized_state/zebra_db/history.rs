//! Availability of historical block bodies.

use zebra_chain::{
    block::{self, Height},
    parameters::NetworkUpgrade,
};

use crate::{BoxError, HistoryCapabilities, StateInitError};

use super::ZebraDb;
use crate::service::finalized_state::{
    DiskWriteBatch, TransactionLocation, WriteDisk, MAX_ON_DISK_HEIGHT,
};

/// Limits each deletion batch to this many complete block heights.
pub(crate) const MAX_PRUNING_BATCH_HEIGHTS: u32 = 100;

/// Remembers the restart witness while the sole pruning worker follows finalized blocks.
#[derive(Default)]
pub(crate) struct PruningWitness {
    tip: Option<(Height, block::Hash)>,
    height: Option<Height>,
}

impl PruningWitness {
    fn update(&mut self, db: &ZebraDb) -> Result<Option<Height>, BoxError> {
        let Some(tip) = db.tip() else {
            *self = Self::default();
            return Ok(None);
        };
        if self.tip == Some(tip) {
            return Ok(self.height);
        }
        if self
            .tip
            .is_some_and(|(height, hash)| db.hash(height) != Some(hash))
        {
            *self = Self::default();
        }
        let network = db.network();
        let Some(activation) = NetworkUpgrade::Nu5.activation_height(&network) else {
            return Ok(None);
        };
        let ancestors = crate::service::block_iter::known_chain_ancestor_iter::<block::Block>(
            None,
            db,
            tip.1.into(),
        );
        // A branchless suffix is valid. Bound the initial search, then inspect only new blocks;
        // no witness means keeping the bodies, not rejecting the verified chain.
        for block in ancestors.take(crate::constants::MAX_LEGACY_CHAIN_BLOCKS) {
            let block = block?;
            let height = block
                .coinbase_height()
                .expect("stored blocks have coinbase heights");
            if height < activation || self.tip.is_some_and(|(_, hash)| block.hash() == hash) {
                break;
            }
            block
                .check_transaction_network_upgrade_consistency(&network)
                .map_err(|error| {
                    format!("inconsistent network upgrade found in transaction: {error:?}")
                })?;
            if block
                .transactions
                .iter()
                .any(|tx| tx.network_upgrade().is_some())
            {
                self.height = Some(height);
                break;
            }
        }
        self.tip = Some(tip);
        Ok(self.height)
    }
}

impl ZebraDb {
    /// Returns body availability from the first stored non-genesis transaction.
    ///
    /// Contiguous prefix deletion advances this boundary atomically without separate metadata.
    /// A missing prefix does not reveal whether pruning or corruption caused the loss.
    /// Availability can decrease after this read; callers must still reject incomplete bodies.
    pub fn history_capabilities(&self) -> Result<HistoryCapabilities, StateInitError> {
        self.db.history_capabilities()
    }
}

impl DiskWriteBatch {
    /// Stages contiguous body deletion, advancing the derived floor in the same batch.
    ///
    /// The caller must be the sole pruning owner and commit this batch before preparing another.
    /// Current block writes may continue concurrently. Genesis and the finalized tip are retained.
    /// The startup checks must be complete. Each batch deletes at most 100 heights and retains
    /// the restart witness. Returns the staged floor, or `None` while waiting for a newer witness.
    /// Errors and `None` leave the batch unchanged.
    pub(crate) fn prepare_history_pruning(
        &mut self,
        db: &ZebraDb,
        lowest_retained_height: Height,
        witness: &mut PruningWitness,
    ) -> Result<Option<Height>, BoxError> {
        if db.major_version() < 30 {
            return Err("body deletion requires database format v30 or newer".into());
        }
        if !db.ready_for_pruning() {
            return Err(
                "history-dependent migrations and checks must finish before pruning".into(),
            );
        }
        let previous = db
            .history_capabilities()?
            .lowest_retained_height
            .max(Height(1));
        if lowest_retained_height <= previous || lowest_retained_height > MAX_ON_DISK_HEIGHT {
            return Err("retained-body floor must advance past a non-genesis body".into());
        }
        if db
            .finalized_tip_height()
            .is_none_or(|tip| lowest_retained_height > tip)
        {
            return Err("retained-body floor must not exceed the finalized tip".into());
        }
        let Some(witness) = witness.update(db)? else {
            return Ok(None);
        };
        let lowest_retained_height = lowest_retained_height
            .min(Height(previous.0.saturating_add(MAX_PRUNING_BATCH_HEIGHTS)))
            .min(witness);
        if lowest_retained_height <= previous {
            return Ok(None);
        }
        let transactions = db
            .db
            .cf_handle("tx_by_loc")
            .expect("transaction column family was created when opening state");
        self.zs_delete_range(
            &transactions,
            TransactionLocation::min_for_height(previous),
            TransactionLocation::min_for_height(lowest_retained_height),
        );
        Ok(Some(lowest_retained_height))
    }
}
