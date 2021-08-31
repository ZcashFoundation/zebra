use std::iter;

use futures::FutureExt;

use zebra_chain::chain_tip::{ChainTip, NoChainTip};

use super::super::ChainTipSender;

#[test]
fn current_best_tip_is_initially_empty() {
    let (_chain_tip_sender, latest_chain_tip, _chain_tip_change) = ChainTipSender::new(None);

    assert_eq!(latest_chain_tip.best_tip_height(), None);
    assert_eq!(latest_chain_tip.best_tip_hash(), None);
    assert_eq!(
        latest_chain_tip.best_tip_mined_transaction_ids(),
        iter::empty().collect()
    );
}

#[test]
fn empty_latest_chain_tip_is_empty() {
    let latest_chain_tip = NoChainTip;

    assert_eq!(latest_chain_tip.best_tip_height(), None);
    assert_eq!(latest_chain_tip.best_tip_hash(), None);
    assert_eq!(
        latest_chain_tip.best_tip_mined_transaction_ids(),
        iter::empty().collect()
    );
}

#[test]
fn chain_tip_change_is_initially_not_ready() {
    let (_chain_tip_sender, _latest_chain_tip, mut chain_tip_change) = ChainTipSender::new(None);

    let first = chain_tip_change
        .next()
        .now_or_never()
        .transpose()
        .expect("watch sender is not dropped");

    assert_eq!(first, None);

    // try again, just to be sure
    let first = chain_tip_change
        .next()
        .now_or_never()
        .transpose()
        .expect("watch sender is not dropped");

    assert_eq!(first, None);

    // also test our manual `Clone` impl
    let first_clone = chain_tip_change
        .clone()
        .next()
        .now_or_never()
        .transpose()
        .expect("watch sender is not dropped");

    assert_eq!(first_clone, None);
}
