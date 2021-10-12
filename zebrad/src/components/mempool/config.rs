//! User-configurable mempool parameters.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Mempool configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    /// The mempool transaction cost limit.
    ///
    /// This limits the total serialized byte size of all transactions in the mempool.
    ///
    /// This corresponds to `mempooltxcostlimit` from [ZIP-401](https://zips.z.cash/zip-0401#specification).
    pub tx_cost_limit: u32,

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
    pub eviction_memory_time: Duration,
}

/// Consensus rules:
///
/// > There MUST be a configuration option mempooltxcostlimit,
/// > which SHOULD default to 80000000.
/// >
/// > There MUST be a configuration option mempoolevictionmemoryminutes,
/// > which SHOULD default to 60 [minutes].
///
/// https://zips.z.cash/zip-0401#specification
impl Default for Config {
    fn default() -> Self {
        Self {
            tx_cost_limit: 80_000_000,
            eviction_memory_time: Duration::from_secs(60 * 60),
        }
    }
}
