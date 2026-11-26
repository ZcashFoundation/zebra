//! Block verification using resident mempool verdicts and ordinary fallback verification.

use std::{
    future::{pending, ready},
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use metrics::{Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit};
use tokio::sync::oneshot;
use tower::{service_fn, util::BoxCloneService};
use zebra_chain::{
    amount::NonNegative, block::ChainHistoryBlockTxAuthCommitmentHash,
    parameters::testnet::ConfiguredActivationHeights, transaction::VerifiedUnminedTx,
};
use zebra_node_services::mempool;

use crate::block::{
    mempool_reuse::{check_candidate, ReuseRules},
    ReuseAudits,
};

use super::*;

type Mempool = BoxCloneService<mempool::Request, mempool::Response, BoxError>;
type State = BoxCloneService<zs::Request, zs::Response, BoxError>;
type TxVerifier = BoxCloneService<tx::BlockRequest, tx::BlockResponse, BoxError>;

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

/// A spend of the output `[8; 32]:index` with the unlock script `unlock_script`, which pays 1
/// zatoshi to `OP_1`. Spends of different outputs have different transaction IDs.
fn spend_output(
    index: u32,
    unlock_script: &[u8],
    lock_time: LockTime,
    expiry_height: Height,
) -> Arc<Transaction> {
    Arc::new(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: [8; 32].into(),
                index,
            },
            unlock_script: transparent::Script::new(unlock_script),
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

fn spend(lock_time: LockTime, expiry_height: Height) -> Arc<Transaction> {
    spend_output(0, &[0x51], lock_time, expiry_height)
}

/// Returns `transaction` with the unlock script of its first input replaced by `unlock_script`.
/// The mined ID of a v5 transaction doesn't cover its unlock scripts, so it stays the same.
fn with_unlock_script(transaction: &Transaction, unlock_script: &[u8]) -> Arc<Transaction> {
    let mut inputs = transaction.inputs();
    let transparent::Input::PrevOut {
        unlock_script: script,
        ..
    } = &mut inputs[0]
    else {
        unreachable!("the test spends have a previous output as their first input")
    };
    *script = transparent::Script::new(unlock_script);
    Arc::new(transaction.clone().with_transparent_inputs(inputs))
}

/// A block at `height` on `network` with `spends` after a coinbase that claims the block subsidy
/// and `fees`.
fn block_with(
    network: &Network,
    height: Height,
    spends: Vec<Arc<Transaction>>,
    fees: u64,
) -> Arc<Block> {
    let subsidy = zebra_chain::parameters::subsidy::block_subsidy(height, network).unwrap();
    let coinbase = Arc::new(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![transparent::Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![transparent::Output::new(
            (subsidy + Amount::<NonNegative>::try_from(fees).unwrap()).unwrap(),
            transparent::Script::new(&[0xac]),
        )],
        LockTime::unlocked(),
        height,
    ));
    let genesis = block::genesis::regtest_genesis_block();
    let mut header = *genesis.header;
    header.previous_block_hash = genesis.hash();
    let transactions: Vec<_> = std::iter::once(coinbase).chain(spends).collect();
    header.merkle_root = transactions.iter().map(|tx| tx.hash()).collect();
    Arc::new(Block {
        header: Arc::new(header),
        transactions,
    })
}

fn candidate(network: &Network, height: Height, spend: Arc<Transaction>) -> Arc<Block> {
    block_with(network, height, vec![spend], FEE)
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

/// A mempool that has verified the spend of each of `blocks`, which share a parent, and answers
/// a lookup for any of them.
fn mempool_with_spends(blocks: &[Arc<Block>]) -> Mempool {
    let entries: std::collections::HashMap<_, _> = blocks
        .iter()
        .map(|block| {
            let spend = block.transactions[1].clone();
            (spend.hash(), entry(spend))
        })
        .collect();
    BoxCloneService::new(service_fn(move |request| {
        let mempool::Request::VerifiedTransactionsForBlock {
            transaction_hashes, ..
        } = request
        else {
            panic!("block verification must request only bounded verified candidates")
        };
        let verified = transaction_hashes
            .iter()
            .filter_map(|hash| Some((*hash, entries.get(hash)?.clone())))
            .collect();
        ready(Ok(mempool::Response::VerifiedTransactionsForBlock(
            verified,
        )))
    }))
}

/// A mempool that panics when the block verifier sends it a request.
fn panicking_mempool() -> Mempool {
    BoxCloneService::new(service_fn(
        |request: mempool::Request| -> std::future::Ready<Result<mempool::Response, BoxError>> {
            panic!("the block verifier must not send this mempool request: {request:?}")
        },
    ))
}

fn mock_state() -> State {
    BoxCloneService::new(service_fn(|request| {
        ready(Ok(match request {
            zs::Request::KnownBlock(_) => zs::Response::KnownBlock(None),
            zs::Request::MiningCandidate { .. } => zs::Response::MiningCandidate(None),
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

/// Returns `state`, which counts the requests that `calls` counts.
fn counting_state(state: State, calls: &Arc<Calls>) -> State {
    let calls = calls.clone();
    BoxCloneService::new(service_fn(move |request| {
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
    }))
}

/// A block verifier whose state and transaction verifier count the requests they get.
struct Harness {
    verifier: SemanticBlockVerifier<State, TxVerifier>,
    /// The block verifier's transaction verifier.
    transaction_verifier: TxVerifier,
    calls: Arc<Calls>,
}

impl Harness {
    /// Returns a block verifier on `network` with `state` and a real transaction verifier.
    fn new(network: &Network, state: State) -> Self {
        let calls = Arc::new(Calls::default());
        let state = counting_state(state, &calls);
        let transaction_verifier =
            Buffer::new(transaction::BlockTxVerifier::new(network, state.clone()), 1);
        let transactions = BoxCloneService::new(service_fn({
            let calls = calls.clone();
            move |request: tx::BlockRequest| {
                if request.transaction.is_coinbase() {
                    calls.coinbase.fetch_add(1, Ordering::SeqCst);
                } else {
                    calls.non_coinbase.fetch_add(1, Ordering::SeqCst);
                }
                transaction_verifier.clone().oneshot(request)
            }
        }));
        Self {
            verifier: SemanticBlockVerifier::new(network, state, transactions.clone()),
            transaction_verifier: transactions,
            calls,
        }
    }

    /// Gives the block verifier `mempool` now.
    fn with_mempool(self, mempool: Mempool) -> Self {
        let (sender, receiver) = oneshot::channel();
        sender.send(mempool).unwrap();
        self.with_mempool_receiver(receiver)
    }

    /// Gives the block verifier the mempool that `receiver` gets, when it arrives.
    fn with_mempool_receiver(mut self, receiver: oneshot::Receiver<Mempool>) -> Self {
        self.verifier = self.verifier.with_mempool(receiver);
        self
    }

    /// Turns on the background audits of reused transactions, which use the block verifier's
    /// transaction verifier.
    fn with_reuse_audits(mut self) -> Self {
        let audits = ReuseAudits::spawn(self.transaction_verifier.clone());
        self.verifier = self.verifier.with_reuse_audits(audits);
        self
    }

    /// Verifies and commits `block`.
    async fn verify(&mut self, block: Arc<Block>) -> Result<block::Hash, VerifyBlockError> {
        let verification = self.verifier.ready().await?.call(Request::Commit(block));
        tokio::time::timeout(Duration::from_secs(10), verification)
            .await
            .expect("mempool lookup and block verification must finish")
    }
}

/// Verifies `block` on `network`, with `mempool` and `state`.
async fn verify(
    network: &Network,
    block: Arc<Block>,
    mempool: Mempool,
    state: State,
) -> (Result<block::Hash, VerifyBlockError>, Arc<Calls>) {
    let mut harness = Harness::new(network, state).with_mempool(mempool);
    let result = harness.verify(block).await;
    (result, harness.calls)
}

/// The counter of transactions that the block verifier takes from the mempool.
const HIT: &str = "zebra.consensus.block.mempool_reuse.hit";
/// The counter of transactions that the block verifier verifies while it has a mempool.
const MISS: &str = "zebra.consensus.block.mempool_reuse.miss";
/// The counter of mempool lookups that the block verifier stopped waiting for.
const TIMEOUT: &str = "zebra.consensus.block.mempool_reuse.timeout";
/// The counter of mempool lookups that failed.
const ERROR: &str = "zebra.consensus.block.mempool_reuse.error";
/// The counter of background audits of reused transactions, by result.
const AUDIT: &str = "zebra.consensus.block.mempool_reuse.audit";
/// The results of the background audits.
const AUDIT_RESULTS: [&str; 4] = ["match", "mismatch", "inconclusive", "skipped"];

/// The counters the block verifier records for its reuse of the mempool's verification, with
/// their `result` labels.
const REUSE_COUNTERS: [(&str, Option<&str>); 8] = [
    (HIT, None),
    (MISS, None),
    (TIMEOUT, None),
    (ERROR, None),
    (AUDIT, Some(AUDIT_RESULTS[0])),
    (AUDIT, Some(AUDIT_RESULTS[1])),
    (AUDIT, Some(AUDIT_RESULTS[2])),
    (AUDIT, Some(AUDIT_RESULTS[3])),
];

/// Records the counters in [`REUSE_COUNTERS`], and ignores every other metric.
#[derive(Default)]
struct ReuseRecorder {
    counts: [Arc<AtomicU64>; REUSE_COUNTERS.len()],
}

impl ReuseRecorder {
    /// Returns the count of the counter `name` with the `result` label `result`.
    fn count(&self, name: &str, result: Option<&str>) -> u64 {
        let index = REUSE_COUNTERS
            .iter()
            .position(|&counter| counter == (name, result))
            .expect("the tests only read counters in REUSE_COUNTERS");
        self.counts[index].load(Ordering::SeqCst)
    }

    /// Returns the counts of `hit` and `miss`.
    fn hits_and_misses(&self) -> (u64, u64) {
        (self.count(HIT, None), self.count(MISS, None))
    }

    /// Returns the counts of the audit results, in the order of [`AUDIT_RESULTS`].
    fn audits(&self) -> [u64; 4] {
        AUDIT_RESULTS.map(|result| self.count(AUDIT, Some(result)))
    }

    /// Waits until the block verifier has counted `count` audit results, so that its background
    /// audits can finish.
    ///
    /// The deadline is longer than the audits' own timeout, so a test with paused time reaches an
    /// audit's timeout first.
    async fn wait_for_audits(&self, count: u64) {
        tokio::time::timeout(Duration::from_secs(120), async {
            while self.audits().iter().sum::<u64>() < count {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the block verifier counts the result of each audit");
    }
}

impl Recorder for ReuseRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        let result = key
            .labels()
            .find(|label| label.key() == "result")
            .map(|label| label.value());
        REUSE_COUNTERS
            .iter()
            .position(|&(name, label)| name == key.name() && label == result)
            .map_or_else(Counter::noop, |index| {
                Counter::from_arc(self.counts[index].clone())
            })
    }

    fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }

    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

fn assert_dispatches(calls: &Calls, non_coinbase: usize) {
    assert_eq!(calls.coinbase.load(Ordering::SeqCst), 1);
    assert_eq!(calls.non_coinbase.load(Ordering::SeqCst), non_coinbase);
    assert_eq!(calls.await_utxo.load(Ordering::SeqCst), non_coinbase);
}

/// The block verifier takes the mempool's verification of a transaction with the mempool's
/// authorizing data, whether the block has the mempool's transaction object, as a block rebuilt
/// from a compact announcement does, or a copy with the same bytes.
#[tokio::test]
async fn resident_hit_skips_transaction_dispatch_and_utxo_lookup() {
    let _init_guard = zebra_test::init();
    let network = network();
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    let shared: fn(&Arc<Transaction>) -> Arc<Transaction> = Arc::clone;
    let copied: fn(&Arc<Transaction>) -> Arc<Transaction> =
        |transaction| Arc::new((**transaction).clone());
    for (name, mempool_transaction) in [("shared", shared), ("copied", copied)] {
        // Even a supplied coinbase candidate must go through the real transaction verifier.
        let entries = block
            .transactions
            .iter()
            .map(mempool_transaction)
            .map(entry)
            .collect();
        let pool = mempool(&block, entries);
        let (result, calls) = verify(&network, block.clone(), pool, mock_state()).await;
        assert_eq!(result.unwrap(), block.hash(), "{name}");
        assert_dispatches(&calls, 0);
        assert_eq!(calls.commits.load(Ordering::SeqCst), 1, "{name}");
    }
}

/// The block verifier doesn't ask the mempool about a block with only a coinbase transaction,
/// because the mempool never has a coinbase transaction.
#[tokio::test]
async fn coinbase_only_block_sends_no_mempool_lookup() {
    let _init_guard = zebra_test::init();
    let network = network();
    let block = block_with(&network, Height(3), Vec::new(), 0);
    let (result, calls) = verify(&network, block.clone(), panicking_mempool(), mock_state()).await;
    assert_eq!(result.unwrap(), block.hash());
    assert_dispatches(&calls, 0);
    assert_eq!(calls.commits.load(Ordering::SeqCst), 1);
}

/// The block verifier asks the mempool for its verified transactions only after the block
/// checks. So a block whose body doesn't match its header sends no lookup, and nor do blocks that
/// fail the time check or the coinbase position check, the checks after the Merkle root check.
#[tokio::test]
async fn block_that_fails_a_block_check_sends_no_mempool_lookup() {
    let _init_guard = zebra_test::init();
    let network = network();
    let spend = spend(LockTime::unlocked(), Height(0));
    let block = candidate(&network, Height(3), spend.clone());

    // A header that commits to other transactions.
    let mut changed_body = (*block).clone();
    Arc::make_mut(&mut changed_body.header).merkle_root = block::merkle::Root([0xff; 32]);

    // A block from too far in the future. Its time isn't in its Merkle root, so only the time
    // check fails.
    let mut future_block = (*block).clone();
    Arc::make_mut(&mut future_block.header).time = chrono::Utc::now() + chrono::Duration::hours(3);

    // A block with a second coinbase transaction after its spend. Its output differs from the
    // first coinbase's, so the two have different IDs and the Merkle root check passes.
    let second_coinbase = Arc::new(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![transparent::Input::Coinbase {
            height: Height(3),
            data: vec![1],
            sequence: u32::MAX,
        }],
        vec![transparent::Output::new(
            1.try_into().unwrap(),
            transparent::Script::new(&[0x51]),
        )],
        LockTime::unlocked(),
        Height(3),
    ));
    let two_coinbases = block_with(&network, Height(3), vec![spend, second_coinbase], FEE);

    let is_bad_merkle_root: fn(&VerifyBlockError) -> bool = |error| {
        matches!(
            error,
            VerifyBlockError::Block {
                source: BlockError::BadMerkleRoot { .. }
            }
        )
    };
    let is_time_error: fn(&VerifyBlockError) -> bool =
        |error| matches!(error, VerifyBlockError::Time(_));
    let is_coinbase_after_first: fn(&VerifyBlockError) -> bool = |error| {
        matches!(
            error,
            VerifyBlockError::Block {
                source: BlockError::Transaction(TransactionError::CoinbaseAfterFirst)
            }
        )
    };
    let cases = [
        (
            "a header that commits to other transactions",
            changed_body,
            is_bad_merkle_root,
        ),
        ("a time too far in the future", future_block, is_time_error),
        (
            "a coinbase transaction after the first transaction",
            (*two_coinbases).clone(),
            is_coinbase_after_first,
        ),
    ];

    for (name, block, is_expected_error) in cases {
        let (result, calls) =
            verify(&network, Arc::new(block), panicking_mempool(), mock_state()).await;
        let Err(error) = result else {
            panic!("{name}: expected a block check error, got {result:?}")
        };
        assert!(is_expected_error(&error), "{name}: {error:?}");
        assert_eq!(calls.coinbase.load(Ordering::SeqCst), 0, "{name}");
        assert_eq!(calls.non_coinbase.load(Ordering::SeqCst), 0, "{name}");
        assert_eq!(calls.commits.load(Ordering::SeqCst), 0, "{name}");
    }
}

/// A version 4 spend of the output `[8; 32]:0`, which pays 1 zatoshi to `OP_1`.
fn v4_spend() -> Arc<Transaction> {
    Arc::new(Transaction::test_v4(
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
        LockTime::unlocked(),
        Height(0),
    ))
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

    // A version 4 transaction doesn't contain the consensus branch ID that its signatures commit
    // to, so the block context rules accept it in both network upgrades, and only the height the
    // mempool verified it for keeps the block verifier from reusing that verification.
    let v4_spend = v4_spend();
    let block = candidate(&network, Height(3), v4_spend.clone());
    for height in [Height(1), Height(3)] {
        let context = tx::BlockContext {
            height,
            time: block.header.time,
            network_upgrade: NetworkUpgrade::current(&network, height),
        };
        assert!(tx::check_block_context_rules(&v4_spend, &context, &network).is_ok());
    }
    let mut old_epoch = entry(v4_spend);
    old_epoch.height = Some(Height(0));
    let pool = mempool(&block, vec![old_epoch]);
    let (result, calls) = verify(&network, block.clone(), pool, mock_state()).await;
    assert_eq!(result.unwrap(), block.hash());
    assert_dispatches(&calls, 1);
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

/// The block's signature operation limit counts the legacy and the P2SH signature operations of
/// the transactions that the block verifier takes from the mempool, together with the ones of the
/// transactions it verifies.
#[tokio::test]
async fn reused_signature_operations_count_toward_the_block_limit() {
    let _init_guard = zebra_test::init();
    let network = network();
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    assert_eq!(block.transactions[0].sigops().unwrap(), 1);
    const MAX: u32 = MAX_BLOCK_SIGOPS;
    for (legacy, p2sh) in [
        (MAX - 1, 0),
        (MAX, 0),
        (0, MAX - 1),
        (0, MAX),
        (MAX / 2, MAX / 2 - 1),
        (MAX / 2, MAX / 2),
        // A count that would wrap the block's total back below the limit.
        (u32::MAX, 0),
    ] {
        let mut cached = entry(block.transactions[1].clone());
        cached.legacy_sigop_count = legacy;
        cached.p2sh_sigop_count = p2sh;
        let pool = mempool(&block, vec![cached]);
        let (result, calls) = verify(&network, block.clone(), pool, mock_state()).await;
        // The coinbase's signature operation, then the reused transaction's.
        let total = 1_u32.saturating_add(legacy).saturating_add(p2sh);
        if total <= MAX {
            assert_eq!(result.unwrap(), block.hash(), "({legacy}, {p2sh})");
        } else {
            assert!(
                matches!(
                    result,
                    Err(VerifyBlockError::Block {
                        source: BlockError::TooManyTransparentSignatureOperations { sigops, .. }
                    }) if sigops == total
                ),
                "({legacy}, {p2sh}): {result:?}"
            );
            assert_eq!(calls.commits.load(Ordering::SeqCst), 0);
        }
        assert_dispatches(&calls, 0);
    }

    // Two reused transactions whose counts would wrap the total when they are added together.
    let spends = vec![
        spend_output(0, &[0x51], LockTime::unlocked(), Height(0)),
        spend_output(1, &[0x51], LockTime::unlocked(), Height(0)),
    ];
    let block = block_with(&network, Height(3), spends.clone(), 2 * FEE);
    let entries = spends
        .into_iter()
        .map(|spend| {
            let mut cached = entry(spend);
            cached.legacy_sigop_count = u32::MAX / 2 + 1;
            cached
        })
        .collect();
    let pool = mempool(&block, entries);
    let (result, calls) = verify(&network, block, pool, mock_state()).await;
    assert!(
        matches!(
            result,
            Err(VerifyBlockError::Block {
                source: BlockError::TooManyTransparentSignatureOperations { sigops, .. }
            }) if sigops == u32::MAX
        ),
        "two reused transactions: {result:?}"
    );
    assert_dispatches(&calls, 0);
}

/// A block in which the block verifier drops a candidate that it can't reuse, the mempool's
/// verifications for the block, and the check of the error that a later candidate must fail
/// with.
struct DroppedCandidateCase {
    name: &'static str,
    entries: Vec<VerifiedUnminedTx>,
    block: Arc<Block>,
    is_expected_error: fn(&TransactionError) -> bool,
}

/// Returns blocks at height 3 on `network` in which the block verifier drops a candidate, and
/// must then reject a later candidate for its authorizing data or its block context rules.
fn dropped_candidate_cases(network: &Network) -> [DroppedCandidateCase; 4] {
    let block = |spends, fees| block_with(network, Height(3), spends, fees);
    // A spend with other authorizing data and the same mined ID.
    let twin = |transaction: &Arc<Transaction>| {
        let twin = with_unlock_script(transaction, &[0x51]);
        assert_eq!(twin.hash(), transaction.hash());
        assert_ne!(twin.unmined_id(), transaction.unmined_id());
        entry(twin)
    };
    let unlocked = LockTime::unlocked();
    // Spends in the same block spend different outputs, so they have different IDs.
    let expired = spend_output(0, &[0x51], unlocked, Height(2));
    let valid = spend_output(1, &[0x51], unlocked, Height(0));
    // `OP_2 OP_1` leaves a true value, so this spend is valid too.
    let valid_52 = spend_output(1, &[0x52], unlocked, Height(0));
    // `OP_RETURN` fails the script.
    let failing = spend_output(1, &[0x6a], unlocked, Height(0));
    let mut old_epoch = entry(valid.clone());
    old_epoch.height = Some(Height(0));

    let is_expired: fn(&TransactionError) -> bool =
        |error| matches!(error, TransactionError::ExpiredTransaction { .. });
    let coinbase_then_expired = block(vec![expired.clone()], FEE);
    let coinbase_then_twin = block(vec![failing.clone()], FEE);
    [
        DroppedCandidateCase {
            name: "a coinbase candidate, then an expired spend",
            entries: vec![
                entry(coinbase_then_expired.transactions[0].clone()),
                entry(expired.clone()),
            ],
            block: coinbase_then_expired,
            is_expected_error: is_expired,
        },
        DroppedCandidateCase {
            name: "a candidate from another network upgrade, then an expired spend",
            entries: vec![old_epoch, entry(expired.clone())],
            block: block(vec![valid, expired.clone()], 2 * FEE),
            is_expected_error: is_expired,
        },
        DroppedCandidateCase {
            name: "a candidate with other authorizing data, then an expired spend",
            entries: vec![twin(&valid_52), entry(expired.clone())],
            block: block(vec![valid_52, expired], 2 * FEE),
            is_expected_error: is_expired,
        },
        DroppedCandidateCase {
            name: "a coinbase candidate, then a candidate with other authorizing data",
            entries: vec![
                entry(coinbase_then_twin.transactions[0].clone()),
                twin(&failing),
            ],
            block: coinbase_then_twin,
            is_expected_error: |error| matches!(error, TransactionError::Script(_)),
        },
    ]
}

/// After the block verifier drops a candidate that it can't reuse, it still checks the
/// candidates after it: their authorizing data, and their block context rules.
#[tokio::test]
async fn candidates_after_a_dropped_candidate_are_still_checked() {
    let _init_guard = zebra_test::init();
    let network = network();
    for case in dropped_candidate_cases(&network) {
        let DroppedCandidateCase {
            name,
            entries,
            block,
            is_expected_error,
        } = case;
        let pool = mempool(&block, entries);
        let (result, calls) = verify(&network, block, pool, mock_state()).await;
        let Err(VerifyBlockError::Transaction(error)) = result else {
            panic!("{name}: expected a transaction error, got {result:?}")
        };
        assert!(is_expected_error(&error), "{name}: {error:?}");
        assert_eq!(calls.commits.load(Ordering::SeqCst), 0, "{name}");
    }
}

/// A transaction that the block verifier verifies can spend an output of an earlier transaction
/// in the block, which the block verifier took from the mempool.
#[tokio::test]
async fn dispatched_transaction_can_spend_a_reused_transactions_output() {
    let _init_guard = zebra_test::init();
    let network = network();
    let half_fee = FEE / 2;

    // `reused` spends an output in the state, worth `FEE + 1`, so its fee is half of `FEE`.
    let reused = Arc::new(Transaction::test_v5(
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
            (half_fee + 1).try_into().unwrap(),
            transparent::Script::new(&[0x51]),
        )],
        LockTime::unlocked(),
        Height(0),
    ));
    // `verified` spends `reused`'s output, and isn't in the mempool. Its fee is the other half.
    let verified = Arc::new(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: reused.hash(),
                index: 0,
            },
            unlock_script: transparent::Script::new(&[0x51]),
            sequence: 0,
        }],
        vec![transparent::Output::new(
            1.try_into().unwrap(),
            transparent::Script::new(&[0x51]),
        )],
        LockTime::unlocked(),
        Height(0),
    ));
    let block = block_with(&network, Height(3), vec![reused.clone(), verified], FEE);
    let mut cached = entry(reused);
    cached.miner_fee = half_fee.try_into().unwrap();

    let pool = mempool(&block, vec![cached]);
    let (result, calls) = verify(&network, block.clone(), pool, mock_state()).await;
    assert_eq!(result.unwrap(), block.hash());
    assert_eq!(calls.coinbase.load(Ordering::SeqCst), 1);
    // Only `verified`, which finds `reused`'s output in the block's own outputs.
    assert_eq!(calls.non_coinbase.load(Ordering::SeqCst), 1);
    assert_eq!(calls.await_utxo.load(Ordering::SeqCst), 0);
    assert_eq!(calls.commits.load(Ordering::SeqCst), 1);
}

/// A mempool lookup that fails, or that the mempool doesn't answer in time, makes the block
/// verifier verify every transaction, and it counts the lookup and the misses.
#[tokio::test]
async fn mempool_errors_and_response_or_readiness_timeouts_fall_back() {
    let _init_guard = zebra_test::init();
    let recorder = ReuseRecorder::default();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let network = network();
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    // The mempool never becomes ready, or never answers the request.
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
    // A readiness wait counts as a timeout, like a request that isn't answered in time.
    assert_eq!(recorder.count(TIMEOUT, None), 2);
    assert_eq!(recorder.hits_and_misses(), (0, 2));

    let pool = BoxCloneService::new(service_fn(|_| {
        ready(Err::<mempool::Response, BoxError>(
            "mempool unavailable".into(),
        ))
    }));
    let (result, calls) = verify(&network, block.clone(), pool, mock_state()).await;
    assert_eq!(result.unwrap(), block.hash());
    assert_dispatches(&calls, 1);
    assert_eq!(recorder.count(ERROR, None), 1);
    assert_eq!(recorder.count(TIMEOUT, None), 2);
    assert_eq!(recorder.hits_and_misses(), (0, 3));
}

/// The block verifier waits for the mempool's answer for at most the reuse timeout: it reuses an
/// answer that arrives just before the timeout, and counts a timeout and verifies the block's
/// transactions itself when the answer arrives just after it.
#[tokio::test(start_paused = true)]
async fn mempool_lookup_waits_for_the_answer_until_the_reuse_timeout() {
    let _init_guard = zebra_test::init();
    let recorder = ReuseRecorder::default();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let network = network();
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    let timeout = crate::block::mempool_reuse::MEMPOOL_REUSE_TIMEOUT;
    // A slow mempool delays a block's verification by this long, so it stays short.
    assert_eq!(timeout, Duration::from_millis(10));
    let millisecond = Duration::from_millis(1);

    // A transaction verifier that answers at once, because paused time skips ahead while a task
    // waits for a thread outside the runtime, as script verification does.
    let dispatched = Arc::new(AtomicUsize::new(0));
    let transaction_verifier: TxVerifier = BoxCloneService::new(service_fn({
        let dispatched = dispatched.clone();
        move |request: tx::BlockRequest| {
            let coinbase = request.transaction.is_coinbase();
            if !coinbase {
                dispatched.fetch_add(1, Ordering::SeqCst);
            }
            ready(Ok::<_, BoxError>(tx::BlockResponse {
                tx_id: request.transaction.unmined_id(),
                miner_fee: (!coinbase).then(|| FEE.try_into().unwrap()),
                sigops: 0,
            }))
        }
    }));

    // The answer after the timeout comes too late, so the block verifier verifies the spend.
    for (delay, dispatches) in [(timeout - millisecond, 0), (timeout + millisecond, 1)] {
        // A mempool that has verified the block's spend, and answers after `delay`.
        let answer = mempool(&block, vec![entry(block.transactions[1].clone())]);
        let pool = BoxCloneService::new(service_fn(move |request| {
            let answer = answer.clone();
            async move {
                tokio::time::sleep(delay).await;
                answer.oneshot(request).await
            }
        }));
        let (sender, receiver) = oneshot::channel();
        sender.send(pool).unwrap();
        let verifier =
            SemanticBlockVerifier::new(&network, mock_state(), transaction_verifier.clone())
                .with_mempool(receiver);
        let result = verifier.oneshot(Request::Commit(block.clone())).await;
        assert_eq!(result.unwrap(), block.hash(), "{delay:?}");
        assert_eq!(dispatched.load(Ordering::SeqCst), dispatches, "{delay:?}");
    }
    assert_eq!(recorder.count(TIMEOUT, None), 1);
    assert_eq!(recorder.hits_and_misses(), (1, 1));
}

/// The block verifier counts the transactions it takes from the mempool once the mempool arrives,
/// even if the mempool arrives after the verifier has started verifying blocks.
#[tokio::test]
async fn mempool_that_arrives_after_a_block_is_used_for_later_blocks() {
    let _init_guard = zebra_test::init();
    let recorder = ReuseRecorder::default();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let network = network();
    // Two blocks on the same parent, which spend different outputs.
    let unlocked = LockTime::unlocked();
    let first = candidate(
        &network,
        Height(3),
        spend_output(0, &[0x51], unlocked, Height(0)),
    );
    let second = candidate(
        &network,
        Height(3),
        spend_output(1, &[0x51], unlocked, Height(0)),
    );
    let (sender, receiver) = oneshot::channel();
    let mut harness = Harness::new(&network, mock_state()).with_mempool_receiver(receiver);

    assert_eq!(harness.verify(first.clone()).await.unwrap(), first.hash());
    assert_eq!(harness.calls.non_coinbase.load(Ordering::SeqCst), 1);
    // Nothing is counted while the verifier has no mempool.
    assert_eq!(recorder.hits_and_misses(), (0, 0));

    sender
        .send(mempool(
            &second,
            vec![entry(second.transactions[1].clone())],
        ))
        .unwrap();
    assert_eq!(harness.verify(second.clone()).await.unwrap(), second.hash());
    // The second block's spend was taken from the mempool, so it wasn't verified.
    assert_eq!(harness.calls.non_coinbase.load(Ordering::SeqCst), 1);
    assert_eq!(recorder.hits_and_misses(), (1, 0));
}

/// The block verifier router gives the block verifier the mempool that its caller sends after
/// the first block, and the block verifier then reuses the mempool's verification.
#[tokio::test]
async fn router_reuses_mempool_verification_from_a_mempool_sent_after_the_first_block() {
    let _init_guard = zebra_test::init();
    let recorder = ReuseRecorder::default();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let network = network();
    let unlocked = LockTime::unlocked();
    let first = candidate(
        &network,
        Height(3),
        spend_output(0, &[0x51], unlocked, Height(0)),
    );
    let second = candidate(
        &network,
        Height(3),
        spend_output(1, &[0x51], unlocked, Height(0)),
    );

    // The router and its checkpoint task also ask the state for its tip and its checkpoints.
    let state: State = BoxCloneService::new(service_fn(|request| match request {
        zs::Request::Tip => ready(Ok(zs::Response::Tip(None))).boxed(),
        zs::Request::BestChainBlockHash(_) => ready(Ok(zs::Response::BlockHash(None))).boxed(),
        request => mock_state().oneshot(request).boxed(),
    }));
    let calls = Arc::new(Calls::default());
    let state = counting_state(state, &calls);
    let (sender, receiver) = oneshot::channel::<Mempool>();
    // Regtest's only checkpoint is its genesis block, so the semantic block verifier verifies
    // these blocks.
    let (router, _mempool_verifier, _handles, _) =
        crate::router::init(crate::Config::default(), &network, state, receiver).await;

    assert_eq!(
        router
            .clone()
            .oneshot(Request::Commit(first.clone()))
            .await
            .unwrap(),
        first.hash()
    );
    assert_eq!(calls.await_utxo.load(Ordering::SeqCst), 1);
    assert_eq!(recorder.hits_and_misses(), (0, 0));

    sender
        .send(mempool(
            &second,
            vec![entry(second.transactions[1].clone())],
        ))
        .unwrap();
    // Let the router's task give the mempool to the verifiers.
    tokio::task::yield_now().await;

    assert_eq!(
        router
            .oneshot(Request::Commit(second.clone()))
            .await
            .unwrap(),
        second.hash()
    );
    assert_eq!(recorder.hits_and_misses(), (1, 0));
    // `router::init` turns the audits on, so the router audits the second block's spend.
    recorder.wait_for_audits(1).await;
    assert_eq!(recorder.audits(), [1, 0, 0, 0]);
    // The first block's spend and the audit looked up their spent outputs, but the second block
    // didn't verify its spend.
    assert_eq!(calls.await_utxo.load(Ordering::SeqCst), 2);
}

/// A state whose `AwaitUtxo` answers are `utxo_lookup`'s, and which answers the other requests
/// like [`mock_state`].
fn state_with_utxo_lookup(
    utxo_lookup: fn() -> BoxFuture<'static, Result<zs::Response, BoxError>>,
) -> State {
    BoxCloneService::new(service_fn(move |request| match request {
        zs::Request::AwaitUtxo(_) => utxo_lookup(),
        request => mock_state().oneshot(request).boxed(),
    }))
}

/// Waits until `condition` is true, so that background tasks can run.
async fn wait_until(condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the background tasks make the condition true");
}

/// The block verifier verifies a transaction that it took from the mempool again after the block
/// is committed, and the transaction verifier agrees with a correct mempool verification.
#[tokio::test]
async fn reuse_audit_agrees_with_a_correct_mempool_verification() {
    let _init_guard = zebra_test::init();
    let recorder = ReuseRecorder::default();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let network = network();
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    let mut harness = Harness::new(&network, mock_state())
        .with_mempool(mempool(&block, vec![entry(block.transactions[1].clone())]))
        .with_reuse_audits();

    assert_eq!(harness.verify(block.clone()).await.unwrap(), block.hash());
    recorder.wait_for_audits(1).await;
    assert_eq!(recorder.audits(), [1, 0, 0, 0]);
    // The audit verified the spend, so the block itself didn't.
    assert_eq!(harness.calls.non_coinbase.load(Ordering::SeqCst), 1);
    assert_eq!(harness.calls.await_utxo.load(Ordering::SeqCst), 1);
}

/// An audit that disagrees with the mempool's verification is counted as a mismatch, but the
/// block it audits stays committed.
#[tokio::test]
async fn reuse_audit_reports_a_mismatch_without_rejecting_the_block() {
    let _init_guard = zebra_test::init();
    let recorder = ReuseRecorder::default();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let network = network();

    // The mempool's verification has a signature operation that the transaction doesn't have.
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    let mut wrong_sigops = entry(block.transactions[1].clone());
    wrong_sigops.legacy_sigop_count = 5;
    // `OP_RETURN` fails the spend's script, but the block verifier takes the mempool's
    // verification of the same transaction object.
    let failing = spend_output(0, &[0x6a], LockTime::unlocked(), Height(0));
    let failing_block = candidate(&network, Height(3), failing.clone());

    let cases = [
        ("a wrong signature operation count", block, wrong_sigops),
        ("a failing script", failing_block, entry(failing)),
    ];
    for (mismatches, (name, block, cached)) in (1..).zip(cases) {
        let mut harness = Harness::new(&network, mock_state())
            .with_mempool(mempool(&block, vec![cached]))
            .with_reuse_audits();
        assert_eq!(harness.verify(block.clone()).await.unwrap(), block.hash());
        assert_eq!(harness.calls.commits.load(Ordering::SeqCst), 1, "{name}");
        recorder.wait_for_audits(mismatches).await;
        assert_eq!(recorder.audits(), [0, mismatches, 0, 0], "{name}");
    }
}

/// An audit that gets no verdict from the transaction verifier is inconclusive: its spent output
/// doesn't arrive in time, the state's lookup times out, or the state fails.
#[tokio::test(start_paused = true)]
async fn reuse_audit_without_a_verdict_is_inconclusive() {
    let _init_guard = zebra_test::init();
    let recorder = ReuseRecorder::default();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let network = network();
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));

    let utxo_lookups: [(&str, fn() -> BoxFuture<'static, _>); 3] = [
        ("an output that doesn't arrive", || pending().boxed()),
        ("a lookup that times out", || {
            ready(Err(tower::timeout::error::Elapsed::new().into())).boxed()
        }),
        ("a state that fails", || {
            ready(Err("state failed".into())).boxed()
        }),
    ];
    for (inconclusive, (name, utxo_lookup)) in (1..).zip(utxo_lookups) {
        let mut harness = Harness::new(&network, state_with_utxo_lookup(utxo_lookup))
            .with_mempool(mempool(&block, vec![entry(block.transactions[1].clone())]))
            .with_reuse_audits();
        assert_eq!(harness.verify(block.clone()).await.unwrap(), block.hash());
        recorder.wait_for_audits(inconclusive).await;
        assert_eq!(recorder.audits(), [0, 0, inconclusive, 0], "{name}");
    }
}

/// The block verifier audits one block at a time, and keeps at most one more block's audit
/// waiting, so it skips the audits of the blocks after that.
#[tokio::test]
async fn reuse_audits_over_the_limit_are_skipped() {
    let _init_guard = zebra_test::init();
    let recorder = ReuseRecorder::default();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let network = network();
    let blocks: Vec<_> = (0..3)
        .map(|index| {
            let spend = spend_output(index, &[0x51], LockTime::unlocked(), Height(0));
            candidate(&network, Height(3), spend)
        })
        .collect();

    // The audits never get their spent outputs, so the first audit keeps running.
    let mut harness = Harness::new(&network, state_with_utxo_lookup(|| pending().boxed()))
        .with_mempool(mempool_with_spends(&blocks))
        .with_reuse_audits();
    for (committed, block) in (1..).zip(&blocks) {
        assert_eq!(harness.verify(block.clone()).await.unwrap(), block.hash());
        if committed == 1 {
            // The first audit is running, and waits for its spent output.
            wait_until(|| harness.calls.await_utxo.load(Ordering::SeqCst) == 1).await;
        }
    }
    // The second block's audit waits, and the third block's audit is skipped.
    assert_eq!(recorder.audits(), [0, 0, 0, 1]);
}

/// A block that solves a validated block proposal is committed with the proposal's validation
/// result, so the block verifier audits a transaction that the proposal took from the mempool once
/// that block is committed.
#[tokio::test]
async fn solved_proposal_is_audited_with_the_proposals_reused_transaction() {
    let _init_guard = zebra_test::init();
    let recorder = ReuseRecorder::default();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let network = network();
    let proposal = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    let mut solved = (*proposal).clone();
    Arc::make_mut(&mut solved.header).nonce = [1; 32].into();
    let solved = Arc::new(solved);
    assert_ne!(solved.hash(), proposal.hash());

    // The state validates the proposal with the spend's output, as `mock_state` answers it, and
    // commits the validation result for the solved block.
    let spent_outputs: std::collections::HashMap<_, _> = proposal.transactions[1]
        .spent_outpoints()
        .map(|outpoint| {
            let output = transparent::Output::new(
                (FEE + 1).try_into().unwrap(),
                transparent::Script::new(&[0x51]),
            );
            (
                outpoint,
                transparent::OrderedUtxo::new(output, Height(1), 1),
            )
        })
        .collect();
    let state: State = BoxCloneService::new(service_fn({
        let network = network.clone();
        move |request| match request {
            zs::Request::CheckBlockProposalValidity(block) => {
                let contextual = zs::ContextuallyVerifiedBlock::with_block_and_spent_utxos(
                    block,
                    spent_outputs.clone(),
                    zebra_chain::amount::DeferredPoolBalanceChange::zero(),
                    &network,
                    Default::default(),
                )
                .expect("the spend's output covers its value");
                ready(Ok(zs::Response::ValidBlockProposal(Box::new(contextual)))).boxed()
            }
            zs::Request::CommitContextuallyVerifiedBlock(contextual) => {
                let committed = zs::SemanticallyVerifiedBlock::from(*contextual);
                ready(Ok(zs::Response::Committed(committed.hash))).boxed()
            }
            request => mock_state().oneshot(request).boxed(),
        }
    }));
    let mut harness = Harness::new(&network, state)
        .with_mempool(mempool(
            &proposal,
            vec![entry(proposal.transactions[1].clone())],
        ))
        .with_reuse_audits();

    let validation = harness
        .verifier
        .ready()
        .await
        .unwrap()
        .call(Request::CheckProposal(proposal.clone()));
    assert_eq!(validation.await.unwrap(), proposal.hash());
    // The proposal took the spend from the mempool.
    assert_eq!(harness.calls.non_coinbase.load(Ordering::SeqCst), 0);

    assert_eq!(harness.verify(solved.clone()).await.unwrap(), solved.hash());
    recorder.wait_for_audits(1).await;
    assert_eq!(recorder.audits(), [1, 0, 0, 0]);
    // Only the audit verified the spend.
    assert_eq!(harness.calls.non_coinbase.load(Ordering::SeqCst), 1);
}

/// A mempool answer of the wrong kind is a bug in the mempool service, so the block verifier panics
/// instead of verifying every transaction without a trace.
#[tokio::test]
#[should_panic(
    expected = "the mempool answers VerifiedTransactionsForBlock with VerifiedTransactionsForBlock"
)]
async fn mempool_answer_of_the_wrong_kind_is_a_bug() {
    let _init_guard = zebra_test::init();
    let network = network();
    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    let pool = BoxCloneService::new(service_fn(|_| {
        ready(Ok::<_, BoxError>(
            mempool::Response::CheckedForVerifiedTransactions,
        ))
    }));
    let _ = verify(&network, block, pool, mock_state()).await;
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

/// A P2SH output with the redeem script hash `script_hash`. The mempool only accepts spends of
/// standard scripts, and P2SH outputs are standard.
fn p2sh(script_hash: [u8; 20]) -> transparent::Output {
    let mut script = vec![0xa9, 0x14];
    script.extend_from_slice(&script_hash);
    script.push(0x87);
    transparent::Output::new(
        (FEE + 1).try_into().unwrap(),
        transparent::Script::new(&script),
    )
}

/// The hash of the redeem script `OP_1`. An input with the unlock script [`PUSH_OP_1`] spends a
/// P2SH output with this hash.
fn op_1_hash() -> [u8; 20] {
    hex::decode("da1745e9b549bd0bfa1a569971c77eba30cd5a4b")
        .unwrap()
        .try_into()
        .unwrap()
}

/// An unlock script that pushes the redeem script `OP_1`.
const PUSH_OP_1: &[u8] = &[0x01, 0x51];

/// The hash of a redeem script with a signature operation that it never runs:
/// `OP_0 OP_IF OP_CHECKSIG OP_ENDIF OP_1`. P2SH signature operations are counted from the
/// script, whether or not they run.
fn checksig_hash() -> [u8; 20] {
    hex::decode("2c93496b19af9ee138c38a1ac362a8de4a91e356")
        .unwrap()
        .try_into()
        .unwrap()
}

/// An unlock script that pushes the redeem script `OP_0 OP_IF OP_CHECKSIG OP_ENDIF OP_1`.
const PUSH_CHECKSIG: &[u8] = &[0x05, 0x00, 0x63, 0xac, 0x68, 0x51];

/// A spend for the differential tests: the transaction, the output it spends, whether that output
/// is a coinbase output, and whether the block verifier reuses the mempool's verification of it.
struct Variant {
    name: &'static str,
    transaction: Arc<Transaction>,
    spent: transparent::Output,
    spent_from_coinbase: bool,
    /// Whether the block verifier reuses the mempool's verification from [`Reuse::older_tip`].
    older: Result<(), NotReused>,
    /// Whether the block verifier reuses the mempool's verification from [`Reuse::newer_tip`].
    newer: Result<(), NotReused>,
}

/// Why the block verifier doesn't reuse the mempool's verification of a [`Variant`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotReused {
    /// The mempool rejects the transaction.
    RejectedByMempool,
    /// The mempool accepts the transaction, but the block verifier's check of the block context
    /// rules at the block's height and time rejects it.
    RejectedByRecheck,
    /// The block verifier verifies the transaction instead of taking the mempool's verification.
    Ineligible,
}

/// The block verifier reuses the mempool's verification of a [`Variant`].
const REUSED: Result<(), NotReused> = Ok(());
/// The mempool rejects a [`Variant`].
const MEMPOOL_REJECTS: Result<(), NotReused> = Err(NotReused::RejectedByMempool);
/// The block verifier's check of the block context rules rejects a [`Variant`].
const RECHECK_REJECTS: Result<(), NotReused> = Err(NotReused::RejectedByRecheck);

impl Variant {
    /// A variant named `name`, whose `transaction` spends a P2SH output with the redeem script
    /// `OP_1`, and whose mempool verification from the older and newer tips the block verifier
    /// uses as `older` and `newer` say.
    fn new(
        name: &'static str,
        transaction: Arc<Transaction>,
        older: Result<(), NotReused>,
        newer: Result<(), NotReused>,
    ) -> Self {
        Self {
            name,
            transaction,
            spent: p2sh(op_1_hash()),
            spent_from_coinbase: false,
            older,
            newer,
        }
    }
}

/// A spend of the output `[8; 32]:0` that pays 1 zatoshi, for the differential tests.
#[derive(Clone, Copy)]
struct Spend {
    upgrade: NetworkUpgrade,
    unlock_script: &'static [u8],
    output_script: &'static [u8],
    lock_time: LockTime,
    expiry_height: Height,
}

impl Default for Spend {
    /// A valid NU6 spend of a P2SH output with the redeem script `OP_1`, which never expires.
    fn default() -> Self {
        Self {
            upgrade: NetworkUpgrade::Nu6,
            unlock_script: PUSH_OP_1,
            output_script: &[0x51],
            lock_time: LockTime::unlocked(),
            expiry_height: Height(0),
        }
    }
}

impl Spend {
    /// Returns this spend as a version 5 transaction.
    fn v5(self) -> Arc<Transaction> {
        Arc::new(Transaction::test_v5(
            self.upgrade,
            self.inputs(),
            self.outputs(),
            self.lock_time,
            self.expiry_height,
        ))
    }

    /// Returns this spend as a version 4 transaction, which doesn't contain its network upgrade.
    fn v4(self) -> Arc<Transaction> {
        Arc::new(Transaction::test_v4(
            self.inputs(),
            self.outputs(),
            self.lock_time,
            self.expiry_height,
        ))
    }

    fn inputs(&self) -> Vec<transparent::Input> {
        vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: [8; 32].into(),
                index: 0,
            },
            unlock_script: transparent::Script::new(self.unlock_script),
            sequence: 0,
        }]
    }

    fn outputs(&self) -> Vec<transparent::Output> {
        vec![transparent::Output::new(
            1.try_into().unwrap(),
            transparent::Script::new(self.output_script),
        )]
    }
}

