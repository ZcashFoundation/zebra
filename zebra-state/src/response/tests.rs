//! Tests for [`NonFinalizedBlocksListener`].

use std::{collections::HashSet, sync::Arc, time::Duration};

use tokio::sync::{mpsc, watch};
use zebra_chain::{
    amount::NonNegative,
    block::{self, Block},
    parameters::Network,
    value_balance::ValueBalance,
};

use crate::{
    arbitrary::Prepare, service::finalized_state::FinalizedState, Config, NonFinalizedState,
    WatchReceiver,
};

use super::NonFinalizedBlocksListener;

/// Builds `len` fake blocks forming a single chain, in ascending height order.
///
/// The first block is a pre-Heartwood block so committing the fake children doesn't trigger
/// history tree updates in the non-finalized state.
fn fake_chain(network: &Network, len: usize) -> Vec<Arc<Block>> {
    let mut blocks = vec![Arc::new(network.test_block(653599, 583999).unwrap())];
    while blocks.len() < len {
        let child = blocks.last().unwrap().make_fake_child().set_work(10);
        blocks.push(child);
    }
    blocks
}

/// Commits `blocks` (a single chain in ascending height order) into a fresh non-finalized state.
fn state_from_chain(network: &Network, blocks: &[Arc<Block>]) -> NonFinalizedState {
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    let mut state = NonFinalizedState::new(network);
    let (root, rest) = blocks.split_first().expect("chain must not be empty");
    state
        .commit_new_chain(root.clone().prepare(), &finalized_state)
        .expect("committing the root block should succeed");
    for block in rest {
        state
            .commit_block(block.clone().prepare(), &finalized_state)
            .expect("committing a child block should succeed");
    }

    state
}

/// Receives the next block hash from the listener, failing if none arrives promptly.
async fn recv_hash(rx: &mut mpsc::Receiver<(block::Hash, Arc<Block>)>) -> block::Hash {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("listener should send a block before timing out")
        .expect("listener channel should stay open")
        .0
}

/// Asserts the listener doesn't send any more blocks within a short window.
async fn assert_idle(rx: &mut mpsc::Receiver<(block::Hash, Arc<Block>)>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err(),
        "listener should not send any more blocks",
    );
}

/// With no known chain tips, every block currently in the non-finalized state is sent, in
/// ascending height order.
#[tokio::test]
async fn sends_all_blocks_when_no_known_tips() {
    let network = Network::Mainnet;
    let blocks = fake_chain(&network, 3);
    let hashes: Vec<_> = blocks.iter().map(|b| b.hash()).collect();

    let (_tx, rx) = watch::channel(state_from_chain(&network, &blocks));
    let listener = NonFinalizedBlocksListener::spawn(WatchReceiver::new(rx), HashSet::new());
    let mut received = listener.unwrap();

    assert_eq!(recv_hash(&mut received).await, hashes[0]);
    assert_eq!(recv_hash(&mut received).await, hashes[1]);
    assert_eq!(recv_hash(&mut received).await, hashes[2]);
    assert_idle(&mut received).await;
}

/// When the caller provides a chain tip it already has, only the blocks above that tip are sent;
/// the tip and its ancestors are skipped.
#[tokio::test]
async fn skips_blocks_at_or_below_known_tip() {
    let network = Network::Mainnet;
    let blocks = fake_chain(&network, 3);
    let hashes: Vec<_> = blocks.iter().map(|b| b.hash()).collect();

    // The caller already has block2 (and therefore its ancestor block1), so only block3 is new.
    let known_tips = std::iter::once(hashes[1]).collect();

    let (_tx, rx) = watch::channel(state_from_chain(&network, &blocks));
    let listener = NonFinalizedBlocksListener::spawn(WatchReceiver::new(rx), known_tips);
    let mut received = listener.unwrap();

    assert_eq!(recv_hash(&mut received).await, hashes[2]);
    assert_idle(&mut received).await;
}

/// When the caller already has the whole chain (its tip is the current best tip), nothing is sent
/// until the state changes.
#[tokio::test]
async fn sends_nothing_when_caller_has_the_tip() {
    let network = Network::Mainnet;
    let blocks = fake_chain(&network, 3);
    let tip = blocks.last().unwrap().hash();

    let (_tx, rx) = watch::channel(state_from_chain(&network, &blocks));
    let listener =
        NonFinalizedBlocksListener::spawn(WatchReceiver::new(rx), std::iter::once(tip).collect());
    let mut received = listener.unwrap();

    assert_idle(&mut received).await;
}

/// After the initial send, only blocks added since the previously seen state are forwarded, and
/// the one-time `known_chain_tips` check doesn't re-filter later updates.
#[tokio::test]
async fn sends_only_new_blocks_on_update() {
    let network = Network::Mainnet;
    let blocks = fake_chain(&network, 4);
    let hashes: Vec<_> = blocks.iter().map(|b| b.hash()).collect();

    let initial = state_from_chain(&network, &blocks[..3]);
    let extended = state_from_chain(&network, &blocks);

    let (tx, rx) = watch::channel(initial);
    let listener = NonFinalizedBlocksListener::spawn(WatchReceiver::new(rx), HashSet::new());
    let mut received = listener.unwrap();

    // Drain the initial three blocks.
    for expected in &hashes[..3] {
        assert_eq!(recv_hash(&mut received).await, *expected);
    }

    // Publishing the extended state forwards only the newly added block.
    tx.send(extended).expect("listener should still be running");
    assert_eq!(recv_hash(&mut received).await, hashes[3]);
    assert_idle(&mut received).await;
}
