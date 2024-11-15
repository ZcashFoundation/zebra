use crate::{block::Block, serialization::ZcashDeserialize, transaction::Transaction};

use super::vectors::BLOCKS;

#[test]
fn issuance_block() {
    let issuance_block =
        Block::zcash_deserialize(BLOCKS[0].as_ref()).expect("issuance block should deserialize");

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
            assert_eq!(issue_bundle.actions()[0].notes().len(), 1);
        }
    }
}
