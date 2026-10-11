//! Template timestamp bounds, minimum difficulty, and early mining candidate context.

use zebra_chain::{
    block::Block,
    parameters::{
        testnet::{ConfiguredActivationHeights, Parameters},
        TESTNET_MAX_TIME_START_HEIGHT,
    },
    serialization::ZcashDeserializeInto,
};

use super::*;

/// Header-only queries must retain genesis and follow the best fork's timestamps.
#[test]
fn header_context_queries_preserve_genesis_and_fork_ancestry() {
    use crate::{
        arbitrary::Prepare,
        tests::{setup::new_state_with_mainnet_genesis, FakeChainHelper},
    };

    let _init_guard = zebra_test::init();
    let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();
    let db = &finalized_state.db;
    let network = Network::Mainnet;
    let genesis_time = DateTime32::try_from(genesis.block.header.time).unwrap();
    let genesis_info = get_block_template_chain_info(&non_finalized_state, db, &network).unwrap();
    assert_eq!(genesis_info.tip_hash, genesis.hash);
    assert_eq!(genesis_info.tip_height, Height(0));
    assert_eq!(
        genesis_info.min_time,
        genesis_time
            .checked_add(Duration32::from_seconds(1))
            .unwrap()
    );
    assert_eq!(
        read::next_median_time_past(&non_finalized_state, db).unwrap(),
        genesis_time
    );

    let base = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    let base_time = base.header.time;
    non_finalized_state
        .commit_new_chain(base.clone().prepare(), db)
        .unwrap();
    let mut best = base.make_fake_child().set_work(100);
    Arc::make_mut(&mut Arc::make_mut(&mut best).header).time =
        base_time + chrono::Duration::seconds(10);
    let mut side = base.make_fake_child().set_work(50);
    Arc::make_mut(&mut Arc::make_mut(&mut side).header).time =
        base_time + chrono::Duration::seconds(30);
    let mut best_tip = best.make_fake_child();
    Arc::make_mut(&mut Arc::make_mut(&mut best_tip).header).time =
        base_time + chrono::Duration::seconds(20);
    for block in [best, best_tip.clone(), side] {
        non_finalized_state
            .commit_block(block.prepare(), db)
            .unwrap();
    }

    let expected_median = DateTime32::try_from(base_time + chrono::Duration::seconds(10)).unwrap();
    let info = get_block_template_chain_info(&non_finalized_state, db, &network).unwrap();
    assert_eq!(info.tip_hash, best_tip.hash());
    assert_eq!(info.tip_height, Height(3));
    assert_eq!(
        info.min_time,
        expected_median
            .checked_add(Duration32::from_seconds(1))
            .unwrap()
    );
    assert_eq!(
        read::next_median_time_past(&non_finalized_state, db).unwrap(),
        expected_median
    );
}

