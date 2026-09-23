//! User-configurable mempool parameters.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Mempool configuration section.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The mempool transaction cost limit.
    ///
    /// This limits the total serialized byte size of all transactions in the mempool.
    ///
    /// Consensus rule:
    /// > There MUST be a configuration option mempooltxcostlimit, which SHOULD default to 80000000.
    ///
    /// This corresponds to `mempooltxcostlimit` from [ZIP-401](https://zips.z.cash/zip-0401#specification).
    pub tx_cost_limit: u64,

    /// The mempool transaction eviction age limit.
    ///
    /// This limits the maximum amount of time evicted transaction IDs stay in
    /// the mempool rejection list. Transactions are randomly evicted from the
    /// mempool when the mempool reaches [`Self::tx_cost_limit`].
    ///
    /// (Transactions can also be rejected by the mempool for other reasons.
    /// Different rejection reasons can have different age limits.)
    ///
    /// This corresponds to `mempoolevictionmemoryminutes` from
    /// [ZIP-401](https://zips.z.cash/zip-0401#specification).
    #[serde(with = "humantime_serde")]
    pub eviction_memory_time: Duration,

    /// If the state's best chain tip has reached this height, always enable the mempool,
    /// regardless of Zebra's sync status.
    ///
    /// Set to `None` by default: Zebra always checks the sync status before enabling the mempool.
    //
    // TODO:
    // - allow the mempool to be enabled before the genesis block is committed?
    //   we could replace `Option` with an enum that has an `AlwaysEnable` variant
    pub debug_enable_at_height: Option<u32>,

    /// Maximum size in bytes of an OP_RETURN script that is considered standard.
    ///
    /// If unset, defaults to [`DEFAULT_MAX_DATACARRIER_BYTES`]. This size includes the OP_RETURN
    /// opcode and pushdata overhead. Matches zcashd's `-datacarriersize` default behavior.
    pub max_datacarrier_bytes: Option<u32>,

    /// The number of pieces a failed admission proposal is split into.
    ///
    /// The mempool checks newly verified transactions together as one block proposal. If that
    /// proposal fails, it is split into this many pieces, which are checked concurrently, until
    /// each failure is attributed to a single transaction. Wider splits find invalid transactions
    /// in fewer rounds and with less total work, but check more proposals at once. The pieces
    /// always come from one failed proposal, so together they never hold more than one block's
    /// worth of new transactions, although each piece also rechecks its own unmined ancestors.
    ///
    /// Values below 2 are treated as 2.
    pub admission_split_width: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // [ZIP-401] Consensus rules:
            //
            // > There MUST be a configuration option mempooltxcostlimit,
            // > which SHOULD default to 80000000.
            // >
            // > There MUST be a configuration option mempoolevictionmemoryminutes,
            // > which SHOULD default to 60 [minutes].
            //
            // [ZIP-401]: https://zips.z.cash/zip-0401#specification
            tx_cost_limit: 80_000_000,
            eviction_memory_time: Duration::from_secs(60 * 60),

            debug_enable_at_height: None,

            max_datacarrier_bytes: Some(DEFAULT_MAX_DATACARRIER_BYTES),

            admission_split_width: DEFAULT_ADMISSION_SPLIT_WIDTH,
        }
    }
}

/// Default maximum size of data carrier scripts (OP_RETURN), in bytes.
///
/// Equivalent to zcashd's `MAX_OP_RETURN_RELAY`:
/// <https://github.com/zcash/zcash/blob/v6.10.0/src/script/standard.h#L22-L26>
pub const DEFAULT_MAX_DATACARRIER_BYTES: u32 = 83;

/// Default number of concurrently checked pieces a failed admission proposal is split into.
pub const DEFAULT_ADMISSION_SPLIT_WIDTH: usize = 4;
