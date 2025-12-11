//! Mainnet-specific constants for block subsidies.

use lazy_static::lazy_static;

use crate::parameters::{
    constants::activation_heights,
    network::{Amount, Height, NonNegative},
    subsidy::{
        constants::POST_NU6_FUNDING_STREAM_NUM_BLOCKS, FundingStreamReceiver,
        FundingStreamRecipient, FundingStreams,
    },
};

/// The start height of post-NU6 funding streams on Mainnet as described in [ZIP-1015](https://zips.z.cash/zip-1015).
pub(crate) const POST_NU6_FUNDING_STREAM_START_HEIGHT: u32 = 2_726_400;

/// The one-time lockbox disbursement output addresses and amounts expected in the NU6.1 activation block's
/// coinbase transaction on Mainnet.
///
/// See:
///
/// - <https://zips.z.cash/zip-0271#one-timelockboxdisbursement>
/// - <https://zips.z.cash/zip-0214#mainnet-recipients-for-revision-2>
pub(crate) const NU6_1_LOCKBOX_DISBURSEMENTS: [(&str, Amount<NonNegative>); 10] = [(
    "t3ev37Q2uL1sfTsiJQJiWJoFzQpDhmnUwYo",
    EXPECTED_NU6_1_LOCKBOX_DISBURSEMENTS_TOTAL.div_exact(10),
); 10];

/// The expected total amount of the one-time lockbox disbursement on Mainnet.
/// See: <https://zips.z.cash/zip-0271#one-timelockboxdisbursement>.
pub(crate) const EXPECTED_NU6_1_LOCKBOX_DISBURSEMENTS_TOTAL: Amount<NonNegative> =
    Amount::new_from_zec(78_750);

/// The post-NU6 funding stream height range on Mainnet
pub(crate) const POST_NU6_FUNDING_STREAM_START_RANGE: std::ops::Range<Height> =
    Height(POST_NU6_FUNDING_STREAM_START_HEIGHT)
        ..Height(POST_NU6_FUNDING_STREAM_START_HEIGHT + POST_NU6_FUNDING_STREAM_NUM_BLOCKS);

/// Number of addresses for each funding stream in the Mainnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub(crate) const FUNDING_STREAMS_NUM_ADDRESSES: usize = 48;

