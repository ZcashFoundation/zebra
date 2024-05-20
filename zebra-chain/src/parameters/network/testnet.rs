//! Types and implementation for Testnet consensus parameters
use std::{collections::BTreeMap, fmt};

use zcash_primitives::constants as zp_constants;

use crate::{
    block::{self, Height},
    parameters::{
        constants::{magics, SLOW_START_INTERVAL, SLOW_START_SHIFT},
        network_upgrade::TESTNET_ACTIVATION_HEIGHTS,
        Network, NetworkUpgrade, NETWORK_UPGRADES_IN_ORDER,
    },
    work::difficulty::{ExpandedDifficulty, U256},
};

use super::magic::Magic;

/// The Regtest NU5 activation height in tests
// TODO: Serialize testnet parameters in Config then remove this and use a configured NU5 activation height.
#[cfg(any(test, feature = "proptest-impl"))]
pub const REGTEST_NU5_ACTIVATION_HEIGHT: u32 = 100;

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

/// Maximum length for a configured human-readable prefix.
pub const MAX_HRP_LENGTH: usize = 30;

/// The block hash of the Regtest genesis block, `zcash-cli -regtest getblockhash 0`
const REGTEST_GENESIS_HASH: &str =
    "029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327";

/// The block hash of the Testnet genesis block, `zcash-cli -testnet getblockhash 0`
const TESTNET_GENESIS_HASH: &str =
    "05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38";

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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParametersBuilder {
    /// The name of this network to be used by the `Display` trait impl.
    network_name: String,
    /// The network magic, acts as an identifier for the network.
    network_magic: Magic,
    /// The genesis block hash
    genesis_hash: block::Hash,
    /// The network upgrade activation heights for this network, see [`Parameters::activation_heights`] for more details.
    activation_heights: BTreeMap<Height, NetworkUpgrade>,
    /// Sapling extended spending key human-readable prefix for this network
    hrp_sapling_extended_spending_key: String,
    /// Sapling extended full viewing key human-readable prefix for this network
    hrp_sapling_extended_full_viewing_key: String,
    /// Sapling payment address human-readable prefix for this network
    hrp_sapling_payment_address: String,
    /// Slow start interval for this network
    slow_start_interval: Height,
    /// Target difficulty limit for this network
    target_difficulty_limit: ExpandedDifficulty,
    /// A flag for disabling proof-of-work checks when Zebra is validating blocks
    disable_pow: bool,
}