/// Early child context must match the normal checkpoint commit's tree and header updates.
#[test]
fn mining_candidate_matches_committed_block() {
    use std::collections::HashMap;

    use crate::{
        arbitrary::Prepare, service::finalized_state::DiskWriteBatch, CheckpointVerifiedBlock,
        OutputLocation, TransactionLocation, WriteDisk,
    };

    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    for (parent_bytes, candidate_bytes) in [
        (
            &zebra_test::vectors::BLOCK_MAINNET_902999_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_903000_BYTES[..],
        ),
        (
            &zebra_test::vectors::BLOCK_MAINNET_1687106_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES[..],
        ),
    ] {
        let parent = parent_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap();
        let candidate = candidate_bytes
            .zcash_deserialize_into::<Arc<Block>>()
            .unwrap();
        let (mut finalized, non_finalized, candidate) =
            mining_candidate_parent_state(&parent, candidate);
        let orchard_root = finalized.db.note_commitment_trees_for_tip().orchard.root();
        let has_orchard = candidate.orchard_note_commitments().next().is_some();
        assert_eq!(
            has_orchard,
            candidate.coinbase_height() == Some(Height(1_687_107))
        );
        assert_eq!(candidate.header.previous_block_hash, parent.hash());
        let early =
            mining_candidate_chain_info(&non_finalized, &finalized.db, &network, candidate.clone())
                .expect("the real block body extends the seeded parent");
        assert_eq!(
            read::best_tip(&non_finalized, &finalized.db),
            Some((parent.coinbase_height().unwrap(), parent.hash())),
            "computing early context must not commit the candidate",
        );
        assert_eq!(early.tip_hash, candidate.hash());
        assert_eq!(early.tip_height, candidate.coinbase_height().unwrap());
        assert!(early.chain_history_root.is_some());
        assert_ne!(
            early.expected_difficulty,
            network.target_difficulty_limit().to_compact(),
            "the seeded header window must exercise retargeting, not the short-chain fallback",
        );

        // Reuse the sparse-state fixture's zero-valued spent outputs. Checkpoint commits do not
        // revalidate fees or anchors, but still run the production note/history-tree updates.
        let mut batch = DiskWriteBatch::new();
        let mut locations = HashMap::new();
        let prepared = candidate.clone().prepare();
        for (index, (outpoint, utxo)) in prepared
            .test_with_zero_spent_utxos()
            .spent_outputs
            .into_iter()
            .filter(|(outpoint, _)| !prepared.new_outputs.contains_key(outpoint))
            .enumerate()
        {
            let location = *locations
                .entry(outpoint.hash)
                .or_insert_with(|| TransactionLocation::from_usize(Height(1), index + 1));
            batch.zs_insert(
                &finalized.db.db().cf_handle("tx_loc_by_hash").unwrap(),
                outpoint.hash,
                location,
            );
            batch.zs_insert(
                &finalized.db.db().cf_handle("utxo_by_out_loc").unwrap(),
                OutputLocation::from_outpoint(location, &outpoint),
                utxo.utxo.output,
            );
        }
        finalized.db.write_batch(batch).unwrap();
        finalized
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(candidate.clone()).into(),
                None,
                "mining candidate equivalence",
            )
            .expect("the real block commits against the sparse checkpoint state");
        if has_orchard {
            assert_ne!(
                finalized.db.note_commitment_trees_for_tip().orchard.root(),
                orchard_root
            );
        }

        let committed =
            get_block_template_chain_info(&non_finalized, &finalized.db, &network).unwrap();
        assert_eq!(early.tip_hash, committed.tip_hash);
        assert_eq!(early.tip_height, committed.tip_height);
        assert_eq!(early.expected_difficulty, committed.expected_difficulty);
        assert_eq!(early.min_time, committed.min_time);
        assert_eq!(early.max_time, committed.max_time);
        assert_eq!(early.cur_time, committed.cur_time);
        // Historical Mainnet timestamps clamp to MTP's upper bound, independent of wall-clock
        // seconds crossed between the early calculation and the disk commit.
        assert_eq!(early.cur_time, early.max_time);
        assert_eq!(
            early.max_time,
            early
                .min_time
                .checked_add(Duration32::from_seconds(BLOCK_MAX_TIME_SINCE_MEDIAN - 1))
                .unwrap(),
        );
        // This is the child header commitment before NU5, and its history component from NU5.
        assert_eq!(early.chain_history_root, committed.chain_history_root);
    }
}

/// Candidate admission must reject stale parents, forged NU5 authorizing data, and bad work.
#[test]
fn mining_candidate_rejects_invalid_parent_auth_data_and_work() {
    use crate::tests::FakeChainHelper;
    use zebra_chain::{serialization::ZcashSerialize, transaction::Transaction};

    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let parent = zebra_test::vectors::BLOCK_MAINNET_1687106_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    let candidate = zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    let (finalized, non_finalized, candidate) = mining_candidate_parent_state(&parent, candidate);
    let db = &finalized.db;

    assert!(
        mining_candidate_chain_info(&non_finalized, db, &network, candidate.clone()).is_some(),
        "the unmodified transaction bodies must pass before testing rejection",
    );

    let mut wrong_parent = candidate.clone();
    Arc::make_mut(&mut Arc::make_mut(&mut wrong_parent).header).previous_block_hash =
        db.hash(Height(0)).unwrap();
    assert!(mining_candidate_chain_info(&non_finalized, db, &network, wrong_parent).is_none());

    let mut extreme_work = candidate.clone();
    let threshold = CompactDifficulty::from_bytes_in_display_order(&0x0101_0000_u32.to_be_bytes())
        .expect("target one has a valid compact encoding");
    assert!(threshold.to_expanded().is_some());
    assert!(threshold.to_work().is_none());
    Arc::make_mut(&mut Arc::make_mut(&mut extreme_work).header).difficulty_threshold = threshold;
    assert!(mining_candidate_chain_info(&non_finalized, db, &network, extreme_work).is_none());

    let mut forged = candidate.clone();
    let tx = Arc::make_mut(&mut forged)
        .transactions
        .iter_mut()
        .find(|tx| tx.version() == 5 && tx.has_shielded_data())
        .expect("the NU5 vector contains a shielded V5 transaction");
    let mut bytes = tx.zcash_serialize_to_vec().unwrap();
    // Without Orchard, its zero action count follows the Sapling binding signature.
    let signature_byte = bytes.len() - 1 - usize::from(tx.orchard_bundle().is_none());
    bytes[signature_byte] ^= 1;
    *tx = bytes.zcash_deserialize_into::<Arc<Transaction>>().unwrap();
    assert_eq!(forged.hash(), candidate.hash());
    assert_eq!(forged.header.merkle_root, candidate.header.merkle_root);
    assert!(
        forged
            .transactions
            .iter()
            .map(|tx| tx.hash())
            .eq(candidate.transactions.iter().map(|tx| tx.hash())),
        "only authorizing data changed, not transaction IDs",
    );
    assert_ne!(forged.auth_data_root(), candidate.auth_data_root());
    assert!(mining_candidate_chain_info(&non_finalized, db, &network, forged).is_none());

    // At Sapling activation the full Sapling frontier really is empty, so this pair
    // additionally exercises the pre-Heartwood final-root check without a synthetic root.
    let parent = zebra_test::vectors::BLOCK_MAINNET_419199_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    let candidate = zebra_test::vectors::BLOCK_MAINNET_419200_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    let (finalized, non_finalized, candidate) = mining_candidate_parent_state(&parent, candidate);
    assert!(mining_candidate_chain_info(
        &non_finalized,
        &finalized.db,
        &network,
        candidate.clone(),
    )
    .is_some());
    assert_ne!(*candidate.header.commitment_bytes, [0; 32]);
    let invalid_root = candidate.set_block_commitment([0; 32]);
    assert!(
        mining_candidate_chain_info(&non_finalized, &finalized.db, &network, invalid_root,)
            .is_none()
    );
}

