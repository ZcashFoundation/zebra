//! Parent-prefetched outputs share the normal transaction lookup and fallback path.

use std::{collections::HashMap, sync::Arc};

use tower::{service_fn, timeout::Timeout};
use zebra_chain::{
    block::Height,
    parameters::NetworkUpgrade,
    transaction::{Hash, LockTime, Transaction},
    transparent::{Input, OutPoint, Output, Script, Utxo},
};

use super::super::BlockTxVerifier;

#[tokio::test]
async fn prefetched_and_fallback_outputs_preserve_input_order() {
    let prefetched = OutPoint {
        hash: Hash([1; 32]),
        index: 0,
    };
    let missing = OutPoint {
        hash: Hash([2; 32]),
        index: 0,
    };
    let make_utxo = |script| {
        Utxo::new(
            Output::new(1.try_into().unwrap(), Script::new(&[script])),
            Height(1),
            false,
        )
    };
    let cached = make_utxo(0x51);
    let fallback = make_utxo(0x52);
    let inputs = [prefetched, missing].map(|outpoint| Input::PrevOut {
        outpoint,
        unlock_script: Script::new(&[]),
        sequence: u32::MAX,
    });
    let tx = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        inputs.to_vec(),
        vec![],
        LockTime::unlocked(),
        Height(2),
    );
    let expected_fallback = fallback.clone();
    let state = service_fn(move |request| {
        assert_eq!(request, zebra_state::Request::AwaitUtxos(vec![missing]));
        let result = zebra_state::Response::Utxos(vec![expected_fallback.clone()]);
        async move { Ok::<_, crate::BoxError>(result) }
    });
    let (utxos, outputs) = BlockTxVerifier::block_spent_utxos(
        Arc::new(tx),
        Arc::new(HashMap::new()),
        Arc::new(HashMap::from([(prefetched, cached.clone())])),
        Timeout::new(state, super::test_timeout()),
    )
    .await
    .unwrap();
    assert_eq!(
        outputs,
        vec![cached.output.clone(), fallback.output.clone()]
    );
    assert_eq!(utxos[&prefetched], cached);
    assert_eq!(utxos[&missing], fallback);
}

#[tokio::test]
async fn all_prefetched_inputs_skip_the_state_lookup() {
    let outpoint = OutPoint {
        hash: Hash([3; 32]),
        index: 0,
    };
    let utxo = Utxo::new(
        Output::new(1.try_into().unwrap(), Script::new(&[0x51])),
        Height(1),
        false,
    );
    let tx = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![Input::PrevOut {
            outpoint,
            unlock_script: Script::new(&[]),
            sequence: u32::MAX,
        }],
        vec![],
        LockTime::unlocked(),
        Height(2),
    );
    let state = service_fn(|_| async { unreachable!("prefetch must avoid the fallback lookup") });
    let (utxos, outputs) = BlockTxVerifier::block_spent_utxos(
        Arc::new(tx),
        Arc::new(HashMap::new()),
        Arc::new(HashMap::from([(outpoint, utxo.clone())])),
        Timeout::new(state, super::test_timeout()),
    )
    .await
    .unwrap();
    assert_eq!(utxos[&outpoint], utxo);
    assert_eq!(outputs, vec![utxo.output]);
}
