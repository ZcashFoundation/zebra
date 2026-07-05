//! Arbitrary implementations for network parameters

use proptest::prelude::*;

use super::{Network, NetworkUpgrade};

impl NetworkUpgrade {
    /// Generates network upgrades.
    pub fn branch_id_strategy() -> BoxedStrategy<NetworkUpgrade> {
        prop_oneof![
            Just(NetworkUpgrade::Overwinter),
            Just(NetworkUpgrade::Sapling),
            Just(NetworkUpgrade::Blossom),
            Just(NetworkUpgrade::Heartwood),
            Just(NetworkUpgrade::Canopy),
            Just(NetworkUpgrade::Nu5),
            // TODO: add future network upgrades (#1974)
        ]
        .boxed()
    }

    /// Generates network upgrades that are valid for V5+ transactions (Nu5 onward).
    pub fn nu5_branch_id_strategy() -> BoxedStrategy<NetworkUpgrade> {
        prop_oneof![
            Just(NetworkUpgrade::Nu5),
            // TODO: add future network upgrades (#1974)
        ]
        .boxed()
    }

    /// Generates network upgrades that are valid for V6 transactions (NU6.3 onward).
    ///
    /// Does not generate `Nu7`: its consensus branch ID is still a placeholder that
    /// librustzcash does not recognise, so transactions carrying it cannot round-trip
    /// through `to_librustzcash` (which computes txids and auth digests).
    pub fn nu6_3_branch_id_strategy() -> BoxedStrategy<NetworkUpgrade> {
        prop_oneof![
            Just(NetworkUpgrade::Nu6_3),
            // TODO: add Nu7 once its consensus branch ID is set in librustzcash
        ]
        .boxed()
    }

    /// Generates network upgrades from a reduced set
    pub fn reduced_branch_id_strategy() -> BoxedStrategy<NetworkUpgrade> {
        // We use this strategy to test legacy chain
        // TODO: We can add Canopy after we have a NU5 activation height
        prop_oneof![
            Just(NetworkUpgrade::Overwinter),
            Just(NetworkUpgrade::Sapling),
            Just(NetworkUpgrade::Blossom),
            Just(NetworkUpgrade::Heartwood),
        ]
        .boxed()
    }
}

impl Arbitrary for Network {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        prop_oneof![Just(Self::Mainnet), Just(Self::new_default_testnet())].boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
