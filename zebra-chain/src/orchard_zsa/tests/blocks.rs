use crate::{block::Block, serialization::ZcashDeserialize, transaction::Transaction};

use super::vectors::BLOCKS;

#[test]
fn deserialize_blocks() {
    let issuance_block =
        Block::zcash_deserialize(BLOCKS[1]).expect("issuance block should deserialize");
    let _transfer_block =
        Block::zcash_deserialize(BLOCKS[2]).expect("transfer block should deserialize");
    let _burn_block = Block::zcash_deserialize(BLOCKS[3]).expect("burn block should deserialize");

    for transaction in issuance_block.transactions {
        if let Transaction::V6 {
            orchard_zsa_issue_data,
            ..
        } = transaction.as_ref()
        {
            let issue_bundle = orchard_zsa_issue_data
                .as_ref()
                .expect("V6 transaction in the issuance test block has orchard_zsa_issue_data")
                .inner();

            assert_eq!(issue_bundle.actions().len(), 1);
            assert_eq!(issue_bundle.actions()[0].notes().len(), 2);
        }
    }
}
