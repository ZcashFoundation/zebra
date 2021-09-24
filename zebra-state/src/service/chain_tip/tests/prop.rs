use std::{collections::HashSet, env, sync::Arc};

use futures::FutureExt;
use proptest::prelude::*;
use proptest_derive::Arbitrary;

use zebra_chain::{
    block::Block,
    chain_tip::ChainTip,
    fmt::{DisplayToDebug, SummaryDebug},
    parameters::{Network, NetworkUpgrade},
};

use crate::service::chain_tip::{ChainTipBlock, ChainTipSender, TipAction};

use TipChangeCheck::*;

const DEFAULT_BLOCK_VEC_PROPTEST_CASES: u32 = 4;

proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BLOCK_VEC_PROPTEST_CASES))
    )]

    /// Check that the best tip uses the non-finalized tip if available,
    /// or otherwise the finalized tip.
    #[test]
    fn best_tip_is_latest_non_finalized_then_latest_finalized(
        tip_updates in any::<SummaryDebug<Vec<(BlockUpdate, TipChangeCheck)>>>(),
        network in any::<Network>(),
    ) {
        let (mut chain_tip_sender, latest_chain_tip, mut chain_tip_change) = ChainTipSender::new(None, network);

        let mut latest_finalized_tip = None;
        let mut latest_non_finalized_tip = None;
        let mut seen_non_finalized_tip = false;

        let mut pending_action = None;
        let mut chain_hashes = HashSet::new();

        for (update, tip_change_check) in tip_updates {
            // do the update
            match update {
                BlockUpdate::Finalized(block) => {
                    let chain_tip = block.clone().map(|block| ChainTipBlock::from(block.0));

                    if let Some(chain_tip) = chain_tip.clone() {
                        if chain_hashes.contains(&chain_tip.hash) {
                            // skip duplicate blocks - they are rejected by zebra-state
                            continue;
                        }
                        chain_hashes.insert(chain_tip.hash);
                    }

                    chain_tip_sender.set_finalized_tip(chain_tip.clone());

                    if let Some(block) = block {
                        latest_finalized_tip = Some((chain_tip.unwrap(), block));
                    }
                }
                BlockUpdate::NonFinalized(block) => {
                    let chain_tip = block.clone().map(|block| ChainTipBlock::from(block.0));

                    if let Some(chain_tip) = chain_tip.clone() {
                        if chain_hashes.contains(&chain_tip.hash) {
                            // skip duplicate blocks - they are rejected by zebra-state
                            continue;
                        }
                        chain_hashes.insert(chain_tip.hash);
                    }

                    chain_tip_sender.set_best_non_finalized_tip(chain_tip.clone());

                    if let Some(block) = block {
                        latest_non_finalized_tip = Some((chain_tip.unwrap(), block));
                        seen_non_finalized_tip = true;
                    }
                }
            }

            // check the results
            let expected_tip = if seen_non_finalized_tip {
                latest_non_finalized_tip.clone()
            } else {
                latest_finalized_tip.clone()
            };

            let chain_tip_height = expected_tip
                .as_ref()
                .map(|(chain_tip, _block)| chain_tip.height);
            let expected_height = expected_tip.as_ref().and_then(|(_chain_tip, block)| block.coinbase_height());
            prop_assert_eq!(latest_chain_tip.best_tip_height(), chain_tip_height);
            prop_assert_eq!(latest_chain_tip.best_tip_height(), expected_height);

            let chain_tip_hash = expected_tip
                .as_ref()
                .map(|(chain_tip, _block)| chain_tip.hash);
            let expected_hash = expected_tip.as_ref().map(|(_chain_tip, block)| block.hash());
            prop_assert_eq!(latest_chain_tip.best_tip_hash(), chain_tip_hash);
            prop_assert_eq!(latest_chain_tip.best_tip_hash(), expected_hash);

            let chain_tip_transaction_ids = expected_tip
                .as_ref()
                .map(|(chain_tip, _block)| chain_tip.transaction_hashes.clone())
                .unwrap_or_else(|| Arc::new([]));
            let expected_transaction_ids = expected_tip
                .as_ref()
                .iter()
                .flat_map(|(_chain_tip, block)| block.transactions.clone())
                .map(|transaction| transaction.hash())
                .collect();
            prop_assert_eq!(
                latest_chain_tip.best_tip_mined_transaction_ids(),
                chain_tip_transaction_ids
            );
            prop_assert_eq!(
                latest_chain_tip.best_tip_mined_transaction_ids(),
                expected_transaction_ids
            );

            let old_last_change_hash = chain_tip_change.last_change_hash;

            let new_action =
                expected_tip.and_then(|(chain_tip, block)| {
                    if Some(chain_tip.hash) == old_last_change_hash {
                        // some updates don't do anything, so there's no new action
                        None
                    } else if Some(chain_tip.previous_block_hash) != old_last_change_hash ||
                        NetworkUpgrade::is_activation_height(network, chain_tip.height) {
                            Some(TipAction::reset_with(block.0.into()))
                    } else {
                            Some(TipAction::grow_with(block.0.into()))
                    }
                });

            let expected_action = match (pending_action.clone(), new_action.clone()) {
                (Some(_pending_action), Some(new_action)) => Some(new_action.into_reset()),
                (None, new_action) => new_action,
                (pending_action, None) => pending_action,
            };

            match tip_change_check {
                WaitFor => {
                    // TODO: use `unconstrained` to avoid spurious cooperative multitasking waits
                    //       (needs a recent tokio version)
                    // See:
                    // https://github.com/ZcashFoundation/zebra/pull/2777#discussion_r712488817
                    // https://docs.rs/tokio/1.11.0/tokio/task/index.html#cooperative-scheduling
                    // https://tokio.rs/blog/2020-04-preemption
                    prop_assert_eq!(
                        chain_tip_change
                            .wait_for_tip_change()
                            .now_or_never()
                            .transpose()
                            .expect("watch sender is not dropped"),
                        expected_action,
                        "\n\
                         unexpected wait_for_tip_change TipAction\n\
                         new_action: {:?}\n\
                         pending_action: {:?}\n\
                         old last_change_hash: {:?}\n\
                         new last_change_hash: {:?}",
                        new_action,
                        pending_action,
                        old_last_change_hash,
                        chain_tip_change.last_change_hash
                    );
                    pending_action = None;
                }

                Last => {
                    prop_assert_eq!(
                        chain_tip_change.last_tip_change(),
                        expected_action,
                        "\n\
                         unexpected last_tip_change TipAction\n\
                         new_action: {:?}\n\
                         pending_action: {:?}\n\
                         old last_change_hash: {:?}\n\
                         new last_change_hash: {:?}",
                        new_action,
                        pending_action,
                        old_last_change_hash,
                        chain_tip_change.last_change_hash
                    );
                    pending_action = None;
                }

                Skip => {
                    pending_action = expected_action;
                }
            }
        }
    }
}

/// Block update test cases for [`ChainTipSender`]
#[derive(Arbitrary, Clone, Debug)]
enum BlockUpdate {
    Finalized(Option<DisplayToDebug<Arc<Block>>>),
    NonFinalized(Option<DisplayToDebug<Arc<Block>>>),
}

/// Block update checks for [`ChainTipChange`]
#[derive(Arbitrary, Copy, Clone, Debug)]
enum TipChangeCheck {
    /// Check that `wait_for_tip_change` returns the correct result
    WaitFor,

    /// Check that `last_tip_change` returns the correct result
    Last,

    /// Don't check this case (causes a `TipAction::Reset` in the next check)
    Skip,
}
