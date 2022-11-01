//! Mining config

use serde::{Deserialize, Serialize};

use zebra_chain::transparent::Address;

/// Mining configuration section.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The address used for miner payouts.
    /// Currently, Zebra only supports transparent addresses.
    ///
    /// Zebra sends mining fees and miner rewards to this address in the
    /// `getblocktemplate` RPC coinbase transaction.
    pub miner_address: Option<Address>,
}
