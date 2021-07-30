use proptest::prelude::*;
use proptest_derive::Arbitrary;

use zebra_chain::block;

use super::super::BestTipHeight;

proptest! {
    #[test]
    fn best_tip_value_is_heighest_of_latest_finalized_and_non_finalized_heights(
        height_updates in any::<Vec<HeightUpdate>>(),
    ) {
        let (mut best_tip_height, receiver) = BestTipHeight::new();

        let mut latest_finalized_height = None;
        let mut latest_non_finalized_height = None;

        for update in height_updates {
            match update {
                HeightUpdate::Finalized(height) => {
                    best_tip_height.set_finalized_height(height);
                    latest_finalized_height = Some(height);
                }
                HeightUpdate::NonFinalized(height) => {
                    best_tip_height.set_best_non_finalized_height(height);
                    latest_non_finalized_height = height;
                }
            }
        }

        let expected_height = match (latest_finalized_height, latest_non_finalized_height) {
            (Some(finalized_height), Some(non_finalized_height)) => {
                Some(finalized_height.max(non_finalized_height))
            }
            (finalized_height, None) => finalized_height,
            (None, non_finalized_height) => non_finalized_height,
        };

        prop_assert_eq!(*receiver.borrow(), expected_height);
    }
}

#[derive(Arbitrary, Clone, Copy, Debug)]
enum HeightUpdate {
    Finalized(block::Height),
    NonFinalized(Option<block::Height>),
}
