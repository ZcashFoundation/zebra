use std::{env, sync::Arc};

use proptest::prelude::*;
use proptest_derive::Arbitrary;

use zebra_chain::{block::Block, chain_tip::ChainTip};

use super::super::ChainTipSender;

const DEFAULT_BLOCK_VEC_PROPTEST_CASES: u32 = 4;

proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BLOCK_VEC_PROPTEST_CASES))
    )]

    #[test]
    fn best_tip_is_highest_of_latest_finalized_and_non_finalized_heights(
        height_updates in any::<Vec<BlockUpdate>>(),
    ) {
        let (mut chain_tip_sender, chain_tip_receiver) = ChainTipSender::new(None);

        let mut latest_finalized_tip = None;
        let mut latest_non_finalized_tip = None;
        let mut seen_non_finalized_tip = false;

        for update in height_updates {
            match update {
                BlockUpdate::Finalized(block) => {
                    chain_tip_sender.set_finalized_tip(block.clone());
                    latest_finalized_tip = block;
                }
                BlockUpdate::NonFinalized(block) => {
                    chain_tip_sender.set_best_non_finalized_tip(block.clone());
                    if block.is_some() {
                        latest_non_finalized_tip = block;
                        seen_non_finalized_tip = true;
                    }
                }
            }
        }

        let expected_tip = if seen_non_finalized_tip {
            latest_non_finalized_tip
        } else {
            latest_finalized_tip
        };
        let expected_height = expected_tip.and_then(|block| block.coinbase_height());

        prop_assert_eq!(chain_tip_receiver.best_tip_height(), expected_height);
    }
}

#[derive(Arbitrary, Clone, Debug)]
enum BlockUpdate {
    Finalized(Option<Arc<Block>>),
    NonFinalized(Option<Arc<Block>>),
}