/// The network of the differential tests: Canopy at 1, NU5 at 2, and NU6 at 4.
fn differential_network() -> Network {
    Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(4),
            ..Default::default()
        }
        .into(),
    )
}

/// The time of the blocks in the differential tests.
fn differential_block_time() -> DateTime<Utc> {
    block::genesis::regtest_genesis_block().header.time + chrono::Duration::hours(1)
}

/// Where the mempool verified a [`Variant`], and the block that reuses that verification.
struct Reuse {
    network: Network,
    /// The height of the next block when the mempool verified the transaction.
    mempool_height: Height,
    /// The next block's median time past when the mempool verified the transaction.
    mempool_time: DateTime<Utc>,
    /// The block that contains the transaction.
    block: tx::BlockContext,
}

impl Reuse {
    /// A block at height 5 on the [`differential_network`], at the [`differential_block_time`],
    /// and the mempool's verification for the next block at `mempool_height` with the median time
    /// past `mempool_time`.
    fn block_at_5(mempool_height: Height, mempool_time: DateTime<Utc>) -> Self {
        let network = differential_network();
        let height = Height(5);
        let block = tx::BlockContext {
            height,
            time: differential_block_time(),
            network_upgrade: NetworkUpgrade::current(&network, height),
        };
        Self {
            network,
            mempool_height,
            mempool_time,
            block,
        }
    }

