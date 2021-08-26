use super::super::ChainTipSender;

#[test]
fn best_tip_value_is_initially_empty() {
    let (_chain_tip_sender, receiver) = ChainTipSender::new();

    assert_eq!(*receiver.borrow(), None);
}
