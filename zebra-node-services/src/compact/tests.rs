//! Tests for reconstructed block evidence.

use zebra_chain::serialization::ZcashDeserializeInto;

use super::*;

fn checked(block: Block) -> Result<ReconstructedBlock, SerializationError> {
    let size = block.zcash_serialized_size();
    ReconstructedBlock::new(Arc::new(block), size)
}

#[test]
fn reconstructed_block_rejects_duplicates_and_merkle_mismatches() {
    let _init_guard = zebra_test::init();
    let block: Block = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();

    let evidence = checked(block.clone()).unwrap();
    let (_, hashes) = evidence.evidence_for(evidence.block()).unwrap();
    assert_eq!(
        hashes.as_ref(),
        block
            .transactions
            .iter()
            .map(|tx| tx.hash())
            .collect::<Vec<_>>()
    );

    let mut mismatch = block.clone();
    Arc::make_mut(&mut mismatch.header).merkle_root = block::merkle::Root([0; 32]);
    assert!(matches!(
        checked(mismatch),
        Err(SerializationError::Parse(error)) if error.contains("Merkle root")
    ));

    // The header commits to the duplicated list, so only the duplicate check can reject it.
    let mut duplicate = block;
    duplicate
        .transactions
        .push(duplicate.transactions[0].clone());
    Arc::make_mut(&mut duplicate.header).merkle_root = duplicate.transactions.iter().collect();
    assert!(matches!(
        checked(duplicate),
        Err(SerializationError::Parse(error)) if error.contains("duplicate")
    ));
}

#[test]
fn reconstructed_block_rejects_empty_and_oversized_blocks() {
    let _init_guard = zebra_test::init();
    let block: Block = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();

    let mut empty = block.clone();
    empty.transactions.clear();
    assert!(matches!(
        checked(empty),
        Err(SerializationError::Parse(error)) if error.contains("no transactions")
    ));

    // Repeating one transaction is a cheap way past the limit: the size check runs before the
    // duplicate check, and the error shows which one rejected the block.
    let mut oversized = block;
    let coinbase = oversized.transactions[0].clone();
    let copies = usize::try_from(MAX_BLOCK_BYTES).unwrap() / coinbase.zcash_serialized_size() + 1;
    oversized.transactions = vec![coinbase; copies];
    assert!(matches!(
        checked(oversized),
        Err(SerializationError::Parse(error)) if error.contains("size limit")
    ));
}
