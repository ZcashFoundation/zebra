//! Availability of historical block bodies.

#[cfg(test)]
use zebra_chain::{block::Height, parameters::NetworkUpgrade};

use crate::{HistoryCapabilities, StateInitError};

use super::ZebraDb;
#[cfg(test)]
use crate::service::finalized_state::{
    DiskWriteBatch, TransactionLocation, WriteDisk, MAX_ON_DISK_HEIGHT,
};
#[cfg(test)]
use crate::BoxError;

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

#[cfg(test)]
impl DiskWriteBatch {
    /// Stages contiguous body deletion, advancing the derived floor in the same batch.
    ///
    /// The caller must be the sole pruning owner and commit this batch before preparing another.
    /// Current block writes may continue concurrently. Genesis and the finalized tip are retained.
    /// The startup checks must be complete, and deletion is capped at the restart witness.
    /// No production caller may use this until pruning startup and consumer protections are
    /// enabled.
    pub(crate) fn prepare_history_pruning(
        &mut self,
        db: &ZebraDb,
        lowest_retained_height: Height,
    ) -> Result<(), BoxError> {
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
        let network = db.network();
        let activation = NetworkUpgrade::Nu5
            .activation_height(&network)
            .ok_or("pruning requires a network with NU5 activated")?;
        let ancestors = crate::service::block_iter::known_chain_ancestor_iter::<
            zebra_chain::block::Block,
        >(None, db, db.finalized_tip_hash().into());
        let witness = crate::service::check::legacy_chain(
            activation,
            ancestors,
            &network,
            crate::constants::MAX_LEGACY_CHAIN_BLOCKS,
        )?
        .ok_or("pruning requires a retained transaction branch ID for restart checks")?;
        let lowest_retained_height = lowest_retained_height.min(witness);
        if lowest_retained_height <= previous {
            return Err("pruning would delete the restart witness".into());
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
        Ok(())
    }
}
