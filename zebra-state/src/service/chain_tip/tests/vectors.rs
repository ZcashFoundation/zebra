use std::iter;

use futures::FutureExt;

use zebra_chain::{
    chain_tip::{ChainTip, NoChainTip},
    parameters::Network::*,
};

use super::super::ChainTipSender;

#[test]
fn current_best_tip_is_initially_empty() {
    let (_chain_tip_sender, latest_chain_tip, _chain_tip_change) =
        ChainTipSender::new(None, &Mainnet);

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
    let (_chain_tip_sender, _latest_chain_tip, mut chain_tip_change) =
        ChainTipSender::new(None, &Mainnet);

    // TODO: use `tokio::task::unconstrained` to avoid spurious waits from tokio's cooperative multitasking
    //       (needs a recent tokio version)
    // See:
    // https://github.com/ZcashFoundation/zebra/pull/2777#discussion_r712488817
    // https://docs.rs/tokio/1.11.0/tokio/task/index.html#cooperative-scheduling
    // https://tokio.rs/blog/2020-04-preemption

    let first = chain_tip_change
        .wait_for_tip_change()
        .now_or_never()
        .transpose()
        .expect("watch sender is not dropped");

    assert_eq!(first, None);

    assert_eq!(chain_tip_change.last_tip_change(), None);

    // try again, just to be sure
    let first = chain_tip_change
        .wait_for_tip_change()
        .now_or_never()
        .transpose()
        .expect("watch sender is not dropped");

    assert_eq!(first, None);

    assert_eq!(chain_tip_change.last_tip_change(), None);

    // also test our manual `Clone` impl
    #[allow(clippy::redundant_clone)]
    let first_clone = chain_tip_change
        .clone()
        .wait_for_tip_change()
        .now_or_never()
        .transpose()
        .expect("watch sender is not dropped");

    assert_eq!(first_clone, None);

    assert_eq!(chain_tip_change.last_tip_change(), None);
}
