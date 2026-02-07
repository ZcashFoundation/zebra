//! Testnet-specific constants for block subsidies.

use lazy_static::lazy_static;

use crate::parameters::{
    constants::activation_heights,
    network::{Amount, Height, NonNegative},
    subsidy::{
        constants::POST_NU6_FUNDING_STREAM_NUM_BLOCKS, FundingStreamReceiver,
        FundingStreamRecipient, FundingStreams,
    },
};

/// The first halving height in the testnet is at block height `1_116_000`
/// as specified in [protocol specification §7.10.1][7.10.1]
///
/// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
pub(crate) const FIRST_HALVING: Height = Height(1_116_000);

/// The start height of post-NU6 funding streams on Testnet as described in [ZIP-1015](https://zips.z.cash/zip-1015).
pub(crate) const POST_NU6_FUNDING_STREAM_START_HEIGHT: u32 = 2_976_000;

/// The one-time lockbox disbursement output addresses and amounts expected in the NU6.1 activation block's
/// coinbase transaction on Testnet.
/// See:
/// - <https://zips.z.cash/zip-0271#one-timelockboxdisbursement>
/// - <https://zips.z.cash/zip-0214#testnet-recipients-for-revision-2>
pub(crate) const NU6_1_LOCKBOX_DISBURSEMENTS: [(&str, Amount<NonNegative>); 10] = [(
    "t2RnBRiqrN1nW4ecZs1Fj3WWjNdnSs4kiX8",
    EXPECTED_NU6_1_LOCKBOX_DISBURSEMENTS_TOTAL.div_exact(10),
); 10];

/// The expected total amount of the one-time lockbox disbursement on Testnet.
/// See <https://zips.z.cash/zip-0271#one-timelockboxdisbursement>.
pub(crate) const EXPECTED_NU6_1_LOCKBOX_DISBURSEMENTS_TOTAL: Amount<NonNegative> =
    Amount::new_from_zec(78_750);

/// The post-NU6 funding stream height range on Testnet
pub(crate) const POST_NU6_FUNDING_STREAM_START_RANGE: std::ops::Range<Height> =
    Height(POST_NU6_FUNDING_STREAM_START_HEIGHT)
        ..Height(POST_NU6_FUNDING_STREAM_START_HEIGHT + POST_NU6_FUNDING_STREAM_NUM_BLOCKS);

/// Number of addresses for each funding stream in the Testnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub(crate) const FUNDING_STREAMS_NUM_ADDRESSES: usize = 51;

/// List of addresses for the ECC funding stream in the Testnet.
pub(crate) const FUNDING_STREAM_ECC_ADDRESSES: [&str; FUNDING_STREAMS_NUM_ADDRESSES] = [
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
pub(crate) const FUNDING_STREAM_ZF_ADDRESSES: [&str; FUNDING_STREAMS_NUM_ADDRESSES] =
    ["t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v"; FUNDING_STREAMS_NUM_ADDRESSES];

/// List of addresses for the Major Grants funding stream in the Testnet.
pub(crate) const FUNDING_STREAM_MG_ADDRESSES: [&str; FUNDING_STREAMS_NUM_ADDRESSES] =
    ["t2Gvxv2uNM7hbbACjNox4H6DjByoKZ2Fa3P"; FUNDING_STREAMS_NUM_ADDRESSES];

/// Number of addresses for each post-NU6 funding stream in the Testnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub(crate) const POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES: usize = 13;

/// Number of addresses for each post-NU6 funding stream in the Testnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// There are 27 funding stream periods across the 939,500 blocks for which the post-NU6.1 funding streams are
/// active. See Testnet funding streams in revision 2 of <https://zips.z.cash/zip-0214#funding-streams>.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub(crate) const POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES: usize = 27;

/// List of addresses for the Major Grants post-NU6 funding stream on Testnet administered by the Financial Privacy Fund (FPF).
pub(crate) const POST_NU6_FUNDING_STREAM_FPF_ADDRESSES: [&str;
    POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES] =
    ["t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu"; POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES];

/// List of addresses for the Major Grants post-NU6.1 funding stream on Testnet administered by the Financial Privacy Fund (FPF).
pub(crate) const POST_NU6_1_FUNDING_STREAM_FPF_ADDRESSES: [&str;
    POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES] =
    ["t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu"; POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES];

lazy_static! {
    /// The funding streams for Testnet as described in:
    /// - [protocol specification §7.10.1][7.10.1]
    /// - [ZIP-1015](https://zips.z.cash/zip-1015)
    /// - [ZIP-214#funding-streams](https://zips.z.cash/zip-0214#funding-streams)
    ///
    /// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    pub(crate) static ref FUNDING_STREAMS: Vec<FundingStreams> = vec![
        FundingStreams {
            height_range: Height(1_028_500)..Height(2_796_000),
            recipients: [
                (
                    FundingStreamReceiver::Ecc,
                    FundingStreamRecipient::new(7, FUNDING_STREAM_ECC_ADDRESSES),
                ),
                (
                    FundingStreamReceiver::ZcashFoundation,
                    FundingStreamRecipient::new(5, FUNDING_STREAM_ZF_ADDRESSES),
                ),
                (
                    FundingStreamReceiver::MajorGrants,
                    FundingStreamRecipient::new(8, FUNDING_STREAM_MG_ADDRESSES),
                ),
            ]
            .into_iter()
            .collect(),
        },
        FundingStreams {
            height_range: POST_NU6_FUNDING_STREAM_START_RANGE,
            recipients: [
                (
                    FundingStreamReceiver::Deferred,
                    FundingStreamRecipient::new::<[&str; 0], &str>(12, []),
                ),
                (
                    FundingStreamReceiver::MajorGrants,
                    FundingStreamRecipient::new(8, POST_NU6_FUNDING_STREAM_FPF_ADDRESSES),
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
                    FundingStreamRecipient::new(8, POST_NU6_1_FUNDING_STREAM_FPF_ADDRESSES),
                ),
            ]
            .into_iter()
            .collect(),
        },
    ];
}
