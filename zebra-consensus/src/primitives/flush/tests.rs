//! Tests for the block crypto enqueue boundary and the transparent fast path.

use std::{
    cell::Cell,
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::{FutureExt, TryFutureExt};
use tower::ServiceExt;
use zebra_chain::{
    amount::DeferredPoolBalanceChange,
    block::Block,
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
};

use crate::{
    block::{Request, SemanticBlockVerifier},
    primitives::{cache::Cached, sapling},
    transaction::{self, BlockRequest, BlockTxVerifier},
    BoxError,
};

use super::{key, BlockFlush, Queueing, Verifier, BLOCKS};

tokio::task_local! {
    /// Overrides only flush routing; tests still use the real bounded crypto batch worker.
    pub(crate) static FLUSH: Box<dyn Fn(Verifier) -> Result<bool, BoxError> + Send + Sync>;
    static REGISTRY_ACCESSES: Cell<usize>;
}

pub(super) fn record_registry_access() {
    let _ = REGISTRY_ACCESSES.try_with(|count| count.set(count.get() + 1));
}

/// Keeps paused time from auto-advancing while blocking crypto workers finish.
pub(in crate::primitives) async fn without_advancing_time<T>(future: impl Future<Output = T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    tokio::pin!(future);
    loop {
        if let std::task::Poll::Ready(result) = futures::poll!(&mut future) {
            return result;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "verification must finish without advancing the batch idle timer"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn shielded_block_transaction_batch_does_not_wait_for_idle() {
    let _init_guard = zebra_test::init();
    let (upgrade, tx) = transaction::tests::cacheable_mainnet_sapling_transaction();
    assert!(upgrade < NetworkUpgrade::Nu6);
    assert!(!tx.has_sprout_joinsplit_data());
    assert!(
        tx.sapling_spends_count() + tx.sapling_outputs().count() < super::super::MAX_BATCH_SIZE,
        "the block must leave an underfilled proof batch"
    );
    let tx_hash = tx.hash();
    let mut block = zebra_test::vectors::MAINNET_BLOCKS
        .values()
        .map(|bytes| bytes.zcash_deserialize_into::<Block>().unwrap())
        .find(|block| block.transactions.iter().any(|tx| tx.hash() == tx_hash))
        .unwrap();
    let coinbase = block.transactions[0].as_ref().clone();
    assert!(!coinbase.has_shielded_data());
    // Burn the miner's largest output, keeping the required subsidy recipients. Before NU6,
    // underclaiming is valid and avoids claiming fees from the transactions removed below.
    let mut outputs = coinbase.outputs();
    let miner = outputs
        .iter()
        .enumerate()
        .max_by_key(|(_, output)| output.value())
        .map(|(index, _)| index)
        .unwrap();
    outputs.remove(miner);
    block.transactions = vec![
        Arc::new(coinbase.with_transparent_outputs(outputs)),
        Arc::new(tx),
    ];
    Arc::make_mut(&mut block.header).merkle_root =
        block.transactions.iter().map(|tx| tx.hash()).collect();
    let block = Arc::new(block);
    let hash = block.hash();
    let cache = Cached::new(sapling::batch_fallback_verifier(), 8, "block_flush_test")
        .with_flush(Verifier::Sapling);
    let flush_cache = cache.clone();
    let flushes = Arc::new(AtomicUsize::new(0));
    let count = flushes.clone();
    let hook = Box::new(move |verifier| {
        assert!(matches!(verifier, Verifier::Sapling));
        count.fetch_add(1, Ordering::SeqCst);
        flush_cache.inner().primary().clone().try_flush()
    });
    let state = tower::service_fn(|request| async move {
        match request {
            zebra_state::Request::KnownBlock(_) => Ok(zebra_state::Response::KnownBlock(None)),
            zebra_state::Request::CheckBlockProposalValidity(prepared) => {
                let contextual =
                    zebra_state::ContextuallyVerifiedBlock::with_block_and_spent_utxos(
                        prepared,
                        HashMap::new(),
                        DeferredPoolBalanceChange::zero(),
                        &Network::Mainnet,
                        Default::default(),
                    )?;
                Ok(zebra_state::Response::ValidBlockProposal(Box::new(
                    contextual,
                )))
            }
            _ => panic!("unexpected state request: {request:?}"),
        }
    });
    let transaction_state = state;
    let transactions = tower::service_fn(move |request| {
        BlockTxVerifier::new(&Network::Mainnet, transaction_state)
            .oneshot(request)
            .map_err(BoxError::from)
    });
    let start = tokio::time::Instant::now();
    FLUSH
        .scope(hook, async {
            transaction::tests::SAPLING_CACHE_VERIFIER
                .scope(cache, async {
                    let verified = without_advancing_time(
                        SemanticBlockVerifier::new(&Network::Mainnet, state, transactions)
                            .oneshot(Request::CheckProposal(block)),
                    )
                    .await
                    .expect("real block proofs and signatures must verify");
                    assert_eq!(verified, hash);
                })
                .await;
        })
        .await;
    assert_eq!(tokio::time::Instant::now(), start);
    assert_eq!(flushes.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn transparent_blocks_do_not_register_or_yield() {
    let _init_guard = zebra_test::init();
    REGISTRY_ACCESSES
        .scope(Cell::new(0), async {
            // A real transparent coinbase needs neither queueing bookkeeping nor a yielded poll.
            let block: Block = zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES
                .zcash_deserialize_into()
                .unwrap();
            let tx = block.transactions[0].clone();
            assert!(!tx.has_shielded_data());
            let state = tower::service_fn(|_| async {
                unreachable!("coinbase verification has no prevout lookups")
            });
            let result = BlockTxVerifier::new(&Network::Mainnet, state)
                .oneshot(BlockRequest {
                    transaction_hash: tx.hash(),
                    transaction: tx,
                    known_utxos: Arc::new(HashMap::new()),
                    height: block.coinbase_height().unwrap(),
                    time: block.header.time,
                })
                .now_or_never();
            assert!(matches!(result, Some(Ok(_))));

            // Exercise block registration too, stopping at the state service after semantic checks.
            let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
                .zcash_deserialize_into()
                .unwrap();
            assert!(block.transactions.iter().all(|tx| !tx.has_shielded_data()));
            let transactions = tower::service_fn(|request: BlockRequest| async move {
                Ok::<_, BoxError>(transaction::BlockResponse {
                    tx_id: request.transaction.unmined_id(),
                    miner_fee: None,
                    sigops: 0,
                })
            });
            let state = tower::service_fn(|request| async move {
                match request {
                    zebra_state::Request::KnownBlock(_) => {
                        Ok(zebra_state::Response::KnownBlock(None))
                    }
                    zebra_state::Request::CheckBlockProposalValidity(_) => {
                        Err::<_, BoxError>("reached contextual verification".into())
                    }
                    _ => panic!("unexpected state request: {request:?}"),
                }
            });
            let error = without_advancing_time(
                SemanticBlockVerifier::new(&Network::Mainnet, state, transactions)
                    .oneshot(Request::CheckProposal(block)),
            )
            .await
            .unwrap_err();
            assert!(matches!(
                error,
                crate::block::VerifyBlockError::ValidateProposal(_)
            ));
            assert_eq!(REGISTRY_ACCESSES.with(Cell::get), 0);
        })
        .await;
}

#[tokio::test(start_paused = true)]
async fn flush_waits_for_late_transactions_and_queue_admission() {
    let context = Arc::new(());
    let registration = BlockFlush::register(&context, 2);
    let count = Arc::new(AtomicUsize::new(0));
    let flushes = count.clone();
    FLUSH
        .scope(
            Box::new(move |_| {
                flushes.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            }),
            async {
                let first = Queueing::for_transaction(&context).unwrap();
                let first_request = first.scope(Queueing::for_request).unwrap();
                drop(first);
                first_request.enqueued(Verifier::Sapling);
                assert_eq!(count.load(Ordering::SeqCst), 0);

                // A Tower buffer may not call the last transaction service until after fan-out.
                tokio::task::yield_now().await;
                let last = Queueing::for_transaction(&context).unwrap();
                let last_request = last.scope(Queueing::for_request).unwrap();
                drop(last);
                tokio::task::yield_now().await;
                assert_eq!(count.load(Ordering::SeqCst), 0);
                last_request.enqueued(Verifier::Sapling);
                assert_eq!(count.load(Ordering::SeqCst), 1);
            },
        )
        .await;
    drop(registration);
    assert!(!BLOCKS.lock().contains_key(&key(&context)));
}

#[tokio::test]
async fn cancellation_removes_registration_without_flushing_unqueued_work() {
    let context = Arc::new(());
    let registration = BlockFlush::register(&context, 2);
    let transaction = Queueing::for_transaction(&context).unwrap();
    let request = transaction.scope(Queueing::for_request).unwrap();
    let pending = transaction.0.clone();
    drop(registration);
    assert!(!BLOCKS.lock().contains_key(&key(&context)));
    drop(transaction);
    drop(request);
    assert_eq!(Arc::strong_count(&pending), 1);
    assert_eq!(pending.lock().queued, 0);
}
