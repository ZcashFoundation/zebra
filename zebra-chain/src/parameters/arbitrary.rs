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

    /// Generates network upgrades from a reduced set
    pub fn reduced_branch_id_strategy() -> BoxedStrategy<NetworkUpgrade> {
        // Used to give a transaction a consensus branch id that is inconsistent with its block
        // height. The upgrades must be NU5 or later so the resulting v5 transactions are still
        // well-formed and can be assigned a txid by `zcash_primitives`.
        prop_oneof![
            Just(NetworkUpgrade::Nu6),
            Just(NetworkUpgrade::Nu6_1),
            Just(NetworkUpgrade::Nu6_2),
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
