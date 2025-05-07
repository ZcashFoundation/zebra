use std::collections::HashMap;

use crossbeam_channel::TryRecvError;
use zebra_chain::{
    amount::NonNegative,
    block::Height,
    block_info::BlockInfo,
    parameters::{
        subsidy::{block_subsidy, funding_stream_values, FundingStreamReceiver},
        Network,
    },
    transparent::Utxo,
    value_balance::ValueBalance,
};

use crate::HashOrHeight;

use super::DiskFormatUpgrade;

/// Implements [`DiskFormatUpgrade`] for adding additionl block data to the
/// database; namely, value pool for each block.
pub struct AddBlockData {
    network: Network,
}

impl AddBlockData {
    /// Creates a new [`BlockData`] upgrade for the given network.
    pub fn new(network: Network) -> Self {
        Self { network }
    }
}

impl DiskFormatUpgrade for AddBlockData {
    fn version(&self) -> semver::Version {
        semver::Version::new(26, 1, 0)
    }

    fn description(&self) -> &'static str {
        "add block data upgrade"
    }

    #[allow(clippy::unwrap_in_result)]
    fn run(
        &self,
        initial_tip_height: zebra_chain::block::Height,
        db: &crate::ZebraDb,
        cancel_receiver: &crossbeam_channel::Receiver<super::CancelFormatChange>,
    ) -> Result<(), super::CancelFormatChange> {
        let mut value_pool = ValueBalance::<NonNegative>::default();
        for h in 0..=initial_tip_height.0 {
            // Return early if the upgrade is cancelled.
            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(super::CancelFormatChange);
            }

            let height = Height(h);

            // The upgrade might have been interrupted and some heights might
            // have already been filled. Skip those.
            if let Some(existing_block_data) = db.block_data_cf().zs_get(&height) {
                value_pool = *existing_block_data.value_pools();
                continue;
            }

            if height.0 % 1000 == 0 {
                tracing::info!(height = ?height, "adding block data for height");
            }

            let (block, size) = db
                .block_and_size(HashOrHeight::Height(height))
                .expect("block data should be in the database");

            let mut utxos = HashMap::new();
            for tx in &block.transactions {
                for input in tx.inputs() {
                    if let Some(outpoint) = input.outpoint() {
                        let (tx, h) = db
                            .transaction(outpoint.hash)
                            .expect("transaction should be in the database");
                        let output = tx
                            .outputs()
                            .get(outpoint.index as usize)
                            .expect("output should exist");

                        let utxo = Utxo {
                            output: output.clone(),
                            height: h,
                            from_coinbase: tx.is_coinbase(),
                        };
                        utxos.insert(outpoint, utxo);
                    }
                }
            }
            let expected_deferred_amount = if height > self.network.slow_start_interval() {
                // See [ZIP-1015](https://zips.z.cash/zip-1015).
                funding_stream_values(
                    height,
                    &self.network,
                    block_subsidy(height, &self.network).unwrap_or_default(),
                )
                .unwrap_or_default()
                .remove(&FundingStreamReceiver::Deferred)
            } else {
                None
            };

            value_pool = value_pool
                .add_chain_value_pool_change(
                    block
                        .chain_value_pool_change(&utxos, expected_deferred_amount)
                        .unwrap_or_default(),
                )
                .expect("value pool change should not overflow");

            let block_data = BlockInfo::new(value_pool, size as u32);

            db.block_data_cf()
                .new_batch_for_writing()
                .zs_insert(&height, &block_data)
                .write_batch()
                .expect("writing block data should succeed");
        }
        Ok(())
    }

    #[allow(clippy::unwrap_in_result)]
    fn validate(
        &self,
        db: &crate::ZebraDb,
        cancel_receiver: &crossbeam_channel::Receiver<super::CancelFormatChange>,
    ) -> Result<Result<(), String>, super::CancelFormatChange> {
        // Return early before the next disk read if the upgrade was cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(super::CancelFormatChange);
        }

        // Read the finalized tip height or return early if the database is empty.
        let Some(tip_height) = db.finalized_tip_height() else {
            return Ok(Ok(()));
        };

        // Check any outputs in the last 1000 blocks. We use MIN + 1 (i.e. 1)
        // instead of MIN (i.e. 0) as the fallback due to an exception in
        // DiskWriteBatch::prepare_block_batch() where we don't store the block
        // data for height 0.
        let start_height =
            (tip_height - 1_000).unwrap_or((Height::MIN + 1).expect("cannot overflow"));

        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(super::CancelFormatChange);
        }

        for height in start_height.0..=tip_height.0 {
            if let Some(block_data) = db.block_data_cf().zs_get(&Height(height)) {
                if block_data == Default::default() {
                    return Ok(Err(format!("zero block data for height: {}", height)));
                }
            } else {
                return Ok(Err(format!("missing block data for height: {}", height)));
            }
        }

        Ok(Ok(()))
    }
}
