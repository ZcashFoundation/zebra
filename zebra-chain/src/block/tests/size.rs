//! Block byte counts compose exact transaction sizes and their count prefix.

use std::sync::Arc;

use crate::{
    block::{genesis::regtest_genesis_block, Block},
    serialization::{FakeWriter, ZcashDeserialize, ZcashSerialize},
};

fn assert_size(block: &Block) {
    let mut independent = FakeWriter(0);
    block.zcash_serialize(&mut independent).unwrap();
    assert_eq!(block.zcash_serialized_size(), independent.0);
    assert_eq!(
        Arc::new(block.clone()).zcash_serialized_size(),
        independent.0
    );
}

#[test]
fn decoded_block_sizes_match_independent_serialization() {
    let _init_guard = zebra_test::init();
    for bytes in zebra_test::vectors::BLOCKS.iter() {
        let block = Block::zcash_deserialize(&bytes[..]).unwrap();
        assert_eq!(block.zcash_serialized_size(), bytes.len());
        assert_size(&block);
    }
    assert_size(&regtest_genesis_block());
}

#[test]
fn block_sizes_include_compactsize_transaction_count_boundaries() {
    let _init_guard = zebra_test::init();
    let mut block = regtest_genesis_block().as_ref().clone();
    let transaction = block.transactions[0].clone();
    for count in [0, 1, 252, 253, 65_535, 65_536] {
        block.transactions = vec![transaction.clone(); count];
        assert_size(&block);
    }
}
