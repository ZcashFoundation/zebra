//! Arbitrary implementations for network parameters

use proptest::prelude::*;

use super::NetworkUpgrade;

impl NetworkUpgrade {
    /// Generates network upgrades with [`BranchId`]s
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
