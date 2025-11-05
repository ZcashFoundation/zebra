//! Constants and calculations for Block Subsidy and Funding Streams
//!
//! This module contains the consensus parameters which are required for
//! verification.
//!
//! Some consensus parameters change based on network upgrades. Each network
//! upgrade happens at a particular block height. Some parameters have a value
//! (or function) before the upgrade height, at the upgrade height, and after
//! the upgrade height. (For example, the value of the reserved field in the
//! block header during the Heartwood upgrade.)
//!
//! Typically, consensus parameters are accessed via a function that takes a
//! `Network` and `block::Height`.

use std::collections::HashMap;

use lazy_static::lazy_static;

use crate::{
    amount::{self, Amount, NonNegative, COIN},
    block::{Height, HeightDiff},
    parameters::{constants::activation_heights, Network, NetworkUpgrade},
    transparent,
};

/// The largest block subsidy, used before the first halving.
///
/// We use `25 / 2` instead of `12.5`, so that we can calculate the correct value without using floating-point.
/// This calculation is exact, because COIN is divisible by 2, and the division is done last.
pub const MAX_BLOCK_SUBSIDY: u64 = ((25 * COIN) / 2) as u64;

/// Used as a multiplier to get the new halving interval after Blossom.
///
/// Calculated as `PRE_BLOSSOM_POW_TARGET_SPACING / POST_BLOSSOM_POW_TARGET_SPACING`
/// in the Zcash specification.
pub const BLOSSOM_POW_TARGET_SPACING_RATIO: u32 = 2;

/// Halving is at about every 4 years, before Blossom block time is 150 seconds.
///
/// `(60 * 60 * 24 * 365 * 4) / 150 = 840960`
pub const PRE_BLOSSOM_HALVING_INTERVAL: HeightDiff = 840_000;

/// After Blossom the block time is reduced to 75 seconds but halving period should remain around 4 years.
pub const POST_BLOSSOM_HALVING_INTERVAL: HeightDiff =
    PRE_BLOSSOM_HALVING_INTERVAL * (BLOSSOM_POW_TARGET_SPACING_RATIO as HeightDiff);

/// The first halving height in the testnet is at block height `1_116_000`
/// as specified in [protocol specification §7.10.1][7.10.1]
///
/// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
pub(crate) const FIRST_HALVING_TESTNET: Height = Height(1_116_000);

/// The first halving height in the regtest is at block height `287`.
const FIRST_HALVING_REGTEST: Height = Height(287);

/// The funding stream receiver categories.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FundingStreamReceiver {
    /// The Electric Coin Company (Bootstrap Foundation) funding stream.
    #[serde(rename = "ECC")]
    Ecc,

    /// The Zcash Foundation funding stream.
    ZcashFoundation,

    /// The Major Grants (Zcash Community Grants) funding stream.
    MajorGrants,

    /// The deferred pool contribution, see [ZIP-1015](https://zips.z.cash/zip-1015) for more details.
    Deferred,
}

impl FundingStreamReceiver {
    /// Returns a human-readable name and a specification URL for the receiver, as described in
    /// [ZIP-1014] and [`zcashd`] before NU6. After NU6, the specification is in the [ZIP-1015].
    ///
    /// [ZIP-1014]: https://zips.z.cash/zip-1014#abstract
    /// [`zcashd`]: https://github.com/zcash/zcash/blob/3f09cfa00a3c90336580a127e0096d99e25a38d6/src/consensus/funding.cpp#L13-L32
    /// [ZIP-1015]: https://zips.z.cash/zip-1015
    pub fn info(&self, is_post_nu6: bool) -> (&'static str, &'static str) {
        if is_post_nu6 {
            (
                match self {
                    FundingStreamReceiver::Ecc => "Electric Coin Company",
                    FundingStreamReceiver::ZcashFoundation => "Zcash Foundation",
                    FundingStreamReceiver::MajorGrants => "Zcash Community Grants NU6",
                    FundingStreamReceiver::Deferred => "Lockbox NU6",
                },
                LOCKBOX_SPECIFICATION,
            )
        } else {
            (
                match self {
                    FundingStreamReceiver::Ecc => "Electric Coin Company",
                    FundingStreamReceiver::ZcashFoundation => "Zcash Foundation",
                    FundingStreamReceiver::MajorGrants => "Major Grants",
                    FundingStreamReceiver::Deferred => "Lockbox NU6",
                },
                FUNDING_STREAM_SPECIFICATION,
            )
        }
    }

    /// Returns true if this [`FundingStreamReceiver`] is [`FundingStreamReceiver::Deferred`].
    pub fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred)
    }
}

/// Denominator as described in [protocol specification §7.10.1][7.10.1].
///
/// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
pub const FUNDING_STREAM_RECEIVER_DENOMINATOR: u64 = 100;

/// The specification for pre-NU6 funding stream receivers, a URL that links to [ZIP-214].
///
/// [ZIP-214]: https://zips.z.cash/zip-0214
pub const FUNDING_STREAM_SPECIFICATION: &str = "https://zips.z.cash/zip-0214";

