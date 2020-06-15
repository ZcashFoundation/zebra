//! Blockchain-related datastructures for Zebra. 🦓

#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_chain")]
#![deny(missing_docs)]

#[macro_use]
extern crate serde;

mod merkle_tree;
mod serde_helpers;
mod sha256d_writer;

pub mod addresses;
pub mod block;
pub mod equihash_solution;
pub mod keys;
pub mod note_commitment_tree;
pub mod notes;
pub mod proofs;
pub mod serialization;
pub mod transaction;
pub mod types;

pub use ed25519_zebra;
pub use redjubjub;

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
