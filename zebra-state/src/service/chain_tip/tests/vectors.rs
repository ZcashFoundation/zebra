use super::super::ChainTipSender;

#[test]
fn best_tip_height_is_initially_empty() {
    let (_chain_tip_sender, chain_tip_receiver) = ChainTipSender::new();

    assert_eq!(chain_tip_receiver.best_tip_height(), None);
}
