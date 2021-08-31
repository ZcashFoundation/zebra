use std::{env, sync::Arc};

use futures::FutureExt;
use proptest::prelude::*;
use proptest_derive::Arbitrary;

use zebra_chain::{block::Block, chain_tip::ChainTip};

use crate::{
    service::chain_tip::{ChainTipBlock, ChainTipSender, TipAction::*},
    FinalizedBlock,
};

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
        tip_updates in any::<Vec<BlockUpdate>>(),
    ) {
        let (mut chain_tip_sender, latest_chain_tip, mut chain_tip_change) = ChainTipSender::new(None);

        let mut latest_finalized_tip = None;
        let mut latest_non_finalized_tip = None;
        let mut seen_non_finalized_tip = false;

        for update in tip_updates {
            match update {
                BlockUpdate::Finalized(block) => {
                    let chain_tip = block.clone().map(FinalizedBlock::from).map(ChainTipBlock::from);
                    chain_tip_sender.set_finalized_tip(chain_tip.clone());
                    if let Some(block) = block {
                        latest_finalized_tip = Some((chain_tip, block));
                    }
                }
                BlockUpdate::NonFinalized(block) => {
                    let chain_tip = block.clone().map(FinalizedBlock::from).map(ChainTipBlock::from);
                    chain_tip_sender.set_best_non_finalized_tip(chain_tip.clone());
                    if let Some(block) = block {
                        latest_non_finalized_tip = Some((chain_tip, block));
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

        let chain_tip_height = expected_tip
            .as_ref()
            .and_then(|(chain_tip, _block)| chain_tip.as_ref())
            .map(|chain_tip| chain_tip.height);
        let expected_height = expected_tip.as_ref().and_then(|(_chain_tip, block)| block.coinbase_height());
        prop_assert_eq!(latest_chain_tip.best_tip_height(), chain_tip_height);
        prop_assert_eq!(latest_chain_tip.best_tip_height(), expected_height);

        let chain_tip_hash = expected_tip
            .as_ref()
            .and_then(|(chain_tip, _block)| chain_tip.as_ref())
            .map(|chain_tip| chain_tip.hash);
        let expected_hash = expected_tip.as_ref().map(|(_chain_tip, block)| block.hash());
        prop_assert_eq!(latest_chain_tip.best_tip_hash(), chain_tip_hash);
        prop_assert_eq!(latest_chain_tip.best_tip_hash(), expected_hash);

        let chain_tip_transaction_ids = expected_tip
            .as_ref()
            .and_then(|(chain_tip, _block)| chain_tip.as_ref())
            .map(|chain_tip| chain_tip.transaction_hashes.clone())
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

        prop_assert_eq!(
            chain_tip_change
                .next()
                .now_or_never()
                .transpose()
                .expect("watch sender is not dropped"),
            expected_tip.map(|(_chain_tip, block)| Grow { block: block.into() })
        );
    }
}

#[derive(Arbitrary, Clone, Debug)]
enum BlockUpdate {
    Finalized(Option<Arc<Block>>),
    NonFinalized(Option<Arc<Block>>),
}
