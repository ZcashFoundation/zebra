//! Constants for Block Subsidy and Funding Streams

use lazy_static::lazy_static;
use std::collections::HashMap;

use zebra_chain::{amount::COIN, block::Height, parameters::Network};

/// An initial period from Genesis to this Height where the block subsidy is gradually incremented. [What is slow-start mining][slow-mining]
///
/// [slow-mining]: https://z.cash/support/faq/#what-is-slow-start-mining
pub const SLOW_START_INTERVAL: Height = Height(20_000);

/// `SlowStartShift()` as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
///
/// This calculation is exact, because `SLOW_START_INTERVAL` is divisible by 2.
pub const SLOW_START_SHIFT: Height = Height(SLOW_START_INTERVAL.0 / 2);

/// The largest block subsidy, used before the first halving.
///
/// We use `25 / 2` instead of `12.5`, so that we can calculate the correct value without using floating-point.
/// This calculation is exact, because COIN is divisible by 2, and the division is done last.
pub const MAX_BLOCK_SUBSIDY: u64 = ((25 * COIN) / 2) as u64;

/// Used as a multiplier to get the new halving interval after Blossom.
///
/// Calculated as `PRE_BLOSSOM_POW_TARGET_SPACING / POST_BLOSSOM_POW_TARGET_SPACING`
/// in the Zcash specification.
pub const BLOSSOM_POW_TARGET_SPACING_RATIO: u64 = 2;

/// Halving is at about every 4 years, before Blossom block time is 150 seconds.
///
/// `(60 * 60 * 24 * 365 * 4) / 150 = 840960`
pub const PRE_BLOSSOM_HALVING_INTERVAL: Height = Height(840_000);

/// After Blossom the block time is reduced to 75 seconds but halving period should remain around 4 years.
pub const POST_BLOSSOM_HALVING_INTERVAL: Height =
    Height((PRE_BLOSSOM_HALVING_INTERVAL.0 as u64 * BLOSSOM_POW_TARGET_SPACING_RATIO) as u32);

/// The first halving height in the testnet is at block height `1_116_000`
/// as specified in [protocol specification §7.10.1][7.10.1]
///
/// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
pub const FIRST_HALVING_TESTNET: Height = Height(1_116_000);

/// The funding stream receiver categories.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FundingStreamReceiver {
    Ecc,
    ZcashFoundation,
    MajorGrants,
}

/// Denominator as described in [protocol specification §7.10.1][7.10.1].
///
/// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
pub const FUNDING_STREAM_RECEIVER_DENOMINATOR: u64 = 100;

lazy_static! {
    /// The numerator for each funding stream receiver category
    /// as described in [protocol specification §7.10.1][7.10.1].
    ///
    /// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    pub static ref FUNDING_STREAM_RECEIVER_NUMERATORS: HashMap<FundingStreamReceiver, u64> = {
        let mut hash_map = HashMap::new();
        hash_map.insert(FundingStreamReceiver::Ecc, 7);
        hash_map.insert(FundingStreamReceiver::ZcashFoundation, 5);
        hash_map.insert(FundingStreamReceiver::MajorGrants, 8);
        hash_map
    };

    /// Start and end Heights for funding streams
    /// as described in [protocol specification §7.10.1][7.10.1].
    ///
    /// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    pub static ref FUNDING_STREAM_HEIGHT_RANGES: HashMap<Network, std::ops::Range<Height>> = {
        let mut hash_map = HashMap::new();
        hash_map.insert(Network::Mainnet, Height(1_046_400)..Height(2_726_400));
        hash_map.insert(Network::Testnet, Height(1_028_500)..Height(2_796_000));
        hash_map
    };

    /// Convenient storage for all addresses, for all receivers and networks
    pub static ref FUNDING_STREAM_ADDRESSES: HashMap<Network, HashMap<FundingStreamReceiver, Vec<String>>> = {
        let mut addresses_by_network = HashMap::with_capacity(2);

        // Mainnet addresses
        let mut mainnet_addresses = HashMap::with_capacity(3);
        mainnet_addresses.insert(FundingStreamReceiver::Ecc, FUNDING_STREAM_ECC_ADDRESSES_MAINNET.iter().map(|a| a.to_string()).collect());
        mainnet_addresses.insert(FundingStreamReceiver::ZcashFoundation, FUNDING_STREAM_ZF_ADDRESSES_MAINNET.iter().map(|a| a.to_string()).collect());
        mainnet_addresses.insert(FundingStreamReceiver::MajorGrants, FUNDING_STREAM_MG_ADDRESSES_MAINNET.iter().map(|a| a.to_string()).collect());
        addresses_by_network.insert(Network::Mainnet, mainnet_addresses);

        // Testnet addresses
        let mut testnet_addresses = HashMap::with_capacity(3);
        testnet_addresses.insert(FundingStreamReceiver::Ecc, FUNDING_STREAM_ECC_ADDRESSES_TESTNET.iter().map(|a| a.to_string()).collect());
        testnet_addresses.insert(FundingStreamReceiver::ZcashFoundation, FUNDING_STREAM_ZF_ADDRESSES_TESTNET.iter().map(|a| a.to_string()).collect());
        testnet_addresses.insert(FundingStreamReceiver::MajorGrants, FUNDING_STREAM_MG_ADDRESSES_TESTNET.iter().map(|a| a.to_string()).collect());
        addresses_by_network.insert(Network::Testnet, testnet_addresses);

        addresses_by_network
    };
}

/// Address change interval function here as a constant
/// as described in [protocol specification §7.10.1][7.10.1].
///
/// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
pub const FUNDING_STREAM_ADDRESS_CHANGE_INTERVAL: Height =
    Height(POST_BLOSSOM_HALVING_INTERVAL.0 / 48);

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

/// List of addresses for the Zcash Foundation funding stream in the Mainnet.
pub const FUNDING_STREAM_ZF_ADDRESSES_MAINNET: [&str; FUNDING_STREAMS_NUM_ADDRESSES_MAINNET] =
    ["t3dvVE3SQEi7kqNzwrfNePxZ1d4hUyztBA1"; FUNDING_STREAMS_NUM_ADDRESSES_MAINNET];

/// List of addresses for the Major Grants funding stream in the Mainnet.
pub const FUNDING_STREAM_MG_ADDRESSES_MAINNET: [&str; FUNDING_STREAMS_NUM_ADDRESSES_MAINNET] =
    ["t3XyYW8yBFRuMnfvm5KLGFbEVz25kckZXym"; FUNDING_STREAMS_NUM_ADDRESSES_MAINNET];

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
