//! How the block verifier runs the transaction verifier: one state read for a block's spent
//! outputs, and a task for each transaction.

use std::{
    future::{pending, ready},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};

use tower::{service_fn, util::BoxCloneService, ServiceExt};

use zebra_chain::{amount::NonNegative, parameters::testnet::ConfiguredActivationHeights};

use super::*;

type State = BoxCloneService<zs::Request, zs::Response, BoxError>;

/// The fee of the transaction that [`spend`] returns.
const FEE: u64 = 10_000;

/// What the state does with a block's [`zs::Request::AnyChainUtxos`] request.
#[derive(Clone, Copy, Debug)]
enum BatchedRead {
    /// Returns every requested output.
    Found,
    /// Returns no outputs, as it does for outputs of blocks that are still queued.
    Unknown,
    /// Fails.
    Error,
}

/// The state requests that a block verification made.
#[derive(Default)]
struct Calls {
    /// The outpoints of each [`zs::Request::AnyChainUtxos`] request.
    batched_reads: Mutex<Vec<Vec<transparent::OutPoint>>>,
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

/// The outpoint of an output that is in the state, not in the block.
fn state_outpoint() -> transparent::OutPoint {
    transparent::OutPoint {
        hash: [8; 32].into(),
        index: 0,
    }
}

/// The output that every outpoint in the state has. Its script is `OP_1`, which
/// [`spend`]'s input script unlocks.
fn state_utxo() -> transparent::Utxo {
    transparent::Utxo::new(
        transparent::Output::new(
            (FEE + 1).try_into().unwrap(),
            transparent::Script::new(&[0x51]),
        ),
        Height(1),
        false,
    )
}

/// A transaction that spends `outpoint` to an output of `value` with the script `OP_1`.
fn spend(outpoint: transparent::OutPoint, value: u64) -> Arc<Transaction> {
    Arc::new(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![transparent::Input::PrevOut {
            outpoint,
            unlock_script: transparent::Script::new(&[0x51]),
            sequence: 0,
        }],
        vec![transparent::Output::new(
            value.try_into().unwrap(),
            transparent::Script::new(&[0x51]),
        )],
        LockTime::unlocked(),
        Height(0),
    ))
}

/// A block at height 3 on the Regtest genesis block, with `transactions` after its coinbase
/// transaction, which claims `fees`.
fn block_with(network: &Network, transactions: Vec<Arc<Transaction>>, fees: u64) -> Arc<Block> {
    let height = Height(3);
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
    let genesis = zebra_chain::block::genesis::regtest_genesis_block();
    let mut header = *genesis.header;
    header.previous_block_hash = genesis.hash();
    let transactions: Vec<_> = std::iter::once(coinbase).chain(transactions).collect();
    header.merkle_root = transactions.iter().map(|tx| tx.hash()).collect();

    Arc::new(Block {
        header: Arc::new(header),
        transactions,
    })
}

/// A state that has [`state_utxo`] at every outpoint, and accepts every block.
fn state(batched_read: BatchedRead, calls: Arc<Calls>) -> State {
    BoxCloneService::new(service_fn(move |request| {
        ready(match request {
            zs::Request::KnownBlock(_) => Ok(zs::Response::KnownBlock(None)),
            zs::Request::MiningCandidate(_) => Ok(zs::Response::MiningCandidate(None)),
            zs::Request::AnyChainUtxos(outpoints) => {
                calls.batched_reads.lock().unwrap().push(outpoints.clone());
                match batched_read {
                    BatchedRead::Found => Ok(zs::Response::AnyChainUtxos(
                        outpoints
                            .into_iter()
                            .map(|outpoint| (outpoint, state_utxo()))
                            .collect(),
                    )),
                    BatchedRead::Unknown => Ok(zs::Response::AnyChainUtxos(Default::default())),
                    BatchedRead::Error => Err("the batched read failed".into()),
                }
            }
            zs::Request::AwaitUtxo(_) => {
                calls.await_utxo.fetch_add(1, Ordering::SeqCst);
                Ok(zs::Response::Utxo(state_utxo()))
            }
            zs::Request::CommitSemanticallyVerifiedBlock(block) => {
                calls.commits.fetch_add(1, Ordering::SeqCst);
                Ok(zs::Response::Committed(block.hash))
            }
            _ => panic!("unexpected state request: {request:?}"),
        })
    }))
}

/// Verifies `block` with the real block transaction verifier, wired as `router::init` does.
async fn verify(
    network: &Network,
    block: Arc<Block>,
    batched_read: BatchedRead,
) -> (Result<block::Hash, VerifyBlockError>, Arc<Calls>) {
    let calls = Arc::new(Calls::default());
    let state = state(batched_read, calls.clone());
    let transactions =
        transaction::BlockTxVerifier::new(network, state.clone()).map_err(BoxError::from);
    let verifier = SemanticBlockVerifier::new(network, state, transactions);

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        verifier.oneshot(Request::Commit(block)),
    )
    .await
    .expect("block verification finishes");

    (result, calls)
}

