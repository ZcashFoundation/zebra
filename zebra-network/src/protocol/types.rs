//! Newtype wrappers assigning semantic meaning to primitive types.

use hex;
use std::fmt;

/// A magic number identifying the network.
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct Magic(pub [u8; 4]);

impl fmt::Debug for Magic {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Magic").field(&hex::encode(&self.0)).finish()
    }
}

/// A protocol version number.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Version(pub u32);

bitflags! {
    /// A bitflag describing services advertised by a node in the network.
    ///
    /// Note that bits 24-31 are reserved for temporary experiments; other
    /// service bits should be allocated via the ZIP process.
    #[derive(Default)]
    pub struct PeerServices: u64 {
        /// NODE_NETWORK means that the node is a full node capable of serving
        /// blocks, as opposed to a light client that makes network requests but
        /// does not provide network services.
        const NODE_NETWORK = (1 << 0);
        /// NODE_BLOOM means that the node supports bloom-filtered connections.
        const NODE_BLOOM = (1 << 2);
    }
}

/// A nonce used in the networking layer to identify messages.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Nonce(pub u64);

impl Default for Nonce {
    fn default() -> Self {
        use rand::{thread_rng, Rng};
        Self(thread_rng().gen())
    }
}

#[cfg(test)]
mod tests {

    use crate::constants::magics;

    #[test]
    fn magic_debug() {
        assert_eq!(format!("{:?}", magics::MAINNET), "Magic(\"24e92764\")");
        assert_eq!(format!("{:?}", magics::TESTNET), "Magic(\"fa1af9bf\")");
    }
}
