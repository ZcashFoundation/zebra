//! Block verification using resident mempool verdicts and ordinary fallback verification.

use std::{
    future::{pending, ready},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use tokio::sync::oneshot;
use tower::{service_fn, util::BoxCloneService};
use zebra_chain::{
    amount::NonNegative, block::ChainHistoryBlockTxAuthCommitmentHash,
    parameters::testnet::ConfiguredActivationHeights, transaction::VerifiedUnminedTx,
};
use zebra_node_services::mempool;

use super::*;

type Mempool = BoxCloneService<mempool::Request, mempool::Response, BoxError>;
type State = BoxCloneService<zs::Request, zs::Response, BoxError>;

const FEE: u64 = 10_000;

#[derive(Default)]
struct Calls {
    coinbase: AtomicUsize,
    non_coinbase: AtomicUsize,
    await_utxo: AtomicUsize,
    commits: AtomicUsize,
}

fn network() -> Network {
    Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            ..Default::default()
        }
        .into(),
    )
}

fn spend(lock_time: LockTime, expiry_height: Height) -> Arc<Transaction> {
    Arc::new(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: [8; 32].into(),
                index: 0,
            },
            unlock_script: transparent::Script::new(&[0x51]),
            sequence: 0,
        }],
        vec![transparent::Output::new(
            1.try_into().unwrap(),
            transparent::Script::new(&[0x51]),
        )],
        lock_time,
        expiry_height,
    ))
}

fn candidate(network: &Network, height: Height, spend: Arc<Transaction>) -> Arc<Block> {
    let subsidy = zebra_chain::parameters::subsidy::block_subsidy(height, network).unwrap();
    let coinbase = Arc::new(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![transparent::Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![transparent::Output::new(
            (subsidy + Amount::<NonNegative>::try_from(FEE).unwrap()).unwrap(),
            transparent::Script::new(&[0xac]),
        )],
        LockTime::unlocked(),
        height,
    ));
    let genesis = block::genesis::regtest_genesis_block();
    let mut header = *genesis.header;
    header.previous_block_hash = genesis.hash();
    let transactions = vec![coinbase, spend];
    header.merkle_root = transactions.iter().map(|tx| tx.hash()).collect();
    Arc::new(Block {
        header: Arc::new(header),
        transactions,
    })
}

fn entry(transaction: Arc<Transaction>) -> VerifiedUnminedTx {
    let mut entry = VerifiedUnminedTx::new(
        transaction.into(),
        FEE.try_into().unwrap(),
        0,
        0,
        Arc::new(Vec::new()),
    )
    .unwrap();
    // Admission at height 2 and mining at height 3 share NU5, but have different heights.
    entry.height = Some(Height(1));
    entry
}

fn mempool(block: &Block, entries: Vec<VerifiedUnminedTx>) -> Mempool {
    let parent = block.header.previous_block_hash;
    let hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash()).collect();
    let entries: std::collections::HashMap<_, _> = entries
        .into_iter()
        .map(|entry| (entry.transaction.id.mined_id(), entry))
        .collect();
    BoxCloneService::new(service_fn(move |request| {
        let mempool::Request::VerifiedTransactionsForBlock {
            transaction_hashes,
            parent_hash,
        } = request
        else {
            panic!("block verification must request only bounded verified candidates")
        };
        assert_eq!(parent_hash, parent);
        assert_eq!(transaction_hashes.as_ref(), hashes.as_slice());
        ready(Ok(mempool::Response::VerifiedTransactionsForBlock(
            entries.clone(),
        )))
    }))
}

fn mock_state() -> State {
    BoxCloneService::new(service_fn(|request| {
        ready(Ok(match request {
            zs::Request::KnownBlock(_) => zs::Response::KnownBlock(None),
            zs::Request::EarlyRelay { .. } => zs::Response::EarlyRelay(None),
            zs::Request::MiningCandidate(_) => zs::Response::MiningCandidate(None),
            zs::Request::AwaitUtxo(_) => zs::Response::Utxo(transparent::Utxo::new(
                transparent::Output::new(
                    (FEE + 1).try_into().unwrap(),
                    transparent::Script::new(&[0x51]),
                ),
                Height(1),
                false,
            )),
            zs::Request::CommitSemanticallyVerifiedBlock(block) => {
                zs::Response::Committed(block.hash)
            }
            _ => panic!("unexpected state request: {request:?}"),
        }))
    }))
}