/// The block verifier reads the outputs that a block spends in one state request, and the
/// transaction verifier uses them instead of asking the state for each input.
#[tokio::test(flavor = "multi_thread")]
async fn spent_outputs_are_read_from_the_state_once() {
    let _init_guard = zebra_test::init();
    let network = network();

    let second_outpoint = transparent::OutPoint {
        hash: [9; 32].into(),
        index: 3,
    };
    let block = block_with(
        &network,
        vec![spend(state_outpoint(), 1), spend(second_outpoint, 1)],
        2 * FEE,
    );

    let (result, calls) = verify(&network, block.clone(), BatchedRead::Found).await;

    assert_eq!(result.expect("the block is valid"), block.hash());
    assert_eq!(
        *calls.batched_reads.lock().unwrap(),
        vec![vec![state_outpoint(), second_outpoint]],
        "the block's spent outpoints are requested once, in block order"
    );
    assert_eq!(calls.await_utxo.load(Ordering::SeqCst), 0);
    assert_eq!(calls.commits.load(Ordering::SeqCst), 1);
}

/// The transaction verifier still waits in the state for each output that the batched read
/// doesn't return, and for every output when the batched read fails.
#[tokio::test(flavor = "multi_thread")]
async fn outputs_missing_from_the_batched_read_are_awaited() {
    let _init_guard = zebra_test::init();
    let network = network();

    for batched_read in [BatchedRead::Unknown, BatchedRead::Error] {
        let block = block_with(&network, vec![spend(state_outpoint(), 1)], FEE);

        let (result, calls) = verify(&network, block.clone(), batched_read).await;

        assert_eq!(
            result.expect("the block is valid"),
            block.hash(),
            "{batched_read:?}"
        );
        assert_eq!(calls.batched_reads.lock().unwrap().len(), 1);
        assert_eq!(
            calls.await_utxo.load(Ordering::SeqCst),
            1,
            "{batched_read:?}"
        );
        assert_eq!(calls.commits.load(Ordering::SeqCst), 1);
    }
}

/// Outputs that the block creates are not requested from the state, and a block that spends
/// none of the state's outputs makes no batched read.
#[tokio::test(flavor = "multi_thread")]
async fn outputs_created_in_the_block_are_not_requested() {
    let _init_guard = zebra_test::init();
    let network = network();

    // The second transaction spends the first one's only output, and pays no fee.
    let first = spend(state_outpoint(), 1);
    let second = spend(
        transparent::OutPoint {
            hash: first.hash(),
            index: 0,
        },
        1,
    );
    let block = block_with(&network, vec![first, second], FEE);

    let (result, calls) = verify(&network, block.clone(), BatchedRead::Found).await;

    assert_eq!(result.expect("the block is valid"), block.hash());
    assert_eq!(
        *calls.batched_reads.lock().unwrap(),
        vec![vec![state_outpoint()]]
    );
    assert_eq!(calls.await_utxo.load(Ordering::SeqCst), 0);

    // A block with only a coinbase transaction spends no outputs.
    let block = block_with(&network, Vec::new(), 0);

    let (result, calls) = verify(&network, block.clone(), BatchedRead::Found).await;

    assert_eq!(result.expect("the block is valid"), block.hash());
    assert!(calls.batched_reads.lock().unwrap().is_empty());
    assert_eq!(calls.await_utxo.load(Ordering::SeqCst), 0);
}

/// A transaction that fails verification fails its block, and the block isn't committed.
#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_transaction_fails_the_block() {
    let _init_guard = zebra_test::init();
    let network = network();

    // The output is worth more than the spent output.
    let block = block_with(&network, vec![spend(state_outpoint(), FEE + 2)], FEE);

    let (result, calls) = verify(&network, block, BatchedRead::Found).await;

    assert!(
        matches!(result, Err(VerifyBlockError::Transaction(_))),
        "unexpected result: {result:?}"
    );
    assert_eq!(calls.commits.load(Ordering::SeqCst), 0);
}

/// Dropping a block's verification cancels the transaction verification tasks that it
/// started, so an abandoned block doesn't keep verifying its transactions.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_block_cancels_its_transaction_tasks() {
    let _init_guard = zebra_test::init();
    let network = network();

    let block = block_with(
        &network,
        vec![
            spend(state_outpoint(), 1),
            spend(
                transparent::OutPoint {
                    hash: [9; 32].into(),
                    index: 0,
                },
                1,
            ),
        ],
        2 * FEE,
    );

    // Each transaction verification holds a clone of this until it is dropped.
    let running = Arc::new(());
    let started = Arc::new(AtomicUsize::new(0));
    let transactions = service_fn({
        let running = running.clone();
        let started = started.clone();
        move |_request: tx::BlockRequest| {
            let running = running.clone();
            let started = started.clone();
            async move {
                let _running = running;
                started.fetch_add(1, Ordering::SeqCst);
                pending::<Result<tx::BlockResponse, BoxError>>().await
            }
        }
    });
    let calls = Arc::new(Calls::default());
    let verifier =
        SemanticBlockVerifier::new(&network, state(BatchedRead::Found, calls), transactions);

    let verification = tokio::spawn(verifier.oneshot(Request::Commit(block.clone())));

    tokio::time::timeout(Duration::from_secs(30), async {
        // The coinbase transaction and the two spends.
        while started.load(Ordering::SeqCst) < block.transactions.len() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // This test and the transaction verifier hold the other two.
        assert_eq!(Arc::strong_count(&running), 2 + block.transactions.len());

        verification.abort();

        while Arc::strong_count(&running) > 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the transaction tasks start, and stop when the block's verification is dropped");
}
