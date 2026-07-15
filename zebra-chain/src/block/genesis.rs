//! Hard-coded genesis blocks.

use std::sync::Arc;

use hex::FromHex;

use crate::{
    block::Block,
    parameters::{Network, NetworkKind},
    serialization::ZcashDeserializeInto,
};

/// Genesis block for Regtest, copied from zcashd via `getblock 0 0` RPC method
pub fn regtest_genesis_block() -> Arc<Block> {
    genesis_block_from_hex(
        include_str!("genesis/block-regtest-0-000-000.txt"),
        "Regtest",
    )
}

/// The genesis block for `network`: the hard-coded Mainnet, Testnet, or
/// Regtest genesis block whose hash matches [`Network::genesis_hash`].
///
/// Selected by hash rather than network kind, because a configured testnet may
/// declare any of the well-known genesis hashes (test harnesses often build
/// testnets on the Regtest genesis block). When no hard-coded block matches (a
/// custom genesis), the network kind's default block is returned; callers that
/// commit the block must check its hash against [`Network::genesis_hash`]
/// first (the startup genesis commit does), and skip the commit when it
/// doesn't match.
pub fn genesis_block(network: &Network) -> Arc<Block> {
    let mainnet =
        || genesis_block_from_hex(include_str!("genesis/block-main-0-000-000.txt"), "Mainnet");
    let testnet =
        || genesis_block_from_hex(include_str!("genesis/block-test-0-000-000.txt"), "Testnet");

    let genesis_hash = network.genesis_hash();
    for candidate in [mainnet(), testnet(), regtest_genesis_block()] {
        if candidate.hash() == genesis_hash {
            return candidate;
        }
    }

    match network.kind() {
        NetworkKind::Mainnet => mainnet(),
        NetworkKind::Testnet => testnet(),
        NetworkKind::Regtest => regtest_genesis_block(),
    }
}

/// Deserializes a hard-coded hex genesis block, panicking on malformed
/// hard-coded data.
fn genesis_block_from_hex(hex: &str, network_name: &str) -> Arc<Block> {
    let bytes = <Vec<u8>>::from_hex(hex.trim())
        .unwrap_or_else(|_| panic!("hard-coded {network_name} genesis block hex is valid"));

    bytes
        .zcash_deserialize_into()
        .map(Arc::new)
        .unwrap_or_else(|_| {
            panic!("hard-coded {network_name} genesis block data must deserialize successfully")
        })
}
