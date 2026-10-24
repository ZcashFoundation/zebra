//! Fixed test vectors for "find" read requests.

use zebra_chain::block::Height;

use crate::{constants, service::read::find::block_locator_heights};

/// Block heights, and the expected minimum block locator height.
///
/// Cases are computed against [`constants::MAX_BLOCK_REORG_HEIGHT`].
static BLOCK_LOCATOR_CASES: &[(u32, u32)] = &[
    (0, 0),
    (1, 0),
    (10, 0),
    (99, 0),
    (100, 0),
    (999, 0),
    (1000, 0),
    (1001, 1),
    (2000, 1000),
    (10000, 9000),
];

/// Tests that `find_fork_point` returns the most recent locator entry on the best chain.
#[test]
fn find_fork_point_locates_the_fork() {
    use std::sync::Arc;

    use zebra_chain::{
        amount::NonNegative, block::Block, parameters::Network::Mainnet,
        value_balance::ValueBalance,
    };

    use crate::{
        arbitrary::Prepare,
        service::{
            finalized_state::FinalizedState, non_finalized_state::NonFinalizedState,
            read::find_fork_point,
        },
        tests::FakeChainHelper,
        Config,
    };

    let _init_guard = zebra_test::init();
    let network = Mainnet;

    // Pre-Heartwood block as a base, to avoid history-tree complications (matches the
    // `any_chain_block_finds_side_chain_blocks` fixture in read/tests/vectors.rs).
    let base: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    // Two competing children of `base`: different work -> different hashes -> a fork.
    let best_block = base.make_fake_child().set_work(100);
    let side_block = base.make_fake_child().set_work(50);

    let base_hash = base.hash();
    let base_height = base.coinbase_height().unwrap();
    let best_hash = best_block.hash();
    let best_height = best_block.coinbase_height().unwrap();
    let side_hash = side_block.hash();

    assert_ne!(
        best_hash, side_hash,
        "could not create distinct block hashes for fork-point test",
    );

    let mut non_finalized_state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    non_finalized_state
        .commit_new_chain(base.prepare(), &finalized_state)
        .unwrap();
    non_finalized_state
        .commit_block(best_block.clone().prepare(), &finalized_state)
        .unwrap();
    non_finalized_state
        .commit_block(side_block.clone().prepare(), &finalized_state)
        .unwrap();
    assert_eq!(
        non_finalized_state.chain_count(),
        2,
        "should have two competing chains"
    );

    let best_chain = non_finalized_state.best_chain();
    let db = &finalized_state.db;

    // No reorg: the locator's top is on the best chain -> it is its own fork point.
    assert_eq!(
        find_fork_point(best_chain, db, vec![best_hash]),
        Some((best_height, best_hash)),
    );

    // Reorg: a locator over the orphaned tip plus its shared ancestor -> the shared
    // ancestor (`base`) is the fork point.
    assert_eq!(
        find_fork_point(best_chain, db, vec![side_hash, base_hash]),
        Some((base_height, base_hash)),
    );

    // A locator containing only the orphaned tip has no entry on the best chain.
    assert_eq!(find_fork_point(best_chain, db, vec![side_hash]), None);

    // An empty locator has no fork point.
    assert_eq!(find_fork_point(best_chain, db, vec![]), None);
}

