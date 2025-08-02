//! Types and implementation for Testnet consensus parameters
use std::{collections::BTreeMap, fmt, sync::Arc};

use crate::{
    amount::{Amount, NonNegative},
    block::{self, Height, HeightDiff},
    parameters::{
        constants::{magics, SLOW_START_INTERVAL, SLOW_START_SHIFT},
        network_upgrade::TESTNET_ACTIVATION_HEIGHTS,
        subsidy::{
            funding_stream_address_period, FUNDING_STREAMS_MAINNET, FUNDING_STREAMS_TESTNET,
            FUNDING_STREAM_RECEIVER_DENOMINATOR, NU6_1_LOCKBOX_DISBURSEMENTS_TESTNET,
        },
        Network, NetworkKind, NetworkUpgrade,
    },
    transparent,
    work::difficulty::{ExpandedDifficulty, U256},
};

use super::{
    magic::Magic,
    subsidy::{
        FundingStreamReceiver, FundingStreamRecipient, FundingStreams,
        BLOSSOM_POW_TARGET_SPACING_RATIO, POST_BLOSSOM_HALVING_INTERVAL,
        PRE_BLOSSOM_HALVING_INTERVAL,
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

/// Maximum length for a configured human-readable prefix.
pub const MAX_HRP_LENGTH: usize = 30;

/// The block hash of the Regtest genesis block, `zcash-cli -regtest getblockhash 0`
const REGTEST_GENESIS_HASH: &str =
    "029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327";

/// The block hash of the Testnet genesis block, `zcash-cli -testnet getblockhash 0`
const TESTNET_GENESIS_HASH: &str =
    "05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38";

/// The halving height interval in the regtest is 6 hours.
/// [zcashd regtest halving interval](https://github.com/zcash/zcash/blob/v5.10.0/src/consensus/params.h#L252)
const PRE_BLOSSOM_REGTEST_HALVING_INTERVAL: HeightDiff = 144;

/// Configurable funding stream recipient for configured Testnets.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredFundingStreamRecipient {
    /// Funding stream receiver, see [`FundingStreams::recipients`] for more details.
    pub receiver: FundingStreamReceiver,
    /// The numerator for each funding stream receiver category, see [`FundingStreamRecipient::numerator`] for more details.
    pub numerator: u64,
    /// Addresses for the funding stream recipient, see [`FundingStreamRecipient::addresses`] for more details.
    pub addresses: Option<Vec<String>>,
}

impl ConfiguredFundingStreamRecipient {
    /// Converts a [`ConfiguredFundingStreamRecipient`] to a [`FundingStreamReceiver`] and [`FundingStreamRecipient`].
    pub fn into_recipient(self) -> (FundingStreamReceiver, FundingStreamRecipient) {
        (
            self.receiver,
            FundingStreamRecipient::new(self.numerator, self.addresses.unwrap_or_default()),
        )
    }
}

/// Configurable one-time lockbox disbursement recipients for configured Testnets.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredLockboxDisbursement {
    /// The expected address for the lockbox disbursement output
    pub address: String,
    /// The expected disbursement amount
    pub amount: Amount<NonNegative>,
}

/// Configurable funding streams for configured Testnets.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredFundingStreams {
    /// Start and end height for funding streams see [`FundingStreams::height_range`] for more details.
    pub height_range: Option<std::ops::Range<Height>>,
    /// Funding stream recipients, see [`FundingStreams::recipients`] for more details.
    pub recipients: Option<Vec<ConfiguredFundingStreamRecipient>>,
}

impl From<&FundingStreams> for ConfiguredFundingStreams {
    fn from(value: &FundingStreams) -> Self {
        Self {
            height_range: Some(value.height_range().clone()),
            recipients: Some(
                value
                    .recipients()
                    .iter()
                    .map(|(receiver, recipient)| ConfiguredFundingStreamRecipient {
                        receiver: *receiver,
                        numerator: recipient.numerator(),
                        addresses: Some(
                            recipient
                                .addresses()
                                .iter()
                                .map(ToString::to_string)
                                .collect(),
                        ),
                    })
                    .collect(),
            ),
        }
    }
}

