use std::{
    convert::TryInto,
    net::{SocketAddr, SocketAddrV6},
};

use proptest::{arbitrary::any, arbitrary::Arbitrary, collection::vec, prelude::*};

use zebra_chain::{block, transaction};

use super::{
    addr::{canonical_socket_addr, ipv6_mapped_socket_addr},
    types::{PeerServices, Version},
    InventoryHash, Message,
};

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

impl Arbitrary for PeerServices {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        any::<u64>()
            .prop_map(PeerServices::from_bits_truncate)
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Message {
    /// Create a strategy that only generates [`Message::Inv`] messages.
    pub fn inv_strategy() -> BoxedStrategy<Self> {
        any::<Vec<InventoryHash>>().prop_map(Message::Inv).boxed()
    }

    /// Create a strategy that only generates [`Message::GetData`] messages.
    pub fn get_data_strategy() -> BoxedStrategy<Self> {
        any::<Vec<InventoryHash>>()
            .prop_map(Message::GetData)
            .boxed()
    }
}

impl Arbitrary for Version {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        prop_oneof![170_002_u32..=170_015, 0_u32..]
            .prop_map(Version)
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// Returns a random canonical Zebra `SocketAddr`.
///
/// See [`canonical_ip_addr`] for details.
pub fn canonical_socket_addr_strategy() -> impl Strategy<Value = SocketAddr> {
    any::<SocketAddr>().prop_map(canonical_socket_addr)
}

/// Returns a random `SocketAddrV6` for use in `addr` (v1) Zcash network messages.
///
/// See [`canonical_ip_addr`] for details.
pub fn addr_v1_ipv6_mapped_socket_addr_strategy() -> impl Strategy<Value = SocketAddrV6> {
    any::<SocketAddr>().prop_map(ipv6_mapped_socket_addr)
}
