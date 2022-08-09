use std::{cmp::max, fmt};

use zebra_chain::{
    block,
    parameters::{
        Network::{self, *},
        NetworkUpgrade::{self, *},
    },
};

use crate::constants::{self, magics};

#[cfg(any(test, feature = "proptest-impl"))]
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
pub struct Version(pub u32);

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_string())
    }
}

impl Version {
    /// Returns the minimum remote node network protocol version for `network` and
    /// `height`. Zebra disconnects from peers with lower versions.
    ///
    /// # Panics
    ///
    /// If we are incompatible with our own minimum remote protocol version.
    pub fn min_remote_for_height(
        network: Network,
        height: impl Into<Option<block::Height>>,
    ) -> Version {
        let height = height.into().unwrap_or(block::Height(0));
        let min_spec = Version::min_specified_for_height(network, height);

        // shut down if our own version is too old
        assert!(
            constants::CURRENT_NETWORK_PROTOCOL_VERSION >= min_spec,
            "Zebra does not implement the minimum specified {:?} protocol version for {:?} at {:?}",
            NetworkUpgrade::current(network, height),
            network,
            height,
        );

        max(min_spec, Version::initial_min_for_network(network))
    }

    /// Returns the minimum supported network protocol version for `network`.
    ///
    /// This is the minimum peer version when Zebra is significantly behind current tip:
    /// - during the initial block download,
    /// - after Zebra restarts, and
    /// - after Zebra's local network is slow or shut down.
    fn initial_min_for_network(network: Network) -> Version {
        *constants::INITIAL_MIN_NETWORK_PROTOCOL_VERSION
            .get(&network)
            .expect("We always have a value for testnet or mainnet")
    }

    /// Returns the minimum specified network protocol version for `network` and
    /// `height`.
    ///
    /// This is the minimum peer version when Zebra is close to the current tip.
    fn min_specified_for_height(network: Network, height: block::Height) -> Version {
        let network_upgrade = NetworkUpgrade::current(network, height);
        Version::min_specified_for_upgrade(network, network_upgrade)
    }

    /// Returns the minimum specified network protocol version for `network` and
    /// `network_upgrade`.
    pub(crate) fn min_specified_for_upgrade(
        network: Network,
        network_upgrade: NetworkUpgrade,
    ) -> Version {
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
            (Testnet, Nu5) => 170_050,
            (Mainnet, Nu5) => 170_100,
        })
    }
}

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
        let _init_guard = zebra_test::init();

        assert_eq!(format!("{:?}", magics::MAINNET), "Magic(\"24e92764\")");
        assert_eq!(format!("{:?}", magics::TESTNET), "Magic(\"fa1af9bf\")");
    }

    proptest! {

        #[test]
        fn proptest_magic_from_array(data in any::<[u8; 4]>()) {
            let _init_guard = zebra_test::init();

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

    /// Test the min_specified_for_upgrade and min_specified_for_height functions for `network` with
    /// extreme values.
    fn version_extremes(network: Network) {
        let _init_guard = zebra_test::init();

        assert_eq!(
            Version::min_specified_for_height(network, block::Height(0)),
            Version::min_specified_for_upgrade(network, BeforeOverwinter),
        );

        // We assume that the last version we know about continues forever
        // (even if we suspect that won't be true)
        assert_ne!(
            Version::min_specified_for_height(network, block::Height::MAX),
            Version::min_specified_for_upgrade(network, BeforeOverwinter),
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

    /// Check that the min_specified_for_upgrade and min_specified_for_height functions
    /// are consistent for `network`.
    fn version_consistent(network: Network) {
        let _init_guard = zebra_test::init();

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
                    Version::min_specified_for_upgrade(network, network_upgrade),
                    Version::min_specified_for_height(network, height)
                );
            }
        }
    }
}