impl From<(transparent::Address, Amount<NonNegative>)> for ConfiguredLockboxDisbursement {
    fn from((address, amount): (transparent::Address, Amount<NonNegative>)) -> Self {
        Self {
            address: address.to_string(),
            amount,
        }
    }
}

impl From<&BTreeMap<Height, NetworkUpgrade>> for ConfiguredActivationHeights {
    fn from(activation_heights: &BTreeMap<Height, NetworkUpgrade>) -> Self {
        let mut configured_activation_heights = ConfiguredActivationHeights::default();

        for (height, network_upgrade) in activation_heights.iter() {
            let field = match network_upgrade {
                NetworkUpgrade::BeforeOverwinter => {
                    &mut configured_activation_heights.before_overwinter
                }
                NetworkUpgrade::Overwinter => &mut configured_activation_heights.overwinter,
                NetworkUpgrade::Sapling => &mut configured_activation_heights.sapling,
                NetworkUpgrade::Blossom => &mut configured_activation_heights.blossom,
                NetworkUpgrade::Heartwood => &mut configured_activation_heights.heartwood,
                NetworkUpgrade::Canopy => &mut configured_activation_heights.canopy,
                NetworkUpgrade::Nu5 => &mut configured_activation_heights.nu5,
                NetworkUpgrade::Nu6 => &mut configured_activation_heights.nu6,
                NetworkUpgrade::Nu6_1 => &mut configured_activation_heights.nu6_1,
                NetworkUpgrade::Nu7 => &mut configured_activation_heights.nu7,
                NetworkUpgrade::Genesis => {
                    continue;
                }
            };

            *field = Some(height.0)
        }

        configured_activation_heights
    }
}

impl From<BTreeMap<Height, NetworkUpgrade>> for ConfiguredActivationHeights {
    fn from(value: BTreeMap<Height, NetworkUpgrade>) -> Self {
        Self::from(&value)
    }
}

impl ConfiguredFundingStreams {
    /// Converts a [`ConfiguredFundingStreams`] to a [`FundingStreams`], using the provided default values
    /// if `height_range` or `recipients` are None.
    fn convert(self, parameters_builder: &ParametersBuilder) -> Option<FundingStreams> {
        let network = parameters_builder.to_network_unchecked();
        let height_range = self.height_range?;

        let recipients = self.recipients.map(|recipients| {
            recipients
                .into_iter()
                .map(ConfiguredFundingStreamRecipient::into_recipient)
                .collect()
        })?;

        assert!(
            height_range.start < height_range.end,
            "funding stream end height must be above start height"
        );

        let funding_streams = FundingStreams::new(height_range.clone(), recipients);

        check_funding_stream_address_period(&funding_streams, &network);

        // check that sum of receiver numerators is valid.

        let sum_numerators: u64 = funding_streams
            .recipients()
            .values()
            .map(|r| r.numerator())
            .sum();

        assert!(
            sum_numerators <= FUNDING_STREAM_RECEIVER_DENOMINATOR,
            "sum of funding stream numerators must not be \
         greater than denominator of {FUNDING_STREAM_RECEIVER_DENOMINATOR}"
        );

        Some(funding_streams)
    }
}

/// Checks that the provided [`FundingStreams`] has sufficient recipient addresses for the
/// funding stream address period of the provided [`Network`].
fn check_funding_stream_address_period(funding_streams: &FundingStreams, network: &Network) {
    let height_range = funding_streams.height_range();
    let expected_min_num_addresses =
        1u32.checked_add(funding_stream_address_period(
            height_range
                .end
                .previous()
                .expect("end height must be above start height and genesis height"),
            network,
        ))
        .expect("no overflow should happen in this sum")
        .checked_sub(funding_stream_address_period(height_range.start, network))
        .expect("no overflow should happen in this sub") as usize;

    for (&receiver, recipient) in funding_streams.recipients() {
        if receiver == FundingStreamReceiver::Deferred {
            // The `Deferred` receiver doesn't need any addresses.
            continue;
        }

        assert!(
            recipient.addresses().len() >= expected_min_num_addresses,
            "recipients must have a sufficient number of addresses for height range, \
         minimum num addresses required: {expected_min_num_addresses}"
        );

        for address in recipient.addresses() {
            assert_eq!(
                address.network_kind(),
                NetworkKind::Testnet,
                "configured funding stream addresses must be for Testnet"
            );
        }
    }
}