/// Seed a real finalized parent with empty note trees and a full synthetic header window.
fn mining_candidate_parent_state(
    parent: &Arc<Block>,
    mut candidate: Arc<Block>,
) -> (
    crate::service::finalized_state::FinalizedState,
    NonFinalizedState,
    Arc<Block>,
) {
    use crate::{
        service::finalized_state::DiskWriteBatch,
        tests::{setup::new_state_with_mainnet_genesis, FakeChainHelper},
        WriteDisk,
    };
    use zebra_chain::{
        block::ChainHistoryBlockTxAuthCommitmentHash,
        primitives::zcash_history::BlockCommitmentTreeRoots,
    };

    let (finalized, non_finalized, _) = new_state_with_mainnet_genesis();
    let db = &finalized.db;
    let height = parent.coinbase_height().unwrap();
    let mut batch = DiskWriteBatch::new();
    batch.zs_insert(
        &db.db().cf_handle("hash_by_height").unwrap(),
        height,
        parent.hash(),
    );
    batch.zs_insert(
        &db.db().cf_handle("height_by_hash").unwrap(),
        parent.hash(),
        height,
    );
    // Missing historical headers would force powLimit and hide retargeting regressions.
    // Only older context is synthesized; the parent and candidate headers stay unchanged.
    for index in 0..MAX_POW_ADJUSTMENT_BLOCK_SPAN {
        let mut header = *parent.header;
        header.time -= chrono::Duration::seconds(i64::try_from(index).unwrap() * 75);
        batch.zs_insert(
            &db.db().cf_handle("block_header_by_height").unwrap(),
            (height - i64::try_from(index).unwrap()).unwrap(),
            header,
        );
    }
    db.write_batch(batch).unwrap();
    db.set_finalized_value_pool(zebra_chain::value_balance::ValueBalance::fake_populated_pool());
    if height
        >= NetworkUpgrade::Nu5
            .activation_height(&Network::Mainnet)
            .unwrap()
    {
        // NU5 vectors lack activation history peaks and full Sapling frontiers.
        // Bind the real transaction bodies to this sparse parent history.
        let trees = db.note_commitment_trees_for_tip();
        let history = HistoryTree::from_block(
            &Network::Mainnet,
            parent.clone(),
            BlockCommitmentTreeRoots {
                sapling: &trees.sapling.root(),
                orchard: &trees.orchard.root(),
                ironwood: &trees.ironwood.root(),
            },
        )
        .unwrap();
        let commitment = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
            &history.hash().unwrap(),
            &candidate.auth_data_root(),
        );
        candidate = candidate.set_block_commitment(commitment.into());
        let mut batch = DiskWriteBatch::new();
        batch.update_history_tree(db, &history);
        db.write_batch(batch).unwrap();
    }
    (finalized, non_finalized, candidate)
}