    /// The mempool verified the transaction for height 4, on an older tip, before the block's
    /// time.
    fn older_tip() -> Self {
        Self::block_at_5(
            Height(4),
            differential_block_time() - chrono::Duration::minutes(10),
        )
    }

    /// The mempool verified the transaction for height 6, on a tip that it hadn't processed yet,
    /// after the block's time.
    fn newer_tip() -> Self {
        Self::block_at_5(
            Height(6),
            differential_block_time() + chrono::Duration::minutes(10),
        )
    }

    /// Verifies `variant` the way the mempool does, then decides whether the block verifier
    /// reuses that verification in [`Self::block`], with the block verifier's own check.
    async fn verify_for_reuse(&self, variant: &Variant) -> Result<VerifiedUnminedTx, NotReused> {
        let utxo = transparent::Utxo::new(
            variant.spent.clone(),
            Height(1),
            variant.spent_from_coinbase,
        );
        let next_median_time_past =
            zebra_chain::serialization::DateTime32::try_from(self.mempool_time)
                .expect("the test time fits");
        let state = service_fn(move |request| {
            ready(Ok::<_, BoxError>(match request {
                zs::Request::UnspentBestChainUtxo(_) => {
                    zs::Response::UnspentBestChainUtxo(Some(utxo.clone()))
                }
                zs::Request::BestChainNextMedianTimePast => {
                    zs::Response::BestChainNextMedianTimePast(next_median_time_past)
                }
                zs::Request::CheckBestChainTipNullifiersAndAnchors(_) => {
                    zs::Response::ValidBestChainTipNullifiersAndAnchors
                }
                _ => panic!("unexpected mempool verification state request: {request:?}"),
            }))
        });
        let mut verified = transaction::MempoolTxVerifier::new_for_tests(&self.network, state)
            .oneshot(transaction::MempoolRequest {
                transaction: variant.transaction.clone().into(),
                height: self.mempool_height,
            })
            .await
            .map_err(|_| NotReused::RejectedByMempool)?
            .transaction;
        // The mempool records the tip height before the height it verified the transaction for.
        verified.height = self.mempool_height.previous().ok();

        let rules = ReuseRules::new(&self.network, self.block.height, self.block.time);
        match check_candidate(
            &verified,
            &variant.transaction,
            variant.transaction.hash(),
            &rules,
        ) {
            Ok(true) => Ok(verified),
            Ok(false) => Err(NotReused::Ineligible),
            Err(_) => Err(NotReused::RejectedByRecheck),
        }
    }

