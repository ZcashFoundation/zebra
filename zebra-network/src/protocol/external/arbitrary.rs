use proptest::{arbitrary::any, arbitrary::Arbitrary, prelude::*};

use super::{types::PeerServices, InventoryHash};

use zebra_chain::{block, transaction};

impl InventoryHash {
    /// Generate a proptest strategy for Inv Errors
    pub fn error_strategy() -> BoxedStrategy<Self> {
        Just(InventoryHash::Error).boxed()
    }

    /// Generate a proptest strategy for Inv Tx hashes
    pub fn tx_strategy() -> BoxedStrategy<Self> {
        // using any::<transaction::Hash> causes a trait impl error
        // when building the zebra-network crate separately
        (any::<[u8; 32]>())
            .prop_map(transaction::Hash)
            .prop_map(InventoryHash::Tx)
            .boxed()
    }

    /// Generate a proptest strategy for Inv Block hashes
    pub fn block_strategy() -> BoxedStrategy<Self> {
        (any::<[u8; 32]>())
            .prop_map(block::Hash)
            .prop_map(InventoryHash::Block)
            .boxed()
    }

    /// Generate a proptest strategy for Inv FilteredBlock hashes
    pub fn filtered_block_strategy() -> BoxedStrategy<Self> {
        (any::<[u8; 32]>())
            .prop_map(block::Hash)
            .prop_map(InventoryHash::FilteredBlock)
            .boxed()
    }
}

impl Arbitrary for InventoryHash {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        prop_oneof![
            Self::error_strategy(),
            Self::tx_strategy(),
            Self::block_strategy(),
            Self::filtered_block_strategy(),
        ]
        .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(any(test, feature = "proptest-impl"))]
impl Arbitrary for PeerServices {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        any::<u64>()
            .prop_map(PeerServices::from_bits_truncate)
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
