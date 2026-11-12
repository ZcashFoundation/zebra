//! Regression tests for block writer notifications.

use std::time::Duration;

use zebra_chain::{
    amount::NonNegative, block::Block, chain_tip::ChainTip, parameters::Network,
    value_balance::ValueBalance,
};

use crate::{
    arbitrary::Prepare,
    tests::{setup::new_state_with_mainnet_genesis, FakeChainHelper},
    Config,
};

use super::*;

/// Returns a roots cache without embedded trees, so the writer never fast syncs.
fn no_fast_tree_sync() -> Arc<TreeRootsCache> {
    Arc::new(TreeRootsCache::new(None, Height(0)))
}

fn populated_state() -> (NonFinalizedState, FinalizedState, Arc<Block>, Arc<Block>) {
    let network = Network::Mainnet;
    let root: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let child = root.make_fake_child().set_work(10);
    let mut state = NonFinalizedState::new(&network);
    let finalized = FinalizedState::new(&Config::ephemeral(), &network).unwrap();
    finalized.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());
    state
        .commit_new_chain(root.clone().prepare(), &finalized)
        .unwrap();
    state
        .commit_block(child.clone().prepare(), &finalized)
        .unwrap();
    (state, finalized, root, child)
}

#[test]
fn invalidating_only_root_keeps_chain_channels_available() {
    let _init_guard = zebra_test::init();
    let (mut state, finalized, root, child) = populated_state();
    let before = state.clone();
    let (mut tip_sender, latest, _change) = ChainTipSender::new(None, &Network::Mainnet);
    let (state_sender, state_receiver) = watch::channel(state.clone());

    let result = state.invalidate_block(root.hash());
    update_latest_chain_channels(&state, &finalized.db, &mut tip_sender, &state_sender, None);
    assert!(matches!(result, Err(InvalidateError::OnlyChainRoot(hash)) if hash == root.hash()));
    assert_eq!(latest.best_tip_hash(), Some(child.hash()));
    assert!(state_receiver.borrow().eq_internal_state(&before));
}

#[test]
fn empty_non_finalized_channels_fall_back_to_current_finalized_tip() {
    let _init_guard = zebra_test::init();
    let (finalized, state, genesis) = new_state_with_mainnet_genesis();
    let stale_tip = genesis.block.make_fake_child();
    let (mut tip_sender, latest, mut change) = ChainTipSender::new(None, &Network::Mainnet);
    tip_sender.set_best_non_finalized_tip(ChainTipBlock::from(stale_tip.prepare()));
    change.last_tip_change().unwrap();
    let (state_sender, state_receiver) = watch::channel(state.clone());

    assert_eq!(
        update_latest_chain_channels(&state, &finalized.db, &mut tip_sender, &state_sender, None),
        Some(Height(0))
    );
    assert_eq!(latest.best_tip_hash(), Some(genesis.hash));
    assert!(state_receiver.borrow().is_chain_set_empty());
    let action = change.last_tip_change().unwrap();
    assert!(action.is_reset());
    assert_eq!(action.best_tip_hash(), genesis.hash);

    // Falling back must also allow subsequent finalized-tip notifications.
    let next_finalized = genesis.block.make_fake_child();
    tip_sender.set_finalized_tip(ChainTipBlock::from(next_finalized.clone().prepare()));
    assert_eq!(latest.best_tip_hash(), Some(next_finalized.hash()));
}

#[test]
fn empty_chain_channels_clear_a_stale_tip() {
    let _init_guard = zebra_test::init();
    let (populated, finalized, _root, child) = populated_state();
    let empty = NonFinalizedState::new(&Network::Mainnet);
    let (mut tip_sender, latest, _change) = ChainTipSender::new(None, &Network::Mainnet);
    tip_sender.set_best_non_finalized_tip(ChainTipBlock::from(child.prepare()));
    let (state_sender, state_receiver) = watch::channel(populated);

    assert_eq!(
        update_latest_chain_channels(&empty, &finalized.db, &mut tip_sender, &state_sender, None),
        None
    );
    assert_eq!(latest.best_tip_hash(), None);
    assert!(state_receiver.borrow().is_chain_set_empty());
}

