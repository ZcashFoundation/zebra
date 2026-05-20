//! Long polling support for the `getblocktemplate` RPC.
//!
//! These implementation details are private, and should not be relied upon by miners.
//! They are also different from the `zcashd` implementation of long polling.

use std::{str::FromStr, sync::Arc};

use derive_getters::Getters;
use derive_new::new;
use serde::{Deserialize, Serialize};

use zebra_chain::{
    block::{self, Height},
    serialization::DateTime32,
    transaction::{self, UnminedTxId},
};
use zebra_node_services::BoxError;

/// The length of a serialized [`LongPollId`] string.
///
/// This is an internal Zebra implementation detail, which does not need to match `zcashd`.
pub const LONG_POLL_ID_LENGTH: usize = 46;

/// The inputs to the long polling check.
///
/// If these inputs change, Zebra should return a response to any open long polls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LongPollInput {
    // Fields that invalidate old work:
    //
    /// The tip height used to generate the template containing this long poll ID.
    ///
    /// If the tip block height changes, a new template must be provided.
    /// Old work is no longer valid.
    ///
    /// The height is technically redundant, but it helps with debugging.
    /// It also reduces the probability of a missed tip change.
    pub tip_height: Height,

    /// The tip hash used to generate the template containing this long poll ID.
    ///
    /// If the tip block changes, a new template must be provided.
    /// Old work is no longer valid.
    pub tip_hash: block::Hash,

    /// The max time in the same template as this long poll ID.
    ///
    /// If the max time is reached, a new template must be provided.
    /// Old work is no longer valid.
    ///
    /// Ideally, a new template should be provided at least one target block interval before
    /// the max time. This avoids wasted work.
    pub max_time: DateTime32,

    // Fields that allow old work:
    //
    /// The effecting hashes of the transactions in the mempool,
    /// when the template containing this long poll ID was generated.
    /// We ignore changes to authorizing data.
    ///
    /// This might be different from the transactions in the template, due to ZIP-317.
    ///
    /// If the mempool transactions change, a new template might be provided.
    /// Old work is still valid.
    pub mempool_transaction_mined_ids: Arc<[transaction::Hash]>,
}

impl LongPollInput {
    /// Returns a new [`LongPollInput`], based on the supplied fields.
    pub fn new(
        tip_height: Height,
        tip_hash: block::Hash,
        max_time: DateTime32,
        mempool_tx_ids: impl IntoIterator<Item = UnminedTxId>,
    ) -> Self {
        let mut tx_mined_ids: Vec<transaction::Hash> =
            mempool_tx_ids.into_iter().map(|id| id.mined_id()).collect();

        // The mempool returns unordered transactions, we need to sort them here so
        // that the longpollid doesn't change unexpectedly.
        tx_mined_ids.sort();

        LongPollInput {
            tip_height,
            tip_hash,
            max_time,
            mempool_transaction_mined_ids: tx_mined_ids.into(),
        }
    }

    /// Returns the [`LongPollId`] for this [`LongPollInput`].
    /// Performs lossy conversion on some fields.
    pub fn generate_id(&self) -> LongPollId {
        let mut tip_hash_checksum = 0;
        update_checksum(&mut tip_hash_checksum, self.tip_hash.0);

        let mut mempool_transaction_content_checksum: u32 = 0;
        for tx_mined_id in self.mempool_transaction_mined_ids.iter() {
            update_checksum(&mut mempool_transaction_content_checksum, tx_mined_id.0);
        }

        LongPollId {
            tip_height: self.tip_height.0,

            tip_hash_checksum,

            max_timestamp: self.max_time.timestamp(),

            // It's ok to do wrapping conversions here,
            // because long polling checks are probabilistic.
            mempool_transaction_count: self.mempool_transaction_mined_ids.len() as u32,

            mempool_transaction_content_checksum,
        }
    }
}

