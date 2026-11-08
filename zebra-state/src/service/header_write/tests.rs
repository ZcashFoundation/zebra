//! Tests for the header writer: appending, forks, and rejections.

use std::sync::Arc;

use chrono::Duration;

use zebra_chain::{
    block::{self, genesis::regtest_genesis_block, Block, Height},
    parameters::{
        testnet::{ConfiguredCheckpoints, RegtestParameters},
        Network,
    },
};

use crate::constants::MAX_BLOCK_REORG_HEIGHT;

use crate::{
    service::{
        finalized_state::{FinalizedState, ZebraDb},
        read,
    },
    tests::FakeChainHelper,
    CheckpointVerifiedBlock, Config,
};

use super::{
    commit_headers, drop_headers_off_the_checkpoints, CommitHeadersError, HeadersCommitted,
};

fn regtest() -> Network {
    Network::new_regtest(RegtestParameters::default())
}

/// A Regtest state with only the genesis block.
fn genesis_state(network: &Network) -> (FinalizedState, Arc<Block>) {
    let mut state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .expect("opening an ephemeral database should succeed");

    let genesis = regtest_genesis_block();
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(genesis.clone()).into(),
            None,
            "header writer test",
        )
        .expect("the genesis block is valid");

    (state, genesis)
}

/// `count` synthetic Regtest headers on fork `fork`, above `parent` at
/// `parent_height`. Regtest has a fixed difficulty and no maximum block time,
/// so headers 75 seconds apart pass the contextual checks.
fn headers_above(
    parent: &block::Header,
    parent_height: Height,
    count: u32,
    fork: u8,
) -> Vec<Arc<block::Header>> {
    let mut previous = *parent;
    let mut headers = Vec::new();

    for offset in 1..=count {
        let height = parent_height.0 + offset;
        let mut header = previous;
        header.previous_block_hash = previous.hash();
        header.time = parent.time + Duration::seconds(75 * i64::from(offset));
        header.nonce[0] = fork;
        header.nonce[1..5].copy_from_slice(&height.to_le_bytes());

        headers.push(Arc::new(header));
        previous = header;
    }

    headers
}

fn tip_of(headers: &[Arc<block::Header>], height: u32) -> (Height, block::Hash) {
    (
        Height(height),
        headers.last().expect("headers are not empty").hash(),
    )
}

fn hashes_above(db: &ZebraDb, hash: block::Hash, limit: u32) -> Vec<block::Hash> {
    read::header_chain_hashes(db, &[hash], limit)
}

#[test]
fn batches_append_and_skip_known_headers() {
    let _init_guard = zebra_test::init();

    let network = regtest();
    let (state, genesis) = genesis_state(&network);
    let chain = headers_above(&genesis.header, Height(0), 8, 0);

    assert_eq!(
        commit_headers(&state.db, &network, chain[..5].to_vec()),
        Ok(HeadersCommitted {
            tip: tip_of(&chain[..5], 5),
            added: 5,
            reorged: false,
        })
    );

    // An overlapping batch only adds the new headers.
    assert_eq!(
        commit_headers(&state.db, &network, chain[3..8].to_vec()),
        Ok(HeadersCommitted {
            tip: tip_of(&chain, 8),
            added: 3,
            reorged: false,
        })
    );

    // A batch the chain already has changes nothing.
    assert_eq!(
        commit_headers(&state.db, &network, chain[..2].to_vec()),
        Ok(HeadersCommitted {
            tip: tip_of(&chain, 8),
            added: 0,
            reorged: false,
        })
    );

    assert_eq!(state.db.header_chain_tip(), Some(tip_of(&chain, 8)));
    assert_eq!(
        hashes_above(&state.db, genesis.hash(), 100),
        chain.iter().map(|header| header.hash()).collect::<Vec<_>>()
    );
    assert_eq!(
        hashes_above(&state.db, chain[5].hash(), 1),
        vec![chain[6].hash()]
    );

    let locator = read::header_locator(&state.db);
    assert_eq!(locator.first(), Some(&tip_of(&chain, 8)));
    assert_eq!(locator.last(), Some(&(Height(0), genesis.hash())));
}

