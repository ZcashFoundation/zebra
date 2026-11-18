//! Commits to chains that a published snapshot shares.

use std::{sync::Arc, time::Instant};

use zebra_chain::{
    amount::NonNegative, block::Block, parameters::Network, value_balance::ValueBalance,
};

use crate::{
    arbitrary::Prepare,
    constants::MAX_BLOCK_REORG_HEIGHT,
    service::{finalized_state::FinalizedState, non_finalized_state::NonFinalizedState},
    tests::FakeChainHelper,
    Config,
};

use super::commit_phases::{samples_by_phase, PhaseRecorder};

/// Returns an empty Mainnet non-finalized state, a finalized state for it, and the block that
/// starts its chain.
fn mainnet_state() -> (NonFinalizedState, FinalizedState, Arc<Block>) {
    let network = Network::Mainnet;
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
        .expect("opening an ephemeral database should succeed");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());
    let root = Arc::new(
        network
            .test_block(653599, 583999)
            .expect("the test vectors have this block"),
    );
    (NonFinalizedState::new(&network), finalized_state, root)
}

/// Commits and finalizations copy the parts of the chains that they change, so a published
/// snapshot that shares those chains keeps the state it was published with.
#[test]
fn published_snapshot_is_unchanged_by_later_commits_and_finalization() {
    let _init_guard = zebra_test::init();

    let (mut state, finalized_state, root) = mainnet_state();
    let mut blocks = vec![root];
    for _ in 0..3 {
        let child = blocks
            .last()
            .expect("blocks is not empty")
            .make_fake_child();
        blocks.push(child);
    }
    let prepared: Vec<_> = blocks.iter().map(|block| block.clone().prepare()).collect();

    // The same blocks committed to a state that nothing else shares.
    let commit_all = |state: &mut NonFinalizedState| {
        state
            .commit_new_chain(prepared[0].clone(), &finalized_state)
            .expect("the root block is valid");
        for block in &prepared[1..] {
            state
                .commit_block(block.clone(), &finalized_state)
                .expect("fake children are valid");
        }
    };
    commit_all(&mut state);
    let mut expected = NonFinalizedState::new(&Network::Mainnet);
    commit_all(&mut expected);

    let published = state.clone();

    // Extend the best chain, fork it below its tip, and finalize the root of both chains.
    let tip = blocks[3].make_fake_child();
    let fork = blocks[2].make_fake_child().set_block_commitment([1; 32]);
    for block in [tip, fork] {
        state
            .commit_block(block.prepare(), &finalized_state)
            .expect("fake children are valid");
    }
    assert_eq!(state.chain_count(), 2);
    state.finalize();

    assert!(
        published.eq_internal_state(&expected),
        "later commits and finalization must not change a published snapshot",
    );
}

/// Returns the median of `values` in milliseconds.
fn median_millis(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2] * 1e3
}

/// Commits fake Mainnet blocks until the best chain holds a full reorg window, then times more
/// commits, each made while the previous commit's snapshot is still published, as the block
/// write task does. The first half of the timed commits run back to back, like a sync. The
/// second half evict the CPU caches before each commit, like the idle time between Mainnet
/// blocks. Prints the medians of each half.
///
/// Run it in release mode with `--run-ignored=only --no-capture`.
#[test]
#[ignore]
#[allow(clippy::print_stdout)]
fn bench_commit_while_snapshot_is_published() {
    const TIMED_COMMITS: u32 = 1000;
    // More than the L3 cache of the hosts that run this.
    const CACHE_EVICTION_BYTES: usize = 512 << 20;

    let _init_guard = zebra_test::init();

    let (mut state, finalized_state, mut tip) = mainnet_state();
    state
        .commit_new_chain(tip.clone().prepare(), &finalized_state)
        .expect("the root block is valid");
    let mut published = state.clone();
    let mut eviction = vec![0_u8; CACHE_EVICTION_BYTES];

    let (recorder, samples) = PhaseRecorder::new();
    // The commit, publish and finalize seconds of each timed commit.
    let mut timings = Vec::new();
    for index in 0..MAX_BLOCK_REORG_HEIGHT + TIMED_COMMITS {
        let child = tip.make_fake_child();
        let prepared = child.clone().prepare();
        if index >= MAX_BLOCK_REORG_HEIGHT + TIMED_COMMITS / 2 {
            for byte in eviction.iter_mut().step_by(64) {
                *byte = byte.wrapping_add(1);
            }
            std::hint::black_box(&eviction);
        }

        let start = Instant::now();
        metrics::with_local_recorder(&recorder, || state.commit_block(prepared, &finalized_state))
            .expect("fake children are valid");
        let commit = start.elapsed();

        // Publishing a snapshot drops the previous one, which held the parent chain.
        let start = Instant::now();
        published = state.clone();
        let publish = start.elapsed();

        let start = Instant::now();
        while state.should_finalize_root() {
            state.finalize();
        }
        let finalize = start.elapsed();

        if index >= MAX_BLOCK_REORG_HEIGHT {
            timings.push([commit, publish, finalize].map(|duration| duration.as_secs_f64()));
        }
        tip = child;
    }

    let chain_clone = samples_by_phase(&samples)
        .remove("chain_clone")
        .expect("commits record their chain clone");
    let chain_clone = chain_clone[chain_clone.len() - timings.len()..].to_vec();
    let half = timings.len() / 2;
    println!(
        "{} commits to a {}-block chain, medians in ms:",
        timings.len(),
        published.best_chain_len().unwrap_or(0),
    );
    println!("  {:<12} {:>8} {:>8}", "", "hot", "cold");
    for (name, values) in [
        ("chain_clone", chain_clone),
        ("commit", timings.iter().map(|timing| timing[0]).collect()),
        ("publish", timings.iter().map(|timing| timing[1]).collect()),
        ("finalize", timings.iter().map(|timing| timing[2]).collect()),
    ] {
        println!(
            "  {name:<12} {:>8.4} {:>8.4}",
            median_millis(&values[..half]),
            median_millis(&values[half..]),
        );
    }
}
