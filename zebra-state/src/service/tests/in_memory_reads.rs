//! The state answers requests for a non-finalized best tip, for its mining candidate, for a block
//! template's chain info, and for a transparent output that a non-finalized chain or the
//! database has in memory, without a blocking thread.

use std::time::Duration;

use tower::{Service, ServiceExt};
use zebra_chain::serialization::DateTime32;

use crate::{
    service::{
        arbitrary::TEST_MINING_CANDIDATE_LIFETIME, check::difficulty::pow_adjustment_block_span,
        read,
    },
    CheckpointVerifiedBlock, ReadRequest, ReadResponse, Request, Response,
};

use super::mining_rejection::{candidate, state};

#[test]
fn in_memory_tip_and_mining_candidate_reads_need_no_blocking_thread() {
    let _init_guard = zebra_test::init();

    // The states are created on their own runtime: committing their blocks needs blocking
    // threads, and a multi-threaded runtime.
    let setup = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (non_finalized, finalized_only) = setup.block_on(async {
        let (_state, non_finalized) = state(false).await;
        let (_state, finalized_only) = state(true).await;
        (non_finalized, finalized_only)
    });
    let expected_tip = non_finalized.best_tip();
    assert!(expected_tip.is_some());

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            // Keep the only blocking thread busy, and queue a probe that shows it stays busy.
            let (started, started_rx) = tokio::sync::oneshot::channel();
            let (release, held) = std::sync::mpsc::channel::<()>();
            let busy = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = held.recv_timeout(Duration::from_secs(30));
            });
            started_rx.await.unwrap();
            let probe = tokio::task::spawn_blocking(|| ());

            let answer = |state: &crate::ReadStateService, request| {
                tokio::time::timeout(Duration::from_secs(5), state.clone().oneshot(request))
            };

            let tip = answer(&non_finalized, ReadRequest::Tip)
                .await
                .expect("a non-finalized tip is read from memory");
            assert!(matches!(tip, Ok(ReadResponse::Tip(tip)) if tip == expected_tip));

            // Without a published candidate, the answer doesn't depend on the tip, wherever
            // the tip is.
            for state in [&non_finalized, &finalized_only] {
                let candidate = answer(state, ReadRequest::MiningCandidate)
                    .await
                    .expect("the mining candidate is read from memory");
                assert!(matches!(candidate, Ok(ReadResponse::MiningCandidate(None))));
            }

            assert!(
                !probe.is_finished(),
                "the blocking thread was busy for all of these reads",
            );
            drop(release);
            busy.await.unwrap();
            probe.await.unwrap();
        });
    drop(setup);
}

/// A published candidate that extends a non-finalized tip must be read from memory too: the
/// template updater and a waiting miner's request read it on their way.
#[test]
fn published_mining_candidate_read_needs_no_blocking_thread() {
    let _init_guard = zebra_test::init();

    // The candidate is published on its own runtime, which keeps its publishing task running.
    let setup = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_state, read, block, _guard, _changes) = setup.block_on(async {
        let (mut state, read) = state(false).await;
        state.mining_candidate_lifetime = TEST_MINING_CANDIDATE_LIFETIME;
        let block = candidate(&read, false, 0).await;
        let ReadResponse::MiningCandidateChanges(mut changes) = read
            .clone()
            .oneshot(ReadRequest::MiningCandidateChanges)
            .await
            .unwrap()
        else {
            panic!("the state returns a mining candidate subscription");
        };
        let Response::MiningCandidate(Some(guard)) = state
            .ready()
            .await
            .unwrap()
            .call(Request::MiningCandidate(block.clone()))
            .await
            .unwrap()
        else {
            panic!("a child of the best tip is admitted");
        };
        changes.changed().await;
        (state, read, block, guard, changes)
    });

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            // Keep the only blocking thread busy, and queue a probe that shows it stays busy.
            let (started, started_rx) = tokio::sync::oneshot::channel();
            let (release, held) = std::sync::mpsc::channel::<()>();
            let busy = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = held.recv_timeout(Duration::from_secs(30));
            });
            started_rx.await.unwrap();
            let probe = tokio::task::spawn_blocking(|| ());

            let published = tokio::time::timeout(
                Duration::from_secs(5),
                read.clone().oneshot(ReadRequest::MiningCandidate),
            )
            .await
            .expect("a published candidate on a non-finalized tip is read from memory");
            assert!(matches!(
                published,
                Ok(ReadResponse::MiningCandidate(Some(info))) if info.tip_hash == block.hash()
            ));

            assert!(
                !probe.is_finished(),
                "the blocking thread was busy for the read",
            );
            drop(release);
            busy.await.unwrap();
            probe.await.unwrap();
        });
    drop(setup);
}