#[test]
fn disconnected_and_invalid_batches_are_rejected() {
    let _init_guard = zebra_test::init();

    let network = regtest();
    let (state, genesis) = genesis_state(&network);
    let chain = headers_above(&genesis.header, Height(0), 4, 0);

    let error = commit_headers(&state.db, &network, chain[2..].to_vec())
        .expect_err("the parent is unknown");
    assert!(matches!(error, CommitHeadersError::Disconnected { .. }));
    assert!(!error.is_misbehavior());

    let gap = vec![chain[0].clone(), chain[2].clone()];
    let error = commit_headers(&state.db, &network, gap).expect_err("the batch has a gap");
    assert_eq!(
        error,
        CommitHeadersError::NotContiguous { height: Height(2) }
    );
    assert!(error.is_misbehavior());

    // A header at or before the median-time-past of its parents.
    let mut early = *chain[0];
    early.time = genesis.header.time;
    let error = commit_headers(&state.db, &network, vec![Arc::new(early)])
        .expect_err("the header is too early");
    assert!(
        matches!(
            error,
            CommitHeadersError::InvalidContext {
                height: Height(1),
                ..
            }
        ),
        "{error:?}"
    );
    assert!(error.is_misbehavior());

    assert_eq!(
        commit_headers(&state.db, &network, Vec::new()),
        Err(CommitHeadersError::Empty)
    );
    assert_eq!(
        state.db.header_chain_tip(),
        Some((Height(0), genesis.hash()))
    );
}

#[test]
fn a_fork_with_more_work_replaces_the_stored_headers() {
    let _init_guard = zebra_test::init();

    let network = regtest();
    let (state, genesis) = genesis_state(&network);
    let chain = headers_above(&genesis.header, Height(0), 5, 0);
    commit_headers(&state.db, &network, chain.clone()).expect("the chain is valid");

    // A shorter fork at height 2, with less work, is not stored.
    let shorter = headers_above(&chain[1], Height(2), 2, 1);
    let error = commit_headers(&state.db, &network, shorter).expect_err("the fork has less work");
    assert_eq!(
        error,
        CommitHeadersError::LessWork {
            fork_parent: Height(2)
        }
    );
    assert!(!error.is_misbehavior());

    // An equal-work fork is not stored either: the first chain wins ties.
    let equal = headers_above(&chain[1], Height(2), 3, 2);
    assert!(matches!(
        commit_headers(&state.db, &network, equal),
        Err(CommitHeadersError::LessWork { .. })
    ));
    assert_eq!(state.db.header_chain_tip(), Some(tip_of(&chain, 5)));

    // A longer fork, sent with the shared headers below it, replaces the
    // stored headers above the fork point.
    let longer = headers_above(&chain[1], Height(2), 4, 3);
    let mut batch = chain[..2].to_vec();
    batch.extend(longer.iter().cloned());
    assert_eq!(
        commit_headers(&state.db, &network, batch),
        Ok(HeadersCommitted {
            tip: tip_of(&longer, 6),
            added: 4,
            reorged: true,
        })
    );

    assert_eq!(
        state.db.header_chain_hash(Height(3)),
        Some(longer[0].hash())
    );
    assert_eq!(state.db.header_chain_height(chain[2].hash()), None);
    assert_eq!(state.db.header_chain_height(chain[4].hash()), None);
    assert_eq!(
        state.db.header_chain_height(longer[3].hash()),
        Some(Height(6))
    );
}

#[test]
fn a_fork_below_the_finalized_tip_is_rejected() {
    let _init_guard = zebra_test::init();

    let network = regtest();
    let (mut state, genesis) = genesis_state(&network);

    // Block 1, committed as a body.
    let block_1 = genesis.make_fake_child();
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(block_1.clone()).into(),
            None,
            "header writer test",
        )
        .expect("block 1 is valid");

    let fork = headers_above(&genesis.header, Height(0), 3, 1);
    let error = commit_headers(&state.db, &network, fork)
        .expect_err("the fork conflicts with finalized block 1");
    assert_eq!(
        error,
        CommitHeadersError::BelowFinalizedTip {
            height: Height(1),
            finalized_tip: Height(1),
        }
    );

    let above = headers_above(&block_1.header, Height(1), 2, 0);
    commit_headers(&state.db, &network, above).expect("headers above the finalized tip");
}

