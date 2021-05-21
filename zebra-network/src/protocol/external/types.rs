#![allow(clippy::unit_arg)]

use crate::constants::magics;

use std::fmt;

use zebra_chain::{
    block,
    parameters::{
        Network::{self, *},
        NetworkUpgrade::{self, *},
    },
};

#[cfg(test)]
use proptest_derive::Arbitrary;

/// A magic number identifying the network.
#[derive(Copy, Clone, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Magic(pub [u8; 4]);

impl fmt::Debug for Magic {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Magic").field(&hex::encode(&self.0)).finish()
    }
}

impl From<Network> for Magic {
    /// Get the magic value associated to this `Network`.
    fn from(network: Network) -> Self {
        match network {
            Network::Mainnet => magics::MAINNET,
            Network::Testnet => magics::TESTNET,
        }
    }
}

/// A protocol version number.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Version(pub u32);

impl Version {
    /// Returns the minimum network protocol version for `network` and
    /// `network_upgrade`.
    pub fn min_for_upgrade(network: Network, network_upgrade: NetworkUpgrade) -> Self {
        // TODO: Should we reject earlier protocol versions during our initial
        //       sync? zcashd accepts 170_002 or later during its initial sync.
        Version(match (network, network_upgrade) {
            (_, Genesis) | (_, BeforeOverwinter) => 170_002,
            (Testnet, Overwinter) => 170_003,
            (Mainnet, Overwinter) => 170_005,
            (_, Sapling) => 170_007,
            (Testnet, Blossom) => 170_008,
            (Mainnet, Blossom) => 170_009,
            (Testnet, Heartwood) => 170_010,
            (Mainnet, Heartwood) => 170_011,
            (Testnet, Canopy) => 170_012,
            (Mainnet, Canopy) => 170_013,
            (Testnet, Nu5) => 170_014,
            (Mainnet, Nu5) => 170_015,
        })
    }

    /// Returns the current minimum protocol version for `network` and `height`.
    ///
    /// Returns None if the network has no branch id at this height.
    #[allow(dead_code)]
    pub fn current_min(network: Network, height: block::Height) -> Version {
        let network_upgrade = NetworkUpgrade::current(network, height);
        Version::min_for_upgrade(network, network_upgrade)
    }
}

bitflags! {
    /// A bitflag describing services advertised by a node in the network.
    ///
    /// Note that bits 24-31 are reserved for temporary experiments; other
    /// service bits should be allocated via the ZIP process.
    #[derive(Default)]
    #[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
    pub struct PeerServices: u64 {
        /// NODE_NETWORK means that the node is a full node capable of serving
        /// blocks, as opposed to a light client that makes network requests but
        /// does not provide network services.
        const NODE_NETWORK = 1;
    }
}

/// A nonce used in the networking layer to identify messages.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Nonce(pub u64);

impl Default for Nonce {
    fn default() -> Self {
        use rand::{thread_rng, Rng};
        Self(thread_rng().gen())
    }
}

/// A random value to add to the seed value in a hash function.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
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
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Filter(pub Vec<u8>);

#[cfg(test)]
mod proptest {

    use proptest::prelude::*;

    use super::Magic;

    use crate::constants::magics;

    #[test]
    fn magic_debug() {
        zebra_test::init();

        assert_eq!(format!("{:?}", magics::MAINNET), "Magic(\"24e92764\")");
        assert_eq!(format!("{:?}", magics::TESTNET), "Magic(\"fa1af9bf\")");
    }

    proptest! {

        #[test]
        fn proptest_magic_from_array(data in any::<[u8; 4]>()) {
            zebra_test::init();

            assert_eq!(format!("{:?}", Magic(data)), format!("Magic({:x?})", hex::encode(data)));
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn version_extremes_mainnet() {
        version_extremes(Mainnet)
    }

    #[test]
    fn version_extremes_testnet() {
        version_extremes(Testnet)
    }

    /// Test the min_for_upgrade and current_min functions for `network` with
    /// extreme values.
    fn version_extremes(network: Network) {
        zebra_test::init();

        assert_eq!(
            Version::current_min(network, block::Height(0)),
            Version::min_for_upgrade(network, BeforeOverwinter),
        );

        // We assume that the last version we know about continues forever
        // (even if we suspect that won't be true)
        assert_ne!(
            Version::current_min(network, block::Height::MAX),
            Version::min_for_upgrade(network, BeforeOverwinter),
        );
    }

    #[test]
    fn version_consistent_mainnet() {
        version_consistent(Mainnet)
    }

    #[test]
    fn version_consistent_testnet() {
        version_consistent(Testnet)
    }

    /// Check that the min_for_upgrade and current_min functions
    /// are consistent for `network`.
    fn version_consistent(network: Network) {
        zebra_test::init();

        let highest_network_upgrade = NetworkUpgrade::current(network, block::Height::MAX);
        assert!(highest_network_upgrade == Nu5 || highest_network_upgrade == Canopy,
                "expected coverage of all network upgrades: add the new network upgrade to the list in this test");

        for &network_upgrade in &[
            BeforeOverwinter,
            Overwinter,
            Sapling,
            Blossom,
            Heartwood,
            Canopy,
            Nu5,
        ] {
            let height = network_upgrade.activation_height(network);
            if let Some(height) = height {
                assert_eq!(
                    Version::min_for_upgrade(network, network_upgrade),
                    Version::current_min(network, height)
                );
            }
        }
    }
}
