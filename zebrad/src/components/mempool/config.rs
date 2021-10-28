//! User-configurable mempool parameters.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Mempool configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// If the state's best chain tip has reached this height, always enable the mempool,
    /// regardless of Zebra's sync status.
    ///
    /// Set to `None` by default: Zebra always checks the sync status before enabling the mempool.
    //
    // TODO:
    // - allow the mempool to be enabled before the genesis block is committed?
    //   we could replace `Option` with an enum that has an `AlwaysEnable` variant
    // - move debug configs last (needs #2847)
    pub debug_enable_at_height: Option<u32>,

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
    /// This limits the maximum amount of time evicted transaction IDs stay in the mempool rejection list.
    /// Transactions are randomly evicted from the mempool when the mempool reaches [`tx_cost_limit`].
    ///
    /// (Transactions can also be rejected by the mempool for other reasons.
    /// Different rejection reasons can have different age limits.)
    ///
    /// This corresponds to `mempoolevictionmemoryminutes` from
    /// [ZIP-401](https://zips.z.cash/zip-0401#specification).
    //
    // Note: Durations become a TOML table, so they must be the final item in the config
    //       We'll replace them with a more user-friendly format in #2847
    pub eviction_memory_time: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            /// Consensus rules:
            ///
            /// > There MUST be a configuration option mempooltxcostlimit,
            /// > which SHOULD default to 80000000.
            /// >
            /// > There MUST be a configuration option mempoolevictionmemoryminutes,
            /// > which SHOULD default to 60 [minutes].
            ///
            /// https://zips.z.cash/zip-0401#specification
            tx_cost_limit: 80_000_000,
            eviction_memory_time: Duration::from_secs(60 * 60),

            debug_enable_at_height: None,
        }
    }
}
