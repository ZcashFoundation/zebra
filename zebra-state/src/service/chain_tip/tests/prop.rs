use proptest::prelude::*;
use proptest_derive::Arbitrary;

use zebra_chain::{block, chain_tip::ChainTip};

use super::super::ChainTipSender;

proptest! {
    #[test]
    fn best_tip_is_highest_of_latest_finalized_and_non_finalized_heights(
        height_updates in any::<Vec<HeightUpdate>>(),
    ) {
        let (mut chain_tip_sender, chain_tip_receiver) = ChainTipSender::new(None);

        let mut latest_finalized_height = None;
        let mut latest_non_finalized_height = None;
        let mut seen_non_finalized_tip = false;

        for update in height_updates {
            match update {
                HeightUpdate::Finalized(height) => {
                    chain_tip_sender.set_finalized_height(height);
                    latest_finalized_height = height;
                }
                HeightUpdate::NonFinalized(height) => {
                    chain_tip_sender.set_best_non_finalized_height(height);
                    latest_non_finalized_height = height;
                    seen_non_finalized_tip = true;
                }
            }
        }

        let expected_height = if seen_non_finalized_tip {
            latest_non_finalized_height
        } else {
            latest_finalized_height
        };

        prop_assert_eq!(chain_tip_receiver.best_tip_height(), expected_height);
    }
}

#[derive(Arbitrary, Clone, Copy, Debug)]
enum HeightUpdate {
    Finalized(Option<block::Height>),
    NonFinalized(Option<block::Height>),
}
