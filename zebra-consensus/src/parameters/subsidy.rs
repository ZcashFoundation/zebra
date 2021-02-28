//! Constants for Block Subsidy, Funding Streams, and Founders’ Reward

use zebra_chain::{amount::COIN, block::Height};

/// An initial period from Genesis to this Height where the block subsidy is gradually incremented. [What is slow-start mining][slow-mining]
///
/// [slow-mining]: https://z.cash/support/faq/#what-is-slow-start-mining
pub const SLOW_START_INTERVAL: Height = Height(20_000);

/// `SlowStartShift()` as described in [protocol specification §7.7][7.7]
///
/// [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies
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

/// The divisor used to calculate the FoundersFraction.
///
/// Derivation: FOUNDERS_FRACTION_DIVISOR = 1/FoundersFraction
///
/// Usage: founders_reward = block_subsidy / FOUNDERS_FRACTION_DIVISOR
pub const FOUNDERS_FRACTION_DIVISOR: u64 = 5;

/// Function `FounderAddressChangeInterval` as specified in [protocol specification §7.8][7.8]
///
/// Rust trucates the division down, to get ceiling effect we sum 1 to the end of the calculation.
/// We use the main network lenght as both networks are the same in size.
///
/// [7.8]: https://zips.z.cash/protocol/canopy.pdf#foundersreward

pub const FOUNDER_ADDRESS_CHANGE_INTERVAL: u64 =
    ((SLOW_START_SHIFT.0 + PRE_BLOSSOM_HALVING_INTERVAL.0)
        / FOUNDERS_REWARD_ADDRESSES_MAINNET.len() as u32) as u64
        + 1;

/// Mainnet founder adress list as specified in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/canopy.pdf#foundersreward
pub const FOUNDERS_REWARD_ADDRESSES_MAINNET: [&str; 48] = [
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

/// Testnet founder adress list as specified in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/canopy.pdf#foundersreward
pub const FOUNDERS_REWARD_ADDRESSES_TESTNET: [&str; 48] = [
    "t2UNzUUx8mWBCRYPRezvA363EYXyEpHokyi",
    "t2N9PH9Wk9xjqYg9iin1Ua3aekJqfAtE543",
    "t2NGQjYMQhFndDHguvUw4wZdNdsssA6K7x2",
    "t2ENg7hHVqqs9JwU5cgjvSbxnT2a9USNfhy",
    "t2BkYdVCHzvTJJUTx4yZB8qeegD8QsPx8bo",
    "t2J8q1xH1EuigJ52MfExyyjYtN3VgvshKDf",
    "t2Crq9mydTm37kZokC68HzT6yez3t2FBnFj",
    "t2EaMPUiQ1kthqcP5UEkF42CAFKJqXCkXC9",
    "t2F9dtQc63JDDyrhnfpzvVYTJcr57MkqA12",
    "t2LPirmnfYSZc481GgZBa6xUGcoovfytBnC",
    "t26xfxoSw2UV9Pe5o3C8V4YybQD4SESfxtp",
    "t2D3k4fNdErd66YxtvXEdft9xuLoKD7CcVo",
    "t2DWYBkxKNivdmsMiivNJzutaQGqmoRjRnL",
    "t2C3kFF9iQRxfc4B9zgbWo4dQLLqzqjpuGQ",
    "t2MnT5tzu9HSKcppRyUNwoTp8MUueuSGNaB",
    "t2AREsWdoW1F8EQYsScsjkgqobmgrkKeUkK",
    "t2Vf4wKcJ3ZFtLj4jezUUKkwYR92BLHn5UT",
    "t2K3fdViH6R5tRuXLphKyoYXyZhyWGghDNY",
    "t2VEn3KiKyHSGyzd3nDw6ESWtaCQHwuv9WC",
    "t2F8XouqdNMq6zzEvxQXHV1TjwZRHwRg8gC",
    "t2BS7Mrbaef3fA4xrmkvDisFVXVrRBnZ6Qj",
    "t2FuSwoLCdBVPwdZuYoHrEzxAb9qy4qjbnL",
    "t2SX3U8NtrT6gz5Db1AtQCSGjrpptr8JC6h",
    "t2V51gZNSoJ5kRL74bf9YTtbZuv8Fcqx2FH",
    "t2FyTsLjjdm4jeVwir4xzj7FAkUidbr1b4R",
    "t2EYbGLekmpqHyn8UBF6kqpahrYm7D6N1Le",
    "t2NQTrStZHtJECNFT3dUBLYA9AErxPCmkka",
    "t2GSWZZJzoesYxfPTWXkFn5UaxjiYxGBU2a",
    "t2RpffkzyLRevGM3w9aWdqMX6bd8uuAK3vn",
    "t2JzjoQqnuXtTGSN7k7yk5keURBGvYofh1d",
    "t2AEefc72ieTnsXKmgK2bZNckiwvZe3oPNL",
    "t2NNs3ZGZFsNj2wvmVd8BSwSfvETgiLrD8J",
    "t2ECCQPVcxUCSSQopdNquguEPE14HsVfcUn",
    "t2JabDUkG8TaqVKYfqDJ3rqkVdHKp6hwXvG",
    "t2FGzW5Zdc8Cy98ZKmRygsVGi6oKcmYir9n",
    "t2DUD8a21FtEFn42oVLp5NGbogY13uyjy9t",
    "t2UjVSd3zheHPgAkuX8WQW2CiC9xHQ8EvWp",
    "t2TBUAhELyHUn8i6SXYsXz5Lmy7kDzA1uT5",
    "t2Tz3uCyhP6eizUWDc3bGH7XUC9GQsEyQNc",
    "t2NysJSZtLwMLWEJ6MH3BsxRh6h27mNcsSy",
    "t2KXJVVyyrjVxxSeazbY9ksGyft4qsXUNm9",
    "t2J9YYtH31cveiLZzjaE4AcuwVho6qjTNzp",
    "t2QgvW4sP9zaGpPMH1GRzy7cpydmuRfB4AZ",
    "t2NDTJP9MosKpyFPHJmfjc5pGCvAU58XGa4",
    "t29pHDBWq7qN4EjwSEHg8wEqYe9pkmVrtRP",
    "t2Ez9KM8VJLuArcxuEkNRAkhNvidKkzXcjJ",
    "t2D5y7J5fpXajLbGrMBQkFg2mFN8fo3n8cX",
    "t2UV2wr1PTaUiybpkV3FdSdGxUJeZdZztyt",
];