#[tokio::test]
async fn write_worker_survives_rejected_root_invalidation() {
    let _init_guard = zebra_test::init();
    let (state, finalized, root, child) = populated_state();
    let before = state.clone();
    let (tip_sender, latest, mut change) = ChainTipSender::new(None, &Network::Mainnet);
    let (state_sender, mut state_receiver) = watch::channel(state.clone());
    let (sender, _resets, _rejections, worker) = BlockWriteSender::spawn(
        finalized,
        state,
        tip_sender,
        state_sender,
        false,
        None,
        no_fast_tree_sync(),
    );
    let tx = sender.non_finalized.as_ref().unwrap();

    let (rsp_tx, rsp_rx) = oneshot::channel();
    tx.send(NonFinalizedWriteMessage::Invalidate {
        hash: root.hash(),
        rsp_tx,
    })
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), rsp_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(InvalidateError::OnlyChainRoot(hash)) if hash == root.hash()));
    tokio::time::timeout(Duration::from_secs(10), state_receiver.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(state_receiver.borrow().eq_internal_state(&before));
    tokio::time::timeout(Duration::from_secs(10), change.wait_for_tip_change())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.best_tip_hash(), Some(child.hash()));

    // Exercise the same worker again: a non-root invalidation must still succeed.
    let (rsp_tx, rsp_rx) = oneshot::channel();
    tx.send(NonFinalizedWriteMessage::Invalidate {
        hash: child.hash(),
        rsp_tx,
    })
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), rsp_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap(), child.hash());
    tokio::time::timeout(Duration::from_secs(10), state_receiver.changed())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), change.wait_for_tip_change())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.best_tip_hash(), Some(root.hash()));

    drop(sender);
    let worker = worker.unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            Arc::try_unwrap(worker).unwrap().join().unwrap();
        }),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn write_worker_handles_requests_with_no_non_finalized_chain() {
    let _init_guard = zebra_test::init();
    let (finalized, state, genesis) = new_state_with_mainnet_genesis();
    let (tip_sender, latest, mut change) = ChainTipSender::new(None, &Network::Mainnet);
    let (state_sender, mut state_receiver) = watch::channel(state.clone());
    let (sender, _resets, _rejections, worker) = BlockWriteSender::spawn(
        finalized,
        state,
        tip_sender,
        state_sender,
        false,
        None,
        no_fast_tree_sync(),
    );
    let tx = sender.non_finalized.as_ref().unwrap();

    let (rsp_tx, rsp_rx) = oneshot::channel();
    tx.send(NonFinalizedWriteMessage::Invalidate {
        hash: genesis.hash,
        rsp_tx,
    })
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), rsp_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(InvalidateError::BlockNotFound(_))));
    tokio::time::timeout(Duration::from_secs(10), state_receiver.changed())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), change.wait_for_tip_change())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.best_tip_hash(), Some(genesis.hash));

    let (rsp_tx, rsp_rx) = oneshot::channel();
    tx.send(NonFinalizedWriteMessage::Reconsider {
        hash: genesis.hash,
        rsp_tx,
    })
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), rsp_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result,
        Err(ReconsiderError::MissingInvalidatedBlock(_))
    ));
    tokio::time::timeout(Duration::from_secs(10), state_receiver.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.best_tip_hash(), Some(genesis.hash));
    assert!(state_receiver.borrow().is_chain_set_empty());

    drop(sender);
    let worker = worker.unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            Arc::try_unwrap(worker).unwrap().join().unwrap();
        }),
    )
    .await
    .unwrap()
    .unwrap();
}

