use std::{convert::From, fmt};

use crate::{block::Height, parameters::NetworkUpgrade::Canopy};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// An enum describing the possible network choices.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum Network {
    /// The production mainnet.
    Mainnet,
    /// The testnet.
    Testnet,
}

impl From<&Network> for &'static str {
    fn from(network: &Network) -> &'static str {
        match network {
            Network::Mainnet => "Mainnet",
            Network::Testnet => "Testnet",
        }
    }
}

impl From<Network> for &'static str {
    fn from(network: Network) -> &'static str {
        (&network).into()
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.into())
    }
}

impl Network {
    /// Get the default port associated to this network.
    pub fn default_port(&self) -> u16 {
        match self {
            Network::Mainnet => 8233,
            Network::Testnet => 18233,
        }
    }

    /// Get the minimum mandatory checkpoint for this network.
    pub fn mandatory_checkpoint_height(&self) -> Height {
        // Currently this is Canopy for both networks.
        Canopy
            .activation_height(*self)
            .expect("Canopy activation height must be present for both networks")
    }
}

impl Default for Network {
    fn default() -> Self {
        Network::Mainnet
    }
}
