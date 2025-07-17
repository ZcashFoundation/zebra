use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crossbeam_channel::TryRecvError;
use itertools::Itertools;
use rayon::iter::{IntoParallelIterator, ParallelIterator as _};
use zebra_chain::{
    amount::NonNegative,
    block::{Block, Height},
    block_info::BlockInfo,
    parameters::subsidy::{block_subsidy, funding_stream_values, FundingStreamReceiver},
    transparent::{self, OutPoint, Utxo},
    value_balance::ValueBalance,
};

use crate::{
    service::finalized_state::disk_format::transparent::{
        AddressBalanceLocationChange, AddressLocation,
    },
    DiskWriteBatch, HashOrHeight, TransactionLocation, WriteDisk,
};

use super::{CancelFormatChange, DiskFormatUpgrade};

/// Implements [`DiskFormatUpgrade`] for adding additionl block info to the
/// database.
pub struct Upgrade;

/// The result of loading data to create a [`BlockInfo`]. If the info was
/// already there we only need to ValueBalance to keep track of the totals.
/// Otherwise we need the block, size and utxos to compute the BlockInfo.
enum LoadResult {
    HasInfo(ValueBalance<NonNegative>),
    LoadedInfo {
        block: Arc<Block>,
        size: usize,
        utxos: HashMap<OutPoint, Utxo>,
        address_balance_changes: HashMap<transparent::Address, AddressBalanceLocationChange>,
    },
}

impl DiskFormatUpgrade for Upgrade {
    fn version(&self) -> semver::Version {
        semver::Version::new(27, 0, 0)
    }

