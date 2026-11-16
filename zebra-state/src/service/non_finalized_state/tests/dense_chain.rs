//! Memory tests for a single Mainnet chain of transparent-output-dense blocks.

use std::{path::PathBuf, sync::Arc, time::Instant};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{Block, MAX_BLOCK_BYTES},
    parameters::Network,
    serialization::ZcashSerialize,
    transparent,
    value_balance::ValueBalance,
};
use zebra_test::prelude::*;

use crate::{
    arbitrary::Prepare,
    constants::{MAX_NON_FINALIZED_INDEX_ENTRIES, MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT},
    service::{finalized_state::FinalizedState, non_finalized_state::NonFinalizedState},
    tests::FakeChainHelper,
    Config,
};

/// The P2SH outputs added to each dense block. With the coinbase of the template block, they
/// fill the block to just under [`MAX_BLOCK_BYTES`].
const DENSE_BLOCK_OUTPUTS: u32 = 61_800;

/// The dense blocks committed when `ZEBRA_TEST_DENSE_BLOCKS` is not set.
///
/// CI runs this size. Set `ZEBRA_TEST_DENSE_BLOCKS=60` for the size that killed an 8 GiB node
/// (GHSA-j8gf-c4rj-6jv5), or more than 100 to pass the index entry limit.
const DEFAULT_DENSE_BLOCKS: u32 = 10;

/// The most resident memory the non-finalized state can use for each transparent output in a
/// single chain, at its peak, in bytes.
///
/// The peak includes the snapshot of the state that the block write task publishes after each
/// commit. Chains that copy their output indexes for that snapshot use more than twice this
/// limit.
const MAX_PEAK_BYTES_PER_OUTPUT: u64 = 1_500;

/// Returns a field of `/proc/self/status` in bytes, or `None` if it is not available.
fn process_status_bytes(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kib: u64 = status
        .lines()
        .find(|line| line.starts_with(field))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kib * 1024)
}

/// Returns the resident set size of this process in bytes.
fn resident_bytes() -> Option<u64> {
    process_status_bytes("VmRSS:")
}

/// Returns the peak resident set size of this process in bytes.
fn peak_resident_bytes() -> Option<u64> {
    process_status_bytes("VmHWM:")
}

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

/// Returns a fake child of `parent` whose coinbase also pays [`DENSE_BLOCK_OUTPUTS`] zero-value
/// outputs to P2SH addresses that no other output in the chain uses.
fn dense_child(parent: &Arc<Block>, network: &Network, block_index: u32) -> Arc<Block> {
    let mut block = Block::clone(&parent.make_fake_child());
    let coinbase = Arc::unwrap_or_clone(block.transactions.remove(0));
    let mut outputs = coinbase.outputs().to_vec();
    outputs.truncate(1);
    outputs.extend((0..DENSE_BLOCK_OUTPUTS).map(|i| {
        let mut script_hash = [0; 20];
        script_hash[..4].copy_from_slice(&i.to_le_bytes());
        script_hash[4..8].copy_from_slice(&block_index.to_le_bytes());
        transparent::Output {
            value: Amount::zero(),
            lock_script: transparent::Address::from_script_hash(network.kind(), script_hash)
                .script(),
        }
    }));
    block
        .transactions
        .insert(0, Arc::new(coinbase.with_transparent_outputs(outputs)));
    Arc::new(block)
}

/// Returns an empty Mainnet non-finalized state, a finalized state for it, and the block that
/// starts its chain.
fn mainnet_state() -> (NonFinalizedState, FinalizedState, Arc<Block>) {
    let network = Network::Mainnet;
    let state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
        .expect("opening an ephemeral database should succeed");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());
    let root: Arc<Block> = Arc::new(
        network
            .test_block(653599, 583999)
            .expect("the test vectors have this block"),
    );
    (state, finalized_state, root)
}

/// Commits `blocks` dense blocks to a single chain in `state`. After each commit, it drops the
/// blocks that the block write task would finalize, and publishes a snapshot of the state, as
/// that task does. Returns the last snapshot.
#[allow(clippy::print_stdout)]
fn commit_dense_chain(
    state: &mut NonFinalizedState,
    finalized_state: &FinalizedState,
    root: Arc<Block>,
    blocks: u32,
    start: u64,
) -> Result<NonFinalizedState> {
    let network = state.network.clone();
    state.commit_new_chain(root.clone().prepare(), finalized_state)?;
    let mut published = state.clone();

    let mut tip = root;
    for block_index in 0..blocks {
        let child = dense_child(&tip, &network, block_index);
        if block_index == 0 {
            let size = child.zcash_serialize_to_vec()?.len();
            assert!(
                u64::try_from(size)? <= MAX_BLOCK_BYTES,
                "dense blocks must be valid: {size} bytes"
            );
            println!("dense block size {size} bytes");
        }

        state.commit_block(child.clone().prepare(), finalized_state)?;
        while state.should_finalize_root() {
            state.finalize();
        }
        // Sending a snapshot to a watch channel drops the previous one after the new one exists.
        published = state.clone();
        tip = child;

        if (block_index + 1) % 10 == 0 {
            println!(
                "blocks {}, kept {}, index entries {}: RSS MiB {}, peak {}, above start {}",
                block_index + 1,
                state.best_chain_len().unwrap_or(0),
                state.index_entry_count(),
                mib(resident_bytes().unwrap_or(0)),
                mib(peak_resident_bytes().unwrap_or(0)),
                mib(resident_bytes().unwrap_or(0).saturating_sub(start)),
            );
        }
    }

    Ok(published)
}

