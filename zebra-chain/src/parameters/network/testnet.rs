//! Types and implementation for Testnet consensus parameters
use std::collections::BTreeMap;

use crate::{
    block::Height,
    parameters::{network_upgrade::TESTNET_ACTIVATION_HEIGHTS, NetworkUpgrade},
};

/// Network consensus parameters for test networks such as Regtest and the default Testnet.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Parameters {
    /// The network upgrade activation heights for this network.
    ///
    /// Note: This value is ignored by [`Network::activation_list()`] when `zebra-chain` is
    ///       compiled with the `zebra-test` feature flag AND the `TEST_FAKE_ACTIVATION_HEIGHTS`
    ///       environment variable is set.
    pub activation_heights: BTreeMap<Height, NetworkUpgrade>,
}

impl Default for Parameters {
    /// Returns an instance of the default public testnet [`NetworkParameters`].
    fn default() -> Self {
        Self {
            activation_heights: TESTNET_ACTIVATION_HEIGHTS.iter().cloned().collect(),
        }
    }
}

impl Parameters {
    /// Returns true if the instance of [`NetworkParameters`] represents the default public Testnet.
    pub fn is_default_testnet(&self) -> bool {
        self == &Self::default()
    }
}