#[test]
fn checkpoint_failure_preserves_error_when_state_closes_during_commit() {
    use std::sync::Mutex;

    use zebra_chain::parameters::testnet::ConfiguredActivationHeights;

    // Close the state only after the writer reports a failed commit, not at its initial
    // shutdown check. Observing the error event makes this interleaving deterministic.
    struct CloseStateOnError {
        resets: Mutex<Option<UnboundedReceiver<(Option<Height>, block::Hash)>>>,
    }

    impl tracing::Subscriber for CloseStateOnError {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            metadata.fields().field("last_valid_height").is_some()
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            unreachable!("only checkpoint failure events are enabled")
        }

        fn event(&self, _: &tracing::Event<'_>) {
            drop(self.resets.lock().unwrap().take());
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    let _init_guard = zebra_test::init();
    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(1),
            ..Default::default()
        }
        .into(),
    );
    let mut finalized = FinalizedState::new(&Config::ephemeral(), &network).unwrap();
    let genesis = block::genesis::regtest_genesis_block();
    finalized
        .commit_finalized(genesis.clone().into(), None)
        .unwrap();
    let child = genesis.make_fake_child().set_block_commitment([0; 32]);
    let non_finalized = NonFinalizedState::new(&network);
    let (chain_tip_sender, _, _) = ChainTipSender::new(None, &network);
    let (non_finalized_state_sender, _snapshot) = watch::channel(non_finalized.clone());
    let (queued, finalized_block_write_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (invalid_block_reset_sender, resets) = tokio::sync::mpsc::unbounded_channel();
    let (rsp_tx, rsp_rx) = oneshot::channel();
    queued.send((child.into(), rsp_tx, None)).unwrap();
    drop(queued);
    let worker = WriteBlockWorkerTask {
        finalized_block_write_receiver,
        non_finalized_block_write_receiver: tokio::sync::mpsc::unbounded_channel().1,
        finalized_state: finalized,
        non_finalized_state: non_finalized,
        invalid_block_reset_sender,
        non_finalized_rejected_sender: tokio::sync::mpsc::unbounded_channel().0,
        chain_tip_sender,
        non_finalized_state_sender,
        backup_dir_path: None,
        tree_roots_cache: no_fast_tree_sync(),
    };
    tracing::subscriber::with_default(
        CloseStateOnError {
            resets: Mutex::new(Some(resets)),
        },
        || worker.run(),
    );
    let error = rsp_rx
        .blocking_recv()
        .expect("writer shutdown must preserve the original commit response")
        .unwrap_err();
    assert!(matches!(
        error.inner(),
        crate::CommitBlockError::ValidateContextError(source)
            if matches!(**source, ValidateContextError::InvalidBlockCommitment(_))
    ));
}

