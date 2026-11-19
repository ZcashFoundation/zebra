//! Small blocks decode and run body checks inline without waiting for blocking admission or Rayon.

use std::{future::ready, sync::atomic::Ordering, time::Duration};

use tower::{service_fn, util::BoxCloneService};
use zebra_chain::{amount::NonNegative, parameters::testnet::ConfiguredActivationHeights};

use super::*;

type State = BoxCloneService<zs::Request, zs::Response, BoxError>;

const FEE: u64 = 10_000;

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
    spend_from(0, lock_time, expiry_height)
}

/// Returns a transaction that spends output `index` of the transaction every [`spend`] spends.
fn spend_from(index: u32, lock_time: LockTime, expiry_height: Height) -> Arc<Transaction> {
    Arc::new(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: [8; 32].into(),
                index,
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

fn mock_state() -> State {
    BoxCloneService::new(service_fn(|request| {
        ready(Ok(match request {
            zs::Request::KnownBlock(_) => zs::Response::KnownBlock(None),
            zs::Request::EarlyRelay { .. } => zs::Response::EarlyRelay(None),
            zs::Request::RejectBlock(_) => zs::Response::Rejected(false),
            zs::Request::MiningCandidate(_) => zs::Response::MiningCandidate(None),
            zs::Request::AwaitUtxos(outpoints) => zs::Response::Utxos(
                outpoints
                    .into_iter()
                    .map(|_| {
                        transparent::Utxo::new(
                            transparent::Output::new(
                                (FEE + 1).try_into().unwrap(),
                                transparent::Script::new(&[0x51]),
                            ),
                            Height(1),
                            false,
                        )
                    })
                    .collect(),
            ),
            zs::Request::CommitSemanticallyVerifiedBlock(block) => {
                zs::Response::Committed(block.hash)
            }
            _ => panic!("unexpected state request: {request:?}"),
        }))
    }))
}

/// Verifies `block` while every blocking check slot is taken, and returns the verifier's result,
/// or `None` if it was still waiting after a second.
async fn verify_without_blocking_admission(
    network: &Network,
    block: &Arc<Block>,
) -> Option<Result<block::Hash, VerifyBlockError>> {
    let bytes = block.zcash_serialize_to_vec().unwrap();
    let decoded = super::super::DecodedBlock::deserialize(&bytes).unwrap();
    let state = mock_state();
    let transactions = Buffer::new(transaction::BlockTxVerifier::new(network, state.clone()), 1);
    let verifier = SemanticBlockVerifier::new(network, state, transactions);
    let slots = u32::try_from(verifier.blocking_checks.available_permits())
        .expect("the test verifier's blocking admission count fits u32");
    let _slots = verifier
        .blocking_checks
        .clone()
        .acquire_many_owned(slots)
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        verifier.oneshot(Request::CommitDecoded(decoded, None)),
    )
    .await
    .ok()
}

#[tokio::test]
async fn only_small_decoded_blocks_bypass_blocking_check_admission() {
    let _init_guard = zebra_test::init();
    let network = network();
    for size in [
        super::super::MAX_INLINE_HASHING_BYTES,
        super::super::MAX_INLINE_HASHING_BYTES + 1,
    ] {
        let mut block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
        let block_mut = Arc::make_mut(&mut block);
        let mut outputs = block_mut.transactions[0].outputs();
        // Keep the script's CompactSize prefix three bytes long when adjusting its payload.
        outputs[0].lock_script = transparent::Script::new(&vec![0; 1024]);
        block_mut.transactions[0] = Arc::new(
            block_mut.transactions[0]
                .as_ref()
                .clone()
                .with_transparent_outputs(outputs),
        );
        let script_size = size - block_mut.zcash_serialized_size() + 1024;
        let mut outputs = block_mut.transactions[0].outputs();
        outputs[0].lock_script = transparent::Script::new(&vec![0; script_size]);
        block_mut.transactions[0] = Arc::new(
            block_mut.transactions[0]
                .as_ref()
                .clone()
                .with_transparent_outputs(outputs),
        );
        Arc::make_mut(&mut block_mut.header).merkle_root =
            block_mut.transactions.iter().map(|tx| tx.hash()).collect();
        assert_eq!(block.zcash_serialized_size(), size);
        let result = verify_without_blocking_admission(&network, &block).await;
        if size == super::super::MAX_INLINE_HASHING_BYTES {
            assert_eq!(result.unwrap().unwrap(), block.hash());
        } else {
            assert!(
                result.is_none(),
                "large-body checks must wait for blocking admission"
            );
        }
    }
}

#[tokio::test]
async fn only_blocks_with_few_transactions_bypass_blocking_check_admission() {
    let _init_guard = zebra_test::init();
    let network = network();
    for transactions in [
        super::super::MAX_INLINE_HASHING_TRANSACTIONS,
        super::super::MAX_INLINE_HASHING_TRANSACTIONS + 1,
    ] {
        let mut block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
        let block_mut = Arc::make_mut(&mut block);
        // The candidate has a coinbase transaction and the spend of output 0.
        for index in 1..transactions - 1 {
            block_mut.transactions.push(spend_from(
                u32::try_from(index).expect("the test's transaction count fits u32"),
                LockTime::unlocked(),
                Height(0),
            ));
        }
        Arc::make_mut(&mut block_mut.header).merkle_root =
            block_mut.transactions.iter().map(|tx| tx.hash()).collect();
        assert_eq!(block.transactions.len(), transactions);
        assert!(block.zcash_serialized_size() <= super::super::MAX_INLINE_HASHING_BYTES);
        let result = verify_without_blocking_admission(&network, &block).await;
        if transactions == super::super::MAX_INLINE_HASHING_TRANSACTIONS {
            assert_eq!(result.unwrap().unwrap(), block.hash());
        } else {
            assert!(
                result.is_none(),
                "checks of a block with many transactions must wait for blocking admission"
            );
        }
    }
}

#[tokio::test]
async fn only_blocks_that_pass_quick_checks_can_be_prepared_for_commit() {
    let _init_guard = zebra_test::init();
    let network = network();
    let can_be_prepared = |block: Block| {
        let network = network.clone();
        async move {
            let bytes = block.zcash_serialize_to_vec().unwrap();
            super::super::DecodedBlock::deserialize(&bytes)
                .unwrap()
                .can_be_prepared_for_commit(&network, None)
                .await
        }
    };
    let merkle_root = |block: &Block| block.transactions.iter().map(|tx| tx.hash()).collect();

    let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
    assert!(can_be_prepared(Block::clone(&block)).await);

    // A block that wasn't decoded from bytes has no size that is known to be within the limit.
    assert!(
        !super::super::DecodedBlock::from(block.clone())
            .can_be_prepared_for_commit(&network, None)
            .await
    );

    // The checks of a block past the inline bounds run on a blocking thread.
    let mut many_transactions = Block::clone(&block);
    for index in 1..=super::super::MAX_INLINE_HASHING_TRANSACTIONS {
        many_transactions.transactions.push(spend_from(
            u32::try_from(index).expect("the test's transaction count fits u32"),
            LockTime::unlocked(),
            Height(0),
        ));
    }
    Arc::make_mut(&mut many_transactions.header).merkle_root = merkle_root(&many_transactions);
    assert!(can_be_prepared(many_transactions).await);

    let mut wrong_merkle_root = Block::clone(&block);
    Arc::make_mut(&mut wrong_merkle_root.header).merkle_root = wrong_merkle_root.transactions[..1]
        .iter()
        .map(|tx| tx.hash())
        .collect();
    assert!(!can_be_prepared(wrong_merkle_root).await);

    // A different transaction that spends the same output.
    let mut double_spend = Block::clone(&block);
    double_spend
        .transactions
        .push(spend(LockTime::unlocked(), Height(4)));
    Arc::make_mut(&mut double_spend.header).merkle_root = merkle_root(&double_spend);
    assert!(!can_be_prepared(double_spend).await);

    let mut coinbase_last = Block::clone(&block);
    coinbase_last.transactions.reverse();
    Arc::make_mut(&mut coinbase_last.header).merkle_root = merkle_root(&coinbase_last);
    assert!(!can_be_prepared(coinbase_last).await);
}

#[test]
fn inline_decoding_and_body_checks_do_not_wait_for_rayon() {
    const CHILD: &str = "ZEBRA_SMALL_BLOCK_RAYON_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Isolate saturation from the other tests using the process-global Rayon pool.
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "block::tests::inline_checks::inline_decoding_and_body_checks_do_not_wait_for_rayon",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("RAYON_NUM_THREADS", "2")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let _init_guard = zebra_test::init();
    let (started, started_rx) = std::sync::mpsc::channel();
    let mut releases = Vec::new();
    for _ in 0..rayon::current_num_threads() {
        let (release, held) = std::sync::mpsc::channel();
        releases.push(release);
        let started = started.clone();
        rayon::spawn(move || {
            started.send(()).unwrap();
            let _ = held.recv_timeout(Duration::from_secs(5));
        });
    }
    for _ in 0..releases.len() {
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_release = released.clone();
    let (release_now, release_request) = std::sync::mpsc::channel();
    let unblock = std::thread::spawn(move || {
        let _ = release_request.recv_timeout(Duration::from_secs(1));
        observed_release.store(true, Ordering::SeqCst);
        for release in releases {
            let _ = release.send(());
        }
    });
    let responsive = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let heartbeat = tokio::spawn(async {
                tokio::time::sleep(Duration::from_millis(10)).await;
            });
            tokio::task::yield_now().await;
            let mut many = (*block::genesis::regtest_genesis_block()).clone();
            // Block parsing rejects transactions averaging fewer than `MIN_TRANSPARENT_TX_SIZE`
            // bytes, so each one has a small output; 66 of them still fit in 4 KiB.
            let repeated_transaction = Arc::new(Transaction::test_v4(
                Vec::new(),
                vec![transparent::Output::new(
                    0.try_into().unwrap(),
                    transparent::Script::new(&[0; 20]),
                )],
                LockTime::unlocked(),
                Height(0),
            ));
            many.transactions = vec![repeated_transaction; 66];
            let bytes = many.zcash_serialize_to_vec().unwrap();
            assert!(bytes.len() <= 4096);
            let decoded = super::super::DecodedBlock::deserialize(&bytes).unwrap();
            assert_eq!(decoded.block().transactions.len(), 66);
            let network = network();
            let block = candidate(&network, Height(3), spend(LockTime::unlocked(), Height(0)));
            let decoded =
                super::super::DecodedBlock::deserialize(&block.zcash_serialize_to_vec().unwrap())
                    .unwrap();
            let transactions = service_fn(|_: tx::BlockRequest| {
                ready(Err::<tx::BlockResponse, BoxError>(
                    TransactionError::BadBalance.into(),
                ))
            });
            let result = SemanticBlockVerifier::new(&network, mock_state(), transactions)
                .oneshot(Request::CommitDecoded(decoded, None))
                .await;
            assert!(matches!(
                result,
                Err(VerifyBlockError::Transaction(TransactionError::BadBalance))
            ));
            heartbeat.await.unwrap();
            !released.load(Ordering::SeqCst)
        });
    let _ = release_now.send(());
    unblock.join().unwrap();
    assert!(
        responsive,
        "small decoding, body checks and Tokio timers must progress with Rayon full"
    );
}