#[test]
fn nu7_template_times_match_difficulty_across_activation() {
    let _init_guard = zebra_test::init();
    const NU7_HEIGHT: u32 = 400_000;
    let network = Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(1),
            nu7: Some(NU7_HEIGHT),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .with_funding_streams(Vec::new())
        .with_slow_start_interval(Height(0))
        .to_network()
        .expect("configured Testnet parameters are valid");
    assert!(!network.is_regtest());

    let limit = network.target_difficulty_limit().to_compact();
    let threshold = (network.target_difficulty_limit() / 8_u64).to_compact();
    let context: Vec<_> = (0..MAX_POW_ADJUSTMENT_BLOCK_SPAN)
        .map(|index| {
            let time = PREV - u32::try_from(index).unwrap() * 75;
            (threshold, DateTime32::from(time).into())
        })
        .collect();

    for (candidate_height, gap, standard_gap) in [
        (NU7_HEIGHT - 1, 450, 450),
        (NU7_HEIGHT - 1, 451, 450),
        (NU7_HEIGHT, 450, 450),
        (NU7_HEIGHT, 451, 450),
        (NU7_HEIGHT + 1, 450, 450),
        (NU7_HEIGHT + 1, 451, 450),
    ] {
        let parent_height = Height(candidate_height - 1);
        let difficulty_at = |time: DateTime32| {
            AdjustedDifficulty::new_from_header_time(
                time.into(),
                parent_height,
                &network,
                context.iter().cloned(),
            )
            .expected_difficulty_threshold()
        };
        let standard = difficulty_at(DateTime32::from(PREV + standard_gap));
        assert_ne!(
            standard, limit,
            "ordinary retargeting must not produce powLimit"
        );

        let mut result = chain_info(PREV + gap);
        result.tip_height = parent_height;
        result.expected_difficulty = difficulty_at(result.cur_time);
        adjust_difficulty_and_time_for_testnet(
            &mut result,
            &network,
            parent_height,
            context.clone(),
        );

        assert_eq!(result.cur_time, DateTime32::from(PREV + gap));
        if gap <= standard_gap {
            assert_eq!(result.expected_difficulty, standard);
            assert_eq!(result.max_time, DateTime32::from(PREV + standard_gap));
        } else {
            assert_eq!(result.expected_difficulty, limit);
            assert_eq!(result.min_time, DateTime32::from(PREV + standard_gap + 1));
        }

        // Difficulty is constant on each side of the time threshold: checking
        // both endpoints verifies that no advertised timestamp crosses it.
        for time in [result.min_time, result.cur_time, result.max_time] {
            assert_eq!(
                result.expected_difficulty,
                difficulty_at(time),
                "candidate height {candidate_height}, initial gap {gap}, advertised time {time:?}"
            );
        }
    }
}

/// Templates below the Testnet time gate retain the context-free two-hour window.
#[test]
fn template_max_time_respects_network_height_gate() {
    let _init_guard = zebra_test::init();
    let custom_testnet = Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(1),
            nu7: Some(2),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .with_funding_streams(Vec::new())
        .with_slow_start_interval(Height(0))
        .to_network()
        .expect("configured Testnet parameters are valid");
    let now = DateTime32::from(PREV + 24 * 60 * 60);

    for (network, candidate_height, enforced) in [
        (custom_testnet, Height(100), false),
        (
            Network::new_default_testnet(),
            (TESTNET_MAX_TIME_START_HEIGHT - 1).unwrap(),
            false,
        ),
        (
            Network::new_default_testnet(),
            TESTNET_MAX_TIME_START_HEIGHT,
            true,
        ),
        (Network::Mainnet, Height(2_000_000), true),
        (Network::new_regtest(Default::default()), Height(100), false),
    ] {
        let context = template_block_context(&network, PREV);
        let result = difficulty_time_and_history_tree(
            context,
            (candidate_height - 1).unwrap(),
            Hash([0; 32]),
            &network,
            Arc::new(HistoryTree::default()),
            zebra_chain::value_balance::ValueBalance::zero(),
            now,
        )
        .expect("the template has a valid timestamp range");

        if enforced {
            let median = PREV - u32::try_from(POW_MEDIAN_BLOCK_SPAN / 2).unwrap() * 75;
            assert_eq!(
                result.max_time,
                DateTime32::from(median + BLOCK_MAX_TIME_SINCE_MEDIAN),
            );
        } else {
            assert_eq!(
                result.max_time,
                now.checked_add(Duration32::from_hours(2)).unwrap(),
                "candidate height {candidate_height:?}",
            );
        }
        assert!(result.min_time <= result.cur_time);
        assert!(result.cur_time <= result.max_time);
        if network.is_regtest() {
            assert_eq!(result.cur_time, result.min_time);
        }
    }
}

