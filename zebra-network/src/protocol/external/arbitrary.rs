use std::convert::TryInto;

use proptest::{arbitrary::any, arbitrary::Arbitrary, collection::vec, prelude::*};

use super::{types::PeerServices, InventoryHash};

use zebra_chain::{block, transaction};

impl InventoryHash {
    /// Generate a proptest strategy for [`InventoryHash::Error`]s.
    pub fn error_strategy() -> BoxedStrategy<Self> {
        Just(InventoryHash::Error).boxed()
    }

    /// Generate a proptest strategy for [`InventoryHash::Tx`] hashes.
    pub fn tx_strategy() -> BoxedStrategy<Self> {
        // using any::<transaction::Hash> causes a trait impl error
        // when building the zebra-network crate separately
        (any::<[u8; 32]>())
            .prop_map(transaction::Hash)
            .prop_map(InventoryHash::Tx)
            .boxed()
    }

    /// Generate a proptest strategy for [`InventotryHash::Block`] hashes.
    pub fn block_strategy() -> BoxedStrategy<Self> {
        (any::<[u8; 32]>())
            .prop_map(block::Hash)
            .prop_map(InventoryHash::Block)
            .boxed()
    }

    /// Generate a proptest strategy for [`InventoryHash::FilteredBlock`] hashes.
    pub fn filtered_block_strategy() -> BoxedStrategy<Self> {
        (any::<[u8; 32]>())
            .prop_map(block::Hash)
            .prop_map(InventoryHash::FilteredBlock)
            .boxed()
    }

    /// Generate a proptest strategy for [`InventoryHash::Wtx`] hashes.
    pub fn wtx_strategy() -> BoxedStrategy<Self> {
        vec(any::<u8>(), 64)
            .prop_map(|bytes| InventoryHash::Wtx(bytes.try_into().unwrap()))
            .boxed()
    }

    /// Generate a proptest strategy for [`InventoryHash`] variants of the smallest serialized size.
    pub fn smallest_types_strategy() -> BoxedStrategy<Self> {
        InventoryHash::arbitrary()
            .prop_filter(
                "inventory type is not one of the smallest",
                |inventory_hash| match inventory_hash {
                    InventoryHash::Error
                    | InventoryHash::Tx(_)
                    | InventoryHash::Block(_)
                    | InventoryHash::FilteredBlock(_) => true,
                    InventoryHash::Wtx(_) => false,
                },
            )
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
            Self::wtx_strategy(),
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
