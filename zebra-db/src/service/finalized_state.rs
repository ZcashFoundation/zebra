//! The finalized storage implementation built upon RocksDB.

use zebra_chain::{
    amount::{DeferredPoolBalanceChange, NonNegative},
    block,
    parameters::{
        subsidy::{block_subsidy_with_parent_pools, SubsidyError},
        Network,
    },
    value_balance::ValueBalance,
};

pub mod column_family;
pub mod disk_db;
pub mod disk_format;
pub mod zebra_db;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;

#[allow(unused_imports)]
pub use column_family::{TypedColumnFamily, WriteTypedBatch};
#[allow(unused_imports)]
pub use disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk};
pub use disk_format::{
    block::{HEIGHT_DISK_BYTES, TRANSACTION_LOCATION_DISK_BYTES},
    chain::{
        check_history_tree_peaks, HistoryTreeParts, LegacyHistoryTreeParts, RawHistoryTree,
        SNAPSHOT_BASE,
    },
    transparent::{AddressBalanceLocation, AddressUnspentOutput, OUTPUT_LOCATION_DISK_BYTES},
    upgrade::{
        can_read_without_migration, restorable_db_versions, CancelFormatChange, DbFormatChange,
        FormatCheckError,
    },
};
#[allow(unused_imports)]
pub use disk_format::{
    FromDisk, IntoDisk, OutputLocation, RawBytes, TransactionIndex, TransactionLocation,
    MAX_ON_DISK_HEIGHT,
};
pub use zebra_db::{
    history::{PruningWitness, MAX_PRUNING_BATCH_HEIGHTS},
    ZebraDb,
};

#[cfg(any(test, feature = "proptest-impl"))]
pub use disk_format::KV;

/// The column families supported by the running `zebra-state` database code.
///
/// Existing column families that aren't listed here are preserved when the database is opened.
pub const STATE_COLUMN_FAMILIES_IN_CODE: &[&str] = &[
    // Blocks
    "hash_by_height",
    "height_by_hash",
    "block_header_by_height",
    // Transactions
    "tx_by_loc",
    "hash_by_tx_loc",
    "tx_loc_by_hash",
    // Transparent
    disk_format::transparent::BALANCE_BY_TRANSPARENT_ADDR,
    "tx_loc_by_transparent_addr_loc",
    "utxo_by_out_loc",
    "utxo_loc_by_transparent_addr_loc",
    zebra_db::transparent::TX_LOC_BY_SPENT_OUT_LOC,
    // Sprout
    "sprout_nullifiers",
    "sprout_anchors",
    "sprout_note_commitment_tree",
    // Sapling
    "sapling_nullifiers",
    "sapling_anchors",
    "sapling_note_commitment_tree",
    "sapling_note_commitment_subtree",
    // Orchard
    "orchard_nullifiers",
    "orchard_anchors",
    "orchard_note_commitment_tree",
    "orchard_note_commitment_subtree",
    // Ironwood (NU6.3). Always registered so the database format is stable across build
    // flags; these stay empty until NU6.3 transactions appear.
    "ironwood_nullifiers",
    "ironwood_anchors",
    "ironwood_note_commitment_tree",
    "ironwood_note_commitment_subtree",
    // Chain
    "history_tree",
    "tip_chain_value_pool",
    zebra_db::chain::BLOCK_INFO,
    // Per-block synchronization metadata served to version 2 peers
    zebra_db::sync_meta::SYNC_META_BY_HEIGHT,
    // Spentness hints
    zebra_db::spentness::SPENTNESS_JOURNAL,
    zebra_db::spentness::SPENTNESS_ARTIFACT,
    // Headers stored ahead of their block bodies by headers-first sync
    zebra_db::header_chain::HEADER_BY_HEIGHT,
    zebra_db::header_chain::HEIGHT_BY_HEADER_HASH,
    // Snapshot sync. Always registered, and empty unless the database was imported from a
    // state snapshot.
    SNAPSHOT_BASE,
];

/// Calculates deferred issuance and fixed lockbox disbursements using the exact parent's pools.
pub fn calculate_deferred_pool_balance_change(
    height: block::Height,
    network: &Network,
    parent_chain_value_pools: ValueBalance<NonNegative>,
) -> Result<DeferredPoolBalanceChange, SubsidyError> {
    zebra_chain::parameters::subsidy::funding_stream_values(
        height,
        network,
        block_subsidy_with_parent_pools(height, network, parent_chain_value_pools)?,
    )?
    .remove(&zebra_chain::parameters::subsidy::FundingStreamReceiver::Deferred)
    .unwrap_or_default()
    .checked_sub(network.lockbox_disbursement_total_amount(height))
    .map(DeferredPoolBalanceChange::new)
    .ok_or(SubsidyError::Underflow)
}