/// Future median times must not move the independent local-clock upper bound.
#[test]
fn template_times_respect_local_clock_bound() {
    let _init_guard = zebra_test::init();
    let now = DateTime32::from(PREV);
    let max_time = now.checked_add(Duration32::from_hours(2)).unwrap();
    let median_offset = u32::try_from(POW_MEDIAN_BLOCK_SPAN / 2).unwrap() * 75;
    let mut header = *zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block fixture must deserialize")
        .header;

    for (network, candidate_height) in [
        (Network::Mainnet, Height(2_000_000)),
        (Network::new_default_testnet(), Height(100)),
        (Network::new_regtest(Default::default()), Height(100)),
    ] {
        for future_median_seconds in [60 * 60, 2 * 60 * 60 - 1, 2 * 60 * 60] {
            let context =
                template_block_context(&network, PREV + future_median_seconds + median_offset);
            let result = difficulty_time_and_history_tree(
                context,
                (candidate_height - 1).unwrap(),
                Hash([0; 32]),
                &network,
                Arc::new(HistoryTree::default()),
                zebra_chain::value_balance::ValueBalance::zero(),
                now,
            );

            if future_median_seconds == 2 * 60 * 60 {
                assert!(
                    result.is_err(),
                    "no timestamp is valid until the clock advances"
                );
                continue;
            }

            let result = result.expect("there is a timestamp inside the local-clock bound");
            assert_eq!(result.max_time, max_time);
            assert_eq!(
                result.min_time,
                DateTime32::from(PREV + future_median_seconds + 1),
            );
            assert!(result.min_time <= result.cur_time);
            assert!(result.cur_time <= result.max_time);

            for time in [result.min_time, result.cur_time, result.max_time] {
                header.time = time.into();
                header
                    .time_is_valid_at(now.into(), &candidate_height, &Hash([0; 32]))
                    .expect("every advertised timestamp must pass context-free time validation");
            }
        }
    }
}

/// An unrepresentable minimum-difficulty threshold leaves the entire valid range standard.
#[test]
fn template_times_near_timestamp_ceiling_stay_standard_difficulty() {
    let _init_guard = zebra_test::init();
    let network = Network::new_default_testnet();
    let median_offset = u32::try_from(POW_MEDIAN_BLOCK_SPAN / 2).unwrap() * 75;

    // Overflow the parent-plus-gap sum, then isolate overflow of its strict successor.
    for previous_time in [u32::MAX - 100, u32::MAX - GAP] {
        let context = template_block_context(&network, previous_time);
        let difficulty_at = |time: DateTime32| {
            AdjustedDifficulty::new_from_header_time(
                time.into(),
                ACTIVE_HEIGHT,
                &network,
                context.iter().cloned(),
            )
            .expected_difficulty_threshold()
        };
        let now = DateTime32::from(previous_time);
        let standard = difficulty_at(now);
        assert_ne!(standard, network.target_difficulty_limit().to_compact());

        let result = difficulty_time_and_history_tree(
            context.clone(),
            ACTIVE_HEIGHT,
            Hash([0; 32]),
            &network,
            Arc::new(HistoryTree::default()),
            zebra_chain::value_balance::ValueBalance::zero(),
            now,
        )
        .expect("standard-difficulty timestamps remain representable");

        assert_eq!(
            result.min_time,
            DateTime32::from(previous_time - median_offset + 1),
        );
        assert_eq!(result.cur_time, now);
        assert_eq!(result.max_time, DateTime32::from(u32::MAX));
        assert_eq!(result.expected_difficulty, standard);

        for time in [result.min_time, result.cur_time, result.max_time] {
            assert_eq!(
                difficulty_at(time),
                result.expected_difficulty,
                "parent time {previous_time}, advertised time {time:?}",
            );
        }
    }
}

/// Header difficulties and times for template calculation, newest first.
fn template_block_context(
    network: &Network,
    previous_time: u32,
) -> Vec<(CompactDifficulty, DateTime<Utc>)> {
    let threshold = (network.target_difficulty_limit() / 8_u64).to_compact();
    (0..MAX_POW_ADJUSTMENT_BLOCK_SPAN)
        .map(|index| {
            let time = DateTime32::from(previous_time - u32::try_from(index).unwrap() * 75);
            (threshold, time.into())
        })
        .collect()
}
