use crate::{constants::magics, protocol::external::types::Magic};

/// An enum describing the possible network choices.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Network {
    /// The production mainnet.
    Mainnet,
    /// The testnet.
    Testnet,
}

impl Network {
    /// Get the magic value associated to this `Network`.
    pub fn magic(&self) -> Magic {
        match self {
            Network::Mainnet => magics::MAINNET,
            Network::Testnet => magics::TESTNET,
        }
    }
}
