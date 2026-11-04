//! Compile-time compatibility checks for finalized-block batch methods.

use std::collections::{BTreeMap, HashMap};

use zebra_chain::{
    amount::NonNegative, parallel::tree::NoteCommitmentTrees, parameters::Network, transparent,
    value_balance::ValueBalance,
};

use crate::{
    request::FinalizedBlock,
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch},
        disk_format::transparent::{AddressBalanceLocationUpdates, OutputLocation},
        ZebraDb,
    },
    CommitCheckpointVerifiedError, ValidateContextError,
};

type OutputsByLocation = BTreeMap<OutputLocation, transparent::Utxo>;
type SpentOutputs = HashMap<transparent::OutPoint, transparent::Utxo>;
#[cfg(feature = "indexer")]
type SpentOutputLocations = HashMap<transparent::OutPoint, OutputLocation>;

type PrepareShielded = fn(&mut DiskWriteBatch, &ZebraDb, &FinalizedBlock);
type PrepareTrees = fn(&mut DiskWriteBatch, &ZebraDb, &FinalizedBlock, Option<NoteCommitmentTrees>);
type PrepareChainValuePools = fn(
    &mut DiskWriteBatch,
    &ZebraDb,
    &FinalizedBlock,
    SpentOutputs,
    ValueBalance<NonNegative>,
) -> Result<(), ValidateContextError>;
#[cfg(not(feature = "indexer"))]
type PrepareTransparent = fn(
    &mut DiskWriteBatch,
    &ZebraDb,
    &Network,
    &FinalizedBlock,
    &OutputsByLocation,
    &SpentOutputs,
    &OutputsByLocation,
    AddressBalanceLocationUpdates,
);
#[cfg(feature = "indexer")]
type PrepareTransparent = fn(
    &mut DiskWriteBatch,
    &ZebraDb,
    &Network,
    &FinalizedBlock,
    &OutputsByLocation,
    &SpentOutputs,
    &OutputsByLocation,
    &SpentOutputLocations,
    AddressBalanceLocationUpdates,
);
#[cfg(not(feature = "indexer"))]
type PrepareBlock = fn(
    &mut DiskWriteBatch,
    &ZebraDb,
    &Network,
    &FinalizedBlock,
    OutputsByLocation,
    SpentOutputs,
    OutputsByLocation,
    AddressBalanceLocationUpdates,
    ValueBalance<NonNegative>,
    Option<NoteCommitmentTrees>,
) -> Result<(), CommitCheckpointVerifiedError>;
#[cfg(feature = "indexer")]
type PrepareBlock = fn(
    &mut DiskWriteBatch,
    &ZebraDb,
    &Network,
    &FinalizedBlock,
    OutputsByLocation,
    SpentOutputs,
    OutputsByLocation,
    SpentOutputLocations,
    AddressBalanceLocationUpdates,
    ValueBalance<NonNegative>,
    Option<NoteCommitmentTrees>,
) -> Result<(), CommitCheckpointVerifiedError>;
type PrepareBlockHeader = fn(&mut DiskWriteBatch, &DiskDb, &FinalizedBlock);

fn assert_shielded_fn<F>(_f: F)
where
    F: Fn(&mut DiskWriteBatch, &ZebraDb, &FinalizedBlock) + Copy,
{
}

fn assert_trees_fn<F>(_f: F)
where
    F: Fn(&mut DiskWriteBatch, &ZebraDb, &FinalizedBlock, Option<NoteCommitmentTrees>) + Copy,
{
}

fn assert_chain_value_pools_fn<F>(_f: F)
where
    F: Fn(
            &mut DiskWriteBatch,
            &ZebraDb,
            &FinalizedBlock,
            SpentOutputs,
            ValueBalance<NonNegative>,
        ) -> Result<(), ValidateContextError>
        + Copy,
{
}

#[cfg(not(feature = "indexer"))]
fn assert_transparent_fn<F>(_f: F)
where
    F: Fn(
            &mut DiskWriteBatch,
            &ZebraDb,
            &Network,
            &FinalizedBlock,
            &OutputsByLocation,
            &SpentOutputs,
            &OutputsByLocation,
            AddressBalanceLocationUpdates,
        ) + Copy,
{
}

#[cfg(feature = "indexer")]
fn assert_transparent_fn<F>(_f: F)
where
    F: Fn(
            &mut DiskWriteBatch,
            &ZebraDb,
            &Network,
            &FinalizedBlock,
            &OutputsByLocation,
            &SpentOutputs,
            &OutputsByLocation,
            &SpentOutputLocations,
            AddressBalanceLocationUpdates,
        ) + Copy,
{
}

#[cfg(not(feature = "indexer"))]
fn assert_block_fn<F>(_f: F)
where
    F: Fn(
            &mut DiskWriteBatch,
            &ZebraDb,
            &Network,
            &FinalizedBlock,
            OutputsByLocation,
            SpentOutputs,
            OutputsByLocation,
            AddressBalanceLocationUpdates,
            ValueBalance<NonNegative>,
            Option<NoteCommitmentTrees>,
        ) -> Result<(), CommitCheckpointVerifiedError>
        + Copy,
{
}

#[cfg(feature = "indexer")]
fn assert_block_fn<F>(_f: F)
where
    F: Fn(
            &mut DiskWriteBatch,
            &ZebraDb,
            &Network,
            &FinalizedBlock,
            OutputsByLocation,
            SpentOutputs,
            OutputsByLocation,
            SpentOutputLocations,
            AddressBalanceLocationUpdates,
            ValueBalance<NonNegative>,
            Option<NoteCommitmentTrees>,
        ) -> Result<(), CommitCheckpointVerifiedError>
        + Copy,
{
}

fn assert_block_header_fn<F>(_f: F)
where
    F: Fn(&mut DiskWriteBatch, &DiskDb, &FinalizedBlock) + Copy,
{
}

#[test]
fn finalized_block_batch_method_items_keep_state_signatures() {
    let _: PrepareShielded = DiskWriteBatch::prepare_shielded_transaction_batch;
    let _: PrepareTrees = DiskWriteBatch::prepare_trees_batch;
    let _: PrepareChainValuePools = DiskWriteBatch::prepare_chain_value_pools_batch;
    let _: PrepareTransparent = DiskWriteBatch::prepare_transparent_transaction_batch;
    let _: PrepareBlock = DiskWriteBatch::prepare_block_batch;
    let _: PrepareBlockHeader = DiskWriteBatch::prepare_block_header_and_transaction_data_batch;

    assert_shielded_fn(DiskWriteBatch::prepare_shielded_transaction_batch);
    assert_trees_fn(DiskWriteBatch::prepare_trees_batch);
    assert_chain_value_pools_fn(DiskWriteBatch::prepare_chain_value_pools_batch);
    assert_transparent_fn(DiskWriteBatch::prepare_transparent_transaction_batch);
    assert_block_fn(DiskWriteBatch::prepare_block_batch);
    assert_block_header_fn(DiskWriteBatch::prepare_block_header_and_transaction_data_batch);
}
