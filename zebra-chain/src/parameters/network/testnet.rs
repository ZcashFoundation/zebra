//! Types and implementation for Testnet consensus parameters
use std::collections::BTreeMap;

use crate::{
    block::Height,
    parameters::{
        network_upgrade::TESTNET_ACTIVATION_HEIGHTS, Network, NetworkUpgrade,
        NETWORK_UPGRADES_IN_ORDER,
    },
};

/// Configurable activation heights for Regtest and configured Testnets.
#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct ConfiguredActivationHeights {
    /// Activation height for `BeforeOverwinter` network upgrade.
    pub before_overwinter: Option<u32>,
    /// Activation height for `Overwinter` network upgrade.
    pub overwinter: Option<u32>,
    /// Activation height for `Sapling` network upgrade.
    pub sapling: Option<u32>,
    /// Activation height for `Blossom` network upgrade.
    pub blossom: Option<u32>,
    /// Activation height for `Heartwood` network upgrade.
    pub heartwood: Option<u32>,
    /// Activation height for `Canopy` network upgrade.
    pub canopy: Option<u32>,
    /// Activation height for `NU5` network upgrade.
    #[serde(rename = "NU5")]
    pub nu5: Option<u32>,
}

/// Builder for the [`Parameters`] struct.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ParametersBuilder {
    /// The network upgrade activation heights for this network, see [`Parameters::activation_heights`] for more details.
    activation_heights: BTreeMap<Height, NetworkUpgrade>,
}

impl Default for ParametersBuilder {
    fn default() -> Self {
        Self {
            // # Correctness
            //
            // `Genesis` network upgrade activation height must always be 0
            activation_heights: [
                (Height(0), NetworkUpgrade::Genesis),
                // TODO: Find out if `BeforeOverwinter` must always be active at Height(1), remove it here if it's not required.
                (Height(1), NetworkUpgrade::BeforeOverwinter),
            ]
            .into_iter()
            .collect(),
        }
    }
}

impl ParametersBuilder {
    /// Checks that the provided network upgrade activation heights are in the correct order, then
    /// sets them as the new network upgrade activation heights.
    pub fn activation_heights(
        mut self,
        ConfiguredActivationHeights {
            before_overwinter,
            overwinter,
            sapling,
            blossom,
            heartwood,
            canopy,
            nu5,
        }: ConfiguredActivationHeights,
    ) -> Self {
        use NetworkUpgrade::*;

        // # Correctness
        //
        // These must be in order so that later network upgrades overwrite prior ones
        // if multiple network upgrades are configured with the same activation height.
        let activation_heights: BTreeMap<_, _> = before_overwinter
            .into_iter()
            .map(|h| (h, BeforeOverwinter))
            .chain(overwinter.into_iter().map(|h| (h, Overwinter)))
            .chain(sapling.into_iter().map(|h| (h, Sapling)))
            .chain(blossom.into_iter().map(|h| (h, Blossom)))
            .chain(heartwood.into_iter().map(|h| (h, Heartwood)))
            .chain(canopy.into_iter().map(|h| (h, Canopy)))
            .chain(nu5.into_iter().map(|h| (h, Nu5))) // TODO: Find out if `BeforeOverwinter` is required at Height(1), remove this filter if it's not required to be at Height(1)
            .filter(|&(_, nu)| nu != NetworkUpgrade::BeforeOverwinter)
            .map(|(h, nu)| (h.try_into().expect("activation height must be valid"), nu))
            .collect();

        let network_upgrades: Vec<_> = activation_heights.iter().map(|(_h, &nu)| nu).collect();

        // Check that the provided network upgrade activation heights are in the same order by height as the default testnet activation heights
        let mut activation_heights_iter = activation_heights.iter();
        for expected_network_upgrade in NETWORK_UPGRADES_IN_ORDER {
            if !network_upgrades.contains(&expected_network_upgrade) {
                continue;
            } else if let Some((&height, &network_upgrade)) = activation_heights_iter.next() {
                assert_ne!(
                    height,
                    Height(0),
                    "Height(0) is reserved for the `Genesis` upgrade"
                );

                assert!(
                    network_upgrade == expected_network_upgrade,
                    "network upgrades must be activated in order, the correct order is {NETWORK_UPGRADES_IN_ORDER:?}"
                );
            }
        }

        // # Correctness
        //
        // Height(0) must be reserved for the `NetworkUpgrade::Genesis`.
        self.activation_heights.split_off(&Height(2));
        self.activation_heights.extend(activation_heights);

        self
    }

    /// Converts the builder to a [`Parameters`] struct
    pub fn finish(self) -> Parameters {
        let Self { activation_heights } = self;
        Parameters { activation_heights }
    }

    /// Converts the builder to a configured [`Network::Testnet`]
    pub fn to_network(self) -> Network {
        Network::new_configured_testnet(self.finish())
    }
}

/// Network consensus parameters for test networks such as Regtest and the default Testnet.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Parameters {
    /// The network upgrade activation heights for this network.
    ///
    /// Note: This value is ignored by `Network::activation_list()` when `zebra-chain` is
    ///       compiled with the `zebra-test` feature flag AND the `TEST_FAKE_ACTIVATION_HEIGHTS`
    ///       environment variable is set.
    activation_heights: BTreeMap<Height, NetworkUpgrade>,
}

impl Default for Parameters {
    /// Returns an instance of the default public testnet [`Parameters`].
    fn default() -> Self {
        Self {
            activation_heights: TESTNET_ACTIVATION_HEIGHTS.iter().cloned().collect(),
        }
    }
}

impl Parameters {
    /// Creates a new [`ParametersBuilder`].
    pub fn build() -> ParametersBuilder {
        ParametersBuilder::default()
    }

    /// Returns true if the instance of [`Parameters`] represents the default public Testnet.
    pub fn is_default_testnet(&self) -> bool {
        self == &Self::default()
    }

    /// Returns a reference to the network upgrade activation heights
    pub fn activation_heights(&self) -> &BTreeMap<Height, NetworkUpgrade> {
        &self.activation_heights
    }
}