    /// Verifies `variant` in [`Self::block`] with the block transaction verifier.
    async fn verify_in_block(
        &self,
        variant: &Variant,
    ) -> Result<tx::BlockResponse, TransactionError> {
        let utxo = transparent::Utxo::new(
            variant.spent.clone(),
            Height(1),
            variant.spent_from_coinbase,
        );
        let state = service_fn(move |request| {
            ready(Ok::<_, BoxError>(match request {
                zs::Request::AwaitUtxo(_) => zs::Response::Utxo(utxo.clone()),
                _ => panic!("unexpected block verification state request: {request:?}"),
            }))
        });
        transaction::BlockTxVerifier::new(&self.network, state)
            .oneshot(tx::BlockRequest {
                transaction_hash: variant.transaction.hash(),
                transaction: variant.transaction.clone(),
                known_utxos: Arc::new(std::collections::HashMap::new()),
                height: self.block.height,
                time: self.block.time,
            })
            .await
    }

    /// Checks that the block verifier reuses the mempool's verification of `variant` as
    /// `expected` says, and that block verification agrees with each reuse, and with each
    /// rejection by the block verifier's check. `tip` names the mempool's tip in the messages.
    async fn assert_agrees_with_block_verification(
        &self,
        tip: &str,
        variant: &Variant,
        expected: Result<(), NotReused>,
    ) {
        let name = format!("{}, {tip}", variant.name);
        let in_block = self.verify_in_block(variant).await;
        let reused = self.verify_for_reuse(variant).await;

        // The property reuse relies on, whatever the variant.
        if let Ok(cached) = &reused {
            let in_block = in_block.as_ref().unwrap_or_else(|error| {
                panic!("{name}: block verification rejects a reused transaction: {error:?}")
            });
            assert_eq!(
                in_block.miner_fee,
                Some(cached.miner_fee),
                "{name}: miner fee"
            );
            assert_eq!(
                in_block.sigops,
                cached.block_sigop_count(),
                "{name}: sigops"
            );
            if variant.name == "signature operation in a redeem script" {
                assert_eq!(
                    in_block.sigops, 1,
                    "{name}: the redeem script's operation is counted"
                );
            }
        }
        assert_eq!(
            reused
                .as_ref()
                .map(|_| ())
                .map_err(|not_reused| *not_reused),
            expected,
            "{name}"
        );
        if expected == Err(NotReused::RejectedByRecheck) {
            assert!(
                in_block.is_err(),
                "{name}: the check rejects what block verification accepts"
            );
        }
    }
}