/// List of addresses for the ECC funding stream in the Mainnet.
pub(crate) const FUNDING_STREAM_ECC_ADDRESSES: [&str; FUNDING_STREAMS_NUM_ADDRESSES] = [
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

/// Number of founder addresses on Mainnet.
pub(crate) const NUM_FOUNDER_ADDRESSES: usize = 48;

/// List of founder addresses on Mainnet.
pub(crate) const FOUNDER_ADDRESS_LIST: [&str; NUM_FOUNDER_ADDRESSES] = [
    "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd",
    "t3cL9AucCajm3HXDhb5jBnJK2vapVoXsop3",
    "t3fqvkzrrNaMcamkQMwAyHRjfDdM2xQvDTR",
    "t3TgZ9ZT2CTSK44AnUPi6qeNaHa2eC7pUyF",
    "t3SpkcPQPfuRYHsP5vz3Pv86PgKo5m9KVmx",
    "t3Xt4oQMRPagwbpQqkgAViQgtST4VoSWR6S",
    "t3ayBkZ4w6kKXynwoHZFUSSgXRKtogTXNgb",
    "t3adJBQuaa21u7NxbR8YMzp3km3TbSZ4MGB",
    "t3K4aLYagSSBySdrfAGGeUd5H9z5Qvz88t2",
    "t3RYnsc5nhEvKiva3ZPhfRSk7eyh1CrA6Rk",
    "t3Ut4KUq2ZSMTPNE67pBU5LqYCi2q36KpXQ",
    "t3ZnCNAvgu6CSyHm1vWtrx3aiN98dSAGpnD",
    "t3fB9cB3eSYim64BS9xfwAHQUKLgQQroBDG",
    "t3cwZfKNNj2vXMAHBQeewm6pXhKFdhk18kD",
    "t3YcoujXfspWy7rbNUsGKxFEWZqNstGpeG4",
    "t3bLvCLigc6rbNrUTS5NwkgyVrZcZumTRa4",
    "t3VvHWa7r3oy67YtU4LZKGCWa2J6eGHvShi",
    "t3eF9X6X2dSo7MCvTjfZEzwWrVzquxRLNeY",
    "t3esCNwwmcyc8i9qQfyTbYhTqmYXZ9AwK3X",
    "t3M4jN7hYE2e27yLsuQPPjuVek81WV3VbBj",
    "t3gGWxdC67CYNoBbPjNvrrWLAWxPqZLxrVY",
    "t3LTWeoxeWPbmdkUD3NWBquk4WkazhFBmvU",
    "t3P5KKX97gXYFSaSjJPiruQEX84yF5z3Tjq",
    "t3f3T3nCWsEpzmD35VK62JgQfFig74dV8C9",
    "t3Rqonuzz7afkF7156ZA4vi4iimRSEn41hj",
    "t3fJZ5jYsyxDtvNrWBeoMbvJaQCj4JJgbgX",
    "t3Pnbg7XjP7FGPBUuz75H65aczphHgkpoJW",
    "t3WeKQDxCijL5X7rwFem1MTL9ZwVJkUFhpF",
    "t3Y9FNi26J7UtAUC4moaETLbMo8KS1Be6ME",
    "t3aNRLLsL2y8xcjPheZZwFy3Pcv7CsTwBec",
    "t3gQDEavk5VzAAHK8TrQu2BWDLxEiF1unBm",
    "t3Rbykhx1TUFrgXrmBYrAJe2STxRKFL7G9r",
    "t3aaW4aTdP7a8d1VTE1Bod2yhbeggHgMajR",
    "t3YEiAa6uEjXwFL2v5ztU1fn3yKgzMQqNyo",
    "t3g1yUUwt2PbmDvMDevTCPWUcbDatL2iQGP",
    "t3dPWnep6YqGPuY1CecgbeZrY9iUwH8Yd4z",
    "t3QRZXHDPh2hwU46iQs2776kRuuWfwFp4dV",
    "t3enhACRxi1ZD7e8ePomVGKn7wp7N9fFJ3r",
    "t3PkLgT71TnF112nSwBToXsD77yNbx2gJJY",
    "t3LQtHUDoe7ZhhvddRv4vnaoNAhCr2f4oFN",
    "t3fNcdBUbycvbCtsD2n9q3LuxG7jVPvFB8L",
    "t3dKojUU2EMjs28nHV84TvkVEUDu1M1FaEx",
    "t3aKH6NiWN1ofGd8c19rZiqgYpkJ3n679ME",
    "t3MEXDF9Wsi63KwpPuQdD6by32Mw2bNTbEa",
    "t3WDhPfik343yNmPTqtkZAoQZeqA83K7Y3f",
    "t3PSn5TbMMAEw7Eu36DYctFezRzpX1hzf3M",
    "t3R3Y5vnBLrEn8L6wFjPjBLnxSUQsKnmFpv",
    "t3Pcm737EsVkGTbhsu2NekKtJeG92mvYyoN",
];

/// List of addresses for the Zcash Foundation funding stream in the Mainnet.
pub(crate) const FUNDING_STREAM_ZF_ADDRESSES: [&str; FUNDING_STREAMS_NUM_ADDRESSES] =
    ["t3dvVE3SQEi7kqNzwrfNePxZ1d4hUyztBA1"; FUNDING_STREAMS_NUM_ADDRESSES];

/// List of addresses for the Major Grants funding stream in the Mainnet.
pub(crate) const FUNDING_STREAM_MG_ADDRESSES: [&str; FUNDING_STREAMS_NUM_ADDRESSES] =
    ["t3XyYW8yBFRuMnfvm5KLGFbEVz25kckZXym"; FUNDING_STREAMS_NUM_ADDRESSES];

/// Number of addresses for each post-NU6 funding stream on Mainnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub(crate) const POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES: usize = 12;

/// List of addresses for the Major Grants post-NU6 funding stream on Mainnet administered by the Financial Privacy Fund (FPF).
pub(crate) const POST_NU6_FUNDING_STREAM_FPF_ADDRESSES: [&str;
    POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES] =
    ["t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"; POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES];

/// Number of addresses for each post-NU6.1 funding stream on Mainnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub(crate) const POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES: usize = 36;

/// List of addresses for the Major Grants post-NU6.1 funding stream on Mainnet administered by the Financial Privacy Fund (FPF).
pub(crate) const POST_NU6_1_FUNDING_STREAM_FPF_ADDRESSES: [&str;
    POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES] =
    ["t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"; POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES];

lazy_static! {
    /// The funding streams for Mainnet as described in:
    /// - [protocol specification §7.10.1][7.10.1]
    /// - [ZIP-1015](https://zips.z.cash/zip-1015)
    /// - [ZIP-214#funding-streams](https://zips.z.cash/zip-0214#funding-streams)
    ///
    /// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    pub(crate) static ref FUNDING_STREAMS: Vec<FundingStreams> = vec![
        FundingStreams {
            height_range: Height(1_046_400)..Height(2_726_400),
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
            height_range: activation_heights::mainnet::NU6_1..Height(4_406_400),
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