impl Default for ParametersBuilder {
    /// Creates a [`ParametersBuilder`] with all of the default Testnet parameters except `network_name`.
    fn default() -> Self {
        Self {
            network_name: "UnknownTestnet".to_string(),
            network_magic: magics::TESTNET,
            // # Correctness
            //
            // `Genesis` network upgrade activation height must always be 0
            activation_heights: TESTNET_ACTIVATION_HEIGHTS.iter().cloned().collect(),
            hrp_sapling_extended_spending_key:
                zp_constants::testnet::HRP_SAPLING_EXTENDED_SPENDING_KEY.to_string(),
            hrp_sapling_extended_full_viewing_key:
                zp_constants::testnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY.to_string(),
            hrp_sapling_payment_address: zp_constants::testnet::HRP_SAPLING_PAYMENT_ADDRESS
                .to_string(),
            genesis_hash: TESTNET_GENESIS_HASH
                .parse()
                .expect("hard-coded hash parses"),
            slow_start_interval: SLOW_START_INTERVAL,
            // Testnet PoWLimit is defined as `2^251 - 1` on page 73 of the protocol specification:
            // <https://zips.z.cash/protocol/protocol.pdf>
            //
            // `zcashd` converts the PoWLimit into a compact representation before
            // using it to perform difficulty filter checks.
            //
            // The Zcash specification converts to compact for the default difficulty
            // filter, but not for testnet minimum difficulty blocks. (ZIP 205 and
            // ZIP 208 don't specify this conversion either.) See #1277 for details.
            target_difficulty_limit: ExpandedDifficulty::from((U256::one() << 251) - 1)
                .to_compact()
                .to_expanded()
                .expect("difficulty limits are valid expanded values"),
            disable_pow: false,
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

    /// Sets the network name to be used in the [`Parameters`] being built.
    pub fn with_network_magic(mut self, network_magic: Magic) -> Self {
        assert!(
            [magics::MAINNET, magics::REGTEST]
                .into_iter()
                .all(|reserved_magic| network_magic != reserved_magic),
            "network magic should be distinct from reserved network magics"
        );

        self.network_magic = network_magic;

        self
    }

    /// Checks that the provided Sapling human-readable prefixes (HRPs) are valid and unique, then
    /// sets the Sapling HRPs to be used in the [`Parameters`] being built.
    pub fn with_sapling_hrps(
        mut self,
        hrp_sapling_extended_spending_key: impl fmt::Display,
        hrp_sapling_extended_full_viewing_key: impl fmt::Display,
        hrp_sapling_payment_address: impl fmt::Display,
    ) -> Self {
        self.hrp_sapling_extended_spending_key = hrp_sapling_extended_spending_key.to_string();
        self.hrp_sapling_extended_full_viewing_key =
            hrp_sapling_extended_full_viewing_key.to_string();
        self.hrp_sapling_payment_address = hrp_sapling_payment_address.to_string();

        let sapling_hrps = [
            &self.hrp_sapling_extended_spending_key,
            &self.hrp_sapling_extended_full_viewing_key,
            &self.hrp_sapling_payment_address,
        ];

        for sapling_hrp in sapling_hrps {
            assert!(sapling_hrp.len() <= MAX_HRP_LENGTH, "Sapling human-readable prefix {sapling_hrp} is too long, must be {MAX_HRP_LENGTH} characters or less");
            assert!(
                sapling_hrp.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "human-readable prefixes should contain only lowercase ASCII characters and dashes, hrp: {sapling_hrp}"
            );
            assert_eq!(
                sapling_hrps
                    .iter()
                    .filter(|&&hrp| hrp == sapling_hrp)
                    .count(),
                1,
                "Sapling human-readable prefixes must be unique, repeated Sapling HRP: {sapling_hrp}"
            );
        }

        self
    }

    /// Parses the hex-encoded block hash and sets it as the genesis hash in the [`Parameters`] being built.
    pub fn with_genesis_hash(mut self, genesis_hash: impl fmt::Display) -> Self {
        self.genesis_hash = genesis_hash
            .to_string()
            .parse()
            .expect("configured genesis hash must parse");
        self
    }

    /// Checks that the provided network upgrade activation heights are in the correct order, then
    /// sets them as the new network upgrade activation heights.
    pub fn with_activation_heights(
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
        self.activation_heights.split_off(&Height(1));
        self.activation_heights.extend(activation_heights);

        self
    }

    /// Sets the slow start interval to be used in the [`Parameters`] being built.
    pub fn with_slow_start_interval(mut self, slow_start_interval: Height) -> Self {
        self.slow_start_interval = slow_start_interval;
        self
    }

    /// Sets the target difficulty limit to be used in the [`Parameters`] being built.
    // TODO: Accept a hex-encoded String instead?
    pub fn with_target_difficulty_limit(
        mut self,
        target_difficulty_limit: impl Into<ExpandedDifficulty>,
    ) -> Self {
        self.target_difficulty_limit = target_difficulty_limit
            .into()
            .to_compact()
            .to_expanded()
            .expect("difficulty limits are valid expanded values");
        self
    }

    /// Sets the `disable_pow` flag to be used in the [`Parameters`] being built.
    pub fn with_disable_pow(mut self, disable_pow: bool) -> Self {
        self.disable_pow = disable_pow;
        self
    }

    /// Converts the builder to a [`Parameters`] struct
    pub fn finish(self) -> Parameters {
        let Self {
            network_name,
            network_magic,
            genesis_hash,
            activation_heights,
            hrp_sapling_extended_spending_key,
            hrp_sapling_extended_full_viewing_key,
            hrp_sapling_payment_address,
            slow_start_interval,
            target_difficulty_limit,
            disable_pow,
        } = self;
        Parameters {
            network_name,
            network_magic,
            genesis_hash,
            activation_heights,
            hrp_sapling_extended_spending_key,
            hrp_sapling_extended_full_viewing_key,
            hrp_sapling_payment_address,
            slow_start_interval,
            slow_start_shift: Height(slow_start_interval.0 / 2),
            target_difficulty_limit,
            disable_pow,
        }
    }

    /// Converts the builder to a configured [`Network::Testnet`]
    pub fn to_network(self) -> Network {
        Network::new_configured_testnet(self.finish())
    }
}

/// Network consensus parameters for test networks such as Regtest and the default Testnet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parameters {
    /// The name of this network to be used by the `Display` trait impl.
    network_name: String,
    /// The network magic, acts as an identifier for the network.
    network_magic: Magic,
    /// The genesis block hash
    genesis_hash: block::Hash,
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
    /// Slow start interval for this network
    slow_start_interval: Height,
    /// Slow start shift for this network, always half the slow start interval
    slow_start_shift: Height,
    /// Target difficulty limit for this network
    target_difficulty_limit: ExpandedDifficulty,
    /// A flag for disabling proof-of-work checks when Zebra is validating blocks
    disable_pow: bool,
}

impl Default for Parameters {
    /// Returns an instance of the default public testnet [`Parameters`].
    fn default() -> Self {
        Self {
            network_name: "Testnet".to_string(),
            ..Self::build().finish()
        }
    }
}

impl Parameters {
    /// Creates a new [`ParametersBuilder`].
    pub fn build() -> ParametersBuilder {
        ParametersBuilder::default()
    }