/// The variants of [`reuse_agrees_with_block_verification`] that break no rule, so the block
/// verifier reuses the mempool's verification of them from either tip.
fn reused_variants() -> Vec<Variant> {
    let spend = Spend::default();
    vec![
        Variant::new("valid", spend.v5(), REUSED, REUSED),
        Variant {
            spent: p2sh(checksig_hash()),
            ..Variant::new(
                "signature operation in a redeem script",
                Spend {
                    unlock_script: PUSH_CHECKSIG,
                    ..spend
                }
                .v5(),
                REUSED,
                REUSED,
            )
        },
        Variant::new(
            "signature operations in an output",
            Spend {
                output_script: &[0x51, 0xac, 0xac],
                ..spend
            }
            .v5(),
            REUSED,
            REUSED,
        ),
        Variant::new(
            "expires after the block",
            Spend {
                expiry_height: Height(6),
                ..spend
            }
            .v5(),
            REUSED,
            REUSED,
        ),
        Variant::new(
            "locked until an earlier height",
            Spend {
                lock_time: LockTime::Height(Height(3)),
                ..spend
            }
            .v5(),
            REUSED,
            REUSED,
        ),
        Variant::new("v4", spend.v4(), REUSED, REUSED),
    ]
}

/// The variants of [`reuse_agrees_with_block_verification`] for a block at `time` that each break
/// one rule, in the block or in the mempool.
fn rejected_variants(time: DateTime<Utc>) -> Vec<Variant> {
    let spend = Spend::default();
    let locked_until = |lock_time| Spend { lock_time, ..spend }.v5();
    vec![
        Variant::new(
            "expired",
            Spend {
                expiry_height: Height(4),
                ..spend
            }
            .v5(),
            RECHECK_REJECTS,
            MEMPOOL_REJECTS,
        ),
        Variant::new(
            "locked until the block height",
            locked_until(LockTime::Height(Height(5))),
            MEMPOOL_REJECTS,
            RECHECK_REJECTS,
        ),
        Variant::new(
            "locked until the block time",
            locked_until(LockTime::Time(time)),
            MEMPOOL_REJECTS,
            RECHECK_REJECTS,
        ),
        Variant::new(
            "earlier network upgrade",
            Spend {
                upgrade: NetworkUpgrade::Nu5,
                ..spend
            }
            .v5(),
            MEMPOOL_REJECTS,
            MEMPOOL_REJECTS,
        ),
        Variant {
            spent: p2sh([0; 20]),
            ..Variant::new(
                "failing script",
                spend.v5(),
                MEMPOOL_REJECTS,
                MEMPOOL_REJECTS,
            )
        },
        Variant {
            spent_from_coinbase: true,
            ..Variant::new(
                "immature coinbase output",
                spend.v5(),
                MEMPOOL_REJECTS,
                MEMPOOL_REJECTS,
            )
        },
    ]
}

