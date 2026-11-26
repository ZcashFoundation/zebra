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

fn mock_state() -> State {
    BoxCloneService::new(service_fn(|request| {
        ready(Ok(match request {
            zs::Request::KnownBlock(_) => zs::Response::KnownBlock(None),
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

/// A spend for the differential test: the transaction, the output it spends, and whether that
/// output is a coinbase output.
struct Variant {
    name: &'static str,
    transaction: Arc<Transaction>,
    spent: transparent::Output,
    spent_from_coinbase: bool,
}

/// Verifies `variant` the way the mempool does when the next block is at `admission_height`,
/// followed by the checks that block verification still does for a transaction it reuses in a
/// block at `height`. Returns the reusable mempool entry if both pass.
async fn verify_for_reuse(
    network: &Network,
    admission_height: Height,
    height: Height,
    time: chrono::DateTime<chrono::Utc>,
    variant: &Variant,
) -> Option<VerifiedUnminedTx> {
    let utxo = transparent::Utxo::new(
        variant.spent.clone(),
        Height(1),
        variant.spent_from_coinbase,
    );
    let state = service_fn(move |request| {
        ready(Ok::<_, BoxError>(match request {
            zs::Request::UnspentBestChainUtxo(_) => {
                zs::Response::UnspentBestChainUtxo(Some(utxo.clone()))
            }
            zs::Request::BestChainNextMedianTimePast => zs::Response::BestChainNextMedianTimePast(
                zebra_chain::serialization::DateTime32::try_from(time).expect("the test time fits"),
            ),
            zs::Request::CheckBestChainTipNullifiersAndAnchors(_) => {
                zs::Response::ValidBestChainTipNullifiersAndAnchors
            }
            _ => panic!("unexpected mempool verification state request: {request:?}"),
        }))
    });
    let verifier = transaction::MempoolTxVerifier::new_for_tests(network, state);
    let verified = verifier
        .oneshot(transaction::MempoolRequest {
            transaction: variant.transaction.clone().into(),
            height: admission_height,
        })
        .await
        .ok()?
        .transaction;

    tx::check_block_context_rules(&variant.transaction, height, time, network).ok()?;

    Some(verified)
}

/// Verifies `variant` the way block verification does when it doesn't reuse the mempool.
async fn verify_in_block(
    network: &Network,
    height: Height,
    time: chrono::DateTime<chrono::Utc>,
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
    transaction::BlockTxVerifier::new(network, state)
        .oneshot(tx::BlockRequest {
            transaction_hash: variant.transaction.hash(),
            transaction: variant.transaction.clone(),
            known_utxos: Arc::new(std::collections::HashMap::new()),
            height,
            time,
        })
        .await
}

/// Block verification reuses what the mempool verified, so the mempool path followed by the
/// reuse checks must never accept a transaction that block verification rejects, and the fee and
/// signature operations it records must be the ones block verification computes.
///
/// Each variant breaks one rule. A rule that only one of the two paths checks makes this fail.
#[tokio::test]
async fn mempool_path_never_accepts_what_block_verification_rejects() {
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
    // The mempool verified the transactions for the block before this one, in the same network
    // upgrade, which block verification requires for reuse.
    let admission_height = Height(4);
    let height = Height(5);
    let time = block::genesis::regtest_genesis_block().header.time + chrono::Duration::hours(1);
    // A P2SH output whose redeem script is `OP_1` is standard, and an input that pushes that
    // redeem script spends it.
    let spendable = p2sh(op_1_hash());

    let v5 = |upgrade, script: &[u8], output_script: &[u8], lock_time, expiry_height| {
        Arc::new(Transaction::test_v5(
            upgrade,
            vec![transparent::Input::PrevOut {
                outpoint: transparent::OutPoint {
                    hash: [8; 32].into(),
                    index: 0,
                },
                unlock_script: transparent::Script::new(script),
                sequence: 0,
            }],
            vec![transparent::Output::new(
                1.try_into().unwrap(),
                transparent::Script::new(output_script),
            )],
            lock_time,
            expiry_height,
        ))
    };
    let valid = |transaction| Variant {
        name: "",
        transaction,
        spent: spendable.clone(),
        spent_from_coinbase: false,
    };
    let named = |name, variant: Variant| Variant { name, ..variant };

    let nu6 = NetworkUpgrade::Nu6;
    let unlocked = LockTime::unlocked();
    let variants = [
        named(
            "valid",
            valid(v5(nu6, PUSH_OP_1, &[0x51], unlocked, Height(0))),
        ),
        named(
            "signature operation in a redeem script",
            Variant {
                spent: p2sh(checksig_hash()),
                ..valid(v5(nu6, PUSH_CHECKSIG, &[0x51], unlocked, Height(0)))
            },
        ),
        named(
            "signature operations in an output",
            valid(v5(nu6, PUSH_OP_1, &[0x51, 0xac, 0xac], unlocked, Height(0))),
        ),
        named(
            "expires after the block",
            valid(v5(nu6, PUSH_OP_1, &[0x51], unlocked, Height(6))),
        ),
        named(
            "expired",
            valid(v5(nu6, PUSH_OP_1, &[0x51], unlocked, Height(4))),
        ),
        named(
            "locked until an earlier height",
            valid(v5(
                nu6,
                PUSH_OP_1,
                &[0x51],
                LockTime::Height(Height(3)),
                Height(0),
            )),
        ),
        named(
            "locked until the block height",
            valid(v5(
                nu6,
                PUSH_OP_1,
                &[0x51],
                LockTime::Height(height),
                Height(0),
            )),
        ),
        named(
            "locked until the block time",
            valid(v5(nu6, PUSH_OP_1, &[0x51], LockTime::Time(time), Height(0))),
        ),
        named(
            "earlier network upgrade",
            valid(v5(
                NetworkUpgrade::Nu5,
                PUSH_OP_1,
                &[0x51],
                unlocked,
                Height(0),
            )),
        ),
        named(
            "failing script",
            Variant {
                spent: p2sh([0; 20]),
                ..valid(v5(nu6, PUSH_OP_1, &[0x51], unlocked, Height(0)))
            },
        ),
        named(
            "immature coinbase output",
            Variant {
                spent_from_coinbase: true,
                ..valid(v5(nu6, PUSH_OP_1, &[0x51], unlocked, Height(0)))
            },
        ),
        named(
            "v4",
            valid(Arc::new(Transaction::test_v4(
                vec![transparent::Input::PrevOut {
                    outpoint: transparent::OutPoint {
                        hash: [8; 32].into(),
                        index: 0,
                    },
                    unlock_script: transparent::Script::new(PUSH_OP_1),
                    sequence: 0,
                }],
                vec![transparent::Output::new(
                    1.try_into().unwrap(),
                    transparent::Script::new(&[0x51]),
                )],
                unlocked,
                Height(0),
            ))),
        ),
    ];

    let mut reused = 0;
    for variant in &variants {
        let name = variant.name;
        let block = verify_in_block(&network, height, time, variant).await;
        let Some(cached) =
            verify_for_reuse(&network, admission_height, height, time, variant).await
        else {
            continue;
        };
        reused += 1;

        let block = block.unwrap_or_else(|error| {
            panic!("{name}: block verification rejects a reusable transaction: {error:?}")
        });
        assert_eq!(block.miner_fee, Some(cached.miner_fee), "{name}: miner fee");
        assert_eq!(block.sigops, cached.block_sigop_count(), "{name}: sigops");
        if name == "signature operation in a redeem script" {
            assert_eq!(
                block.sigops, 1,
                "{name}: the redeem script's operation is counted"
            );
        }
    }

    // The valid variants must exercise the comparison above.
    assert_eq!(reused, 6, "the valid variants are reusable");
}

/// The same comparison for a shielded transaction with real proofs and signatures: the mempool
/// path accepts it, and block verification accepts it with the same fee. With one byte of its
/// authorizing data changed, it keeps its transaction ID, both paths reject it, and its mempool
/// lookup key is different.
#[tokio::test(flavor = "multi_thread")]
async fn mempool_path_and_block_verification_agree_on_shielded_transactions() {
    let _init_guard = zebra_test::init();

    // A network where the heights below are in NU5, like the Mainnet blocks the transaction
    // comes from. Its proofs and signatures don't depend on the network.
    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(4),
            ..Default::default()
        }
        .into(),
    );
    let admission_height = Height(2);
    let height = Height(3);
    let time = block::genesis::regtest_genesis_block().header.time + chrono::Duration::hours(1);

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

    let variant = |name, transaction| Variant {
        name,
        transaction,
        // Unused: the transaction has no transparent inputs.
        spent: transparent::Output::new(1.try_into().unwrap(), transparent::Script::new(&[0x51])),
        spent_from_coinbase: false,
    };

    let valid = variant("shielded", transaction);
    let block = verify_in_block(&network, height, time, &valid)
        .await
        .expect("block verification accepts the transaction");
    let cached = verify_for_reuse(&network, admission_height, height, time, &valid)
        .await
        .expect("the mempool path accepts the transaction");
    assert_eq!(block.miner_fee, Some(cached.miner_fee));
    assert_eq!(block.sigops, cached.block_sigop_count());

    let changed = variant("shielded with a changed binding signature", changed);
    assert!(verify_in_block(&network, height, time, &changed)
        .await
        .is_err());
    assert!(
        verify_for_reuse(&network, admission_height, height, time, &changed)
            .await
            .is_none()
    );
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
