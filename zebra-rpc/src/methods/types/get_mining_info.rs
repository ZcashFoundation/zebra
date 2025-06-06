//! Response type for the `getmininginfo` RPC.

use derive_getters::Getters;
use derive_new::new;
use zebra_chain::parameters::Network;

/// Response to a `getmininginfo` RPC request.
#[derive(
    Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Getters, new,
)]
pub struct Response {
    /// The current tip height.
    #[serde(rename = "blocks")]
    tip_height: u32,

    /// The size of the last mined block if any.
    #[serde(rename = "currentblocksize", skip_serializing_if = "Option::is_none")]
    #[getter(copy)]
    current_block_size: Option<usize>,

    /// The number of transactions in the last mined block if any.
    #[serde(rename = "currentblocktx", skip_serializing_if = "Option::is_none")]
    #[getter(copy)]
    current_block_tx: Option<usize>,

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
    pub(crate) fn new_internal(
        tip_height: u32,
        current_block_size: Option<usize>,
        current_block_tx: Option<usize>,
        network: Network,
        networksolps: u64,
    ) -> Self {
        Self {
            tip_height,
            current_block_size,
            current_block_tx,
            networksolps,
            networkhashps: networksolps,
            chain: network.bip70_network_name(),
            testnet: network.is_a_test_network(),
        }
    }
}