/// The specification for post-NU6 funding stream and lockbox receivers, a URL that links to [ZIP-1015].
///
/// [ZIP-1015]: https://zips.z.cash/zip-1015
pub const LOCKBOX_SPECIFICATION: &str = "https://zips.z.cash/zip-1015";

/// Funding stream recipients and height ranges.
#[derive(Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct FundingStreams {
    /// Start and end Heights for funding streams
    /// as described in [protocol specification §7.10.1][7.10.1].
    ///
    /// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    height_range: std::ops::Range<Height>,
    /// Funding stream recipients by [`FundingStreamReceiver`].
    recipients: HashMap<FundingStreamReceiver, FundingStreamRecipient>,
}

impl FundingStreams {
    /// Creates a new [`FundingStreams`].
    pub fn new(
        height_range: std::ops::Range<Height>,
        recipients: HashMap<FundingStreamReceiver, FundingStreamRecipient>,
    ) -> Self {
        Self {
            height_range,
            recipients,
        }
    }

    /// Creates a new empty [`FundingStreams`] representing no funding streams.
    pub fn empty() -> Self {
        Self::new(Height::MAX..Height::MAX, HashMap::new())
    }

    /// Returns height range where these [`FundingStreams`] should apply.
    pub fn height_range(&self) -> &std::ops::Range<Height> {
        &self.height_range
    }

    /// Returns recipients of these [`FundingStreams`].
    pub fn recipients(&self) -> &HashMap<FundingStreamReceiver, FundingStreamRecipient> {
        &self.recipients
    }

    /// Returns a recipient with the provided receiver.
    pub fn recipient(&self, receiver: FundingStreamReceiver) -> Option<&FundingStreamRecipient> {
        self.recipients.get(&receiver)
    }

    /// Accepts a target number of addresses that all recipients of this funding stream
    /// except the [`FundingStreamReceiver::Deferred`] receiver should have.
    ///
    /// Extends the addresses for all funding stream recipients by repeating their
    /// existing addresses until reaching the provided target number of addresses.
    pub fn extend_recipient_addresses(&mut self, target_len: usize) {
        for (receiver, recipient) in &mut self.recipients {
            if receiver.is_deferred() {
                continue;
            }

            recipient.extend_addresses(target_len);
        }
    }
}

/// A funding stream recipient as specified in [protocol specification §7.10.1][7.10.1]
///
/// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
#[derive(Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct FundingStreamRecipient {
    /// The numerator for each funding stream receiver category
    /// as described in [protocol specification §7.10.1][7.10.1].
    ///
    /// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    numerator: u64,
    /// Addresses for the funding stream recipient
    addresses: Vec<transparent::Address>,
}

impl FundingStreamRecipient {
    /// Creates a new [`FundingStreamRecipient`].
    pub fn new<I, T>(numerator: u64, addresses: I) -> Self
    where
        T: ToString,
        I: IntoIterator<Item = T>,
    {
        Self {
            numerator,
            addresses: addresses
                .into_iter()
                .map(|addr| {
                    let addr = addr.to_string();
                    addr.parse()
                        .expect("funding stream address must deserialize")
                })
                .collect(),
        }
    }

    /// Returns the numerator for this funding stream.
    pub fn numerator(&self) -> u64 {
        self.numerator
    }

    /// Returns the receiver of this funding stream.
    pub fn addresses(&self) -> &[transparent::Address] {
        &self.addresses
    }

    /// Accepts a target number of addresses that this recipient should have.
    ///
    /// Extends the addresses for this funding stream recipient by repeating
    /// existing addresses until reaching the provided target number of addresses.
    ///
    /// # Panics
    ///
    /// If there are no recipient addresses.
    pub fn extend_addresses(&mut self, target_len: usize) {
        assert!(
            !self.addresses.is_empty(),
            "cannot extend addresses for empty recipient"
        );

        self.addresses = self
            .addresses
            .iter()
            .cycle()
            .take(target_len)
            .cloned()
            .collect();
    }
}

