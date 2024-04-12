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
            // TODO: Find out if `BeforeOverwinter` must always be active at height 1
            activation_heights: [
                (Height(0), NetworkUpgrade::Genesis),
                (Height(1), NetworkUpgrade::BeforeOverwinter),
            ]
            .into_iter()
            .collect(),
        }
    }
}

impl ParametersBuilder {
    /// Extends network upgrade activation heights with the provided activation heights.
    pub fn activation_heights(
        mut self,
        new_activation_heights: Vec<(Height, NetworkUpgrade)>,
    ) -> Self {
        let new_activation_heights: BTreeMap<_, _> =
            new_activation_heights.iter().cloned().collect();

        // # Correctness
        //
        // Height(0) must be reserved for the `NetworkUpgrade::Genesis`.
        self.activation_heights
            .extend(new_activation_heights.range(Height(1)..));
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