/// Configurable activation heights for Regtest and configured Testnets.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
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
    /// Activation height for `NU6` network upgrade.
    #[serde(rename = "NU6")]
    pub nu6: Option<u32>,
    /// Activation height for `NU6.1` network upgrade.
    #[serde(rename = "NU6.1")]
    pub nu6_1: Option<u32>,
    /// Activation height for `NU7` network upgrade.
    #[serde(rename = "NU7")]
    pub nu7: Option<u32>,
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
    /// Slow start interval for this network
    slow_start_interval: Height,
    /// Funding streams for this network
    funding_streams: Vec<FundingStreams>,
    /// A flag indicating whether to allow changes to fields that affect
    /// the funding stream address period.
    should_lock_funding_stream_address_period: bool,
    /// Target difficulty limit for this network
    target_difficulty_limit: ExpandedDifficulty,
    /// A flag for disabling proof-of-work checks when Zebra is validating blocks
    disable_pow: bool,
    /// Whether to allow transactions with transparent outputs to spend coinbase outputs,
    /// similar to `fCoinbaseMustBeShielded` in zcashd.
    should_allow_unshielded_coinbase_spends: bool,
    /// The pre-Blossom halving interval for this network
    pre_blossom_halving_interval: HeightDiff,
    /// The post-Blossom halving interval for this network
    post_blossom_halving_interval: HeightDiff,
    /// Expected one-time lockbox disbursement outputs in NU6.1 activation block coinbase for this network
    lockbox_disbursements: Vec<(String, Amount<NonNegative>)>,
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
            funding_streams: FUNDING_STREAMS_TESTNET.clone(),
            should_lock_funding_stream_address_period: false,
            pre_blossom_halving_interval: PRE_BLOSSOM_HALVING_INTERVAL,
            post_blossom_halving_interval: POST_BLOSSOM_HALVING_INTERVAL,
            should_allow_unshielded_coinbase_spends: false,
            lockbox_disbursements: NU6_1_LOCKBOX_DISBURSEMENTS_TESTNET
                .iter()
                .map(|(addr, amount)| (addr.to_string(), *amount))
                .collect(),
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
            nu6,
            nu6_1,
            nu7,
        }: ConfiguredActivationHeights,
    ) -> Self {
        use NetworkUpgrade::*;

        if self.should_lock_funding_stream_address_period {
            panic!("activation heights on ParametersBuilder must not be set after setting funding streams");
        }

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
            .chain(nu6.into_iter().map(|h| (h, Nu6)))
            .chain(nu6_1.into_iter().map(|h| (h, Nu6_1)))
            .chain(nu7.into_iter().map(|h| (h, Nu7)))
            .map(|(h, nu)| (h.try_into().expect("activation height must be valid"), nu))
            .collect();

        let network_upgrades: Vec<_> = activation_heights.iter().map(|(_h, &nu)| nu).collect();

        // Check that the provided network upgrade activation heights are in the same order by height as the default testnet activation heights
        let mut activation_heights_iter = activation_heights.iter();
        for expected_network_upgrade in NetworkUpgrade::iter() {
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
                    "network upgrades must be activated in order specified by the protocol"
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

    /// Sets funding streams to be used in the [`Parameters`] being built.
    pub fn with_funding_streams(mut self, funding_streams: Vec<ConfiguredFundingStreams>) -> Self {
        let original_len = funding_streams.len();
        self.funding_streams = funding_streams
            .into_iter()
            .filter_map(|streams| streams.convert(&self))
            .collect();
        if self.funding_streams.len() != original_len {
            self.funding_streams = FUNDING_STREAMS_TESTNET.clone();
        }
        self.should_lock_funding_stream_address_period = true;
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

    /// Sets the `disable_pow` flag to be used in the [`Parameters`] being built.
    pub fn with_unshielded_coinbase_spends(
        mut self,
        should_allow_unshielded_coinbase_spends: bool,
    ) -> Self {
        self.should_allow_unshielded_coinbase_spends = should_allow_unshielded_coinbase_spends;
        self
    }

    /// Sets the pre and post Blosssom halving intervals to be used in the [`Parameters`] being built.
    pub fn with_halving_interval(mut self, pre_blossom_halving_interval: HeightDiff) -> Self {
        if self.should_lock_funding_stream_address_period {
            panic!("halving interval on ParametersBuilder must not be set after setting funding streams");
        }

        self.pre_blossom_halving_interval = pre_blossom_halving_interval;
        self.post_blossom_halving_interval =
            self.pre_blossom_halving_interval * (BLOSSOM_POW_TARGET_SPACING_RATIO as HeightDiff);
        self
    }

    /// Sets the expected one-time lockbox disbursement outputs for this network
    pub fn with_lockbox_disbursements(
        mut self,
        lockbox_disbursements: Vec<ConfiguredLockboxDisbursement>,
    ) -> Self {
        self.lockbox_disbursements = lockbox_disbursements
            .into_iter()
            .map(|ConfiguredLockboxDisbursement { address, amount }| (address, amount))
            .collect();
        self
    }

    /// Converts the builder to a [`Parameters`] struct
    fn finish(self) -> Parameters {
        let Self {
            network_name,
            network_magic,
            genesis_hash,
            activation_heights,
            slow_start_interval,
            funding_streams,
            should_lock_funding_stream_address_period: _,
            target_difficulty_limit,
            disable_pow,
            should_allow_unshielded_coinbase_spends,
            pre_blossom_halving_interval,
            post_blossom_halving_interval,
            lockbox_disbursements,
        } = self;
        Parameters {
            network_name,
            network_magic,
            genesis_hash,
            activation_heights,
            slow_start_interval,
            slow_start_shift: Height(slow_start_interval.0 / 2),
            funding_streams,
            target_difficulty_limit,
            disable_pow,
            should_allow_unshielded_coinbase_spends,
            pre_blossom_halving_interval,
            post_blossom_halving_interval,
            lockbox_disbursements,
        }
    }

    /// Converts the builder to a configured [`Network::Testnet`]
    fn to_network_unchecked(&self) -> Network {
        Network::new_configured_testnet(self.clone().finish())
    }

    /// Checks funding streams and converts the builder to a configured [`Network::Testnet`]
    pub fn to_network(self) -> Network {
        let network = self.to_network_unchecked();

        // Final check that the configured funding streams will be valid for these Testnet parameters.
        // TODO: Always check funding stream address period once the testnet parameters are being serialized (#8920).
        #[cfg(not(any(test, feature = "proptest-impl")))]
        {
            check_funding_stream_address_period(&self.pre_nu6_funding_streams, &network);
            check_funding_stream_address_period(&self.post_nu6_funding_streams, &network);
        }

        network
    }

    /// Returns true if these [`Parameters`] should be compatible with the default Testnet parameters.
    pub fn is_compatible_with_default_parameters(&self) -> bool {
        let Self {
            network_name: _,
            network_magic,
            genesis_hash,
            activation_heights,
            slow_start_interval,
            funding_streams,
            should_lock_funding_stream_address_period: _,
            target_difficulty_limit,
            disable_pow,
            should_allow_unshielded_coinbase_spends,
            pre_blossom_halving_interval,
            post_blossom_halving_interval,
            lockbox_disbursements,
        } = Self::default();

        self.activation_heights == activation_heights
            && self.network_magic == network_magic
            && self.genesis_hash == genesis_hash
            && self.slow_start_interval == slow_start_interval
            && self.funding_streams == funding_streams
            && self.target_difficulty_limit == target_difficulty_limit
            && self.disable_pow == disable_pow
            && self.should_allow_unshielded_coinbase_spends
                == should_allow_unshielded_coinbase_spends
            && self.pre_blossom_halving_interval == pre_blossom_halving_interval
            && self.post_blossom_halving_interval == post_blossom_halving_interval
            && self.lockbox_disbursements == lockbox_disbursements
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
    /// Slow start interval for this network
    slow_start_interval: Height,
    /// Slow start shift for this network, always half the slow start interval
    slow_start_shift: Height,
    /// Funding streams for this network
    funding_streams: Vec<FundingStreams>,
    /// Target difficulty limit for this network
    target_difficulty_limit: ExpandedDifficulty,
    /// A flag for disabling proof-of-work checks when Zebra is validating blocks
    disable_pow: bool,
    /// Whether to allow transactions with transparent outputs to spend coinbase outputs,
    /// similar to `fCoinbaseMustBeShielded` in zcashd.
    should_allow_unshielded_coinbase_spends: bool,
    /// Pre-Blossom halving interval for this network
    pre_blossom_halving_interval: HeightDiff,
    /// Post-Blossom halving interval for this network
    post_blossom_halving_interval: HeightDiff,
    /// Expected one-time lockbox disbursement outputs in NU6.1 activation block coinbase for this network
    lockbox_disbursements: Vec<(String, Amount<NonNegative>)>,
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
    pub fn new_regtest(
        ConfiguredActivationHeights { nu5, nu6, nu7, .. }: ConfiguredActivationHeights,
    ) -> Self {
        #[cfg(any(test, feature = "proptest-impl"))]
        let nu5 = nu5.or(Some(100));

        let parameters = Self::build()
            .with_genesis_hash(REGTEST_GENESIS_HASH)
            // This value is chosen to match zcashd, see: <https://github.com/zcash/zcash/blob/master/src/chainparams.cpp#L654>
            .with_target_difficulty_limit(U256::from_big_endian(&[0x0f; 32]))
            .with_disable_pow(true)
            .with_unshielded_coinbase_spends(true)
            .with_slow_start_interval(Height::MIN)
            // Removes default Testnet activation heights if not configured,
            // most network upgrades are disabled by default for Regtest in zcashd
            .with_activation_heights(ConfiguredActivationHeights {
                canopy: Some(1),
                nu5,
                nu6,
                nu7,
                ..Default::default()
            })
            .with_halving_interval(PRE_BLOSSOM_REGTEST_HALVING_INTERVAL)
            .with_lockbox_disbursements(Vec::new());

        // TODO: Always clear funding streams on Regtest once the testnet parameters are being serialized (#8920).
        // #[cfg(not(any(test, feature = "proptest-impl")))]
        let parameters = parameters.with_funding_streams(Default::default());

        Self {
            network_name: "Regtest".to_string(),
            network_magic: magics::REGTEST,
            ..parameters.finish()
        }
    }

    /// Returns true if the instance of [`Parameters`] represents the default public Testnet.
    pub fn is_default_testnet(&self) -> bool {
        self == &Self::default()
    }

    /// Returns true if the instance of [`Parameters`] represents Regtest.
    pub fn is_regtest(&self) -> bool {
        if self.network_magic != magics::REGTEST {
            return false;
        }

        let Self {
            network_name,
            // Already checked network magic above
            network_magic: _,
            genesis_hash,
            // Activation heights are configurable on Regtest
            activation_heights: _,
            slow_start_interval,
            slow_start_shift,
            funding_streams,
            target_difficulty_limit,
            disable_pow,
            should_allow_unshielded_coinbase_spends,
            pre_blossom_halving_interval,
            post_blossom_halving_interval,
            lockbox_disbursements,
        } = Self::new_regtest(Default::default());

        self.network_name == network_name
            && self.genesis_hash == genesis_hash
            && self.slow_start_interval == slow_start_interval
            && self.slow_start_shift == slow_start_shift
            && self.funding_streams == funding_streams
            && self.target_difficulty_limit == target_difficulty_limit
            && self.disable_pow == disable_pow
            && self.should_allow_unshielded_coinbase_spends
                == should_allow_unshielded_coinbase_spends
            && self.pre_blossom_halving_interval == pre_blossom_halving_interval
            && self.post_blossom_halving_interval == post_blossom_halving_interval
            && self.lockbox_disbursements == lockbox_disbursements
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

    /// Returns slow start interval for this network
    pub fn slow_start_interval(&self) -> Height {
        self.slow_start_interval
    }

    /// Returns slow start shift for this network
    pub fn slow_start_shift(&self) -> Height {
        self.slow_start_shift
    }

    /// Returns funding streams for this network. Will incorporate
    /// `pre_nu6_funding_streams` and `post_nu6_funding_streams` if they were
    /// specified.
    pub fn funding_streams(&self) -> &Vec<FundingStreams> {
        &self.funding_streams
    }

    /// Returns the target difficulty limit for this network
    pub fn target_difficulty_limit(&self) -> ExpandedDifficulty {
        self.target_difficulty_limit
    }

    /// Returns true if proof-of-work validation should be disabled for this network
    pub fn disable_pow(&self) -> bool {
        self.disable_pow
    }

    /// Returns true if this network should allow transactions with transparent outputs
    /// that spend coinbase outputs.
    pub fn should_allow_unshielded_coinbase_spends(&self) -> bool {
        self.should_allow_unshielded_coinbase_spends
    }

    /// Returns the pre-Blossom halving interval for this network
    pub fn pre_blossom_halving_interval(&self) -> HeightDiff {
        self.pre_blossom_halving_interval
    }

    /// Returns the post-Blossom halving interval for this network
    pub fn post_blossom_halving_interval(&self) -> HeightDiff {
        self.post_blossom_halving_interval
    }

    /// Returns the expected total value of the sum of all NU6.1 one-time lockbox disbursement output values for this network.
    pub fn lockbox_disbursement_total_amount(&self) -> Amount<NonNegative> {
        self.lockbox_disbursements()
            .into_iter()
            .map(|(_addr, amount)| amount)
            .reduce(|a, b| (a + b).expect("sum of configured amounts should be valid"))
            .unwrap_or_default()
    }

    /// Returns the expected NU6.1 lockbox disbursement outputs for this network.
    pub fn lockbox_disbursements(&self) -> Vec<(transparent::Address, Amount<NonNegative>)> {
        self.lockbox_disbursements
            .iter()
            .map(|(addr, amount)| {
                (
                    addr.parse().expect("hard-coded address must deserialize"),
                    *amount,
                )
            })
            .collect()
    }
}

impl Network {
    /// Returns the parameters of this network if it is a Testnet.
    pub fn parameters(&self) -> Option<Arc<Parameters>> {
        if let Self::Testnet(parameters) = self {
            Some(parameters.clone())
        } else {
            None
        }
    }

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

    /// Returns post-Canopy funding streams for this network at the provided height
    pub fn funding_streams(&self, height: Height) -> Option<FundingStreams> {
        for streams in self.all_funding_streams() {
            if streams.height_range().contains(&height) {
                return Some(streams.clone());
            }
        }
        None
    }

    /// Returns post-Canopy funding streams for this network at the provided height
    pub fn all_funding_streams(&self) -> &Vec<FundingStreams> {
        if let Self::Testnet(params) = self {
            params.funding_streams()
        } else {
            &FUNDING_STREAMS_MAINNET
        }
    }

    /// Returns true if this network should allow transactions with transparent outputs
    /// that spend coinbase outputs.
    pub fn should_allow_unshielded_coinbase_spends(&self) -> bool {
        if let Self::Testnet(params) = self {
            params.should_allow_unshielded_coinbase_spends()
        } else {
            false
        }
    }
}