lazy_static! {
    /// The funding streams for Mainnet as described in:
    /// - [protocol specification §7.10.1][7.10.1]
    /// - [ZIP-1015](https://zips.z.cash/zip-1015)
    /// - [ZIP-214#funding-streams](https://zips.z.cash/zip-0214#funding-streams)
    ///
    /// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    pub static ref FUNDING_STREAMS_MAINNET: Vec<FundingStreams> = vec![
        FundingStreams {
            height_range: Height(1_046_400)..Height(2_726_400),
            recipients: [
                (
                    FundingStreamReceiver::Ecc,
                    FundingStreamRecipient::new(7, FUNDING_STREAM_ECC_ADDRESSES_MAINNET),
                ),
                (
                    FundingStreamReceiver::ZcashFoundation,
                    FundingStreamRecipient::new(5, FUNDING_STREAM_ZF_ADDRESSES_MAINNET),
                ),
                (
                    FundingStreamReceiver::MajorGrants,
                    FundingStreamRecipient::new(8, FUNDING_STREAM_MG_ADDRESSES_MAINNET),
                ),
            ]
            .into_iter()
            .collect(),
        },
        FundingStreams {
            height_range: POST_NU6_FUNDING_STREAM_START_RANGE_MAINNET,
            recipients: [
                (
                    FundingStreamReceiver::Deferred,
                    FundingStreamRecipient::new::<[&str; 0], &str>(12, []),
                ),
                (
                    FundingStreamReceiver::MajorGrants,
                    FundingStreamRecipient::new(8, POST_NU6_FUNDING_STREAM_FPF_ADDRESSES_MAINNET),
                ),
            ]
            .into_iter()
            .collect(),
        },

        FundingStreams {
            height_range: activation_heights::mainnet::NU6_1..Height(4_406_400),
            recipients: [
                (
                    FundingStreamReceiver::Deferred,
                    FundingStreamRecipient::new::<[&str; 0], &str>(12, []),
                ),
                (
                    FundingStreamReceiver::MajorGrants,
                    FundingStreamRecipient::new(8, POST_NU6_1_FUNDING_STREAM_FPF_ADDRESSES_MAINNET),
                ),
            ]
            .into_iter()
            .collect(),
        },
    ];

    /// The funding streams for Testnet as described in:
    /// - [protocol specification §7.10.1][7.10.1]
    /// - [ZIP-1015](https://zips.z.cash/zip-1015)
    /// - [ZIP-214#funding-streams](https://zips.z.cash/zip-0214#funding-streams)
    ///
    /// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    pub static ref FUNDING_STREAMS_TESTNET: Vec<FundingStreams> = vec![
        FundingStreams {
            height_range: Height(1_028_500)..Height(2_796_000),
            recipients: [
                (
                    FundingStreamReceiver::Ecc,
                    FundingStreamRecipient::new(7, FUNDING_STREAM_ECC_ADDRESSES_TESTNET),
                ),
                (
                    FundingStreamReceiver::ZcashFoundation,
                    FundingStreamRecipient::new(5, FUNDING_STREAM_ZF_ADDRESSES_TESTNET),
                ),
                (
                    FundingStreamReceiver::MajorGrants,
                    FundingStreamRecipient::new(8, FUNDING_STREAM_MG_ADDRESSES_TESTNET),
                ),
            ]
            .into_iter()
            .collect(),
        },
        FundingStreams {
            height_range: POST_NU6_FUNDING_STREAM_START_RANGE_TESTNET,
            recipients: [
                (
                    FundingStreamReceiver::Deferred,
                    FundingStreamRecipient::new::<[&str; 0], &str>(12, []),
                ),
                (
                    FundingStreamReceiver::MajorGrants,
                    FundingStreamRecipient::new(8, POST_NU6_FUNDING_STREAM_FPF_ADDRESSES_TESTNET),
                ),
            ]
            .into_iter()
            .collect(),
        },
        FundingStreams {
            height_range: activation_heights::testnet::NU6_1..Height(4_476_000),
            recipients: [
                (
                    FundingStreamReceiver::Deferred,
                    FundingStreamRecipient::new::<[&str; 0], &str>(12, []),
                ),
                (
                    FundingStreamReceiver::MajorGrants,
                    FundingStreamRecipient::new(8, POST_NU6_1_FUNDING_STREAM_FPF_ADDRESSES_TESTNET),
                ),
            ]
            .into_iter()
            .collect(),
        },
    ];
}

/// The start height of post-NU6 funding streams on Mainnet as described in [ZIP-1015](https://zips.z.cash/zip-1015).
const POST_NU6_FUNDING_STREAM_START_HEIGHT_MAINNET: u32 = 2_726_400;

/// The start height of post-NU6 funding streams on Testnet as described in [ZIP-1015](https://zips.z.cash/zip-1015).
const POST_NU6_FUNDING_STREAM_START_HEIGHT_TESTNET: u32 = 2_976_000;

/// The one-time lockbox disbursement output addresses and amounts expected in the NU6.1 activation block's
/// coinbase transaction on Mainnet.
/// See:
/// - <https://zips.z.cash/zip-0271#one-timelockboxdisbursement>
/// - <https://zips.z.cash/zip-0214#mainnet-recipients-for-revision-2>
pub const NU6_1_LOCKBOX_DISBURSEMENTS_MAINNET: [(&str, Amount<NonNegative>); 10] = [(
    "t3ev37Q2uL1sfTsiJQJiWJoFzQpDhmnUwYo",
    EXPECTED_NU6_1_LOCKBOX_DISBURSEMENTS_TOTAL_MAINNET.div_exact(10),
); 10];

/// The one-time lockbox disbursement output addresses and amounts expected in the NU6.1 activation block's
/// coinbase transaction on Testnet.
/// See:
/// - <https://zips.z.cash/zip-0271#one-timelockboxdisbursement>
/// - <https://zips.z.cash/zip-0214#testnet-recipients-for-revision-2>
pub const NU6_1_LOCKBOX_DISBURSEMENTS_TESTNET: [(&str, Amount<NonNegative>); 10] = [(
    "t2RnBRiqrN1nW4ecZs1Fj3WWjNdnSs4kiX8",
    EXPECTED_NU6_1_LOCKBOX_DISBURSEMENTS_TOTAL_TESTNET.div_exact(10),
); 10];

