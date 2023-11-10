use std::{iter, sync::Arc};

use futures::FutureExt;

use tokio::sync::watch;
use zebra_chain::{
    block::Block,
    chain_tip::{ChainTip, NoChainTip},
    parameters::Network::*,
    serialization::ZcashDeserializeInto,
};

use crate::{service::non_finalized_state::NonFinalizedState, ChainTipBlock, WatchReceiver};

use super::super::ChainTipSender;

#[test]
fn current_best_tip_is_initially_empty() {
    let (_sender, non_finalized_state_receiver) = watch::channel(NonFinalizedState::new(Mainnet));
    let non_finalized_state_receiver: WatchReceiver<NonFinalizedState> =
        WatchReceiver::new(non_finalized_state_receiver);

    let (_chain_tip_sender, latest_chain_tip, _chain_tip_change) =
        ChainTipSender::new(None, non_finalized_state_receiver, Mainnet);

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
    let (_sender, non_finalized_state_receiver) = watch::channel(NonFinalizedState::new(Mainnet));
    let non_finalized_state_receiver = WatchReceiver::new(non_finalized_state_receiver);

    let (_chain_tip_sender, _latest_chain_tip, mut chain_tip_change) =
        ChainTipSender::new(None, non_finalized_state_receiver, Mainnet);

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

#[tokio::test]
async fn check_wait_for_blocks() {
    let network = Mainnet;
    let non_finalized_state = NonFinalizedState::new(network);

    let (_non_finalized_state_sender, non_finalized_state_receiver) =
        watch::channel(non_finalized_state.clone());

    let non_finalized_state_receiver: WatchReceiver<NonFinalizedState> =
        WatchReceiver::new(non_finalized_state_receiver);

    // Check that wait_for_blocks works correctly when the write task sets a new finalized tip
    {
        let (mut chain_tip_sender, _latest_chain_tip, chain_tip_change) =
            ChainTipSender::new(None, non_finalized_state_receiver, network);

        let mut blocks_rx = chain_tip_change.spawn_wait_for_blocks();

        let mut prev_hash = None;
        for (_, block_bytes) in zebra_test::vectors::MAINNET_BLOCKS.iter() {
            let tip_block: ChainTipBlock = Arc::new(
                block_bytes
                    .zcash_deserialize_into::<Block>()
                    .expect("block should deserialize"),
            )
            .into();

            // Skip non-continguous blocks
            if let Some(prev_hash) = prev_hash {
                if prev_hash != tip_block.previous_block_hash() {
                    continue;
                }
            }

            prev_hash = Some(tip_block.hash);

            chain_tip_sender.set_finalized_tip(tip_block.clone());

            let block = blocks_rx
                .recv()
                .await
                .expect("should have message in channel");

            assert_eq!(
                block, tip_block.block,
                "blocks should match the send tip block"
            );
        }

        std::mem::drop(chain_tip_sender);
    }
}
