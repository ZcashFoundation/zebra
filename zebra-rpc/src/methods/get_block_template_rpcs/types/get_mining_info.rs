//! Response type for the `getmininginfo` RPC.

use zebra_chain::parameters::Network;

/// Response to a `getmininginfo` RPC request.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct Response {
    /// The estimated network solution rate in Sol/s.
    networksolps: u64,

    /// The estimated network solution rate in Sol/s.
    networkhashps: u64,

    /// Current network name as defined in BIP70 (main, test, regtest)
    chain: String,

    /// If using testnet or not
    testnet: bool,
}

impl Response {
    /// Creates a new `getmininginfo` response
    pub fn new(network: Network, networksolps: u64) -> Self {
        Self {
            networksolps,
            networkhashps: networksolps,
            chain: network.bip70_network_name(),
            testnet: network.is_a_test_network(),
        }
    }
}
