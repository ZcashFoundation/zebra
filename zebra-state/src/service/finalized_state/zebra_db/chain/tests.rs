//! Serialized block-size equivalence tests.

use zebra_chain::{
    block::Block,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};
use zebra_test::vectors::{MAINNET_BLOCKS, TESTNET_BLOCKS};

#[test]
fn parallel_block_size_matches_serialization() {
    let _init_guard = zebra_test::init();

    for bytes in MAINNET_BLOCKS.values().chain(TESTNET_BLOCKS.values()) {
        let block: Block = bytes
            .zcash_deserialize_into()
            .expect("test vector is a block");
        assert_eq!(
            super::block_serialized_size(&block),
            block.zcash_serialized_size()
        );
    }

    let mut block: Block = MAINNET_BLOCKS
        .values()
        .last()
        .expect("mainnet block vectors are nonempty")
        .zcash_deserialize_into()
        .expect("test vector is a block");
    let transaction = block.transactions[0].clone();
    for count in [0, 1, 15, 16, 17, 252, 253, 254] {
        block.transactions = vec![transaction.clone(); count];
        assert_eq!(
            super::block_serialized_size(&block),
            block.zcash_serialized_size(),
            "transaction count {count}"
        );
    }
}