/// Regtest with checkpoints at genesis and at the heights and hashes of `checkpoints`.
fn regtest_with_checkpoints(checkpoints: &[(Height, block::Hash)]) -> Network {
    let mut list = vec![(Height(0), regtest_genesis_block().hash())];
    list.extend_from_slice(checkpoints);

    Network::new_regtest(RegtestParameters {
        checkpoints: Some(ConfiguredCheckpoints::HeightsAndHashes(list)),
        ..RegtestParameters::default()
    })
}

#[test]
fn a_fork_deeper_than_a_reorg_with_more_work_replaces_the_stored_headers() {
    let _init_guard = zebra_test::init();

    let network = regtest();
    let (state, genesis) = genesis_state(&network);

    // A peer stores a chain first, deeper than any block chain reorg.
    let depth = MAX_BLOCK_REORG_HEIGHT + 1;
    let first = headers_above(&genesis.header, Height(0), depth, 0);
    commit_headers(&state.db, &network, first).expect("the chain is valid");

    // Regtest headers all have the same work, so a longer fork has more work.
    let fork = headers_above(&genesis.header, Height(0), depth + 1, 1);
    assert_eq!(
        commit_headers(&state.db, &network, fork.clone()),
        Ok(HeadersCommitted {
            tip: tip_of(&fork, depth + 1),
            added: usize::try_from(depth + 1).expect("the depth fits in a usize"),
            reorged: true,
        })
    );

    // A deep fork with less work is still not stored.
    let shorter = headers_above(&genesis.header, Height(0), depth, 2);
    assert_eq!(
        commit_headers(&state.db, &network, shorter),
        Err(CommitHeadersError::LessWork {
            fork_parent: Height(0)
        })
    );
    assert_eq!(state.db.header_chain_tip(), Some(tip_of(&fork, depth + 1)));
}

#[test]
fn a_fork_below_the_last_checkpoint_is_rejected() {
    let _init_guard = zebra_test::init();

    let chain = headers_above(&regtest_genesis_block().header, Height(0), 5, 0);
    let network = regtest_with_checkpoints(&[(Height(3), chain[2].hash())]);
    let (state, genesis) = genesis_state(&network);
    commit_headers(&state.db, &network, chain.clone()).expect("the chain is valid");

    // The fork has more work, but it replaces the checkpointed header at 3.
    let fork = headers_above(&chain[0], Height(1), 10, 1);
    let error = commit_headers(&state.db, &network, fork)
        .expect_err("the fork can't reach the checkpoint at 3");
    assert_eq!(
        error,
        CommitHeadersError::BelowLastCheckpoint {
            fork_parent: Height(1),
            checkpoint: Height(3),
        }
    );
    assert!(!error.is_misbehavior());

    // A fork above the last checkpoint is still chosen by work.
    let fork = headers_above(&chain[2], Height(3), 3, 1);
    assert_eq!(
        commit_headers(&state.db, &network, fork.clone()),
        Ok(HeadersCommitted {
            tip: tip_of(&fork, 6),
            added: 3,
            reorged: true,
        })
    );
    assert_eq!(state.db.header_chain_hash(Height(0)), Some(genesis.hash()));
}

#[test]
fn headers_off_a_new_checkpoint_are_dropped_at_startup() {
    let _init_guard = zebra_test::init();

    // An older Zebra stored this chain above its last checkpoint, genesis.
    let network = regtest();
    let (state, genesis) = genesis_state(&network);
    let chain = headers_above(&genesis.header, Height(0), 6, 0);
    commit_headers(&state.db, &network, chain.clone()).expect("the chain is valid");

    // A newer Zebra matches the first checkpoint, but not the second.
    let other = headers_above(&genesis.header, Height(0), 6, 1);
    let newer =
        regtest_with_checkpoints(&[(Height(2), chain[1].hash()), (Height(4), other[3].hash())]);
    let kept = Some((Height(2), chain[1].hash()));
    drop_headers_off_the_checkpoints(&state.db, &newer);
    assert_eq!(state.db.header_chain_tip(), kept);

    // Headers that match every checkpoint they reach are kept.
    drop_headers_off_the_checkpoints(&state.db, &newer);
    assert_eq!(state.db.header_chain_tip(), kept);
}