    /// Accepts a [`ConfiguredActivationHeights`].
    ///
    /// Creates an instance of [`Parameters`] with `Regtest` values.
    pub fn new_regtest(nu5_activation_height: Option<u32>) -> Self {
        #[cfg(any(test, feature = "proptest-impl"))]
        let nu5_activation_height = nu5_activation_height.or(Some(100));

        Self {
            network_name: "Regtest".to_string(),
            network_magic: magics::REGTEST,
            ..Self::build()
                .with_genesis_hash(REGTEST_GENESIS_HASH)
                // This value is chosen to match zcashd, see: <https://github.com/zcash/zcash/blob/master/src/chainparams.cpp#L654>
                .with_target_difficulty_limit(U256::from_big_endian(&[0x0f; 32]))
                .with_disable_pow(true)
                .with_slow_start_interval(Height::MIN)
                .with_sapling_hrps(
                    zp_constants::regtest::HRP_SAPLING_EXTENDED_SPENDING_KEY,
                    zp_constants::regtest::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY,
                    zp_constants::regtest::HRP_SAPLING_PAYMENT_ADDRESS,
                )
                // Removes default Testnet activation heights if not configured,
                // most network upgrades are disabled by default for Regtest in zcashd
                .with_activation_heights(ConfiguredActivationHeights {
                    canopy: Some(1),
                    nu5: nu5_activation_height,
                    ..Default::default()
                })
                .finish()
        }
    }

    /// Returns true if the instance of [`Parameters`] represents the default public Testnet.
    pub fn is_default_testnet(&self) -> bool {
        self == &Self::default()
    }

    /// Returns true if the instance of [`Parameters`] represents Regtest.
    pub fn is_regtest(&self) -> bool {
        let Self {
            network_name,
            network_magic,
            genesis_hash,
            // Activation heights are configurable on Regtest
            activation_heights: _,
            hrp_sapling_extended_spending_key,
            hrp_sapling_extended_full_viewing_key,
            hrp_sapling_payment_address,
            slow_start_interval,
            slow_start_shift,
            target_difficulty_limit,
            disable_pow,
        } = Self::new_regtest(None);

        self.network_name == network_name
            && self.network_magic == network_magic
            && self.genesis_hash == genesis_hash
            && self.hrp_sapling_extended_spending_key == hrp_sapling_extended_spending_key
            && self.hrp_sapling_extended_full_viewing_key == hrp_sapling_extended_full_viewing_key
            && self.hrp_sapling_payment_address == hrp_sapling_payment_address
            && self.slow_start_interval == slow_start_interval
            && self.slow_start_shift == slow_start_shift
            && self.target_difficulty_limit == target_difficulty_limit
            && self.disable_pow == disable_pow
    }

    /// Returns the network name
    pub fn network_name(&self) -> &str {
        &self.network_name
    }

    /// Returns the network magic
    pub fn network_magic(&self) -> Magic {
        self.network_magic
    }

    /// Returns the genesis hash
    pub fn genesis_hash(&self) -> block::Hash {
        self.genesis_hash
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

    /// Returns slow start interval for this network
    pub fn slow_start_interval(&self) -> Height {
        self.slow_start_interval
    }

    /// Returns slow start shift for this network
    pub fn slow_start_shift(&self) -> Height {
        self.slow_start_shift
    }

    /// Returns the target difficulty limit for this network
    pub fn target_difficulty_limit(&self) -> ExpandedDifficulty {
        self.target_difficulty_limit
    }

    /// Returns true if proof-of-work validation should be disabled for this network
    pub fn disable_pow(&self) -> bool {
        self.disable_pow
    }
}

impl Network {
    /// Returns true if proof-of-work validation should be disabled for this network
    pub fn disable_pow(&self) -> bool {
        if let Self::Testnet(params) = self {
            params.disable_pow()
        } else {
            false
        }
    }

    /// Returns slow start interval for this network
    pub fn slow_start_interval(&self) -> Height {
        if let Self::Testnet(params) = self {
            params.slow_start_interval()
        } else {
            SLOW_START_INTERVAL
        }
    }

    /// Returns slow start shift for this network
    pub fn slow_start_shift(&self) -> Height {
        if let Self::Testnet(params) = self {
            params.slow_start_shift()
        } else {
            SLOW_START_SHIFT
        }
    }
}
