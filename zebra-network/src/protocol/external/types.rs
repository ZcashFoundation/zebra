use hex;
use std::fmt;

#[cfg(test)]
use proptest_derive::Arbitrary;

/// A magic number identifying the network.
#[derive(Copy, Clone, Eq, PartialEq)]
#[cfg_attr(test, derive(Arbitrary))]
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

/// A random value to add to the seed value in a hash function.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Tweak(pub u32);

impl Default for Tweak {
    fn default() -> Self {
        use rand::{thread_rng, Rng};
        Self(thread_rng().gen())
    }
}

/// A Bloom filter consisting of a bit field of arbitrary byte-aligned
/// size, maximum size is 36,000 bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Filter(pub Vec<u8>);

#[cfg(test)]
mod tests {

    use crate::constants::magics;

    #[test]
    fn magic_debug() {
        assert_eq!(format!("{:?}", magics::MAINNET), "Magic(\"24e92764\")");
        assert_eq!(format!("{:?}", magics::TESTNET), "Magic(\"fa1af9bf\")");
    }
}

#[cfg(test)]
mod proptest {

    use hex;

    use proptest::prelude::*;

    use super::Magic;

    // impl Arbitrary for Magic {
    //     type Parameters = ();

    //     fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
    //         Magic
    //     }

    //     type Strategy = BoxedStrategy<Self>;
    // }

    proptest! {

        #[test]
        fn proptest_magic_from_array(data in any::<[u8; 4]>()) {
            assert_eq!(format!("{:?}", Magic(data)), format!("Magic({:x?})", hex::encode(data)));
        }
    }
}