/// Regression test for GHSA-j8gf-c4rj-6jv5 on a single chain.
///
/// One Mainnet chain of blocks filled with P2SH outputs to distinct addresses, with no side
/// chains, must not use more than [`MAX_PEAK_BYTES_PER_OUTPUT`] of resident memory per output.
/// Past the index entry limit, it must stop growing, and keep at least
/// [`MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT`] blocks. `ZEBRA_TEST_DENSE_BLOCKS` sets the number
/// of dense blocks.
#[test]
#[allow(clippy::print_stdout)]
fn dense_single_chain_memory_stays_within_limit() -> Result<()> {
    let _init_guard = zebra_test::init();

    let blocks: u32 = env_or("ZEBRA_TEST_DENSE_BLOCKS", DEFAULT_DENSE_BLOCKS);
    // Finalizing early stops just under the limit, or one block over it at the minimum length.
    let outputs = (u64::from(blocks) * u64::from(DENSE_BLOCK_OUTPUTS))
        .min(u64::try_from(MAX_NON_FINALIZED_INDEX_ENTRIES)? + 2 * u64::from(DENSE_BLOCK_OUTPUTS));

    let (mut state, finalized_state, root) = mainnet_state();
    let start = resident_bytes().unwrap_or(0);

    let published = commit_dense_chain(&mut state, &finalized_state, root, blocks, start)?;
    assert_eq!(1, state.chain_count());
    let kept = state.best_chain_len().expect("the chain was committed");
    assert!(
        kept == blocks + 1 || kept >= MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT,
        "finalizing early keeps the minimum number of blocks: kept {kept} of {blocks}"
    );
    assert!(
        state.index_entry_count()
            <= MAX_NON_FINALIZED_INDEX_ENTRIES + usize::try_from(2 * DENSE_BLOCK_OUTPUTS)?,
        "the chain stays near the index entry limit: {} entries in {kept} blocks",
        state.index_entry_count()
    );

    let Some(peak) = peak_resident_bytes() else {
        println!("Skipping the memory limit: this platform has no '/proc/self/status'.");
        return Ok(());
    };
    let peak_above_start = peak.saturating_sub(start);
    let resting = resident_bytes().unwrap_or(0).saturating_sub(start);
    println!(
        "dense blocks {blocks}, kept {kept}, outputs counted {outputs}: peak RSS MiB {}, start {}, \
         peak bytes per output {}, resting bytes per output {}",
        mib(peak),
        mib(start),
        peak_above_start / outputs.max(1),
        resting / outputs.max(1),
    );
    drop(published);

    assert!(
        peak_above_start <= outputs * MAX_PEAK_BYTES_PER_OUTPUT,
        "a single chain with {outputs} transparent outputs used {} MiB at its peak, \
         over the limit of {} MiB ({MAX_PEAK_BYTES_PER_OUTPUT} bytes per output)",
        mib(peak_above_start),
        mib(outputs * MAX_PEAK_BYTES_PER_OUTPUT),
    );

    Ok(())
}

/// Measures the memory used to restore a single dense chain from the non-finalized backup.
///
/// Run it twice with `ZEBRA_TEST_DENSE_BACKUP_DIR` set to the same directory, and
/// `--run-ignored=only --no-capture`. When the directory is empty, the test commits
/// `ZEBRA_TEST_DENSE_BLOCKS` dense blocks and writes the backup. Otherwise it restores the
/// backup in this new process, as a restarted node does.
#[test]
#[ignore]
#[allow(clippy::print_stdout)]
fn measure_dense_chain_restart() -> Result<()> {
    let _init_guard = zebra_test::init();

    let Some(backup_dir) = std::env::var_os("ZEBRA_TEST_DENSE_BACKUP_DIR").map(PathBuf::from)
    else {
        println!("Skipping: '$ZEBRA_TEST_DENSE_BACKUP_DIR' is not set.");
        return Ok(());
    };
    std::fs::create_dir_all(&backup_dir)?;

    let (mut state, finalized_state, root) = mainnet_state();
    let start = resident_bytes().unwrap_or(0);

    if std::fs::read_dir(&backup_dir)?.next().is_none() {
        let blocks: u32 = env_or("ZEBRA_TEST_DENSE_BLOCKS", DEFAULT_DENSE_BLOCKS);
        let _published = commit_dense_chain(&mut state, &finalized_state, root, blocks, start)?;
        state.write_to_backup(&backup_dir);
        println!(
            "wrote {} backup files to {backup_dir:?}",
            std::fs::read_dir(&backup_dir)?.count()
        );
        return Ok(());
    }

    let timer = Instant::now();
    let restored = super::super::backup::restore_backup(state, &backup_dir, &finalized_state.db);
    // The restored state is published to the watch channel before the node starts syncing.
    let _published = restored.clone();
    let elapsed = timer.elapsed();

    let blocks = restored.best_chain().map_or(0, |chain| chain.len());
    println!(
        "restored {blocks} blocks in {} chains in {elapsed:?}: RSS MiB {}, peak {}, start {}",
        restored.chain_count(),
        mib(resident_bytes().unwrap_or(0)),
        mib(peak_resident_bytes().unwrap_or(0)),
        mib(start),
    );

    Ok(())
}
