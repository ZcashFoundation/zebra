use super::super::BestTipHeight;

#[test]
fn best_tip_value_is_initially_empty() {
    let (_best_tip_height, receiver) = BestTipHeight::new();

    assert_eq!(*receiver.borrow(), None);
}
