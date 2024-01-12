//! Mining config

use serde::{Deserialize, Serialize};

use zebra_chain::transparent;

/// Mining configuration section.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The address used for miner payouts.
    /// Zebra currently only supports P2SH and P2PKH transparent addresses.
    ///
    /// Zebra sends mining fees and miner rewards to this address in the
    /// `getblocktemplate` RPC coinbase transaction.
    pub miner_address: Option<transparent::Address>,

    /// Mine blocks using Zebra's internal miner, without an external mining pool or equihash solver.
    ///
    /// This experimental feature is only supported on testnet.
    /// Mainnet miners should use a mining pool with GPUs or ASICs designed for efficient mining.
    ///
    /// The internal miner is off by default.
    #[cfg(feature = "internal-miner")]
    pub internal_miner: bool,

    /// The number of internal miner threads used by Zebra.
    /// These threads are scheduled at low priority.
    ///
    /// The number of threads is limited by the available parallelism reported by the OS.
    /// If the number of threads isn't configured, or can't be detected, Zebra uses one thread.
    /// This is different from Zebra's other parallelism configs, because mining runs constantly and
    /// uses a large amount of memory. (144 MB of RAM and 100% of a core per thread.)
    ///
    /// If the number of threads is set to zero, Zebra disables mining.
    /// This matches `zcashd`'s behaviour, but is different from Zebra's other parallelism configs.
    #[cfg(feature = "internal-miner")]
    pub internal_miner_threads: usize,

    /// Extra data to include in coinbase transaction inputs.
    /// Limited to around 95 bytes by the consensus rules.
    ///
    /// If this string is hex-encoded, it will be hex-decoded into bytes.
    /// Otherwise, it will be UTF-8 encoded into bytes.
    pub extra_coinbase_data: Option<String>,

    /// Should Zebra's block templates try to imitate `zcashd`?
    ///
    /// This developer-only config is not supported for general use.
    pub debug_like_zcashd: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            miner_address: None,
            // For now, act like `zcashd` as much as possible.
            // TODO: do we want to default to v5 transactions and Zebra coinbase data?
            extra_coinbase_data: None,
            debug_like_zcashd: true,
            // TODO: ignore and warn rather than panicking if these fields are in the config,
            //       but the feature isn't enabled.
            #[cfg(feature = "internal-miner")]
            internal_miner: false,
            #[cfg(feature = "internal-miner")]
            internal_miner_threads: 1,
        }
    }
}

impl Config {
    /// Return true if `getblocktemplate-rpcs` rust feature is not turned on, false otherwise.
    ///
    /// This is used to ignore the mining section of the configuration if the feature is not
    /// enabled, allowing us to log a warning when the config found is different from the default.
    pub fn skip_getblocktemplate(&self) -> bool {
        !cfg!(feature = "getblocktemplate-rpcs")
    }

    /// Is the internal miner enabled using at least one thread?
    #[cfg(feature = "internal-miner")]
    pub fn is_internal_miner_enabled(&self) -> bool {
        self.internal_miner && self.internal_miner_threads > 0
    }
}
