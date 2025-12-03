//! Mainnet-specific constants for block subsidies.

use crate::parameters::{
    network::{Amount, Height, NonNegative},
    subsidy::constants::POST_NU6_FUNDING_STREAM_NUM_BLOCKS,
};

/// The start height of post-NU6 funding streams on Mainnet as described in [ZIP-1015](https://zips.z.cash/zip-1015).
pub(crate) const POST_NU6_FUNDING_STREAM_START_HEIGHT_MAINNET: u32 = 2_726_400;

/// The one-time lockbox disbursement output addresses and amounts expected in the NU6.1 activation block's
/// coinbase transaction on Mainnet.
///
/// See:
///
/// - <https://zips.z.cash/zip-0271#one-timelockboxdisbursement>
/// - <https://zips.z.cash/zip-0214#mainnet-recipients-for-revision-2>
pub(crate) const NU6_1_LOCKBOX_DISBURSEMENTS_MAINNET: [(&str, Amount<NonNegative>); 10] = [(
    "t3ev37Q2uL1sfTsiJQJiWJoFzQpDhmnUwYo",
    EXPECTED_NU6_1_LOCKBOX_DISBURSEMENTS_TOTAL_MAINNET.div_exact(10),
); 10];

/// The expected total amount of the one-time lockbox disbursement on Mainnet.
/// See: <https://zips.z.cash/zip-0271#one-timelockboxdisbursement>.
pub(crate) const EXPECTED_NU6_1_LOCKBOX_DISBURSEMENTS_TOTAL_MAINNET: Amount<NonNegative> =
    Amount::new_from_zec(78_750);

/// The post-NU6 funding stream height range on Mainnet
pub(crate) const POST_NU6_FUNDING_STREAM_START_RANGE_MAINNET: std::ops::Range<Height> =
    Height(POST_NU6_FUNDING_STREAM_START_HEIGHT_MAINNET)
        ..Height(POST_NU6_FUNDING_STREAM_START_HEIGHT_MAINNET + POST_NU6_FUNDING_STREAM_NUM_BLOCKS);

/// Number of addresses for each funding stream in the Mainnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub(crate) const FUNDING_STREAMS_NUM_ADDRESSES_MAINNET: usize = 48;

/// List of addresses for the ECC funding stream in the Mainnet.
pub(crate) const FUNDING_STREAM_ECC_ADDRESSES_MAINNET: [&str;
    FUNDING_STREAMS_NUM_ADDRESSES_MAINNET] = [
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

/// List of addresses for the Zcash Foundation funding stream in the Mainnet.
pub(crate) const FUNDING_STREAM_ZF_ADDRESSES_MAINNET: [&str;
    FUNDING_STREAMS_NUM_ADDRESSES_MAINNET] =
    ["t3dvVE3SQEi7kqNzwrfNePxZ1d4hUyztBA1"; FUNDING_STREAMS_NUM_ADDRESSES_MAINNET];

/// List of addresses for the Major Grants funding stream in the Mainnet.
pub(crate) const FUNDING_STREAM_MG_ADDRESSES_MAINNET: [&str;
    FUNDING_STREAMS_NUM_ADDRESSES_MAINNET] =
    ["t3XyYW8yBFRuMnfvm5KLGFbEVz25kckZXym"; FUNDING_STREAMS_NUM_ADDRESSES_MAINNET];

/// Number of addresses for each post-NU6 funding stream on Mainnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub(crate) const POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES_MAINNET: usize = 12;

/// List of addresses for the Major Grants post-NU6 funding stream on Mainnet administered by the Financial Privacy Fund (FPF).
pub(crate) const POST_NU6_FUNDING_STREAM_FPF_ADDRESSES_MAINNET: [&str;
    POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES_MAINNET] =
    ["t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"; POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES_MAINNET];

/// Number of addresses for each post-NU6.1 funding stream on Mainnet.
/// In the spec ([protocol specification §7.10][7.10]) this is defined as: `fs.addressindex(fs.endheight - 1)`
/// however we know this value beforehand so we prefer to make it a constant instead.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub(crate) const POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES_MAINNET: usize = 36;

/// List of addresses for the Major Grants post-NU6.1 funding stream on Mainnet administered by the Financial Privacy Fund (FPF).
pub(crate) const POST_NU6_1_FUNDING_STREAM_FPF_ADDRESSES_MAINNET: [&str;
    POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES_MAINNET] =
    ["t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"; POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES_MAINNET];
