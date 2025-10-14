//! Types and implementation for Testnet consensus parameters
use std::{collections::BTreeMap, fmt, sync::Arc};

use crate::{
    amount::{Amount, NonNegative},
    block::{self, Height, HeightDiff},
    parameters::{
        checkpoint::list::{CheckpointList, TESTNET_CHECKPOINTS},
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

        for (height, network_upgrade) in activation_heights {
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
                #[cfg(zcash_unstable = "zfuture")]
                NetworkUpgrade::ZFuture => &mut configured_activation_heights.zfuture,
                NetworkUpgrade::Genesis => continue,
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
    ///
    /// # Panics
    ///
    /// If a default is required but was not passed
    fn convert_with_default(
        self,
        default_funding_streams: Option<FundingStreams>,
    ) -> FundingStreams {
        let height_range = self.height_range.unwrap_or_else(|| {
            default_funding_streams
                .as_ref()
                .expect("default required")
                .height_range()
                .clone()
        });

        let recipients = self
            .recipients
            .map(|recipients| {
                recipients
                    .into_iter()
                    .map(ConfiguredFundingStreamRecipient::into_recipient)
                    .collect()
            })
            .unwrap_or_else(|| {
                default_funding_streams
                    .as_ref()
                    .expect("default required")
                    .recipients()
                    .clone()
            });

        assert!(
            height_range.start <= height_range.end,
            "funding stream end height must be above start height"
        );

        let funding_streams = FundingStreams::new(height_range.clone(), recipients);

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

        funding_streams
    }

    /// Converts the [`ConfiguredFundingStreams`] to a [`FundingStreams`].
    ///
    /// # Panics
    ///
    /// If `height_range` is None.
    pub fn into_funding_streams_unchecked(self) -> FundingStreams {
        let height_range = self.height_range.expect("must have height range");
        let recipients = self
            .recipients
            .into_iter()
            .flat_map(|recipients| {
                recipients
                    .into_iter()
                    .map(ConfiguredFundingStreamRecipient::into_recipient)
            })
            .collect();

        FundingStreams::new(height_range, recipients)
    }
}

/// Returns the number of funding stream address periods there are for the provided network and height range.
fn num_funding_stream_addresses_required_for_height_range(
    height_range: &std::ops::Range<Height>,
    network: &Network,
) -> usize {
    1u32.checked_add(funding_stream_address_period(
        height_range
            .end
            .previous()
            .expect("end height must be above start height and genesis height"),
        network,
    ))
    .expect("no overflow should happen in this sum")
    .checked_sub(funding_stream_address_period(height_range.start, network))
    .expect("no overflow should happen in this sub") as usize
}

/// Checks that the provided [`FundingStreams`] has sufficient recipient addresses for the
/// funding stream address period of the provided [`Network`].
fn check_funding_stream_address_period(funding_streams: &FundingStreams, network: &Network) {
    let expected_min_num_addresses = num_funding_stream_addresses_required_for_height_range(
        funding_streams.height_range(),
        network,
    );

    for (&receiver, recipient) in funding_streams.recipients() {
        if receiver == FundingStreamReceiver::Deferred {
            // The `Deferred` receiver doesn't need any addresses.
            continue;
        }

        let num_addresses = recipient.addresses().len();
        assert!(
            num_addresses >= expected_min_num_addresses,
            "recipients must have a sufficient number of addresses for height range, \
             minimum num addresses required: {expected_min_num_addresses}, only {num_addresses} were provided.\
             receiver: {receiver:?}, recipient: {recipient:?}"
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
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq)]
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
    /// Activation height for `ZFuture` network upgrade.
    #[serde(rename = "ZFuture")]
    #[cfg(zcash_unstable = "zfuture")]
    pub zfuture: Option<u32>,
}

impl ConfiguredActivationHeights {
    /// Converts a [`ConfiguredActivationHeights`] to one that uses the default values for Regtest where
    /// no activation heights are specified.
    fn for_regtest(self) -> Self {
        let Self {
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
            #[cfg(zcash_unstable = "zfuture")]
            zfuture,
        } = self;

        let overwinter = overwinter.or(before_overwinter).or(Some(1));
        let sapling = sapling.or(overwinter);
        let blossom = blossom.or(sapling);
        let heartwood = heartwood.or(blossom);
        let canopy = canopy.or(heartwood);

        Self {
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
            #[cfg(zcash_unstable = "zfuture")]
            zfuture,
        }
    }
}

/// Configurable checkpoints, either a path to a checkpoints file, a "default" keyword to indicate
/// that Zebra should use the default Testnet checkpoints, or a list of block heights and hashes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ConfiguredCheckpoints {
    /// A boolean indicating whether Zebra should use the default Testnet checkpoints.
    Default(bool),
    /// A path to a checkpoints file to be used as Zebra's checkpoints.
    Path(std::path::PathBuf),
    /// Directly configured block heights and hashes to be used as Zebra's checkpoints.
    HeightsAndHashes(Vec<(block::Height, block::Hash)>),
}

impl Default for ConfiguredCheckpoints {
    fn default() -> Self {
        Self::Default(false)
    }
}

impl From<Arc<CheckpointList>> for ConfiguredCheckpoints {
    fn from(value: Arc<CheckpointList>) -> Self {
        Self::HeightsAndHashes(value.iter_cloned().collect())
    }
}

impl From<bool> for ConfiguredCheckpoints {
    fn from(value: bool) -> Self {
        Self::Default(value)
    }
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
    /// Checkpointed block hashes and heights for this network.
    checkpoints: Arc<CheckpointList>,
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
            // The PoWLimit must be converted into a compact representation before using it
            // to perform difficulty filter checks (see https://github.com/zcash/zips/pull/417).
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
            checkpoints: TESTNET_CHECKPOINTS
                .parse()
                .map(Arc::new)
                .expect("must be able to parse checkpoints"),
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
            #[cfg(zcash_unstable = "zfuture")]
            zfuture,
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
        let activation_heights: BTreeMap<_, _> = {
            let activation_heights = before_overwinter
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
                .chain(nu7.into_iter().map(|h| (h, Nu7)));

            #[cfg(zcash_unstable = "zfuture")]
            let activation_heights =
                activation_heights.chain(zfuture.into_iter().map(|h| (h, ZFuture)));

            activation_heights
                .map(|(h, nu)| (h.try_into().expect("activation height must be valid"), nu))
                .collect()
        };

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
    ///
    /// # Panics
    ///
    /// If `funding_streams` is longer than `FUNDING_STREAMS_TESTNET`, and one
    /// of the extra streams requires a default value.
    pub fn with_funding_streams(mut self, funding_streams: Vec<ConfiguredFundingStreams>) -> Self {
        self.funding_streams = funding_streams
            .into_iter()
            .enumerate()
            .map(|(idx, streams)| {
                let default_streams = FUNDING_STREAMS_TESTNET.get(idx).cloned();
                streams.convert_with_default(default_streams)
            })
            .collect();
        self.should_lock_funding_stream_address_period = true;
        self
    }

    /// Clears funding streams from the [`Parameters`] being built.
    pub fn clear_funding_streams(mut self) -> Self {
        self.funding_streams = vec![];
        self
    }

    /// Extends the configured funding streams to have as many recipients as are required for their
    /// height ranges by repeating the recipients that have been configured.
    ///
    /// This should be called after configuring the desired network upgrade activation heights.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn extend_funding_streams(mut self) -> Self {
        // self.funding_streams.extend(FUNDING_STREAMS_TESTNET);

        let network = self.to_network_unchecked();

        for funding_streams in &mut self.funding_streams {
            funding_streams.extend_recipient_addresses(
                num_funding_stream_addresses_required_for_height_range(
                    funding_streams.height_range(),
                    &network,
                ),
            );
        }

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

    /// Sets the checkpoints for the network as the provided [`ConfiguredCheckpoints`].
    pub fn with_checkpoints(mut self, checkpoints: impl Into<ConfiguredCheckpoints>) -> Self {
        self.checkpoints = Arc::new(match checkpoints.into() {
            ConfiguredCheckpoints::Default(true) => TESTNET_CHECKPOINTS
                .parse()
                .expect("checkpoints file format must be valid"),
            ConfiguredCheckpoints::Default(false) => {
                CheckpointList::from_list([(block::Height(0), self.genesis_hash)])
                    .expect("must parse checkpoints")
            }
            ConfiguredCheckpoints::Path(path_buf) => {
                let Ok(raw_checkpoints_str) = std::fs::read_to_string(&path_buf) else {
                    panic!("could not read file at configured checkpoints file path: {path_buf:?}");
                };

                raw_checkpoints_str.parse().unwrap_or_else(|err| {
                    panic!("could not parse checkpoints at the provided path: {path_buf:?}, err: {err}")
                })
            }
            ConfiguredCheckpoints::HeightsAndHashes(items) => {
                CheckpointList::from_list(items).expect("configured checkpoints must be valid")
            }
        });

        self
    }

    /// Clears checkpoints from the [`Parameters`] being built, keeping the genesis checkpoint.
    pub fn clear_checkpoints(self) -> Self {
        self.with_checkpoints(ConfiguredCheckpoints::Default(false))
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
            checkpoints,
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
            checkpoints,
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
        for fs in &self.funding_streams {
            // Check that the funding streams are valid for the configured Testnet parameters.
            check_funding_stream_address_period(fs, &network);
        }

        // Final check that the configured checkpoints are valid for this network.
        assert_eq!(
            network.checkpoint_list().hash(Height(0)),
            Some(network.genesis_hash()),
            "first checkpoint hash must match genesis hash"
        );
        assert!(
            network.checkpoint_list().max_height() >= network.mandatory_checkpoint_height(),
            "checkpoints must be provided for block heights below the mandatory checkpoint height"
        );

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
            checkpoints: _,
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

/// A struct of parameters for configuring Regtest in Zebra.
#[derive(Debug, Default, Clone)]
pub struct RegtestParameters {
    /// The configured network upgrade activation heights to use on Regtest
    pub activation_heights: ConfiguredActivationHeights,
    /// Configured funding streams
    pub funding_streams: Option<Vec<ConfiguredFundingStreams>>,
    /// Expected one-time lockbox disbursement outputs in NU6.1 activation block coinbase for Regtest
    pub lockbox_disbursements: Option<Vec<ConfiguredLockboxDisbursement>>,
    /// Configured checkpointed block heights and hashes.
    pub checkpoints: Option<ConfiguredCheckpoints>,
    /// Automatically repeats funding stream addresess to fill all required periods
    /// if est to `true`. Only available with `proptest-impl` feature enabled
    pub extend_funding_stream_addresses_as_required: Option<bool>,
}

impl From<ConfiguredActivationHeights> for RegtestParameters {
    fn from(value: ConfiguredActivationHeights) -> Self {
        Self {
            activation_heights: value,
            ..Default::default()
        }
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
    /// List of checkpointed block heights and hashes
    checkpoints: Arc<CheckpointList>,
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
        RegtestParameters {
            activation_heights,
            funding_streams,
            lockbox_disbursements,
            checkpoints,
            extend_funding_stream_addresses_as_required,
        }: RegtestParameters,
    ) -> Self {
        let mut parameters = Self::build()
            .with_genesis_hash(REGTEST_GENESIS_HASH)
            // This value is chosen to match zcashd, see: <https://github.com/zcash/zcash/blob/master/src/chainparams.cpp#L654>
            .with_target_difficulty_limit(U256::from_big_endian(&[0x0f; 32]))
            .with_disable_pow(true)
            .with_unshielded_coinbase_spends(true)
            .with_slow_start_interval(Height::MIN)
            // Removes default Testnet activation heights if not configured,
            // most network upgrades are disabled by default for Regtest in zcashd
            .with_activation_heights(activation_heights.for_regtest())
            .with_halving_interval(PRE_BLOSSOM_REGTEST_HALVING_INTERVAL)
            .with_funding_streams(funding_streams.unwrap_or_default())
            .with_lockbox_disbursements(lockbox_disbursements.unwrap_or_default())
            .with_checkpoints(checkpoints.unwrap_or_default());

        if Some(true) == extend_funding_stream_addresses_as_required {
            parameters = parameters.extend_funding_streams();
        }

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
            funding_streams: _,
            target_difficulty_limit,
            disable_pow,
            should_allow_unshielded_coinbase_spends,
            pre_blossom_halving_interval,
            post_blossom_halving_interval,
            lockbox_disbursements: _,
            checkpoints: _,
        } = Self::new_regtest(Default::default());

        self.network_name == network_name
            && self.genesis_hash == genesis_hash
            && self.slow_start_interval == slow_start_interval
            && self.slow_start_shift == slow_start_shift
            && self.target_difficulty_limit == target_difficulty_limit
            && self.disable_pow == disable_pow
            && self.should_allow_unshielded_coinbase_spends
                == should_allow_unshielded_coinbase_spends
            && self.pre_blossom_halving_interval == pre_blossom_halving_interval
            && self.post_blossom_halving_interval == post_blossom_halving_interval
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

    /// Returns funding streams for this network.
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

    /// Returns the checkpoints for this network.
    pub fn checkpoints(&self) -> Arc<CheckpointList> {
        self.checkpoints.clone()
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
    pub fn funding_streams(&self, height: Height) -> Option<&FundingStreams> {
        self.all_funding_streams()
            .iter()
            .find(|&streams| streams.height_range().contains(&height))
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
