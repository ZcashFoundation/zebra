use crate::{block::Block, serialization::ZcashDeserialize};

use super::vectors::BLOCKS;

#[test]
fn issuance_block() {
    let issuance_block =
        Block::zcash_deserialize(BLOCKS[0].as_ref()).expect("issuance block should deserialize");
}
