//! The tree-first commit gate: re-derives the tree-first commit model from the commit phase timers.

use std::time::Instant;

use proptest::{
    strategy::{Strategy, ValueTree},
    test_runner::TestRunner,
};

use zebra_chain::{amount::NonNegative, value_balance::ValueBalance};

use crate::{
    service::{
        arbitrary::PreparedChain, finalized_state::FinalizedState,
        non_finalized_state::NonFinalizedState,
    },
    Config,
};

use super::commit_phases::{samples_by_phase, PhaseRecorder, COMMIT_PHASES};

/// The smallest modelled median saving that justifies starting the trees first.
const GATE_MILLIS: f64 = 0.5;

/// Returns the median of `values`, or `NaN` if there are none.
fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted.get(sorted.len() / 2).copied().unwrap_or(f64::NAN)
}

/// Commits a generated chain, then prints the per-block model of starting the note commitment
/// trees before the rest of the commit, from the commit phase timers.
///
/// A commit in series runs `transparent_spend` and `anchor_reads`, then waits for a rayon thread to
/// start the job (`pickup`) that runs `chain_clone`, then the trees in parallel with
/// `chain_updates`, then `history`. Started first, the trees run in parallel with the reads, the
/// pickup, the clone and the chain updates, and `history` follows both:
///
/// - sequential: `transparent_spend + anchor_reads + pickup + chain_clone
///   + max(trees, chain_updates) + history`
/// - tree first: `max(trees, transparent_spend + anchor_reads + pickup + chain_clone
///   + chain_updates) + history`
///
/// This test only asserts that every phase is recorded once per block. Debug builds and generated
/// blocks don't have Mainnet timings, so a Mainnet block replay decides whether the model's
/// saving passes the gate. Run it with `--no-capture` to see the model.
#[test]
#[allow(clippy::print_stdout, reason = "print the model for the gate")]
fn tree_first_model_from_commit_phase_timers() {
    let _init_guard = zebra_test::init();

    let (chain, _count, network, _history_tree) = PreparedChain::default()
        .new_tree(&mut TestRunner::deterministic())
        .expect("the prepared chain strategy generates a chain")
        .current();

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    let (recorder, samples) = PhaseRecorder::new();
    let mut commit_millis = Vec::new();
    let mut shielded = Vec::new();

    // The generated genesis block is the finalized tip's stand-in, so it isn't committed.
    for (index, block) in chain.iter().skip(1).cloned().enumerate() {
        shielded.push(
            block.block.sapling_note_commitments().next().is_some()
                || block.block.orchard_note_commitments().next().is_some(),
        );

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block.block);

        let start = Instant::now();
        let result = metrics::with_local_recorder(&recorder, || {
            if index == 0 {
                state.commit_new_chain(block, &finalized_state)
            } else {
                state.commit_block(block, &finalized_state)
            }
        });
        commit_millis.push(start.elapsed().as_secs_f64() * 1e3);
        result.expect("the prepared chain generates valid blocks");
    }

    let recorded = samples_by_phase(&samples);
    let millis = |phase: &str| -> Vec<f64> {
        recorded
            .get(phase)
            .unwrap_or_else(|| panic!("phase {phase} was recorded: {recorded:?}"))
            .iter()
            .map(|seconds| seconds * 1e3)
            .collect()
    };

    let blocks = commit_millis.len();
    for phase in COMMIT_PHASES {
        assert_eq!(
            millis(phase).len(),
            blocks,
            "phase {phase} must be recorded once per committed block",
        );
    }

    let [transparent_spend, anchor_reads, pickup, chain_clone, trees, chain_updates, history] =
        COMMIT_PHASES.map(millis);

    let mut sequential = Vec::with_capacity(blocks);
    let mut tree_first = Vec::with_capacity(blocks);
    let mut saving = Vec::with_capacity(blocks);
    for i in 0..blocks {
        let non_tree = transparent_spend[i] + anchor_reads[i] + pickup[i] + chain_clone[i];
        let today = non_tree + trees[i].max(chain_updates[i]) + history[i];
        let first = trees[i].max(non_tree + chain_updates[i]) + history[i];
        sequential.push(today);
        tree_first.push(first);
        saving.push(today - first);
    }

    // The medians of `values` over all blocks, the shielded blocks, and the other blocks.
    let strata = |values: &[f64]| -> [f64; 3] {
        let subset = |is_shielded: Option<bool>| -> Vec<f64> {
            values
                .iter()
                .zip(&shielded)
                .filter(|(_, block_is_shielded)| {
                    is_shielded.is_none_or(|is_shielded| **block_is_shielded == is_shielded)
                })
                .map(|(value, _)| *value)
                .collect()
        };
        [None, Some(true), Some(false)].map(|is_shielded| median(&subset(is_shielded)))
    };

    let shielded_blocks = shielded.iter().filter(|is_shielded| **is_shielded).count();
    println!(
        "tree-first commit model on {blocks} generated {network} blocks, \
         medians in ms (debug build, not Mainnet timings):"
    );
    println!(
        "  {:<20} {:>8} {:>8} {:>8}",
        "",
        format!("all {blocks}"),
        format!("sh. {shielded_blocks}"),
        format!("none {}", blocks - shielded_blocks),
    );
    for (name, values) in COMMIT_PHASES.iter().copied().zip([
        &transparent_spend,
        &anchor_reads,
        &pickup,
        &chain_clone,
        &trees,
        &chain_updates,
        &history,
    ]) {
        let [all, shielded, none] = strata(values);
        println!("  {name:<20} {all:>8.3} {shielded:>8.3} {none:>8.3}");
    }
    for (name, values) in [
        ("measured commit", &commit_millis),
        ("modelled sequential", &sequential),
        ("modelled tree-first", &tree_first),
        ("modelled saving", &saving),
    ] {
        let [all, shielded, none] = strata(values);
        println!("  {name:<20} {all:>8.3} {shielded:>8.3} {none:>8.3}");
    }
    println!(
        "  (gate: a modelled median saving of at least {GATE_MILLIS} ms on the Mainnet replay)"
    );
}
