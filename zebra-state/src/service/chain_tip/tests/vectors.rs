use std::iter;

use zebra_chain::chain_tip::{ChainTip, NoChainTip};

use super::super::ChainTipSender;

#[test]
fn best_tip_is_initially_empty() {
    let (_chain_tip_sender, chain_tip_receiver) = ChainTipSender::new(None);

    assert_eq!(chain_tip_receiver.best_tip_height(), None);
    assert_eq!(chain_tip_receiver.best_tip_hash(), None);
    assert_eq!(
        chain_tip_receiver.best_tip_mined_transaction_ids(),
        iter::empty().collect()
    );
}

#[test]
fn empty_chain_tip_is_empty() {
    let chain_tip_receiver = NoChainTip;

    assert_eq!(chain_tip_receiver.best_tip_height(), None);
    assert_eq!(chain_tip_receiver.best_tip_hash(), None);
    assert_eq!(
        chain_tip_receiver.best_tip_mined_transaction_ids(),
        iter::empty().collect()
    );
}
