//! Regtest genesis block

use hex::FromHex;

use crate::{block::Block, serialization::ZcashDeserializeInto};

/// Genesis block for Regtest, copied from zcashd via `getblock 0 0` RPC method
pub fn regtest_genesis_block() -> Block {
    let regtest_genesis_block_bytes =
        <Vec<u8>>::from_hex(include_str!("genesis/block-regtest-0-000-000.txt").trim())
            .expect("Block bytes are in valid hex representation");

    regtest_genesis_block_bytes
        .zcash_deserialize_into()
        .expect("hard-coded Regtest genesis block data must deserialize successfully")
}
