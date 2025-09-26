use std::{cmp::max, fmt};

use zebra_chain::{
    block,
    parameters::{
        Network::{self, *},
        NetworkUpgrade::{self, *},
    },
};

use crate::constants::{self, CURRENT_NETWORK_PROTOCOL_VERSION};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

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
        network: &Network,
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
    fn initial_min_for_network(network: &Network) -> Version {
        *constants::INITIAL_MIN_NETWORK_PROTOCOL_VERSION
            .get(&network.kind())
            .expect("We always have a value for testnet or mainnet")
    }

    /// Returns the minimum specified network protocol version for `network` and
    /// `height`.
    ///
    /// This is the minimum peer version when Zebra is close to the current tip.
    fn min_specified_for_height(network: &Network, height: block::Height) -> Version {
        let network_upgrade = NetworkUpgrade::current(network, height);
        Version::min_specified_for_upgrade(network, network_upgrade)
    }

    /// Returns the minimum specified network protocol version for `network` and
    /// `network_upgrade`.
    ///
    /// ## ZIP-253
    ///
    /// > Nodes compatible with a network upgrade activation MUST advertise a network protocol
    /// > version that is greater than or equal to the MIN_NETWORK_PROTOCOL_VERSION for that
    /// > activation.
    ///
    /// <https://zips.z.cash/zip-0253>
    ///
    /// ### Notes
    ///
    /// - The citation above is a generalization of a statement in ZIP-253 since that ZIP is
    ///   concerned only with NU6 on Mainnet and Testnet.
    pub(crate) fn min_specified_for_upgrade(
        network: &Network,
        network_upgrade: NetworkUpgrade,
    ) -> Version {
        Version(match (network, network_upgrade) {
            (_, Genesis) | (_, BeforeOverwinter) => 170_002,
            (Testnet(params), Overwinter) if params.is_default_testnet() => 170_003,
            (Mainnet, Overwinter) => 170_005,
            (Testnet(params), Sapling) if params.is_default_testnet() => 170_007,
            (Testnet(params), Sapling) if params.is_regtest() => 170_006,
            (Mainnet, Sapling) => 170_007,
            (Testnet(params), Blossom) if params.is_default_testnet() || params.is_regtest() => {
                170_008
            }
            (Mainnet, Blossom) => 170_009,
            (Testnet(params), Heartwood) if params.is_default_testnet() || params.is_regtest() => {
                170_010
            }
            (Mainnet, Heartwood) => 170_011,
            (Testnet(params), Canopy) if params.is_default_testnet() || params.is_regtest() => {
                170_012
            }
            (Mainnet, Canopy) => 170_013,
            (Testnet(params), Nu5) if params.is_default_testnet() || params.is_regtest() => 170_050,
            (Mainnet, Nu5) => 170_100,
            (Testnet(params), Nu6) if params.is_default_testnet() || params.is_regtest() => 170_110,
            (Mainnet, Nu6) => 170_120,
            (Testnet(params), Nu6_1) if params.is_default_testnet() || params.is_regtest() => {
                170_130
            }
            (Mainnet, Nu6_1) => 170_140,
            (Testnet(params), Nu7) if params.is_default_testnet() || params.is_regtest() => 170_150,
            (Mainnet, Nu7) => 170_160,

            // It should be fine to reject peers with earlier network protocol versions on custom testnets for now.
            (Testnet(_), _) => CURRENT_NETWORK_PROTOCOL_VERSION.0,

            #[cfg(zcash_unstable = "zfuture")]
            (Mainnet, ZFuture) => {
                panic!("ZFuture network upgrade should not be active on Mainnet")
            }
        })
    }
}

bitflags! {
    /// A bitflag describing services advertised by a node in the network.
    ///
    /// Note that bits 24-31 are reserved for temporary experiments; other
    /// service bits should be allocated via the ZIP process.
    #[derive(Copy, Clone, Debug, Default, Hash, PartialEq, Eq, PartialOrd, Ord)]
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
mod test {
    use super::*;

    #[test]
    fn version_extremes_mainnet() {
        version_extremes(&Mainnet)
    }

    #[test]
    fn version_extremes_testnet() {
        version_extremes(&Network::new_default_testnet())
    }

    /// Test the min_specified_for_upgrade and min_specified_for_height functions for `network` with
    /// extreme values.
    fn version_extremes(network: &Network) {
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
        version_consistent(&Mainnet)
    }

    #[test]
    fn version_consistent_testnet() {
        version_consistent(&Network::new_default_testnet())
    }

    /// Check that the min_specified_for_upgrade and min_specified_for_height functions
    /// are consistent for `network`.
    fn version_consistent(network: &Network) {
        let _init_guard = zebra_test::init();

        let highest_network_upgrade = NetworkUpgrade::current(network, block::Height::MAX);
        assert!(
            matches!(highest_network_upgrade, Nu6 | Nu6_1 | Nu7),
            "expected coverage of all network upgrades: \
            add the new network upgrade to the list in this test"
        );

        for &network_upgrade in &[
            BeforeOverwinter,
            Overwinter,
            Sapling,
            Blossom,
            Heartwood,
            Canopy,
            Nu5,
            Nu6,
            Nu6_1,
            Nu7,
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