#[test]
fn bodies_that_leave_the_header_chain_drop_the_stored_headers() {
    let _init_guard = zebra_test::init();

    let network = regtest();
    let (mut state, genesis) = genesis_state(&network);
    let chain = headers_above(&genesis.header, Height(0), 3, 7);
    commit_headers(&state.db, &network, chain).expect("the chain is valid");

    // The body chain finalizes another block 1.
    let block_1 = genesis.make_fake_child();
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(block_1.clone()).into(),
            None,
            "header writer test",
        )
        .expect("block 1 is valid");

    super::collect_stale_headers(&state.db);

    assert_eq!(
        state.db.header_chain_tip(),
        Some((Height(1), block_1.hash()))
    );
    assert_eq!(state.db.stored_header_tip(), None);
    assert!(state
        .db
        .header_by_height_cf()
        .zs_forward_range_iter(Height(0)..)
        .next()
        .is_none());
}

/// The header writer thread checks the stored headers when it starts, and
/// deletes stale ones while it has no batch to commit.
#[test]
fn the_header_writer_collects_stale_headers_without_a_batch() {
    let _init_guard = zebra_test::init();

    let network = regtest();
    let (mut state, genesis) = genesis_state(&network);
    let no_header_rows = |state: &FinalizedState| {
        state
            .db
            .header_by_height_cf()
            .zs_forward_range_iter(Height(0)..)
            .next()
            .is_none()
    };
    let wait_for_no_header_rows = |state: &FinalizedState| {
        for _ in 0..100 {
            if no_header_rows(state) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    };

    // Before the writer starts, the body chain finalizes another block 1.
    let chain = headers_above(&genesis.header, Height(0), 3, 7);
    commit_headers(&state.db, &network, chain).expect("the chain is valid");
    let block_1 = genesis.make_fake_child();
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(block_1.clone()).into(),
            None,
            "header writer test",
        )
        .expect("block 1 is valid");

    let _sender = super::spawn(state.db.clone(), network.clone());
    assert!(
        wait_for_no_header_rows(&state),
        "the writer drops the headers when it starts"
    );

    // While the writer is idle, a row at the finalized tip appears, like the
    // row of a header whose body the block writer just committed.
    let mut batch = crate::service::finalized_state::DiskWriteBatch::new();
    batch.prepare_header_chain_append(&state.db, Height(1), std::slice::from_ref(&block_1.header));
    state.db.write_batch(batch).expect("the write succeeds");
    assert!(!no_header_rows(&state));

    assert!(
        wait_for_no_header_rows(&state),
        "the idle writer drops the stale headers"
    );
}

/// The state service sends batches to the header writer thread, and the read
/// requests see what it stored.
#[tokio::test(flavor = "multi_thread")]
async fn state_service_commits_and_reads_headers() {
    use tower::ServiceExt;

    use crate::{Request, Response};

    let _init_guard = zebra_test::init();

    let network = regtest();
    let (state, _read_state, _latest_chain_tip, _chain_tip_change) =
        crate::init_test_services(&network).await;

    let genesis = regtest_genesis_block();
    state
        .clone()
        .oneshot(Request::CommitCheckpointVerifiedBlock(
            CheckpointVerifiedBlock::from(genesis.clone()),
        ))
        .await
        .expect("the genesis block commits");

    let chain = headers_above(&genesis.header, Height(0), 3, 0);
    let response = state
        .clone()
        .oneshot(Request::CommitHeaders(chain.clone()))
        .await
        .expect("the headers are valid");
    assert_eq!(
        response,
        Response::HeadersCommitted(HeadersCommitted {
            tip: tip_of(&chain, 3),
            added: 3,
            reorged: false,
        })
    );

    let error = state
        .clone()
        .oneshot(Request::CommitHeaders(chain[2..].to_vec()))
        .await
        .err()
        .map(|error| error.to_string());
    assert_eq!(error, None, "a known header is not an error");

    let error = state
        .clone()
        .oneshot(Request::CommitHeaders(
            headers_above(&chain[2], Height(3), 2, 0)[1..].to_vec(),
        ))
        .await
        .expect_err("the batch doesn't connect");
    assert!(matches!(
        error.downcast_ref::<CommitHeadersError>(),
        Some(CommitHeadersError::Disconnected { .. })
    ));

    assert_eq!(
        state.clone().oneshot(Request::HeaderTip).await.ok(),
        Some(Response::HeaderTip(Some(tip_of(&chain, 3))))
    );
    assert_eq!(
        state
            .clone()
            .oneshot(Request::HeaderChainHashes {
                known_blocks: vec![chain[0].hash(), genesis.hash()],
                limit: 10,
            })
            .await
            .ok(),
        Some(Response::BlockHashes(vec![
            chain[1].hash(),
            chain[2].hash()
        ]))
    );
    let Ok(Response::HeaderLocator(locator)) = state.clone().oneshot(Request::HeaderLocator).await
    else {
        panic!("the header locator request returns a locator");
    };
    assert_eq!(locator.first(), Some(&tip_of(&chain, 3)));
}