    fn description(&self) -> &'static str {
        "add block info and address received balances upgrade"
    }

    #[allow(clippy::unwrap_in_result)]
    fn run(
        &self,
        initial_tip_height: zebra_chain::block::Height,
        db: &crate::ZebraDb,
        cancel_receiver: &crossbeam_channel::Receiver<super::CancelFormatChange>,
    ) -> Result<(), super::CancelFormatChange> {
        let network = db.network();
        let balance_by_transparent_addr = db.address_balance_cf();
        let chunk_size = rayon::current_num_threads();
        tracing::info!(chunk_size = ?chunk_size, "adding block info data");

        let chunks = (0..=initial_tip_height.0).chunks(chunk_size);
        // Since transaction parsing is slow, we want to parallelize it.
        // Get chunks of block heights and load them in parallel.
        let seq_iter = chunks.into_iter().flat_map(|height_span| {
            let height_vec = height_span.collect_vec();
            let result_vec = height_vec
                .into_par_iter()
                .map(|h| {
                    // Return early if the upgrade is cancelled.
                    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                        return Err(super::CancelFormatChange);
                    }

                    let height = Height(h);

                    // The upgrade might have been interrupted and some heights might
                    // have already been filled. Return a value indicating that
                    // along with the loaded value pool.
                    if let Some(existing_block_info) = db.block_info_cf().zs_get(&height) {
                        let value_pool = *existing_block_info.value_pools();
                        return Ok((h, LoadResult::HasInfo(value_pool)));
                    }

                    // Load the block. This is slow since transaction
                    // parsing is slow.
                    let (block, size) = db
                        .block_and_size(HashOrHeight::Height(height))
                        .expect("block info should be in the database");

                    // Load the utxos for all the transactions inputs in the block.
                    // This is required to compute the value pool change.
                    // This is slow because transaction parsing is slow.
                    let mut utxos = HashMap::new();
                    let mut address_balance_changes = HashMap::new();
                    for tx in &block.transactions {
                        for input in tx.inputs() {
                            if let Some(outpoint) = input.outpoint() {
                                let (tx, h, _) = db
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

                        for output in tx.outputs() {
                            if let Some(address) = output.address(&network) {
                                // Note: using `empty()` will set the location
                                // to a dummy zero value. This only works
                                // because the addition operator for
                                // `AddressBalanceLocationChange` (which reuses
                                // the `AddressBalanceLocationInner` addition
                                // operator) will ignore these dummy values when
                                // adding balances during the merge operator.
                                *address_balance_changes
                                    .entry(address)
                                    .or_insert_with(AddressBalanceLocationChange::empty)
                                    .received_mut() += u64::from(output.value());
                            }
                        }
                    }

                    Ok((
                        h,
                        LoadResult::LoadedInfo {
                            block,
                            size,
                            utxos,
                            address_balance_changes,
                        },
                    ))
                })
                .collect::<Vec<_>>();
            // The collected Vec is in-order as required as guaranteed by Rayon.
            // Note that since we use flat_map() above, the result iterator will
            // iterate through individual results as expected.
            result_vec
        });

        // Keep track of the current value pool as we iterate the blocks.
        let mut value_pool = ValueBalance::<NonNegative>::default();

        for result in seq_iter {
            let (h, load_result) = result?;
            let height = Height(h);
            if height.0 % 1000 == 0 {
                tracing::info!(height = ?height, "adding block info for height");
            }
            // Get the data loaded from the parallel iterator
            let (block, size, utxos, address_balance_changes) = match load_result {
                LoadResult::HasInfo(prev_value_pool) => {
                    // BlockInfo already stored; we just need the its value pool
                    // then skip the block
                    value_pool = prev_value_pool;
                    continue;
                }
                LoadResult::LoadedInfo {
                    block,
                    size,
                    utxos,
                    address_balance_changes,
                } => (block, size, utxos, address_balance_changes),
            };

            // Get the deferred amount which is required to update the value pool.
            let expected_deferred_amount = if height > network.slow_start_interval() {
                // See [ZIP-1015](https://zips.z.cash/zip-1015).
                funding_stream_values(
                    height,
                    &network,
                    block_subsidy(height, &network).unwrap_or_default(),
                )
                .unwrap_or_default()
                .remove(&FundingStreamReceiver::Deferred)
            } else {
                None
            };

            // Add this block's value pool changes to the total value pool.
            value_pool = value_pool
                .add_chain_value_pool_change(
                    block
                        .chain_value_pool_change(&utxos, expected_deferred_amount)
                        .unwrap_or_default(),
                )
                .expect("value pool change should not overflow");

            let mut batch = DiskWriteBatch::new();

            // Create and store the BlockInfo for this block.
            let block_info = BlockInfo::new(value_pool, size as u32);
            let _ = db
                .block_info_cf()
                .with_batch_for_writing(&mut batch)
                .zs_insert(&height, &block_info);

            // Update transparent addresses that received funds in this block.
            for (address, change) in address_balance_changes {
                // Note that the logic of the merge operator is set up by
                // calling `set_merge_operator_associative()` in `DiskDb`.
                batch.zs_merge(balance_by_transparent_addr, address, change);
            }

            db.write_batch(batch)
                .expect("writing block info and address received changes should succeed");
        }

        Ok(())
    }

    #[allow(clippy::unwrap_in_result)]
    fn validate(
        &self,
        db: &crate::ZebraDb,
        cancel_receiver: &crossbeam_channel::Receiver<super::CancelFormatChange>,
    ) -> Result<Result<(), String>, super::CancelFormatChange> {
        let network = db.network();

        // Return early before the next disk read if the upgrade was cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(super::CancelFormatChange);
        }

        // Read the finalized tip height or return early if the database is empty.
        let Some(tip_height) = db.finalized_tip_height() else {
            return Ok(Ok(()));
        };

        // Check any outputs in the last 1000 blocks.
        let start_height = (tip_height - 1_000).unwrap_or(Height::MIN);

        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // Check that all blocks in the range have a BlockInfo.

        for height in start_height.0..=tip_height.0 {
            if let Some(block_info) = db.block_info_cf().zs_get(&Height(height)) {
                if block_info == Default::default() {
                    return Ok(Err(format!("zero block info for height: {height}")));
                }
            } else {
                return Ok(Err(format!("missing block info for height: {height}")));
            }
        }

        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // Check that all recipient addresses of transparent transfers in the range have a non-zero received balance.

        // Collect the set of addresses that received transparent funds in the last query range (last 1000 blocks).
        let tx_loc_range = TransactionLocation::min_for_height(start_height)..;
        let addresses: HashSet<_> = db
            .transactions_by_location_range(tx_loc_range)
            .flat_map(|(_, tx)| tx.outputs().to_vec())
            .filter_map(|output| {
                if output.value != 0 {
                    output.address(&network)
                } else {
                    None
                }
            })
            .collect();

        // Check that no address balances for that set of addresses have a received field of `0`.
        for address in addresses {
            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            let balance = db
                .address_balance_location(&address)
                .expect("should have address balances in finalized state");

            if balance.received() == 0 {
                return Ok(Err(format!(
                    "unexpected balance received for address {}: {}",
                    address,
                    balance.received(),
                )));
            }
        }

        Ok(Ok(()))
    }
}

impl AddressBalanceLocationChange {
    /// Creates a new [`AddressBalanceLocationChange`] with all zero values and a dummy location.
    fn empty() -> Self {
        Self::new(AddressLocation::from_usize(Height(0), 0, 0))
    }
}