/// The chain info for a block template must be computed without a blocking thread once the best
/// chain holds every header that the next block's difficulty adjustment reads, and it must
/// match what a read of the whole state returns. With one header fewer in the chain, the read
/// needs the finalized state, so it is left to a blocking thread.
#[test]
fn chain_info_read_needs_no_blocking_thread_once_the_chain_holds_its_headers() {
    let _init_guard = zebra_test::init();

    let setup = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    // Returns a state whose best chain has `missing` fewer blocks than the difficulty adjustment
    // of its next block reads.
    let chain_with_headers = |missing: usize| async move {
        let (mut state, read) = state(false).await;
        loop {
            let chain = read
                .latest_best_chain()
                .expect("the state has a non-finalized block");
            let next_height = chain.non_finalized_tip_height().next().unwrap();
            if chain.len() + missing >= pow_adjustment_block_span(&read.network, next_height) {
                return (state, read);
            }
            let block = candidate(&read, false, 0).await;
            state
                .ready()
                .await
                .unwrap()
                .call(Request::CommitSemanticallyVerifiedBlock(
                    CheckpointVerifiedBlock::from(block).into(),
                ))
                .await
                .unwrap();
        }
    };
    let ((_complete_state, complete), (_short_state, short)) =
        setup.block_on(async { (chain_with_headers(0).await, chain_with_headers(1).await) });

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            // Keep the only blocking thread busy, so a read that needs one can't finish.
            let (started, started_rx) = tokio::sync::oneshot::channel();
            let (release, held) = std::sync::mpsc::channel::<()>();
            let busy = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = held.recv_timeout(Duration::from_secs(30));
            });
            started_rx.await.unwrap();

            // Both reads use the local clock, so they are repeated if the clock's second changes
            // between them.
            let (from_memory, from_state) = loop {
                let second = DateTime32::now();
                let from_memory = futures::poll!(complete.clone().oneshot(ReadRequest::ChainInfo));
                let from_state = read::difficulty::get_block_template_chain_info(
                    &complete.latest_non_finalized_state(),
                    &complete.db,
                    &complete.network,
                )
                .expect("the state has a tip");
                if DateTime32::now() == second {
                    break (from_memory, from_state);
                }
            };
            let std::task::Poll::Ready(Ok(ReadResponse::ChainInfo(from_memory))) = from_memory
            else {
                panic!("a chain with every header answers at once: {from_memory:?}");
            };
            assert_eq!(from_memory, from_state);

            let mut needs_database = Box::pin(short.clone().oneshot(ReadRequest::ChainInfo));
            assert!(
                futures::poll!(&mut needs_database).is_pending(),
                "a chain without every header leaves the read to a blocking thread",
            );

            drop(release);
            busy.await.unwrap();
            assert!(matches!(
                needs_database.await,
                Ok(ReadResponse::ChainInfo(_))
            ));
        });
    drop(setup);
}

/// A transparent output must be read without a blocking thread if a non-finalized chain created
/// it, or if the database has it in memory: a block's transaction checks wait for the outputs
/// their inputs spend. An outpoint that the database's memory shows as absent is answered there
/// too.
#[test]
fn utxo_reads_from_memory_need_no_blocking_thread() {
    let _init_guard = zebra_test::init();

    // The states are created on their own runtime: committing their blocks needs blocking
    // threads, and a multi-threaded runtime.
    let setup = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (states, outpoints) = setup.block_on(async {
        let mut states = Vec::new();
        let mut outpoints = Vec::new();

        // A block in a non-finalized chain, and the same block in the database.
        for finalized in [false, true] {
            let (mut state, read) = state(true).await;
            let block = candidate(&read, false, 0).await;
            let coinbase = block.transactions[0].hash();

            let block = CheckpointVerifiedBlock::from(block);
            let commit = if finalized {
                Request::CommitCheckpointVerifiedBlock(block)
            } else {
                Request::CommitSemanticallyVerifiedBlock(block.into())
            };
            state.ready().await.unwrap().call(commit).await.unwrap();
            assert_eq!(read.non_finalized_best_tip().is_none(), finalized);

            outpoints.push(zebra_chain::transparent::OutPoint {
                hash: coinbase,
                index: 0,
            });
            states.push((state, read));
        }

        (states, outpoints)
    });

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            // Keep the only blocking thread busy, and queue a probe that shows it stays busy.
            let (started, started_rx) = tokio::sync::oneshot::channel();
            let (release, held) = std::sync::mpsc::channel::<()>();
            let busy = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = held.recv_timeout(Duration::from_secs(30));
            });
            started_rx.await.unwrap();
            let probe = tokio::task::spawn_blocking(|| ());

            for ((_state, read), outpoint) in states.iter().zip(&outpoints) {
                let utxo = tokio::time::timeout(
                    Duration::from_secs(5),
                    read.clone().oneshot(ReadRequest::AnyChainUtxo(*outpoint)),
                )
                .await
                .expect("an output in memory is read without a blocking thread");
                assert!(
                    matches!(&utxo, Ok(ReadResponse::AnyChainUtxo(Some(utxo))) if utxo.from_coinbase),
                    "{utxo:?}",
                );

                let missing = zebra_chain::transparent::OutPoint {
                    hash: [8; 32].into(),
                    index: 0,
                };
                let utxo = tokio::time::timeout(
                    Duration::from_secs(5),
                    read.clone().oneshot(ReadRequest::AnyChainUtxo(missing)),
                )
                .await
                .expect("an output that memory shows as absent needs no blocking thread");
                assert!(matches!(utxo, Ok(ReadResponse::AnyChainUtxo(None))), "{utxo:?}");
            }

            assert!(
                !probe.is_finished(),
                "the blocking thread was busy for all of these reads",
            );
            drop(release);
            busy.await.unwrap();
            probe.await.unwrap();
        });
    drop(states);
    drop(setup);
}

