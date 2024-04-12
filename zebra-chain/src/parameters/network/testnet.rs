//! Types and implementation for Testnet consensus parameters
use std::collections::BTreeMap;

use crate::{
    block::Height,
    parameters::{network_upgrade::TESTNET_ACTIVATION_HEIGHTS, NetworkUpgrade},
};

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
    pub fn activation_heights(mut self, activation_heights: Vec<(Height, NetworkUpgrade)>) -> Self {
        let activation_heights: BTreeMap<_, _> = activation_heights
            .into_iter()
            // TODO: Find out if `BeforeOverwinter` is required at Height(1), remove this filter if it's not required to be at Height(1)
            .filter(|&(_, nu)| nu != NetworkUpgrade::BeforeOverwinter)
            .collect();

        // Check that the provided network upgrade activation heights are in the same order by height as the default testnet activation heights
        let network_upgrades: Vec<_> = activation_heights.iter().map(|(_h, &nu)| nu).collect();
        let mut activation_heights_iter = activation_heights.iter();
        for expected_network_upgrade in TESTNET_ACTIVATION_HEIGHTS.iter().map(|&(_, nu)| nu) {
            if !network_upgrades.contains(&expected_network_upgrade) {
                continue;
            } else if let Some((_h, &network_upgrade)) = activation_heights_iter.next() {
                assert!(
                    network_upgrade == expected_network_upgrade,
                    "network upgrades must be in order"
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
        Self::build()
            .activation_heights(TESTNET_ACTIVATION_HEIGHTS.to_vec())
            .finish()
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
