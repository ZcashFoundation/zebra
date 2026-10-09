//! Borrowed created-output lookup retains transparent spend ordering and duplicate rejection.

use std::sync::Arc;

use zebra_chain::{block::Height, transaction};

use super::*;
use crate::tests::setup::new_state_with_mainnet_genesis;

#[test]
fn spent_index_precedes_created_index_and_intra_block_order_is_preserved() {
    let _init_guard = zebra_test::init();
    let (finalized, _, _) = new_state_with_mainnet_genesis();
    let outpoint = transparent::OutPoint {
        hash: transaction::Hash([1; 32]),
        index: 0,
    };
    let output = transparent::OrderedUtxo::new(
        transparent::Output {
            value: 1u64.try_into().unwrap(),
            lock_script: transparent::Script::new(&[]),
        },
        Height(1),
        0,
    );
    let mut created = CreatedUtxos::default();
    created.insert(outpoint, Arc::new(output.clone()));
    let snapshot = created.clone();
    #[cfg(feature = "indexer")]
    let spender = transaction::Hash([2; 32]);
    #[cfg(not(feature = "indexer"))]
    let spender = ();
    let spent = HashMap::from([(outpoint, spender)]);

    assert_eq!(
        transparent_spend_chain_order(
            outpoint,
            1,
            &HashMap::new(),
            &created,
            &spent,
            &finalized.db
        ),
        Err(DuplicateTransparentSpend {
            outpoint,
            location: "the non-finalized chain",
        })
    );
    assert_eq!(
        transparent_spend_chain_order(
            outpoint,
            1,
            &HashMap::new(),
            &snapshot,
            &HashMap::new(),
            &finalized.db
        ),
        Ok(output.clone())
    );

    // Intra-block outputs keep precedence, including the early-spend error's ordering.
    let new_outputs = HashMap::from([(outpoint, output.clone())]);
    assert_eq!(
        transparent_spend_chain_order(outpoint, 0, &new_outputs, &created, &spent, &finalized.db),
        Err(EarlyTransparentSpend { outpoint })
    );
    assert_eq!(
        transparent_spend_chain_order(outpoint, 1, &new_outputs, &created, &spent, &finalized.db),
        Ok(output)
    );
}