/// The block verifier reuses the mempool's verification of a transaction only if block
/// verification accepts that transaction, with the same miner fee and signature operation count.
///
/// Each variant is verified by the mempool for a block before this one, and for a block after it,
/// because the mempool keeps verifications from older tips, and can add ones from a tip it hasn't
/// processed yet. Each variant states whether the block verifier reuses it, and if not, whether
/// the mempool or the check at this block's height and time rejects it. A reused variant must be
/// accepted by block verification with the same fee and signature operations. A variant that the
/// check rejects must also be rejected by block verification. So this fails if a rule that block
/// verification checks is skipped for reused transactions, or if a rule that the check checks
/// stops being checked.
///
/// The variants that the mempool rejects only show that they aren't reused: the mempool also
/// checks policy rules, and rules such as coinbase maturity, that the state checks for blocks.
#[tokio::test]
async fn reuse_agrees_with_block_verification() {
    let _init_guard = zebra_test::init();
    let (older, newer) = (Reuse::older_tip(), Reuse::newer_tip());
    let variants = reused_variants()
        .into_iter()
        .chain(rejected_variants(differential_block_time()));
    for variant in variants {
        older
            .assert_agrees_with_block_verification("older tip", &variant, variant.older)
            .await;
        newer
            .assert_agrees_with_block_verification("newer tip", &variant, variant.newer)
            .await;
    }
}

