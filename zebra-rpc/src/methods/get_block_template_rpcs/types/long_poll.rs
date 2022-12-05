//! Long polling support for the `getblocktemplate` RPC.
//!
//! These implementation details are private, and should not be relied upon by miners.
//! They are also different from the `zcashd` implementation of long polling.

use std::{str::FromStr, sync::Arc};

use chrono::{DateTime, Utc};

use zebra_chain::{
    block::{self, Height},
    transaction,
};
use zebra_node_services::BoxError;

/// The inputs to the long polling check.
///
/// If these inputs change, Zebra should return a response to any open long polls.
#[derive(Clone, Debug, Eq, PartialEq)]
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
/// `zcashd` IDs are currently 69 hex/decimal digits long.
/// Since Zebra's IDs are only 46 hex/decimal digits, mining pools should be able to handle them.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    /// so we choose a 1 in 2^32 chance of missing a block change.
    pub tip_block_checksum: u32,

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
    /// Lossy conversion from LongPollInput to LongPollId.
    fn from(input: LongPollInput) -> Self {
        let mut tip_block_checksum = 0;
        update_checksum(&mut tip_block_checksum, input.tip_block_hash.0);

        let mut mempool_transaction_content_checksum: u32 = 0;
        for tx_mined_id in input.mempool_transaction_mined_ids.iter() {
            update_checksum(&mut mempool_transaction_content_checksum, tx_mined_id.0);
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

/// Update `checksum` from `item`, so changes in `item` are likely to also change `checksum`.
///
/// This checksum is not cryptographically secure.
fn update_checksum(checksum: &mut u32, item: [u8; 32]) {
    for chunk in item.chunks(4) {
        let chunk = chunk.try_into().expect("chunk is u32 size");
        let chunk = u32::from_le_bytes(chunk);

        // It's ok to use xor here, because long polling checks are probabilistic,
        // and the height, time, and transaction count fields will detect most changes.
        //
        // Without those fields, miners could game the xor-ed block hash,
        // and hide block changes from other miners, gaining an advantage.
        // But this would reduce their profit under proof of work,
        // because the first valid block hash a miner generates will pay
        // a significant block subsidy.
        *checksum ^= chunk;
    }
}

impl ToString for LongPollId {
    /// Exact conversion from LongPollId to a string.
    fn to_string(&self) -> String {
        let LongPollId {
            tip_block_height,
            tip_block_checksum,
            max_timestamp,
            mempool_transaction_count,
            mempool_transaction_content_checksum,
        } = self;

        // We can't do this using `serde`, because it names each field,
        // but we want a single string containing all the fields.
        format!(
            // Height as decimal, padded with zeroes to the width of Height::MAX
            // Checksums as hex, padded with zeroes to the width of u32::MAX
            // Timestamp as decimal, padded with zeroes to the width of u32::MAX
            // Transaction Count as decimal, padded with zeroes to the width of u32::MAX
            "{tip_block_height:010}\
             {tip_block_checksum:08x}\
             {max_timestamp:010}\
             {mempool_transaction_count:010}\
             {mempool_transaction_content_checksum:08x}"
        )
    }
}

impl FromStr for LongPollId {
    type Err = BoxError;

    /// Exact conversion from a string to LongPollId.
    fn from_str(long_poll_id: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            tip_block_height: long_poll_id[0..10].parse()?,
            tip_block_checksum: u32::from_str_radix(&long_poll_id[10..18], 16)?,
            max_timestamp: long_poll_id[18..28].parse()?,
            mempool_transaction_count: long_poll_id[28..38].parse()?,
            mempool_transaction_content_checksum: u32::from_str_radix(&long_poll_id[38..46], 16)?,
        })
    }
}