async fn verify(
    network: &Network,
    block: Arc<Block>,
    mempool: Mempool,
    state: State,
) -> (Result<block::Hash, VerifyBlockError>, Arc<Calls>) {
    let calls = Arc::new(Calls::default());
    let state = service_fn({
        let calls = calls.clone();
        move |request| {
            match &request {
                zs::Request::AwaitUtxo(_) => {
                    calls.await_utxo.fetch_add(1, Ordering::SeqCst);
                }
                zs::Request::CommitSemanticallyVerifiedBlock(_) => {
                    calls.commits.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
            state.clone().oneshot(request)
        }
    });
    let transaction_verifier =
        Buffer::new(transaction::BlockTxVerifier::new(network, state.clone()), 1);
    let transactions = service_fn({
        let calls = calls.clone();
        move |request: tx::BlockRequest| {
            if request.transaction.is_coinbase() {
                calls.coinbase.fetch_add(1, Ordering::SeqCst);
            } else {
                calls.non_coinbase.fetch_add(1, Ordering::SeqCst);
            }
            transaction_verifier.clone().oneshot(request)
        }
    });
    let (sender, receiver) = oneshot::channel();
    let verifier = SemanticBlockVerifier::new(network, state, transactions).with_mempool(receiver);
    sender.send(mempool).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        verifier.oneshot(Request::Commit(block)),
    )
    .await
    .expect("mempool lookup and block verification must finish");
    (result, calls)
}

fn assert_dispatches(calls: &Calls, non_coinbase: usize) {
    assert_eq!(calls.coinbase.load(Ordering::SeqCst), 1);
    assert_eq!(calls.non_coinbase.load(Ordering::SeqCst), non_coinbase);
    assert_eq!(calls.await_utxo.load(Ordering::SeqCst), non_coinbase);
}

#[tokio::test]
async fn resident_hit_skips_transaction_dispatch_and_utxo_lookup() {
    let _init_guard = zebra_test::init();
    let network = network();
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    // Even a supplied coinbase candidate must go through the real transaction verifier.
    let entries = block.transactions.iter().cloned().map(entry).collect();
    let pool = mempool(&block, entries);
    let (result, calls) = verify(&network, block.clone(), pool, mock_state()).await;
    assert_eq!(result.unwrap(), block.hash());
    assert_dispatches(&calls, 0);
    assert_eq!(calls.commits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn misses_and_ineligible_metadata_use_ordinary_verification() {
    let _init_guard = zebra_test::init();
    let network = network();
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    let mut missing_height = entry(block.transactions[1].clone());
    missing_height.height = None;
    let mut old_epoch = missing_height.clone();
    old_epoch.height = Some(Height(0));
    assert_ne!(
        NetworkUpgrade::current(&network, Height(1)),
        NetworkUpgrade::current(&network, Height(3))
    );
    for entries in [Vec::new(), vec![missing_height], vec![old_epoch]] {
        let pool = mempool(&block, entries);
        let (result, calls) = verify(&network, block.clone(), pool, mock_state()).await;
        assert_eq!(result.unwrap(), block.hash());
        assert_dispatches(&calls, 1);
    }
}

#[tokio::test]
async fn same_txid_with_different_authorizing_data_is_a_miss() {
    let _init_guard = zebra_test::init();
    let network = network();
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    let original = &block.transactions[1];
    let mut inputs = original.inputs();
    let transparent::Input::PrevOut { unlock_script, .. } = &mut inputs[0] else {
        unreachable!()
    };
    *unlock_script = transparent::Script::new(&[0x52]);
    let twin = Arc::new((**original).clone().with_transparent_inputs(inputs));
    assert_eq!(original.hash(), twin.hash());
    assert_ne!(original.unmined_id(), twin.unmined_id());
    let pool = mempool(&block, vec![entry(twin)]);
    let (result, calls) = verify(&network, block.clone(), pool, mock_state()).await;
    assert_eq!(result.unwrap(), block.hash());
    assert_dispatches(&calls, 1);
}

#[tokio::test]
async fn eligible_hits_recheck_expiry_height_and_lock_time() {
    let _init_guard = zebra_test::init();
    let network = network();
    let time = block::genesis::regtest_genesis_block().header.time;
    for (lock_time, expiry_height, expected) in [
        (LockTime::unlocked(), Height(2), "expiry"),
        (LockTime::Height(Height(3)), Height(0), "height"),
        (LockTime::Time(time), Height(0), "time"),
    ] {
        let block = candidate(&network, Height(3), spend(lock_time, expiry_height));
        let mut cached = entry(block.transactions[1].clone());
        if expected == "height" {
            cached.height = Some(Height(4));
        }
        let pool = mempool(&block, vec![cached]);
        let (result, calls) = verify(&network, block, pool, mock_state()).await;
        let VerifyBlockError::Transaction(error) = result.unwrap_err() else {
            panic!("an eligible cached transaction must fail its contextual semantic check")
        };
        match expected {
            "expiry" => assert!(matches!(error, TransactionError::ExpiredTransaction { .. })),
            "height" => assert!(matches!(
                error,
                TransactionError::LockedUntilAfterBlockHeight(Height(3))
            )),
            "time" => assert!(matches!(
                error,
                TransactionError::LockedUntilAfterBlockTime(locked) if locked == time
            )),
            _ => unreachable!(),
        }
        assert_eq!(calls.non_coinbase.load(Ordering::SeqCst), 0);
        assert_eq!(calls.await_utxo.load(Ordering::SeqCst), 0);
        assert_eq!(calls.commits.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn cached_p2sh_sigops_share_the_block_limit_with_dispatched_transactions() {
    let _init_guard = zebra_test::init();
    let network = network();
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    assert_eq!(block.transactions[0].sigops().unwrap(), 1);
    for cached_sigops in [MAX_BLOCK_SIGOPS - 1, MAX_BLOCK_SIGOPS] {
        let mut cached = entry(block.transactions[1].clone());
        cached.p2sh_sigop_count = cached_sigops;
        let pool = mempool(&block, vec![cached]);
        let (result, calls) = verify(&network, block.clone(), pool, mock_state()).await;
        if cached_sigops < MAX_BLOCK_SIGOPS {
            assert_eq!(result.unwrap(), block.hash());
        } else {
            assert!(matches!(
                result,
                Err(VerifyBlockError::Block {
                    source: BlockError::TooManyTransparentSignatureOperations { sigops, .. }
                }) if sigops == MAX_BLOCK_SIGOPS + 1
            ));
            assert_eq!(calls.commits.load(Ordering::SeqCst), 0);
        }
        assert_dispatches(&calls, 0);
    }
}

#[tokio::test]
async fn mempool_errors_and_response_or_readiness_timeouts_fall_back() {
    let _init_guard = zebra_test::init();
    let network = network();
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    for permits in [0, 1] {
        let requests = Arc::new(AtomicUsize::new(0));
        let pool = service_fn({
            let requests = requests.clone();
            move |_| {
                requests.fetch_add(1, Ordering::SeqCst);
                pending::<Result<mempool::Response, BoxError>>()
            }
        });
        let pool = BoxCloneService::new(tower::limit::ConcurrencyLimit::new(pool, permits));
        let (result, calls) = verify(&network, block.clone(), pool, mock_state()).await;
        assert_eq!(result.unwrap(), block.hash());
        assert_dispatches(&calls, 1);
        assert_eq!(requests.load(Ordering::SeqCst), permits);
    }
    let pool = BoxCloneService::new(service_fn(|_| {
        ready(Err::<mempool::Response, BoxError>(
            "mempool unavailable".into(),
        ))
    }));
    let (result, calls) = verify(&network, block.clone(), pool, mock_state()).await;
    assert_eq!(result.unwrap(), block.hash());
    assert_dispatches(&calls, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn resident_hit_still_rejects_a_missing_contextual_spend() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let network = Network::new_regtest(
            ConfiguredActivationHeights {
                canopy: Some(1),
                nu5: Some(1),
                ..Default::default()
            }
            .into(),
        );
        let (state, read, _, _) = zs::init(zs::Config::ephemeral(), &network, Height(0), 0).await;
        let state = Buffer::new(state, 10);
        state
            .clone()
            .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
                block::genesis::regtest_genesis_block().into(),
            ))
            .await
            .unwrap();
        let zs::ReadResponse::ChainInfo(info) =
            read.oneshot(zs::ReadRequest::ChainInfo).await.unwrap()
        else {
            panic!("expected chain info for the finalized genesis block")
        };
        let mut block = candidate(&network, Height(1), spend(LockTime::unlocked(), Height(0)));
        let auth_root = block.auth_data_root();
        let header = Arc::make_mut(&mut Arc::make_mut(&mut block).header);
        header.previous_block_hash = info.tip_hash;
        header.time = info.cur_time.into();
        header.difficulty_threshold = info.expected_difficulty;
        header.commitment_bytes =
            <[u8; 32]>::from(ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
                &info
                    .chain_history_root
                    .unwrap_or(block::CHAIN_HISTORY_ACTIVATION_RESERVED.into()),
                &auth_root,
            ))
            .into();
        let mut cached = entry(block.transactions[1].clone());
        cached.height = Some(Height(0));
        let outpoint = block.transactions[1].spent_outpoints().next().unwrap();
        let pool = mempool(&block, vec![cached]);
        let (result, calls) = verify(&network, block, pool, BoxCloneService::new(state)).await;
        let VerifyBlockError::Commit(zs::CommitBlockError::ValidateContextError(error)) =
            result.unwrap_err()
        else {
            panic!("the cached transaction must still reach contextual spend validation")
        };
        assert!(matches!(
            *error,
            zs::ValidateContextError::MissingTransparentOutput { outpoint: missing, .. }
                if missing == outpoint
        ));
        assert_dispatches(&calls, 0);
        assert_eq!(calls.commits.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("contextual validation must finish");
}

#[tokio::test]
async fn cached_shielded_prefix_does_not_hold_the_miss_batch_open() {
    use crate::primitives::flush::{tests::FLUSH, Queueing, Verifier};

    let _init_guard = zebra_test::init();
    for reuse in [false, true] {
        let block: Arc<Block> = zebra_test::vectors::MAINNET_BLOCKS
            .values()
            .map(|bytes| bytes.zcash_deserialize_into::<Block>().unwrap())
            .find(|block| {
                NetworkUpgrade::current(&Network::Mainnet, block.coinbase_height().unwrap())
                    >= NetworkUpgrade::Sapling
                    && !block.transactions[0].has_shielded_data()
                    && block
                        .transactions
                        .iter()
                        .filter(|tx| tx.has_shielded_data())
                        .count()
                        >= 2
            })
            .expect("Sapling-era fixtures include multiple shielded payments")
            .into();
        let cached = block
            .transactions
            .iter()
            .find(|tx| tx.has_shielded_data())
            .unwrap();
        let cached_hash = cached.hash();
        let unmined = zebra_chain::transaction::UnminedTx::from(cached.clone());
        let fee = unmined.conventional_fee;
        let mut cached = VerifiedUnminedTx::new(unmined, fee, 0, 0, Arc::new(Vec::new())).unwrap();
        cached.height = Some((block.coinbase_height().unwrap() - 1).unwrap());
        let pool = mempool(&block, if reuse { vec![cached] } else { Vec::new() });
        let dispatched = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let transactions = service_fn({
            let dispatched = dispatched.clone();
            move |request: tx::BlockRequest| {
                dispatched.lock().push(request.transaction_hash);
                if request.transaction.has_shielded_data() {
                    let queueing = Queueing::for_transaction(&request.known_utxos).unwrap();
                    let queued = queueing.scope(Queueing::for_request).unwrap();
                    drop(queueing);
                    queued.enqueued(Verifier::Sapling);
                }
                // Queue admission, rather than proof validity, determines this block's flush count.
                ready(Err::<tx::BlockResponse, BoxError>(
                    TransactionError::BadBalance.into(),
                ))
            }
        });
        let state = service_fn(|request| {
            assert!(matches!(request, zs::Request::KnownBlock(_)));
            ready(Ok::<_, BoxError>(zs::Response::KnownBlock(None)))
        });
        let (sender, receiver) = oneshot::channel();
        let verifier = SemanticBlockVerifier::new(&Network::Mainnet, state, transactions)
            .with_mempool(receiver);
        sender.send(pool).unwrap();
        let flushes = Arc::new(AtomicUsize::new(0));
        let count = flushes.clone();
        let result = FLUSH
            .scope(
                Box::new(move |verifier| {
                    assert!(matches!(verifier, Verifier::Sapling));
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(true)
                }),
                tokio::time::timeout(
                    Duration::from_secs(10),
                    verifier.oneshot(Request::CheckProposal(block.clone())),
                ),
            )
            .await
            .expect("mixed shielded dispatch must finish");
        assert!(
            matches!(
                result,
                Err(VerifyBlockError::Transaction(TransactionError::BadBalance))
            ),
            "reuse={reuse}: {result:?}",
        );
        let dispatched = dispatched.lock();
        assert_eq!(
            dispatched.len(),
            block.transactions.len() - usize::from(reuse)
        );
        assert_eq!(dispatched.contains(&cached_hash), !reuse);
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
    }
}
