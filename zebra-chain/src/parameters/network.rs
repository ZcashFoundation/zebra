#[cfg(test)]
use proptest_derive::Arbitrary;

/// An enum describing the possible network choices.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(Arbitrary))]
pub enum Network {
    /// The production mainnet.
    Mainnet,
    /// The testnet.
    Testnet,
}

impl Default for Network {
    fn default() -> Self {
        Network::Mainnet
    }
}