/// Observe the real writer at notification, response, database write, and snapshot boundaries.
///
/// Every observation reads each block in the window exactly once, although the snapshot keeps the
/// newest finalized block until the next commit.
#[test]
fn commit_response_precedes_retention_finalization_without_read_gaps() {
    use std::{
        collections::HashSet,
        future::Future,
        pin::Pin,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        task::{Context, Wake, Waker},
    };

    use metrics::{
        Counter, Gauge, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder, SharedString,
        Unit,
    };

    use zebra_chain::{serialization::ZcashDeserializeInto, transparent};

    use crate::{service::read, MinedBlockNotifications};

    struct BeforeFinalization {
        db: ZebraDb,
        snapshot: watch::Receiver<NonFinalizedState>,
        tip: LatestChainTip,
        root: Arc<Block>,
        committed: Arc<Block>,
        conflicting_root: block::Hash,
        deferred_chain: Option<std::sync::Weak<Chain>>,
        /// An address that receives `reward` zatoshis in every block after genesis.
        address: transparent::Address,
        reward: i64,
        wakes: AtomicUsize,
        before_write: AtomicBool,
        overlapping_read: AtomicBool,
    }

    impl BeforeFinalization {
        fn readable(&self) -> bool {
            let snapshot = self.snapshot.borrow();
            let chain = snapshot.best_chain();
            let tip_height = self.committed.coinbase_height().unwrap();
            self.tip.best_tip_hash() == Some(self.committed.hash())
                && !snapshot.any_chain_contains(&self.conflicting_root)
                && [&self.root, &self.committed].into_iter().all(|block| {
                    [block.hash().into(), block.coinbase_height().unwrap().into()]
                        .into_iter()
                        .all(|key| read::block(chain, &self.db, key) == Ok(Some(block.clone())))
                })
                && (0..=tip_height.0).all(|height| {
                    read::block(chain, &self.db, Height(height).into())
                        .is_ok_and(|block| block.is_some())
                })
                && read::transparent_balance(
                    chain.cloned(),
                    &self.db,
                    HashSet::from([self.address]),
                )
                .is_ok_and(|(balance, _)| {
                    balance.zatoshis() == self.reward * i64::from(tip_height.0)
                })
        }
    }

    impl Wake for BeforeFinalization {
        fn wake(self: Arc<Self>) {
            // Record failures rather than panic in a wake callback during writer unwinding.
            self.before_write.fetch_and(
                self.readable()
                    && self.db.finalized_tip_height().unwrap() + 1 == self.root.coinbase_height()
                    && self.snapshot.borrow().any_chain_contains(&self.root.hash())
                    && self
                        .deferred_chain
                        .as_ref()
                        .is_none_or(|chain| chain.strong_count() > 0),
                Ordering::SeqCst,
            );
            self.wakes.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct AtFinalizedWrite {
        observations: Vec<Arc<BeforeFinalization>>,
        writes: Arc<AtomicUsize>,
    }

    impl HistogramFn for AtFinalizedWrite {
        fn record(&self, _: f64) {
            let index = self.writes.fetch_add(1, Ordering::SeqCst);
            let observation = &self.observations[index];
            observation.overlapping_read.store(
                observation.readable()
                    && observation.wakes.load(Ordering::SeqCst) == 2
                    && observation.db.block(observation.root.hash().into())
                        == Ok(Some(observation.root.clone()))
                    && observation
                        .snapshot
                        .borrow()
                        .any_chain_contains(&observation.root.hash()),
                Ordering::SeqCst,
            );
        }
    }

    struct FinalizedWriteRecorder(Arc<AtFinalizedWrite>);

    impl Recorder for FinalizedWriteRecorder {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn register_counter(&self, _: &Key, _: &Metadata<'_>) -> Counter {
            Counter::noop()
        }

        fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
            Gauge::noop()
        }

        fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
            if key.name() == "zebra.state.rocksdb.batch_commit.duration_seconds" {
                Histogram::from_arc(self.0.clone())
            } else {
                Histogram::noop()
            }
        }
    }

    let _init_guard = zebra_test::init();
    let (finalized, mut state, _) = new_state_with_mainnet_genesis();
    let root: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state
        .commit_new_chain(root.clone().prepare(), &finalized.db)
        .unwrap();
    let second_root = root.make_fake_child();
    let third_root = second_root.make_fake_child();
    let coinbase_outputs = root.transactions[0].outputs();
    let address = coinbase_outputs
        .iter()
        .find_map(|output| output.address(&Network::Mainnet))
        .unwrap();
    let reward = coinbase_outputs
        .iter()
        .filter(|output| output.address(&Network::Mainnet) == Some(address))
        .map(|output| output.value().zatoshis())
        .sum();
    assert!(reward > 0, "the balance checks need a non-zero reward");
    let mut parent = root.clone();
    for _ in 1..MAX_BLOCK_REORG_HEIGHT {
        parent = parent.make_fake_child();
        state
            .commit_block(parent.clone().prepare(), &finalized.db)
            .unwrap();
    }
    let retained = state.clone();
    let conflicting_root = root.clone().set_work(1);
    state
        .commit_new_chain(conflicting_root.clone().prepare(), &finalized.db)
        .unwrap();
    assert_eq!(state.chain_count(), 2);
    let deferred_chain = Arc::downgrade(
        &state
            .find_chain(|chain| chain.contains_block_hash(conflicting_root.hash()))
            .unwrap(),
    );
    let (chain_tip_sender, tip, _) = ChainTipSender::new(None, &Network::Mainnet);
    let (non_finalized_state_sender, snapshot) = watch::channel(state.clone());
    let (queued, non_finalized_block_write_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (invalid_block_reset_sender, _resets) = tokio::sync::mpsc::unbounded_channel();
    let db = finalized.db.clone();
    let mut proposals = retained.clone();
    let mut observations = Vec::new();
    let mut responses = Vec::new();
    for root in [root, second_root, third_root] {
        let child = parent.make_fake_child();
        proposals
            .commit_block(child.clone().prepare(), &db)
            .unwrap();
        let contextual = proposals.best_tip_block().unwrap().clone();
        let observation = Arc::new(BeforeFinalization {
            db: db.clone(),
            snapshot: snapshot.clone(),
            tip: tip.clone(),
            root,
            conflicting_root: conflicting_root.hash(),
            deferred_chain: observations.is_empty().then(|| deferred_chain.clone()),
            committed: child.clone(),
            address,
            reward,
            wakes: AtomicUsize::new(0),
            before_write: AtomicBool::new(true),
            overlapping_read: AtomicBool::new(false),
        });
        let waker = Waker::from(observation.clone());
        let mut cx = Context::from_waker(&waker);
        let (rsp_tx, mut rsp_rx) = oneshot::channel();
        assert!(Pin::new(&mut rsp_rx).poll(&mut cx).is_pending());
        let (committed, mut commits) = tokio::sync::mpsc::channel(1);
        assert!(commits.poll_recv(&mut cx).is_pending());
        let (validated, _bodies) = watch::channel(None);
        queued
            .send(NonFinalizedWriteMessage::Commit(
                (
                    child.clone().prepare(),
                    rsp_tx,
                    Some(Box::new(contextual)),
                    Some(MinedBlockNotifications::new(committed, validated)),
                ),
                Instant::now(),
            ))
            .unwrap();
        observations.push(observation);
        responses.push((rsp_rx, commits));
        parent = child;
    }
    drop(queued);
    let writes = Arc::new(AtomicUsize::new(0));
    let worker = WriteBlockWorkerTask {
        finalized_block_write_receiver: tokio::sync::mpsc::unbounded_channel().1,
        non_finalized_block_write_receiver,
        finalized_state: finalized,
        non_finalized_state: state,
        invalid_block_reset_sender,
        non_finalized_rejected_sender: tokio::sync::mpsc::unbounded_channel().0,
        chain_tip_sender,
        non_finalized_state_sender,
        backup_dir_path: None,
        tree_roots_cache: no_fast_tree_sync(),
    };
    // Observe the synchronous batch write even when release builds compile out trace events.
    let recorder = FinalizedWriteRecorder(Arc::new(AtFinalizedWrite {
        observations: observations.clone(),
        writes: writes.clone(),
    }));
    metrics::with_local_recorder(&recorder, || worker.run());
    assert_eq!(writes.load(Ordering::SeqCst), 3);
    assert_eq!(deferred_chain.strong_count(), 0);
    // The newest finalized root stays published until the next commit pops it.
    assert_eq!(
        snapshot.borrow().best_chain_len(),
        Some(MAX_BLOCK_REORG_HEIGHT + 1)
    );
    let newest_root = observations.last().unwrap().root.hash();
    for (observation, (mut response, mut commits)) in observations.iter().zip(responses) {
        assert_eq!(observation.wakes.load(Ordering::SeqCst), 2);
        assert!(
            observation.before_write.load(Ordering::SeqCst),
            "notifications and responses must precede the root's finalized write",
        );
        assert!(
            observation.overlapping_read.load(Ordering::SeqCst),
            "the published root must remain readable until its database write completes",
        );
        assert_eq!(
            response.try_recv().unwrap().unwrap(),
            observation.committed.hash()
        );
        assert_eq!(
            commits.try_recv().unwrap(),
            (
                observation.committed.hash(),
                observation.committed.coinbase_height().unwrap()
            ),
        );
        assert_eq!(
            snapshot
                .borrow()
                .any_chain_contains(&observation.root.hash()),
            observation.root.hash() == newest_root,
        );
        for chain in [snapshot.borrow().best_chain(), retained.best_chain()] {
            assert_eq!(
                read::block(chain, &db, observation.root.hash().into()),
                Ok(Some(observation.root.clone())),
            );
        }
        assert!(retained.any_chain_contains(&observation.root.hash()));
    }
}