/// The expected total amount of the one-time lockbox disbursement on Mainnet.
/// See: <https://zips.z.cash/zip-0271#one-timelockboxdisbursement>.
pub(crate) const EXPECTED_NU6_1_LOCKBOX_DISBURSEMENTS_TOTAL_MAINNET: Amount<NonNegative> =
    Amount::new_from_zec(78_750);

/// The expected total amount of the one-time lockbox disbursement on Testnet.
/// See <https://zips.z.cash/zip-0271#one-timelockboxdisbursement>.
pub(crate) const EXPECTED_NU6_1_LOCKBOX_DISBURSEMENTS_TOTAL_TESTNET: Amount<NonNegative> =
    Amount::new_from_zec(78_750);

/// The number of blocks contained in the post-NU6 funding streams height ranges on Mainnet or Testnet, as specified
/// in [ZIP-1015](https://zips.z.cash/zip-1015).
const POST_NU6_FUNDING_STREAM_NUM_BLOCKS: u32 = 420_000;

/// The post-NU6 funding stream height range on Mainnet
const POST_NU6_FUNDING_STREAM_START_RANGE_MAINNET: std::ops::Range<Height> =
    Height(POST_NU6_FUNDING_STREAM_START_HEIGHT_MAINNET)
        ..Height(POST_NU6_FUNDING_STREAM_START_HEIGHT_MAINNET + POST_NU6_FUNDING_STREAM_NUM_BLOCKS);

/// The post-NU6 funding stream height range on Testnet
const POST_NU6_FUNDING_STREAM_START_RANGE_TESTNET: std::ops::Range<Height> =
    Height(POST_NU6_FUNDING_STREAM_START_HEIGHT_TESTNET)
        ..Height(POST_NU6_FUNDING_STREAM_START_HEIGHT_TESTNET + POST_NU6_FUNDING_STREAM_NUM_BLOCKS);

/// Address change interval function here as a constant
/// as described in [protocol specification §7.10.1][7.10.1].
///
/// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
pub const FUNDING_STREAM_ADDRESS_CHANGE_INTERVAL: HeightDiff = POST_BLOSSOM_HALVING_INTERVAL / 48;

/// Number of addresses for each funding stream in the Mainnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub const FUNDING_STREAMS_NUM_ADDRESSES_MAINNET: usize = 48;

/// List of addresses for the ECC funding stream in the Mainnet.
pub const FUNDING_STREAM_ECC_ADDRESSES_MAINNET: [&str; FUNDING_STREAMS_NUM_ADDRESSES_MAINNET] = [
    "t3LmX1cxWPPPqL4TZHx42HU3U5ghbFjRiif",
    "t3Toxk1vJQ6UjWQ42tUJz2rV2feUWkpbTDs",
    "t3ZBdBe4iokmsjdhMuwkxEdqMCFN16YxKe6",
    "t3ZuaJziLM8xZ32rjDUzVjVtyYdDSz8GLWB",
    "t3bAtYWa4bi8VrtvqySxnbr5uqcG9czQGTZ",
    "t3dktADfb5Rmxncpe1HS5BRS5Gcj7MZWYBi",
    "t3hgskquvKKoCtvxw86yN7q8bzwRxNgUZmc",
    "t3R1VrLzwcxAZzkX4mX3KGbWpNsgtYtMntj",
    "t3ff6fhemqPMVujD3AQurxRxTdvS1pPSaa2",
    "t3cEUQFG3KYnFG6qYhPxSNgGi3HDjUPwC3J",
    "t3WR9F5U4QvUFqqx9zFmwT6xFqduqRRXnaa",
    "t3PYc1LWngrdUrJJbHkYPCKvJuvJjcm85Ch",
    "t3bgkjiUeatWNkhxY3cWyLbTxKksAfk561R",
    "t3Z5rrR8zahxUpZ8itmCKhMSfxiKjUp5Dk5",
    "t3PU1j7YW3fJ67jUbkGhSRto8qK2qXCUiW3",
    "t3S3yaT7EwNLaFZCamfsxxKwamQW2aRGEkh",
    "t3eutXKJ9tEaPSxZpmowhzKhPfJvmtwTEZK",
    "t3gbTb7brxLdVVghSPSd3ycGxzHbUpukeDm",
    "t3UCKW2LrHFqPMQFEbZn6FpjqnhAAbfpMYR",
    "t3NyHsrnYbqaySoQqEQRyTWkjvM2PLkU7Uu",
    "t3QEFL6acxuZwiXtW3YvV6njDVGjJ1qeaRo",
    "t3PdBRr2S1XTDzrV8bnZkXF3SJcrzHWe1wj",
    "t3ZWyRPpWRo23pKxTLtWsnfEKeq9T4XPxKM",
    "t3he6QytKCTydhpztykFsSsb9PmBT5JBZLi",
    "t3VWxWDsLb2TURNEP6tA1ZSeQzUmPKFNxRY",
    "t3NmWLvZkbciNAipauzsFRMxoZGqmtJksbz",
    "t3cKr4YxVPvPBG1mCvzaoTTdBNokohsRJ8n",
    "t3T3smGZn6BoSFXWWXa1RaoQdcyaFjMfuYK",
    "t3gkDUe9Gm4GGpjMk86TiJZqhztBVMiUSSA",
    "t3eretuBeBXFHe5jAqeSpUS1cpxVh51fAeb",
    "t3dN8g9zi2UGJdixGe9txeSxeofLS9t3yFQ",
    "t3S799pq9sYBFwccRecoTJ3SvQXRHPrHqvx",
    "t3fhYnv1S5dXwau7GED3c1XErzt4n4vDxmf",
    "t3cmE3vsBc5xfDJKXXZdpydCPSdZqt6AcNi",
    "t3h5fPdjJVHaH4HwynYDM5BB3J7uQaoUwKi",
    "t3Ma35c68BgRX8sdLDJ6WR1PCrKiWHG4Da9",
    "t3LokMKPL1J8rkJZvVpfuH7dLu6oUWqZKQK",
    "t3WFFGbEbhJWnASZxVLw2iTJBZfJGGX73mM",
    "t3L8GLEsUn4QHNaRYcX3EGyXmQ8kjpT1zTa",
    "t3PgfByBhaBSkH8uq4nYJ9ZBX4NhGCJBVYm",
    "t3WecsqKDhWXD4JAgBVcnaCC2itzyNZhJrv",
    "t3ZG9cSfopnsMQupKW5v9sTotjcP5P6RTbn",
    "t3hC1Ywb5zDwUYYV8LwhvF5rZ6m49jxXSG5",
    "t3VgMqDL15ZcyQDeqBsBW3W6rzfftrWP2yB",
    "t3LC94Y6BwLoDtBoK2NuewaEbnko1zvR9rm",
    "t3cWCUZJR3GtALaTcatrrpNJ3MGbMFVLRwQ",
    "t3YYF4rPLVxDcF9hHFsXyc5Yq1TFfbojCY6",
    "t3XHAGxRP2FNfhAjxGjxbrQPYtQQjc3RCQD",
];

