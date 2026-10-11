//! Batched UTXO requests against queued, in-flight, and pending state outputs.

use std::{sync::Arc, time::Duration};

use futures::FutureExt;
use tower::Service;

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction, transparent,
};

use crate::{
    arbitrary::Prepare, service::StateService, AwaitUtxoError, Config, Request, Response,
    SemanticallyVerifiedBlock,
};

fn blocks() -> Vec<SemanticallyVerifiedBlock> {
    zebra_test::vectors::MAINNET_BLOCKS
        .range(1..=2)
        .map(|(_, bytes)| {
            let block: Arc<Block> = bytes.zcash_deserialize_into().unwrap();
            block.prepare()
        })
        .collect()
}

#[tokio::test]
async fn await_utxos_preserves_order_for_queued_and_in_flight_outputs() {
    let _init_guard = zebra_test::init();
    let (mut state, _, _, _) =
        StateService::new(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;
    let blocks = blocks();
    let queued = blocks[0].new_outputs.iter().next().unwrap();
    let sent = blocks[1].new_outputs.iter().next().unwrap();
    let (sender, _receiver) = tokio::sync::oneshot::channel();
    state
        .non_finalized_state_queued_blocks
        .queue((blocks[0].clone(), sender, None));
    state.non_finalized_block_write_sent_hashes.add(&blocks[1]);

    let response = tokio::time::timeout(
        Duration::from_secs(10),
        state.call(Request::AwaitUtxos(vec![*sent.0, *queued.0, *sent.0])),
    )
    .await
    .expect("queued and in-flight outputs are immediately available")
    .unwrap();
    assert_eq!(
        response,
        Response::Utxos(vec![
            sent.1.utxo.clone(),
            queued.1.utxo.clone(),
            sent.1.utxo.clone(),
        ])
    );
    assert_eq!(
        state.call(Request::AwaitUtxos(Vec::new())).await.unwrap(),
        Response::Utxos(Vec::new())
    );
}

#[tokio::test]
async fn await_utxos_keeps_found_outputs_while_waiting_for_a_queued_block() {
    let _init_guard = zebra_test::init();
    let (mut state, _, _, _) =
        StateService::new(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;
    let blocks = blocks();
    let found = blocks[0].new_outputs.iter().next().unwrap();
    let pending = blocks[1].new_outputs.iter().next().unwrap();
    state.non_finalized_block_write_sent_hashes.add(&blocks[0]);
    let mut lookup = state.call(Request::AwaitUtxos(vec![*pending.0, *found.0]));
    assert!(lookup.as_mut().now_or_never().is_none());

    // Arrival of a semantically verified block notifies pending requests before commit.
    state
        .pending_utxos
        .check_against_ordered(&blocks[1].new_outputs);
    let (sender, _receiver) = tokio::sync::oneshot::channel();
    state
        .non_finalized_state_queued_blocks
        .queue((blocks[1].clone(), sender, None));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), lookup)
            .await
            .expect("the newly queued block supplies the pending output")
            .unwrap(),
        Response::Utxos(vec![pending.1.utxo.clone(), found.1.utxo.clone()])
    );
}

#[tokio::test]
async fn await_utxos_returns_a_later_error_without_waiting_for_earlier_inputs() {
    let _init_guard = zebra_test::init();
    let (mut state, _, _, _) =
        StateService::new(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;
    // Exceed try_join_all's small-batch threshold: ordered collection can hide a later error.
    let outpoints: Vec<_> = (0..40)
        .map(|index| transparent::OutPoint {
            hash: transaction::Hash([index; 32]),
            index: 0,
        })
        .collect();
    let lookup = state.call(Request::AwaitUtxos(outpoints.clone()));
    assert!(state
        .pending_utxos
        .0
        .remove(outpoints.last().unwrap())
        .is_some());
    let error = tokio::time::timeout(Duration::from_secs(10), lookup)
        .await
        .expect("a later error must not wait for earlier missing outputs")
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<AwaitUtxoError>(),
        Some(AwaitUtxoError::Cancelled)
    ));
    state.pending_utxos.prune();
    assert_eq!(
        state.pending_utxos.len(),
        0,
        "failed batches drop pending receivers"
    );
}

#[tokio::test]
async fn await_utxos_timeout_drops_pending_receivers() {
    let _init_guard = zebra_test::init();
    let (mut state, _, _, _) =
        StateService::new(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;
    let lookup = state.call(Request::AwaitUtxos(vec![transparent::OutPoint {
        hash: transaction::Hash([0; 32]),
        index: 0,
    }]));
    assert!(tokio::time::timeout(Duration::from_millis(10), lookup)
        .await
        .is_err());
    state.pending_utxos.prune();
    assert_eq!(
        state.pending_utxos.len(),
        0,
        "timed-out batches drop pending receivers"
    );
}
