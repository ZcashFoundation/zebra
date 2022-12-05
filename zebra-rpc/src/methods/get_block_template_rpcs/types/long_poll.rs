//! Long polling support for the `getblocktemplate` RPC.
//!
//! These implementation details are private, and should not be relied upon by miners.
//! They are also different from the `zcashd` implementation of long polling.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use zebra_chain::{
    block::{self, Height},
    transaction,
};

/// The inputs to the long polling check.
///
/// If these inputs change, Zebra should return a response to any open long polls.
pub struct LongPollInput {
    // Fields that invalidate old work:
    //
    /// If the tip block height changes, a new template must be provided.
    /// Old work is no longer valid.
    ///
    /// We use the height as well as the hash, to reduce the probability of a missed tip change.
    pub tip_block_height: Height,

    /// If the tip block hash changes, a new template must be provided.
    /// Old work is no longer valid.
    pub tip_block_hash: block::Hash,

    /// If the max time is reached, a new template must be provided.
    /// Old work is no longer valid.
    ///
    /// Ideally, a new template should be provided at least one target block interval before
    /// the max time. This avoids wasted work.
    pub max_time: DateTime<Utc>,

    // Fields that allow old work:
    //
    /// If the mempool transactions change, a new template might be provided.
    /// Old work is still valid.
    ///
    /// We ignore changes to authorizing data.
    pub mempool_transaction_mined_ids: Arc<[transaction::Hash]>,
}

/// The encoded long poll ID, generated from the [`LongPollInput`].
///
/// `zcashd` IDs are currently 69 hex digits long.
/// If Zebra's IDs are less than that, we should have good compatibility with mining pools.
pub struct LongPollId {
    // Fields that invalidate old work:
    //
    /// If the tip block height changes, a new template must be provided.
    /// Old work is no longer valid.
    ///
    /// We use the height as well as the hash, to reduce the probability of a missed tip change.
    pub tip_block_height: u32,

    /// If the tip block changes, a new template must be provided.
    /// Old work is no longer valid.
    /// This checksum is not cryptographically secure.
    ///
    /// It's ok to do a probabilistic check here,
    /// so we choose a 1 in 2^64 chance of missing a block change.
    pub tip_block_checksum: u64,

    /// If the max time is reached, a new template must be provided.
    /// Old work is no longer valid.
    ///
    /// Ideally, a new template should be provided at least one target block interval before
    /// the max time. This avoids wasted work.
    ///
    /// Zcash times are limited to 32 bits by the consensus rules.
    pub max_timestamp: u32,

    // Fields that allow old work:
    //
    /// If the number of mempool transactions changes, a new template might be provided.
    /// Old work is still valid.
    ///
    /// The number of transactions is limited by the mempool DoS limit.
    pub mempool_transaction_count: u32,

    /// If the content of the mempool changes, a new template might be provided.
    /// Old work is still valid.
    /// This checksum is not cryptographically secure.
    ///
    /// We ignore changes to authorizing data.
    ///
    /// It's ok to do a probabilistic check here,
    /// so we choose a 1 in 2^32 chance of missing a transaction change.
    pub mempool_transaction_content_checksum: u32,
}

impl From<LongPollInput> for LongPollId {
    fn from(input: LongPollInput) -> Self {
        // TODO: make a generic checksum function?
        let mut tip_block_checksum = 0;
        for chunk in input.tip_block_hash.0.chunks(8) {
            let chunk = chunk.try_into().expect("chunk is u64 size");
            let chunk = u64::from_le_bytes(chunk);

            // This checksum is not cryptographically secure.
            // But it's ok to use xor here, because long polling checks are probabilistic,
            // and the height, time, and transaction count fields will detect most changes.
            //
            // Without those fields, miners could game the xor-ed block hash,
            // and hide block changes from other miners, gaining an advantage.
            // But this would reduce their profit under proof of work,
            // because the first valid block hash a miner generates will pay
            // a significant block subsidy.
            tip_block_checksum ^= chunk;
        }

        let mut mempool_transaction_content_checksum: u32 = 0;
        for tx_mined_id in input.mempool_transaction_mined_ids.iter() {
            for chunk in tx_mined_id.0.chunks(4) {
                let chunk = chunk.try_into().expect("chunk is u32 size");
                let chunk = u32::from_le_bytes(chunk);

                mempool_transaction_content_checksum ^= chunk;
            }
        }

        Self {
            tip_block_height: input.tip_block_height.0,

            tip_block_checksum,

            // It's ok to do wrapping conversions here,
            // because long polling checks are probabilistic.
            max_timestamp: input.max_time.timestamp() as u32,

            mempool_transaction_count: input.mempool_transaction_mined_ids.len() as u32,

            mempool_transaction_content_checksum,
        }
    }
}