/// Functionality specific to block subsidy-related consensus rules
pub trait ParameterSubsidy {
    /// Returns the minimum height after the first halving
    /// as described in [protocol specification §7.10][7.10]
    ///
    /// [7.10]: <https://zips.z.cash/protocol/protocol.pdf#fundingstreams>
    fn height_for_first_halving(&self) -> Height;

    /// Returns the halving interval after Blossom
    fn post_blossom_halving_interval(&self) -> HeightDiff;

    /// Returns the halving interval before Blossom
    fn pre_blossom_halving_interval(&self) -> HeightDiff;

    /// Returns the address change interval for funding streams
    /// as described in [protocol specification §7.10][7.10].
    ///
    /// > FSRecipientChangeInterval := PostBlossomHalvingInterval / 48
    ///
    /// [7.10]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    fn funding_stream_address_change_interval(&self) -> HeightDiff;
}

/// Network methods related to Block Subsidy and Funding Streams
impl ParameterSubsidy for Network {
    fn height_for_first_halving(&self) -> Height {
        // First halving on Mainnet is at Canopy
        // while in Testnet is at block constant height of `1_116_000`
        // <https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams>
        match self {
            Network::Mainnet => NetworkUpgrade::Canopy
                .activation_height(self)
                .expect("canopy activation height should be available"),
            Network::Testnet(params) => {
                if params.is_regtest() {
                    FIRST_HALVING_REGTEST
                } else if params.is_default_testnet() {
                    FIRST_HALVING_TESTNET
                } else {
                    height_for_halving(1, self).expect("first halving height should be available")
                }
            }
        }
    }

    fn post_blossom_halving_interval(&self) -> HeightDiff {
        match self {
            Network::Mainnet => POST_BLOSSOM_HALVING_INTERVAL,
            Network::Testnet(params) => params.post_blossom_halving_interval(),
        }
    }

    fn pre_blossom_halving_interval(&self) -> HeightDiff {
        match self {
            Network::Mainnet => PRE_BLOSSOM_HALVING_INTERVAL,
            Network::Testnet(params) => params.pre_blossom_halving_interval(),
        }
    }

    fn funding_stream_address_change_interval(&self) -> HeightDiff {
        self.post_blossom_halving_interval() / 48
    }
}

/// List of addresses for the Zcash Foundation funding stream in the Mainnet.
pub const FUNDING_STREAM_ZF_ADDRESSES_MAINNET: [&str; FUNDING_STREAMS_NUM_ADDRESSES_MAINNET] =
    ["t3dvVE3SQEi7kqNzwrfNePxZ1d4hUyztBA1"; FUNDING_STREAMS_NUM_ADDRESSES_MAINNET];