/// The block verifier doesn't reuse a transaction that the mempool verified for a height in a
/// later verification rules epoch than the block's, even if the block context rules accept the
/// transaction at the block's height. A version 4 transaction doesn't contain the consensus
/// branch ID that its signatures commit to, so only the epoch check keeps the mempool's
/// verification for the next network upgrade out of a block in the current one.
#[test]
fn transaction_verified_for_a_later_epoch_is_not_reused() {
    let _init_guard = zebra_test::init();
    let network = differential_network();
    let rules = ReuseRules::new(&network, Height(3), differential_block_time());
    let v4_spend = v4_spend();
    assert_eq!(rules.block.network_upgrade, NetworkUpgrade::Nu5);
    assert_eq!(
        NetworkUpgrade::current(&network, Height(4)),
        NetworkUpgrade::Nu6
    );
    assert!(tx::check_block_context_rules(&v4_spend, &rules.block, &network).is_ok());

    // The mempool verified the spend on the tip at height 3, for height 4, in the next network
    // upgrade.
    let mut later_epoch = entry(v4_spend.clone());
    later_epoch.height = Some(Height(3));
    let result = check_candidate(&later_epoch, &v4_spend, v4_spend.hash(), &rules);
    assert_eq!(result, Ok(false));
}

/// The same comparison for a shielded transaction with real proofs and signatures: the mempool
/// path accepts it, and block verification accepts it with the same fee. With one byte of its
/// authorizing data changed, it keeps its transaction ID, both paths reject it, and its mempool
/// lookup key is different.
#[tokio::test(flavor = "multi_thread")]
async fn mempool_path_and_block_verification_agree_on_shielded_transactions() {
    let _init_guard = zebra_test::init();

    // The mempool verified the transaction for height 2, and the block is at height 3. Both are in
    // NU5, like the Mainnet blocks the transaction comes from. Its proofs and signatures don't
    // depend on the network.
    let network = differential_network();
    let time = differential_block_time();
    let height = Height(3);
    let reuse = Reuse {
        block: tx::BlockContext {
            height,
            time,
            network_upgrade: NetworkUpgrade::current(&network, height),
        },
        network,
        mempool_height: Height(2),
        mempool_time: time,
    };

    // The first Mainnet test transaction that only has Orchard actions.
    let transaction = zebra_test::vectors::MAINNET_BLOCKS
        .values()
        .map(|bytes| {
            Block::zcash_deserialize(&bytes[..]).expect("the test vector is a valid block")
        })
        .flat_map(|block| block.transactions.clone())
        .find(|transaction| {
            transaction.version() == 5
                && transaction.inputs().is_empty()
                && transaction.outputs().is_empty()
                && transaction.orchard_actions().next().is_some()
                && !transaction.has_sapling_shielded_data()
                && transaction.lock_time().is_none()
        })
        .expect("the Mainnet test vectors have an Orchard-only transaction");

    // The Orchard binding signature is the last field of a V5 transaction.
    let mut bytes =
        zebra_chain::serialization::ZcashSerialize::zcash_serialize_to_vec(transaction.as_ref())
            .unwrap();
    *bytes.last_mut().expect("a transaction has bytes") ^= 1;
    let changed = Arc::new(Transaction::zcash_deserialize(&bytes[..]).unwrap());
    assert_eq!(changed.hash(), transaction.hash());
    assert_ne!(changed.unmined_id(), transaction.unmined_id());

    // The transactions have no transparent inputs, so they don't use the variants' spent outputs.
    let valid = Variant::new("shielded", transaction, REUSED, REUSED);
    let changed = Variant::new(
        "shielded with a changed binding signature",
        changed,
        MEMPOOL_REJECTS,
        MEMPOOL_REJECTS,
    );
    // The mempool verified the transactions on an older tip than the block's parent.
    for variant in [&valid, &changed] {
        reuse
            .assert_agrees_with_block_verification("older tip", variant, variant.older)
            .await;
    }
    assert!(reuse.verify_in_block(&changed).await.is_err());
}

