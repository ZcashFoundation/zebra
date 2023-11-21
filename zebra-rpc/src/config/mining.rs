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
}