#[test]
fn find_hashes_stop_at_pruned_history_without_skipping_the_gap() {
    use std::sync::Arc;

    use zebra_chain::{
        block::Block, parameters::Network::Mainnet, serialization::ZcashDeserializeInto,
    };

    use crate::{
        service::{
            finalized_state::{DiskWriteBatch, FinalizedState, TransactionLocation, WriteDisk},
            non_finalized_state::Chain,
            read::{find_chain_hashes, find_chain_headers},
        },
        CheckpointVerifiedBlock, Config, StateInitError,
    };

    let _init_guard = zebra_test::init();
    let mut state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        &Mainnet,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .unwrap();
    let chain = None::<&Arc<Chain>>;
    assert!(find_chain_hashes(chain, &state.db, vec![], None, 10)
        .unwrap()
        .is_empty());
    assert!(find_chain_headers(chain, &state.db, vec![], None, 10).is_empty());

    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .take(4)
        .map(|bytes| bytes.zcash_deserialize_into().unwrap())
        .collect();
    for block in &blocks {
        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block.clone()).into(),
                None,
                "pruned block locator test",
            )
            .unwrap();
    }
    let db = &state.db;
    let hashes: Vec<_> = blocks.iter().map(|block| block.hash()).collect();
    let headers: Vec<_> = blocks.iter().map(|block| block.header.clone()).collect();
    assert_eq!(
        find_chain_hashes(chain, db, vec![hashes[0]], None, 10).unwrap(),
        hashes[1..],
    );

    let mut batch = DiskWriteBatch::new();
    batch.zs_delete_range(
        &db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(Height(1)),
        TransactionLocation::min_for_height(Height(2)),
    );
    db.write_batch(batch).unwrap();

    // A locator before the gap must not jump to the retained suffix.
    assert!(find_chain_hashes(chain, db, vec![hashes[0]], None, 10)
        .unwrap()
        .is_empty());
    // Without an intersection, preserve the genesis prefix and stop at the gap.
    assert_eq!(
        find_chain_hashes(chain, db, vec![], None, 10).unwrap(),
        hashes[..1],
    );
    // The successor at the retained floor is available, with normal stop and count limits.
    for (stop, max_len, expected) in [
        (None, 10, &hashes[2..]),
        (Some(hashes[2]), 10, &hashes[2..3]),
        (None, 1, &hashes[2..3]),
    ] {
        assert_eq!(
            find_chain_hashes(chain, db, vec![hashes[1]], stop, max_len).unwrap(),
            expected,
        );
    }
    assert_eq!(
        find_chain_headers(chain, db, vec![hashes[0]], None, 10),
        headers[1..],
    );
    assert!(find_chain_headers(chain, db, vec![hashes[3]], None, 10).is_empty());

    // A damaged availability boundary is an error, not an empty inventory.
    let mut batch = DiskWriteBatch::new();
    batch.zs_delete_range(
        &db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(Height(2)),
        TransactionLocation::min_for_height(Height(4)),
    );
    db.write_batch(batch).unwrap();
    let error = find_chain_hashes(chain, db, vec![hashes[0]], None, 10).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<StateInitError>(),
        Some(StateInitError::HistoryRead { .. })
    ));
    assert_eq!(
        find_chain_headers(chain, db, vec![hashes[0]], None, 10),
        headers[1..],
    );
}

/// Check that the block locator heights are sensible.
#[test]
fn test_block_locator_heights() {
    let _init_guard = zebra_test::init();

    for (height, min_height) in BLOCK_LOCATOR_CASES.iter().cloned() {
        let locator = block_locator_heights(Height(height));

        assert!(!locator.is_empty(), "locators must not be empty");
        if (height - min_height) > 1 {
            assert!(
                locator.len() > 2,
                "non-trivial locators must have some intermediate heights"
            );
        }

        assert_eq!(
            locator[0],
            Height(height),
            "locators must start with the tip height"
        );

        // Check that the locator is sorted, and that it has no duplicates
        // TODO: replace with dedup() and is_sorted_by() when sorting stabilises.
        assert!(locator.windows(2).all(|v| match v {
            [a, b] => a.0 > b.0,
            _ => unreachable!("windows returns exact sized slices"),
        }));

        let final_height = locator[locator.len() - 1];
        assert_eq!(
            final_height,
            Height(min_height),
            "locators must end with the specified final height"
        );
        assert!(
            height - final_height.0 <= constants::MAX_BLOCK_REORG_HEIGHT,
            "locator for {} must not be more than MAX_BLOCK_REORG_HEIGHT ({}) below the tip, \
             but {} is {} blocks below the tip",
            height,
            constants::MAX_BLOCK_REORG_HEIGHT,
            final_height.0,
            height - final_height.0
        );
    }
}