/// Above the last checkpoint, the stored header chain runs at most
/// `MAX_HEADER_LOOKAHEAD` past the finalized tip's reorg window, so every fork
/// competes with fewer stored headers than one batch can hold.
#[test]
fn stored_headers_stop_at_the_lookahead_limit() {
    let _init_guard = zebra_test::init();

    let network = regtest();
    let (state, genesis) = genesis_state(&network);
    let limit = MAX_BLOCK_REORG_HEIGHT + super::MAX_HEADER_LOOKAHEAD;
    let batch = u32::try_from(super::MAX_COMMIT_HEADERS).expect("the batch limit fits in a u32");

    let chain = headers_above(&genesis.header, Height(0), batch + 1, 0);
    let _ = commit_headers(
        &state.db,
        &network,
        chain[..super::MAX_COMMIT_HEADERS].to_vec(),
    );
    let _ = commit_headers(
        &state.db,
        &network,
        chain[super::MAX_COMMIT_HEADERS..].to_vec(),
    );
    assert_eq!(
        state.db.header_chain_tip(),
        Some(tip_of(&chain[..limit as usize], limit)),
        "a batch that crosses the limit stores the headers below it"
    );

    // A fork from genesis that also crosses the limit only ties the stored
    // headers up to it, so the stored chain stays.
    let fork = headers_above(&genesis.header, Height(0), batch, 1);
    assert_eq!(
        commit_headers(&state.db, &network, fork),
        Err(CommitHeadersError::LessWork {
            fork_parent: Height(0)
        })
    );

    // Headers above the limit are not stored until the limit moves up.
    let above = headers_above(&chain[limit as usize - 1], Height(limit), 1, 0);
    let error =
        commit_headers(&state.db, &network, above).expect_err("the header is above the limit");
    assert_eq!(
        error,
        CommitHeadersError::AboveLookahead {
            height: Height(limit + 1),
            limit: Height(limit),
        }
    );
    assert!(!error.is_misbehavior());
}

/// A fork that crosses the lookahead limit replaces a shorter stored chain,
/// and stores only its headers up to the limit.
#[test]
fn a_fork_across_the_lookahead_limit_replaces_a_shorter_chain() {
    let _init_guard = zebra_test::init();

    let network = regtest();
    let (state, genesis) = genesis_state(&network);
    let limit = MAX_BLOCK_REORG_HEIGHT + super::MAX_HEADER_LOOKAHEAD;
    let batch = u32::try_from(super::MAX_COMMIT_HEADERS).expect("the batch limit fits in a u32");

    let stored = headers_above(&genesis.header, Height(0), 1000, 0);
    commit_headers(&state.db, &network, stored).expect("the chain is valid");

    let fork = headers_above(&genesis.header, Height(0), batch, 1);
    assert_eq!(
        commit_headers(&state.db, &network, fork.clone()),
        Ok(HeadersCommitted {
            tip: tip_of(&fork[..limit as usize], limit),
            added: limit as usize,
            reorged: true,
        })
    );
}
