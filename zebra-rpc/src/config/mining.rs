//! Mining config

use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};

use zcash_address::ZcashAddress;

/// Mining configuration section.
#[serde_as]
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Address for receiving miner subsidy and tx fees.
    ///
    /// Used in coinbase tx constructed in `getblocktemplate` RPC.
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub miner_address: Option<ZcashAddress>,

    /// Optional data that Zebra will include in the transparent input of a coinbase transaction.
    /// Limited to 94 bytes.
    ///
    /// If this string is hex-encoded, it will be hex-decoded into bytes. Otherwise, it will be
    /// UTF-8 encoded into bytes.
    pub miner_data: Option<String>,

    /// Optional shielded memo that Zebra will include in the output of a shielded coinbase
    /// transaction. Limited to 512 bytes.
    ///
    /// Applies only if [`Self::miner_address`] contains a shielded component.
    pub miner_memo: Option<String>,

    /// Mine blocks using Zebra's internal miner, without an external mining pool or equihash solver.
    ///
    /// This experimental feature is only supported on regtest as it uses null solutions and skips checking
    /// for a valid Proof of Work.
    ///
    /// The internal miner is off by default.
    #[serde(default)]
    pub internal_miner: bool,
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
