//! Response type for the `getmininginfo` RPC.

use zebra_chain::parameters::Network;

/// Response to a `getmininginfo` RPC request.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct Response {
    /// The estimated network solution rate in Sol/s.
    networksolps: u128,

    /// The estimated network solution rate in Sol/s.
    networkhashps: u128,

    /// Current network name as defined in BIP70 (main, test, regtest)
    chain: Network,
}

impl Response {
    pub fn new(chain: Network, networksolps: u128) -> Self {
        Self {
            networksolps,
            networkhashps: networksolps,
            chain,
        }
    }
}
