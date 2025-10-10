//! Definitions of Zebra chain constants, including:
//! - slow start interval,
//! - slow start shift

use crate::block::Height;

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

/// Magic numbers used to identify different Zcash networks.
pub mod magics {
    use crate::parameters::network::magic::Magic;

    /// The production mainnet.
    pub const MAINNET: Magic = Magic([0x24, 0xe9, 0x27, 0x64]);
    /// The testnet.
    pub const TESTNET: Magic = Magic([0xfa, 0x1a, 0xf9, 0xbf]);
    /// The regtest, see <https://github.com/zcash/zcash/blob/master/src/chainparams.cpp#L716-L719>
    pub const REGTEST: Magic = Magic([0xaa, 0xe8, 0x3f, 0x5f]);
}

/// The block heights at which network upgrades activate.
pub mod activation_heights {
    /// Network upgrade activation heights for Testnet.
    pub mod testnet {
        use crate::block::Height;

        /// The block height at which `BeforeOverwinter` activates on Testnet.
        pub const BEFORE_OVERWINTER: Height = Height(1);
        /// The block height at which `Overwinter` activates on Testnet.
        pub const OVERWINTER: Height = Height(207_500);
        /// The block height at which `Sapling` activates on Testnet.
        pub const SAPLING: Height = Height(280_000);
        /// The block height at which `Blossom` activates on Testnet.
        pub const BLOSSOM: Height = Height(584_000);
        /// The block height at which `Heartwood` activates on Testnet.
        pub const HEARTWOOD: Height = Height(903_800);
        /// The block height at which `Canopy` activates on Testnet.
        pub const CANOPY: Height = Height(1_028_500);
        /// The block height at which `NU5` activates on Testnet.
        pub const NU5: Height = Height(1_842_420);
        /// The block height at which `NU6` activates on Testnet.
        pub const NU6: Height = Height(2_976_000);
        /// The block height at which `NU6.1` activates on Testnet.
        pub const NU6_1: Height = Height(3_536_500);
    }

    /// Network upgrade activation heights for Mainnet.
    pub mod mainnet {
        use crate::block::Height;

        /// The block height at which `BeforeOverwinter` activates on Mainnet.
        pub const BEFORE_OVERWINTER: Height = Height(1);
        /// The block height at which `Overwinter` activates on Mainnet.
        pub const OVERWINTER: Height = Height(347_500);
        /// The block height at which `Sapling` activates on Mainnet.
        pub const SAPLING: Height = Height(419_200);
        /// The block height at which `Blossom` activates on Mainnet.
        pub const BLOSSOM: Height = Height(653_600);
        /// The block height at which `Heartwood` activates on Mainnet.
        pub const HEARTWOOD: Height = Height(903_000);
        /// The block height at which `Canopy` activates on Mainnet.
        pub const CANOPY: Height = Height(1_046_400);
        /// The block height at which `NU5` activates on Mainnet.
        pub const NU5: Height = Height(1_687_104);
        /// The block height at which `NU6` activates on Mainnet.
        pub const NU6: Height = Height(2_726_400);
        /// The block height at which `NU6.1` activates on Mainnet.
        pub const NU6_1: Height = Height(3_146_400);
    }
}