/// List of addresses for the Major Grants funding stream in the Mainnet.
pub const FUNDING_STREAM_MG_ADDRESSES_MAINNET: [&str; FUNDING_STREAMS_NUM_ADDRESSES_MAINNET] =
    ["t3XyYW8yBFRuMnfvm5KLGFbEVz25kckZXym"; FUNDING_STREAMS_NUM_ADDRESSES_MAINNET];

/// Number of addresses for each post-NU6 funding stream on Mainnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub const POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES_MAINNET: usize = 12;

/// List of addresses for the Major Grants post-NU6 funding stream on Mainnet administered by the Financial Privacy Fund (FPF).
pub const POST_NU6_FUNDING_STREAM_FPF_ADDRESSES_MAINNET: [&str;
    POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES_MAINNET] =
    ["t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"; POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES_MAINNET];

/// Number of addresses for each post-NU6.1 funding stream on Mainnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub const POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES_MAINNET: usize = 36;

/// List of addresses for the Major Grants post-NU6.1 funding stream on Mainnet administered by the Financial Privacy Fund (FPF).
pub const POST_NU6_1_FUNDING_STREAM_FPF_ADDRESSES_MAINNET: [&str;
    POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES_MAINNET] =
    ["t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"; POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES_MAINNET];

/// Number of addresses for each funding stream in the Testnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub const FUNDING_STREAMS_NUM_ADDRESSES_TESTNET: usize = 51;

/// List of addresses for the ECC funding stream in the Testnet.
pub const FUNDING_STREAM_ECC_ADDRESSES_TESTNET: [&str; FUNDING_STREAMS_NUM_ADDRESSES_TESTNET] = [
    "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz",
    "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz",
    "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz",
    "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz",
    "t2NNHrgPpE388atmWSF4DxAb3xAoW5Yp45M",
    "t2VMN28itPyMeMHBEd9Z1hm6YLkQcGA1Wwe",
    "t2CHa1TtdfUV8UYhNm7oxbzRyfr8616BYh2",
    "t2F77xtr28U96Z2bC53ZEdTnQSUAyDuoa67",
    "t2ARrzhbgcpoVBDPivUuj6PzXzDkTBPqfcT",
    "t278aQ8XbvFR15mecRguiJDQQVRNnkU8kJw",
    "t2Dp1BGnZsrTXZoEWLyjHmg3EPvmwBnPDGB",
    "t2KzeqXgf4ju33hiSqCuKDb8iHjPCjMq9iL",
    "t2Nyxqv1BiWY1eUSiuxVw36oveawYuo18tr",
    "t2DKFk5JRsVoiuinK8Ti6eM4Yp7v8BbfTyH",
    "t2CUaBca4k1x36SC4q8Nc8eBoqkMpF3CaLg",
    "t296SiKL7L5wvFmEdMxVLz1oYgd6fTfcbZj",
    "t29fBCFbhgsjL3XYEZ1yk1TUh7eTusB6dPg",
    "t2FGofLJXa419A76Gpf5ncxQB4gQXiQMXjK",
    "t2ExfrnRVnRiXDvxerQ8nZbcUQvNvAJA6Qu",
    "t28JUffLp47eKPRHKvwSPzX27i9ow8LSXHx",
    "t2JXWPtrtyL861rFWMZVtm3yfgxAf4H7uPA",
    "t2QdgbJoWfYHgyvEDEZBjHmgkr9yNJff3Hi",
    "t2QW43nkco8r32ZGRN6iw6eSzyDjkMwCV3n",
    "t2DgYDXMJTYLwNcxighQ9RCgPxMVATRcUdC",
    "t2Bop7dg33HGZx3wunnQzi2R2ntfpjuti3M",
    "t2HVeEwovcLq9RstAbYkqngXNEsCe2vjJh9",
    "t2HxbP5keQSx7p592zWQ5bJ5GrMmGDsV2Xa",
    "t2TJzUg2matao3mztBRJoWnJY6ekUau6tPD",
    "t29pMzxmo6wod25YhswcjKv3AFRNiBZHuhj",
    "t2QBQMRiJKYjshJpE6RhbF7GLo51yE6d4wZ",
    "t2F5RqnqguzZeiLtYHFx4yYfy6pDnut7tw5",
    "t2CHvyZANE7XCtg8AhZnrcHCC7Ys1jJhK13",
    "t2BRzpMdrGWZJ2upsaNQv6fSbkbTy7EitLo",
    "t2BFixHGQMAWDY67LyTN514xRAB94iEjXp3",
    "t2Uvz1iVPzBEWfQBH1p7NZJsFhD74tKaG8V",
    "t2CmFDj5q6rJSRZeHf1SdrowinyMNcj438n",
    "t2ErNvWEReTfPDBaNizjMPVssz66aVZh1hZ",
    "t2GeJQ8wBUiHKDVzVM5ZtKfY5reCg7CnASs",
    "t2L2eFtkKv1G6j55kLytKXTGuir4raAy3yr",
    "t2EK2b87dpPazb7VvmEGc8iR6SJ289RywGL",
    "t2DJ7RKeZJxdA4nZn8hRGXE8NUyTzjujph9",
    "t2K1pXo4eByuWpKLkssyMLe8QKUbxnfFC3H",
    "t2TB4mbSpuAcCWkH94Leb27FnRxo16AEHDg",
    "t2Phx4gVL4YRnNsH3jM1M7jE4Fo329E66Na",
    "t2VQZGmeNomN8c3USefeLL9nmU6M8x8CVzC",
    "t2RicCvTVTY5y9JkreSRv3Xs8q2K67YxHLi",
    "t2JrSLxTGc8wtPDe9hwbaeUjCrCfc4iZnDD",
    "t2Uh9Au1PDDSw117sAbGivKREkmMxVC5tZo",
    "t2FDwoJKLeEBMTy3oP7RLQ1Fihhvz49a3Bv",
    "t2FY18mrgtb7QLeHA8ShnxLXuW8cNQ2n1v8",
    "t2L15TkDYum7dnQRBqfvWdRe8Yw3jVy9z7g",
];

