//! Pending output delivery without creating unused responses.

use futures::FutureExt;

use zebra_chain::{block::Height, transparent};

use super::PendingUtxos;
use crate::Response;

#[test]
fn pending_outputs_are_only_created_for_waiters() {
    let _init_guard = zebra_test::init();
    let matching = transparent::OutPoint {
        hash: [1; 32].into(),
        index: 0,
    };
    let unrelated = transparent::OutPoint {
        hash: [2; 32].into(),
        index: 0,
    };
    let output = transparent::OrderedUtxo::new(
        transparent::Output::new(1.try_into().unwrap(), transparent::Script::new(&[0x51])),
        Height(1),
        0,
    );
    let outputs = [(matching, output.clone())].into_iter().collect();
    let mut pending = PendingUtxos::default();
    pending.check_against_ordered(&outputs);
    assert_eq!(pending.len(), 0);
    pending.respond(&matching, || panic!("no waiter needs an output"));

    let unrelated_response = pending.queue(unrelated);
    pending.check_against_ordered(&outputs);
    assert_eq!(pending.len(), 1);
    pending.respond(&matching, || {
        panic!("unrelated waiters need no matching output")
    });

    let response = pending.queue(matching);
    pending.check_against_ordered(&outputs);
    let Response::Utxo(utxo) = response.now_or_never().unwrap().unwrap() else {
        panic!("a matching waiter receives its UTXO");
    };
    assert_eq!(utxo, output.utxo);
    assert_eq!(pending.len(), 1);
    drop(unrelated_response);
    pending.prune();
    assert_eq!(pending.len(), 0);

    // A dropped receiver must not leave its sender behind or prevent subsequent delivery.
    drop(pending.queue(matching));
    pending.check_against_ordered(&outputs);
    assert_eq!(pending.len(), 0);
    let response = pending.queue(matching);
    pending.respond(&matching, || output.utxo);
    assert!(response.now_or_never().unwrap().is_ok());
    assert_eq!(pending.len(), 0);
}