/// The encoded long poll ID, generated from the [`LongPollInput`].
///
/// `zcashd` IDs are currently 69 hex/decimal digits long.
/// Since Zebra's IDs are only 46 hex/decimal digits, mining pools should be able to handle them.
#[derive(
    Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Getters, new, schemars::JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct LongPollId {
    // Fields that invalidate old work:
    //
    /// The tip height used to generate the template containing this long poll ID.
    ///
    /// If the tip block height changes, a new template must be provided.
    /// Old work is no longer valid.
    ///
    /// The height is technically redundant, but it helps with debugging.
    /// It also reduces the probability of a missed tip change.
    pub(crate) tip_height: u32,

    /// A checksum of the tip hash used to generate the template containing this long poll ID.
    ///
    /// If the tip block changes, a new template must be provided.
    /// Old work is no longer valid.
    /// This checksum is not cryptographically secure.
    ///
    /// It's ok to do a probabilistic check here,
    /// so we choose a 1 in 2^32 chance of missing a block change.
    pub(crate) tip_hash_checksum: u32,

    /// The max time in the same template as this long poll ID.
    ///
    /// If the max time is reached, a new template must be provided.
    /// Old work is no longer valid.
    ///
    /// Ideally, a new template should be provided at least one target block interval before
    /// the max time. This avoids wasted work.
    ///
    /// Zcash times are limited to 32 bits by the consensus rules.
    pub(crate) max_timestamp: u32,

    // Fields that allow old work:
    //
    /// The number of transactions in the mempool when the template containing this long poll ID
    /// was generated. This might be different from the number of transactions in the template,
    /// due to ZIP-317.
    ///
    /// If the number of mempool transactions changes, a new template might be provided.
    /// Old work is still valid.
    ///
    /// The number of transactions is limited by the mempool DoS limit.
    ///
    /// Using the number of transactions makes mempool checksum attacks much harder.
    /// It also helps with debugging, and reduces the probability of a missed mempool change.
    pub(crate) mempool_transaction_count: u32,

    /// A checksum of the effecting hashes of the transactions in the mempool,
    /// when the template containing this long poll ID was generated.
    /// We ignore changes to authorizing data.
    ///
    /// This might be different from the transactions in the template, due to ZIP-317.
    ///
    /// If the content of the mempool changes, a new template might be provided.
    /// Old work is still valid.
    ///
    /// This checksum is not cryptographically secure.
    ///
    /// It's ok to do a probabilistic check here,
    /// so we choose a 1 in 2^32 chance of missing a transaction change.
    ///
    /// # Security
    ///
    /// Attackers could use dust transactions to keep the checksum at the same value.
    /// But this would likely change the number of transactions in the mempool.
    ///
    /// If an attacker could also keep the number of transactions constant,
    /// a new template will be generated when the tip hash changes, or the max time is reached.
    pub(crate) mempool_transaction_content_checksum: u32,
}

impl LongPollId {
    /// Returns `true` if shares using `old_long_poll_id` can be submitted in response to the
    /// template for `self`:
    /// <https://en.bitcoin.it/wiki/BIP_0022#Optional:_Long_Polling>
    ///
    /// Old shares may be valid if only the mempool transactions have changed,
    /// because newer transactions don't have to be included in the old shares.
    ///
    /// But if the chain tip has changed, the block header has changed, so old shares are invalid.
    /// (And if the max time has changed on testnet, the block header has changed.)
    pub fn submit_old(&self, old_long_poll_id: &LongPollId) -> bool {
        self.tip_height == old_long_poll_id.tip_height
            && self.tip_hash_checksum == old_long_poll_id.tip_hash_checksum
            && self.max_timestamp == old_long_poll_id.max_timestamp
    }
}

/// Update `checksum` from `item`, so changes in `item` are likely to also change `checksum`.
///
/// This checksum is not cryptographically secure.
fn update_checksum(checksum: &mut u32, item: [u8; 32]) {
    for chunk in item.chunks(4) {
        let chunk = chunk.try_into().expect("chunk is u32 size");

        // The endianness of this conversion doesn't matter,
        // so we make it efficient on the most common platforms.
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

impl std::fmt::Display for LongPollId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let LongPollId {
            tip_height,
            tip_hash_checksum,
            max_timestamp,
            mempool_transaction_count,
            mempool_transaction_content_checksum,
        } = self;

        // We can't do this using `serde`, because it names each field,
        // but we want a single string containing all the fields.
        write!(
            f,
            // Height as decimal, padded with zeroes to the width of Height::MAX
            // Checksums as hex, padded with zeroes to the width of u32::MAX
            // Timestamp as decimal, padded with zeroes to the width of u32::MAX
            // Transaction Count as decimal, padded with zeroes to the width of u32::MAX
            "{tip_height:010}\
             {tip_hash_checksum:08x}\
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
        if long_poll_id.len() != LONG_POLL_ID_LENGTH {
            return Err(format!(
                "incorrect long poll id length, must be {LONG_POLL_ID_LENGTH} for Zebra"
            )
            .into());
        }

        Ok(Self {
            tip_height: long_poll_id[0..10].parse()?,
            tip_hash_checksum: u32::from_str_radix(&long_poll_id[10..18], 16)?,
            max_timestamp: long_poll_id[18..28].parse()?,
            mempool_transaction_count: long_poll_id[28..38].parse()?,
            mempool_transaction_content_checksum: u32::from_str_radix(
                &long_poll_id[38..LONG_POLL_ID_LENGTH],
                16,
            )?,
        })
    }
}

// Wrappers for serde conversion
impl From<LongPollId> for String {
    fn from(id: LongPollId) -> Self {
        id.to_string()
    }
}

impl TryFrom<String> for LongPollId {
    type Error = BoxError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// Check that [`LongPollInput::new`] will sort mempool transaction ids.
///
/// The mempool does not currently guarantee the order in which it will return transactions and
/// may return the same items in a different order, while the long poll id should be the same if
/// its other components are equal and no transactions have been added or removed in the mempool.
#[test]
fn long_poll_input_mempool_tx_ids_are_sorted() {
    let mempool_tx_ids = || {
        (0..10)
            .map(|i| transaction::Hash::from([i; 32]))
            .map(UnminedTxId::Legacy)
    };

    assert_eq!(
        LongPollInput::new(Height::MIN, Default::default(), 0.into(), mempool_tx_ids()),
        LongPollInput::new(
            Height::MIN,
            Default::default(),
            0.into(),
            mempool_tx_ids().rev()
        ),
        "long poll input should sort mempool tx ids"
    );
}