/// List of addresses for the Zcash Foundation funding stream in the Testnet.
pub const FUNDING_STREAM_ZF_ADDRESSES_TESTNET: [&str; FUNDING_STREAMS_NUM_ADDRESSES_TESTNET] =
    ["t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v"; FUNDING_STREAMS_NUM_ADDRESSES_TESTNET];

/// List of addresses for the Major Grants funding stream in the Testnet.
pub const FUNDING_STREAM_MG_ADDRESSES_TESTNET: [&str; FUNDING_STREAMS_NUM_ADDRESSES_TESTNET] =
    ["t2Gvxv2uNM7hbbACjNox4H6DjByoKZ2Fa3P"; FUNDING_STREAMS_NUM_ADDRESSES_TESTNET];

/// Number of addresses for each post-NU6 funding stream in the Testnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub const POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES_TESTNET: usize = 13;

/// Number of addresses for each post-NU6 funding stream in the Testnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// There are 27 funding stream periods across the 939,500 blocks for which the post-NU6.1 funding streams are
/// active. See Testnet funding streams in revision 2 of <https://zips.z.cash/zip-0214#funding-streams>.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub const POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES_TESTNET: usize = 27;

/// List of addresses for the Major Grants post-NU6 funding stream on Testnet administered by the Financial Privacy Fund (FPF).
pub const POST_NU6_FUNDING_STREAM_FPF_ADDRESSES_TESTNET: [&str;
    POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES_TESTNET] =
    ["t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu"; POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES_TESTNET];

/// List of addresses for the Major Grants post-NU6.1 funding stream on Testnet administered by the Financial Privacy Fund (FPF).
pub const POST_NU6_1_FUNDING_STREAM_FPF_ADDRESSES_TESTNET: [&str;
    POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES_TESTNET] =
    ["t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu"; POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES_TESTNET];

/// Returns the address change period
/// as described in [protocol specification §7.10][7.10]
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub fn funding_stream_address_period<N: ParameterSubsidy>(height: Height, network: &N) -> u32 {
    // Spec equation: `address_period = floor((height - (height_for_halving(1) - post_blossom_halving_interval))/funding_stream_address_change_interval)`,
    // <https://zips.z.cash/protocol/protocol.pdf#fundingstreams>
    //
    // Note that the brackets make it so the post blossom halving interval is added to the total.
    //
    // In Rust, "integer division rounds towards zero":
    // <https://doc.rust-lang.org/stable/reference/expressions/operator-expr.html#arithmetic-and-logical-binary-operators>
    // This is the same as `floor()`, because these numbers are all positive.

    let height_after_first_halving = height - network.height_for_first_halving();

    let address_period = (height_after_first_halving + network.post_blossom_halving_interval())
        / network.funding_stream_address_change_interval();

    address_period
        .try_into()
        .expect("all values are positive and smaller than the input height")
}

/// The first block height of the halving at the provided halving index for a network.
///
/// See `Halving(height)`, as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn height_for_halving(halving: u32, network: &Network) -> Option<Height> {
    if halving == 0 {
        return Some(Height(0));
    }

    let slow_start_shift = i64::from(network.slow_start_shift().0);
    let blossom_height = i64::from(NetworkUpgrade::Blossom.activation_height(network)?.0);
    let pre_blossom_halving_interval = network.pre_blossom_halving_interval();
    let halving_index = i64::from(halving);

    let unscaled_height = halving_index.checked_mul(pre_blossom_halving_interval)?;

    let pre_blossom_height = unscaled_height
        .min(blossom_height)
        .checked_add(slow_start_shift)?;

    let post_blossom_height = 0
        .max(unscaled_height - blossom_height)
        .checked_mul(i64::from(BLOSSOM_POW_TARGET_SPACING_RATIO))?
        .checked_add(slow_start_shift)?;

    let height = pre_blossom_height.checked_add(post_blossom_height)?;

    let height = u32::try_from(height).ok()?;
    height.try_into().ok()
}

