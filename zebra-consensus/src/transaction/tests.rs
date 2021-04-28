use zebra_chain::{
    block::Block,
    serialization::ZcashDeserializeInto,
    transaction::{arbitrary::transaction_to_fake_v5, Transaction},
};

use crate::error::TransactionError::*;
use color_eyre::eyre::Report;

#[test]
fn v5_fake_transactions() -> Result<(), Report> {
    zebra_test::init();

    // get all the blocks we have available
    for original_bytes in zebra_test::vectors::BLOCKS.iter() {
        let original_block = original_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        // convert all transactions from the block to V5
        let transactions: Vec<Transaction> = original_block
            .transactions
            .iter()
            .map(AsRef::as_ref)
            .map(transaction_to_fake_v5)
            .map(Into::into)
            .collect();

        // after the conversion some transactions end up with no inputs nor outputs.
        for transaction in transactions {
            match super::check::has_inputs_and_outputs(&transaction) {
                Err(e) => {
                    if e != NoInputs && e != NoOutputs {
                        panic!("error must be NoInputs or NoOutputs")
                    }
                }
                Ok(()) => (),
            };

            // make sure there are no joinsplits nor spends in coinbase
            super::check::coinbase_tx_no_prevout_joinsplit_spend(&transaction)?;

            // validate the sapling shielded data
            match transaction {
                Transaction::V5 {
                    sapling_shielded_data,
                    ..
                } => {
                    if let Some(s) = sapling_shielded_data {
                        super::check::sapling_balances_match(&s)?;

                        for spend in s.spends_per_anchor() {
                            super::check::spend_cv_rk_not_small_order(&spend)?
                        }
                        for output in s.outputs() {
                            super::check::output_cv_epk_not_small_order(&output)?;
                        }
                    }
                }
                _ => panic!("we should have no tx other than 5"),
            }
        }
    }
    Ok(())
}
