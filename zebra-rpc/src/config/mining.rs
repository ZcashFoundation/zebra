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

    // TODO: Internal miner config code was removed as part of https://github.com/ZcashFoundation/zebra/issues/8180
    // Find the removed code at https://github.com/ZcashFoundation/zebra/blob/v1.5.1/zebra-rpc/src/config/mining.rs#L18-L38
    // Restore the code when conditions are met. https://github.com/ZcashFoundation/zebra/issues/8183
    /// Extra data to include in coinbase transaction inputs.
    /// Limited to around 95 bytes by the consensus rules.
    ///
    /// If this string is hex-encoded, it will be hex-decoded into bytes.
    /// Otherwise, it will be UTF-8 encoded into bytes.
    pub extra_coinbase_data: Option<String>,

    /// Should Zebra's block templates try to imitate `zcashd`?
    ///
    /// This developer-only config is not supported for general use.
    /// TODO: remove this option as part of zcashd deprecation
    pub debug_like_zcashd: bool,

    /// Mine blocks using Zebra's internal miner, without an external mining pool or equihash solver.
    ///
    /// This experimental feature is only supported on regtest as it uses null solutions and skips checking
    /// for a valid Proof of Work.
    ///
    /// The internal miner is off by default.
    // TODO: Restore equihash solver and recommend that Mainnet miners should use a mining pool with
    //       GPUs or ASICs designed for efficient mining.
    #[cfg(feature = "internal-miner")]
    pub internal_miner: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            miner_address: None,
            // For now, act like `zcashd` as much as possible.
            // TODO: do we want to default to v5 transactions and Zebra coinbase data?
            extra_coinbase_data: None,
            debug_like_zcashd: true,
            // TODO: Internal miner config code was removed as part of https://github.com/ZcashFoundation/zebra/issues/8180
            // Find the removed code at https://github.com/ZcashFoundation/zebra/blob/v1.5.1/zebra-rpc/src/config/mining.rs#L61-L66
            // Restore the code when conditions are met. https://github.com/ZcashFoundation/zebra/issues/8183
            #[cfg(feature = "internal-miner")]
            internal_miner: false,
        }
    }
}

impl Config {
    /// Is the internal miner enabled using at least one thread?
    #[cfg(feature = "internal-miner")]
    pub fn is_internal_miner_enabled(&self) -> bool {
        // TODO: Changed to return always false so internal miner is never started. Part of https://github.com/ZcashFoundation/zebra/issues/8180
        // Find the removed code at https://github.com/ZcashFoundation/zebra/blob/v1.5.1/zebra-rpc/src/config/mining.rs#L83
        // Restore the code when conditions are met. https://github.com/ZcashFoundation/zebra/issues/8183
        self.internal_miner
    }
}