/// Returns a database with the first Mainnet blocks, and those blocks. Everything in the database
/// is only in its files: it was reopened after its write buffers were flushed, which leaves its
/// block cache empty.
///
/// The directory is removed when its handle is dropped.
fn database_with_blocks_in_files() -> (
    tempfile::TempDir,
    crate::service::finalized_state::FinalizedState,
    Vec<std::sync::Arc<zebra_chain::block::Block>>,
) {
    use zebra_chain::{parameters::Network, serialization::ZcashDeserializeInto};

    use crate::{service::finalized_state::FinalizedState, Config};

    let cache = tempfile::tempdir().expect("the temporary cache directory can be created");
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let open = || {
        FinalizedState::new_with_debug(&config, &Network::Mainnet, true, false)
            .expect("the persistent database configuration is valid")
    };

    let mut finalized = open();
    finalized
        .db
        .update_format_version_on_disk(&finalized.db.format_version_in_code())
        .expect("the format version can be written");
    let blocks: Vec<std::sync::Arc<zebra_chain::block::Block>> =
        zebra_test::vectors::MAINNET_BLOCKS
            .range(0..=1)
            .map(|(_, bytes)| {
                bytes
                    .zcash_deserialize_into()
                    .expect("test vector blocks are valid")
            })
            .collect();
    for block in &blocks {
        finalized
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block.clone()).into(),
                None,
                "in-memory read test",
            )
            .expect("test vector blocks are valid");
    }

    finalized.db.flush().expect("the database can be flushed");
    drop(finalized);

    (cache, open(), blocks)
}

/// A transparent output that the database only has in a file is left to a blocking thread, and
/// the read returns it from there.
#[test]
fn utxo_read_from_a_database_file_is_left_to_a_blocking_thread() {
    use tokio::sync::watch;
    use zebra_chain::{parameters::Network, transparent};

    use crate::{
        service::{non_finalized_state::NonFinalizedState, ReadStateService},
        WatchReceiver,
    };

    let _init_guard = zebra_test::init();

    let (_cache, finalized, blocks) = database_with_blocks_in_files();
    let network = Network::Mainnet;
    let outpoint = transparent::OutPoint {
        hash: blocks[1].transactions[0].hash(),
        index: 0,
    };
    assert_eq!(finalized.db.utxo_from_memory(&outpoint), None);

    let (_non_finalized_state_sender, non_finalized_state_receiver) =
        watch::channel(NonFinalizedState::new(&network));
    let read = ReadStateService::new(
        &finalized,
        None,
        WatchReceiver::new(non_finalized_state_receiver),
    );

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            // Keep the only blocking thread busy, so a read that needs one can't finish.
            let (started, started_rx) = tokio::sync::oneshot::channel();
            let (release, held) = std::sync::mpsc::channel::<()>();
            let busy = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = held.recv_timeout(Duration::from_secs(30));
            });
            started_rx.await.unwrap();

            let lookup = read.clone().oneshot(ReadRequest::AnyChainUtxo(outpoint));
            tokio::pin!(lookup);
            assert!(
                tokio::time::timeout(Duration::from_millis(200), &mut lookup)
                    .await
                    .is_err(),
                "an output in a file is left to a blocking thread",
            );

            drop(release);
            busy.await.unwrap();
            let utxo = lookup.await;
            // A normal read after the lookup, so that it doesn't put the output's block into
            // the block cache before the lookup.
            let expected = finalized
                .db
                .utxo(&outpoint)
                .expect("the first block's coinbase output is unspent")
                .utxo;
            assert!(
                matches!(&utxo, Ok(ReadResponse::AnyChainUtxo(Some(utxo))) if *utxo == expected),
                "{utxo:?}",
            );

            // The blocking read put the output's block into the block cache.
            assert_eq!(
                finalized
                    .db
                    .utxo_from_memory(&outpoint)
                    .map(|utxo| utxo.map(|utxo| utxo.utxo)),
                Some(Some(expected)),
            );
        });
}