/// An output of a transaction in the mempool, and its outpoint.
fn mempool_spend() -> (transparent::OutPoint, transparent::Output) {
    let outpoint = transparent::OutPoint {
        hash: [9; 32].into(),
        index: 0,
    };
    (outpoint, p2sh(checksig_hash()))
}

/// An output in the chain, and its outpoint.
fn chain_spend() -> (transparent::OutPoint, transparent::Output) {
    let outpoint = transparent::OutPoint {
        hash: [8; 32].into(),
        index: 0,
    };
    (outpoint, p2sh(op_1_hash()))
}

/// A transaction whose first input spends [`mempool_spend`] with the unlock script `first`, and
/// whose second input spends [`chain_spend`] with the unlock script `second`.
fn mixed_spend(first: &[u8], second: &[u8]) -> Arc<Transaction> {
    let input = |outpoint, unlock_script: &[u8]| transparent::Input::PrevOut {
        outpoint,
        unlock_script: transparent::Script::new(unlock_script),
        sequence: 0,
    };
    Arc::new(Transaction::test_v5(
        NetworkUpgrade::Nu6,
        vec![
            input(mempool_spend().0, first),
            input(chain_spend().0, second),
        ],
        vec![transparent::Output::new(
            1.try_into().unwrap(),
            transparent::Script::new(&[0x51]),
        )],
        LockTime::unlocked(),
        Height(0),
    ))
}

/// Verifies `transaction` the way the mempool does when the next block is at `height`, with the
/// output of [`mempool_spend`] in the mempool and the output of [`chain_spend`] in the chain.
async fn verify_mixed_in_mempool(
    network: &Network,
    height: Height,
    transaction: Arc<Transaction>,
) -> Result<transaction::MempoolResponse, TransactionError> {
    let (mempool_outpoint, mempool_output) = mempool_spend();
    let (chain_outpoint, chain_output) = chain_spend();
    let chain_utxo = transparent::Utxo::new(chain_output, Height(1), false);
    let state = service_fn(move |request| {
        ready(Ok::<_, BoxError>(match request {
            zs::Request::UnspentBestChainUtxo(outpoint) => zs::Response::UnspentBestChainUtxo(
                (outpoint == chain_outpoint).then(|| chain_utxo.clone()),
            ),
            zs::Request::CheckBestChainTipNullifiersAndAnchors(_) => {
                zs::Response::ValidBestChainTipNullifiersAndAnchors
            }
            _ => panic!("unexpected mempool verification state request: {request:?}"),
        }))
    });
    let mempool: Mempool = BoxCloneService::new(service_fn(move |request| {
        ready(Ok(match request {
            mempool::Request::AwaitOutput(outpoint) if outpoint == mempool_outpoint => {
                mempool::Response::UnspentOutput(mempool_output.clone())
            }
            // The verifier polls the mempool after it accepts a transaction.
            mempool::Request::CheckForVerifiedTransactions => {
                mempool::Response::CheckedForVerifiedTransactions
            }
            _ => panic!("unexpected mempool request: {request:?}"),
        }))
    }));
    let (sender, receiver) = oneshot::channel();
    sender.send(mempool).unwrap();
    transaction::MempoolTxVerifier::new(network, state, receiver)
        .oneshot(transaction::MempoolRequest {
            transaction: transaction.into(),
            height,
        })
        .await
}

/// Verifies `transaction` the way block verification does when it doesn't reuse the mempool, in
/// a block at `height` whose earlier transaction created the output of [`mempool_spend`], with
/// the output of [`chain_spend`] in the chain.
async fn verify_mixed_in_block(
    network: &Network,
    height: Height,
    time: chrono::DateTime<chrono::Utc>,
    transaction: Arc<Transaction>,
) -> Result<tx::BlockResponse, TransactionError> {
    let (mempool_outpoint, mempool_output) = mempool_spend();
    let (chain_outpoint, chain_output) = chain_spend();
    let chain_utxo = transparent::Utxo::new(chain_output, Height(1), false);
    let state = service_fn(move |request| {
        ready(Ok::<_, BoxError>(match request {
            zs::Request::AwaitUtxo(outpoint) if outpoint == chain_outpoint => {
                zs::Response::Utxo(chain_utxo.clone())
            }
            _ => panic!("unexpected block verification state request: {request:?}"),
        }))
    });
    let known_utxos = std::collections::HashMap::from([(
        mempool_outpoint,
        transparent::OrderedUtxo::new(mempool_output, height, 1),
    )]);
    transaction::BlockTxVerifier::new(network, state)
        .oneshot(tx::BlockRequest {
            transaction_hash: transaction.hash(),
            transaction,
            known_utxos: Arc::new(known_utxos),
            height,
            time,
        })
        .await
}

/// When a transaction spends an output of another mempool transaction and an output in the chain,
/// the mempool pairs each input with its own spent output, so it accepts and rejects the same
/// transactions as block verification, and records the spent outputs, fee and signature
/// operations that block verification computes.
///
/// The spent outputs have different redeem scripts. With the inputs' unlock scripts exchanged,
/// the transaction is valid only if its spent outputs are swapped, which is what an earlier
/// version of the mempool did when it added the outputs from the mempool after the outputs from
/// the chain. Both paths must reject it.
#[tokio::test]
async fn mempool_path_pairs_mempool_and_chain_spends_with_their_inputs() {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(4),
            ..Default::default()
        }
        .into(),
    );
    let height = Height(5);
    let time = block::genesis::regtest_genesis_block().header.time + chrono::Duration::hours(1);
    let (mempool_outpoint, mempool_output) = mempool_spend();
    let (_, chain_output) = chain_spend();

    let swapped = mixed_spend(PUSH_OP_1, PUSH_CHECKSIG);
    assert!(
        verify_mixed_in_block(&network, height, time, swapped.clone())
            .await
            .is_err()
    );
    assert!(
        verify_mixed_in_mempool(&network, height, swapped)
            .await
            .is_err(),
        "the mempool accepted a transaction that block verification rejects"
    );

    let valid = mixed_spend(PUSH_CHECKSIG, PUSH_OP_1);
    let response = verify_mixed_in_mempool(&network, height, valid.clone())
        .await
        .expect("the mempool accepts the transaction");
    assert_eq!(response.spent_mempool_outpoints, vec![mempool_outpoint]);
    let cached = response.transaction;
    assert_eq!(
        *cached.spent_outputs,
        vec![mempool_output, chain_output],
        "each input is paired with its own spent output"
    );
    // Only the first input's redeem script has a signature operation.
    assert_eq!(cached.p2sh_sigop_count, 1);

    let block = verify_mixed_in_block(&network, height, time, valid)
        .await
        .expect("block verification accepts the transaction");
    assert_eq!(block.miner_fee, Some(cached.miner_fee));
    assert_eq!(block.sigops, cached.block_sigop_count());
}