/// Returns the `fs.Value(height)` for each stream receiver
/// as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn funding_stream_values(
    height: Height,
    network: &Network,
    expected_block_subsidy: Amount<NonNegative>,
) -> Result<HashMap<FundingStreamReceiver, Amount<NonNegative>>, crate::amount::Error> {
    let canopy_height = NetworkUpgrade::Canopy.activation_height(network).unwrap();
    let mut results = HashMap::new();

    if height >= canopy_height {
        let funding_streams = network.funding_streams(height);
        if let Some(funding_streams) = funding_streams {
            for (&receiver, recipient) in funding_streams.recipients() {
                // - Spec equation: `fs.value = floor(block_subsidy(height)*(fs.numerator/fs.denominator))`:
                //   https://zips.z.cash/protocol/protocol.pdf#subsidies
                // - In Rust, "integer division rounds towards zero":
                //   https://doc.rust-lang.org/stable/reference/expressions/operator-expr.html#arithmetic-and-logical-binary-operators
                //   This is the same as `floor()`, because these numbers are all positive.
                let amount_value = ((expected_block_subsidy * recipient.numerator())?
                    / FUNDING_STREAM_RECEIVER_DENOMINATOR)?;

                results.insert(receiver, amount_value);
            }
        }
    }

    Ok(results)
}

/// Block subsidy errors.
#[derive(thiserror::Error, Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum SubsidyError {
    #[error("no coinbase transaction in block")]
    NoCoinbase,

    #[error("funding stream expected output not found")]
    FundingStreamNotFound,

    #[error("one-time lockbox disbursement output not found")]
    OneTimeLockboxDisbursementNotFound,

    #[error("miner fees are invalid")]
    InvalidMinerFees,

    #[error("a sum of amounts overflowed")]
    SumOverflow,

    #[error("unsupported height")]
    UnsupportedHeight,

    #[error("invalid amount")]
    InvalidAmount(#[from] amount::Error),
}

/// The divisor used for halvings.
///
/// `1 << Halving(height)`, as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
///
/// Returns `None` if the divisor would overflow a `u64`.
pub fn halving_divisor(height: Height, network: &Network) -> Option<u64> {
    // Some far-future shifts can be more than 63 bits
    1u64.checked_shl(num_halvings(height, network))
}

/// The halving index for a block height and network.
///
/// `Halving(height)`, as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn num_halvings(height: Height, network: &Network) -> u32 {
    let slow_start_shift = network.slow_start_shift();
    let blossom_height = NetworkUpgrade::Blossom
        .activation_height(network)
        .expect("blossom activation height should be available");

    let halving_index = if height < slow_start_shift {
        0
    } else if height < blossom_height {
        let pre_blossom_height = height - slow_start_shift;
        pre_blossom_height / network.pre_blossom_halving_interval()
    } else {
        let pre_blossom_height = blossom_height - slow_start_shift;
        let scaled_pre_blossom_height =
            pre_blossom_height * HeightDiff::from(BLOSSOM_POW_TARGET_SPACING_RATIO);

        let post_blossom_height = height - blossom_height;

        (scaled_pre_blossom_height + post_blossom_height) / network.post_blossom_halving_interval()
    };

    halving_index
        .try_into()
        .expect("already checked for negatives")
}

/// `BlockSubsidy(height)` as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn block_subsidy(
    height: Height,
    network: &Network,
) -> Result<Amount<NonNegative>, SubsidyError> {
    let blossom_height = NetworkUpgrade::Blossom
        .activation_height(network)
        .expect("blossom activation height should be available");

    // If the halving divisor is larger than u64::MAX, the block subsidy is zero,
    // because amounts fit in an i64.
    //
    // Note: bitcoind incorrectly wraps here, which restarts large block rewards.
    let Some(halving_div) = halving_divisor(height, network) else {
        return Ok(Amount::zero());
    };

    // Zebra doesn't need to calculate block subsidies for blocks with heights in the slow start
    // interval because it handles those blocks through checkpointing.
    if height < network.slow_start_interval() {
        Err(SubsidyError::UnsupportedHeight)
    } else if height < blossom_height {
        // this calculation is exact, because the halving divisor is 1 here
        Ok(Amount::try_from(MAX_BLOCK_SUBSIDY / halving_div)?)
    } else {
        let scaled_max_block_subsidy =
            MAX_BLOCK_SUBSIDY / u64::from(BLOSSOM_POW_TARGET_SPACING_RATIO);
        // in future halvings, this calculation might not be exact
        // Amount division is implemented using integer division,
        // which truncates (rounds down) the result, as specified
        Ok(Amount::try_from(scaled_max_block_subsidy / halving_div)?)
    }
}

/// `MinerSubsidy(height)` as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn miner_subsidy(
    height: Height,
    network: &Network,
    expected_block_subsidy: Amount<NonNegative>,
) -> Result<Amount<NonNegative>, amount::Error> {
    let total_funding_stream_amount: Result<Amount<NonNegative>, _> =
        funding_stream_values(height, network, expected_block_subsidy)?
            .values()
            .sum();

    expected_block_subsidy - total_funding_stream_amount?
}
