//! Types and implementation for Testnet consensus parameters
use std::{collections::BTreeMap, fmt};

use zcash_primitives::constants as zp_constants;

use crate::{
    block::Height,
    parameters::{
        network_upgrade::TESTNET_ACTIVATION_HEIGHTS, Network, NetworkUpgrade,
        NETWORK_UPGRADES_IN_ORDER,
    },
};

/// Reserved network names that should not be allowed for configured Testnets.
pub const RESERVED_NETWORK_NAMES: [&str; 6] = [
    "Mainnet",
    "Testnet",
    "Regtest",
    "MainnetKind",
    "TestnetKind",
    "RegtestKind",
];

/// Maximum length for a configured network name.
pub const MAX_NETWORK_NAME_LENGTH: usize = 30;

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
    /// The name of this network to be used by the `Display` trait impl.
    network_name: String,
    /// The network upgrade activation heights for this network, see [`Parameters::activation_heights`] for more details.
    activation_heights: BTreeMap<Height, NetworkUpgrade>,
    /// Sapling extended spending key human-readable prefix for this network
    hrp_sapling_extended_spending_key: String,
    /// Sapling extended full viewing key human-readable prefix for this network
    hrp_sapling_extended_full_viewing_key: String,
    /// Sapling payment address human-readable prefix for this network
    hrp_sapling_payment_address: String,
}

impl Default for ParametersBuilder {
    fn default() -> Self {
        Self {
            network_name: "UnknownTestnet".to_string(),
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
            hrp_sapling_extended_spending_key:
                zp_constants::testnet::HRP_SAPLING_EXTENDED_SPENDING_KEY.to_string(),
            hrp_sapling_extended_full_viewing_key:
                zp_constants::testnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY.to_string(),
            hrp_sapling_payment_address: zp_constants::testnet::HRP_SAPLING_PAYMENT_ADDRESS
                .to_string(),
        }
    }
}

impl ParametersBuilder {
    /// Sets the network name to be used in the [`Parameters`] being built.
    pub fn with_network_name(mut self, network_name: impl fmt::Display) -> Self {
        self.network_name = network_name.to_string();

        assert!(
            !RESERVED_NETWORK_NAMES.contains(&self.network_name.as_str()),
            "cannot use reserved network name '{network_name}' as configured Testnet name, reserved names: {RESERVED_NETWORK_NAMES:?}"
        );

        assert!(
            self.network_name.len() <= MAX_NETWORK_NAME_LENGTH,
            "network name {network_name} is too long, must be {MAX_NETWORK_NAME_LENGTH} characters or less"
        );

        assert!(
            self.network_name
                .chars()
                .all(|x| x.is_alphanumeric() || x == '_'),
            "network name must include only alphanumeric characters or '_'"
        );

        self
    }

    /// Checks that the provided network upgrade activation heights are in the correct order, then
    /// sets them as the new network upgrade activation heights.
    pub fn with_activation_heights(
        mut self,
        ConfiguredActivationHeights {
            // TODO: Find out if `BeforeOverwinter` is required at Height(1), allow for
            //       configuring its activation height if it's not required to be at Height(1)
            before_overwinter: _,
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
        let activation_heights: BTreeMap<_, _> = overwinter
            .into_iter()
            .map(|h| (h, Overwinter))
            .chain(sapling.into_iter().map(|h| (h, Sapling)))
            .chain(blossom.into_iter().map(|h| (h, Blossom)))
            .chain(heartwood.into_iter().map(|h| (h, Heartwood)))
            .chain(canopy.into_iter().map(|h| (h, Canopy)))
            .chain(nu5.into_iter().map(|h| (h, Nu5)))
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
        let Self {
            network_name,
            activation_heights,
            hrp_sapling_extended_spending_key,
            hrp_sapling_extended_full_viewing_key,
            hrp_sapling_payment_address,
        } = self;
        Parameters {
            network_name,
            activation_heights,
            hrp_sapling_extended_spending_key,
            hrp_sapling_extended_full_viewing_key,
            hrp_sapling_payment_address,
        }
    }

    /// Converts the builder to a configured [`Network::Testnet`]
    pub fn to_network(self) -> Network {
        Network::new_configured_testnet(self.finish())
    }
}

/// Network consensus parameters for test networks such as Regtest and the default Testnet.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Parameters {
    /// The name of this network to be used by the `Display` trait impl.
    network_name: String,
    /// The network upgrade activation heights for this network.
    ///
    /// Note: This value is ignored by `Network::activation_list()` when `zebra-chain` is
    ///       compiled with the `zebra-test` feature flag AND the `TEST_FAKE_ACTIVATION_HEIGHTS`
    ///       environment variable is set.
    activation_heights: BTreeMap<Height, NetworkUpgrade>,
    /// Sapling extended spending key human-readable prefix for this network
    hrp_sapling_extended_spending_key: String,
    /// Sapling extended full viewing key human-readable prefix for this network
    hrp_sapling_extended_full_viewing_key: String,
    /// Sapling payment address human-readable prefix for this network
    hrp_sapling_payment_address: String,
}

impl Default for Parameters {
    /// Returns an instance of the default public testnet [`Parameters`].
    fn default() -> Self {
        Self {
            network_name: "Testnet".to_string(),
            activation_heights: TESTNET_ACTIVATION_HEIGHTS.iter().cloned().collect(),
            ..Self::build().finish()
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

    /// Returns the network upgrade activation heights
    pub fn network_name(&self) -> &str {
        &self.network_name
    }

    /// Returns the network upgrade activation heights
    pub fn activation_heights(&self) -> &BTreeMap<Height, NetworkUpgrade> {
        &self.activation_heights
    }

    /// Returns the `hrp_sapling_extended_spending_key` field
    pub fn hrp_sapling_extended_spending_key(&self) -> &str {
        &self.hrp_sapling_extended_spending_key
    }

    /// Returns the `hrp_sapling_extended_full_viewing_key` field
    pub fn hrp_sapling_extended_full_viewing_key(&self) -> &str {
        &self.hrp_sapling_extended_full_viewing_key
    }

    /// Returns the `hrp_sapling_payment_address` field
    pub fn hrp_sapling_payment_address(&self) -> &str {
        &self.hrp_sapling_payment_address
    }
}
