//! Resource-use tests for `spent_outputs_for_block` (`getblock` verbosity 3).
//!
//! These build fake finalized chains whose shape makes the spent-output lookup expensive, and
//! measure the heap use of the lookup on the calling thread.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::Arc,
};

use zebra_chain::{
    amount::Amount,
    block::{Block, Height},
    parameters::Network::Mainnet,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::Transaction,
    transparent::{Input, OutPoint, Output, Script},
};

use crate::{
    service::{
        finalized_state::FinalizedState, non_finalized_state::Chain, read::spent_outputs_for_block,
    },
    tests::FakeChainHelper,
    CheckpointVerifiedBlock, Config,
};

/// Counts heap use per thread, so tests running in parallel don't pollute each other.
struct CountingAlloc;

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static CURRENT: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
    static TOTAL: Cell<usize> = const { Cell::new(0) };
}

fn record(delta: isize) {
    let _ = TRACKING.try_with(|tracking| {
        if !tracking.get() {
            return;
        }
        CURRENT.with(|current| {
            let now = current.get() + delta;
            current.set(now);
            PEAK.with(|peak| peak.set(peak.get().max(now)));
        });
        if delta > 0 {
            TOTAL.with(|total| total.set(total.get() + delta as usize));
        }
    });
}

// Test-only: forwards every call to `System` and only adds bookkeeping.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            record(layout.size() as isize);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() {
            record(layout.size() as isize);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        record(-(layout.size() as isize));
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = System.realloc(ptr, layout, new_size);
        if !new_ptr.is_null() {
            record(new_size as isize - layout.size() as isize);
        }
        new_ptr
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// Heap use of one measured call on this thread.
struct HeapUse {
    peak: usize,
    total: usize,
}

fn measure<T>(f: impl FnOnce() -> T) -> (T, HeapUse) {
    CURRENT.with(|c| c.set(0));
    PEAK.with(|p| p.set(0));
    TOTAL.with(|t| t.set(0));
    TRACKING.with(|t| t.set(true));
    let result = f();
    TRACKING.with(|t| t.set(false));
    let heap = HeapUse {
        peak: PEAK.with(|p| p.get()).max(0) as usize,
        total: TOTAL.with(|t| t.get()),
    };
    (result, heap)
}

/// Returns `block` with its coinbase outputs replaced by `outputs`.
fn with_coinbase_outputs(block: Arc<Block>, outputs: Vec<Output>) -> Arc<Block> {
    let mut block = Block::clone(&block);
    let coinbase = Transaction::clone(&block.transactions[0]).with_transparent_outputs(outputs);
    block.transactions[0] = Arc::new(coinbase);
    Arc::new(block)
}

/// A transaction spending `outpoints`, with no outputs.
fn spending_tx(
    template: &Transaction,
    outpoints: impl IntoIterator<Item = OutPoint>,
) -> Transaction {
    let inputs = outpoints
        .into_iter()
        .map(|outpoint| Input::PrevOut {
            outpoint,
            unlock_script: Script::new(&[]),
            sequence: u32::MAX,
        })
        .collect();
    template
        .clone()
        .with_transparent_inputs(inputs)
        .with_transparent_outputs(Vec::new())
}

fn small_output() -> Output {
    // A P2PKH-sized lock script.
    Output::new(Amount::try_from(1).unwrap(), Script::new(&[0x76; 25]))
}

/// Commits `parents` (children of genesis, in order) and then a block spending `spends` from
/// them, and returns the finalized state and the spending block.
fn commit_chain(
    parent_outputs: Vec<Vec<Output>>,
    spends: impl Fn(&[Arc<Block>]) -> Vec<OutPoint>,
) -> (FinalizedState, Vec<Arc<Block>>, Arc<Block>) {
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("block should deserialize");

    let mut state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        &Mainnet,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .expect("opening an ephemeral database should succeed");

    let mut tip = genesis.clone();
    let mut commit = |block: Arc<Block>| {
        state
            .commit_finalized_direct(CheckpointVerifiedBlock::from(block).into(), None, "test")
            .expect("fake block should commit");
    };
    commit(genesis.clone());

    let mut parents = Vec::new();
    for outputs in parent_outputs {
        let parent = with_coinbase_outputs(tip.make_fake_child(), outputs);
        commit(parent.clone());
        parents.push(parent.clone());
        tip = parent;
    }

    let spend = spending_tx(&genesis.transactions[0], spends(&parents));
    let mut spender = Block::clone(&with_coinbase_outputs(
        tip.make_fake_child(),
        vec![small_output()],
    ));
    spender.transactions.push(Arc::new(spend));
    let spender = Arc::new(spender);
    commit(spender.clone());

    (state, parents, spender)
}

/// The resolved [`Utxo`] must carry the spent output's own value, its parent's creation height,
/// and the parent's coinbase flag — not another output's value, nor the spending block's height.
///
/// The spent output is deleted from the finalized UTXO set when the spending block commits, so
/// this exercises the parent-transaction fallback that reconstructs the fields from the parent.
#[test]
fn spent_outputs_resolves_value_height_and_generated() {
    let _init_guard = zebra_test::init();

    // Three outputs with distinct values, so resolving the wrong output index would surface as a
    // wrong value rather than passing silently.
    let spent_value = Amount::try_from(7).expect("valid amount");
    let parent_outputs = vec![vec![
        Output::new(
            Amount::try_from(1).expect("valid amount"),
            Script::new(&[0x76; 25]),
        ),
        Output::new(spent_value, Script::new(&[0x76; 25])),
        Output::new(
            Amount::try_from(3).expect("valid amount"),
            Script::new(&[0x76; 25]),
        ),
    ]];

    // Spend output index 1 of the single parent's coinbase transaction.
    let spent_outpoint = |parents: &[Arc<Block>]| OutPoint {
        hash: parents[0].transactions[0].hash(),
        index: 1,
    };
    let (state, parents, _spender) =
        commit_chain(parent_outputs, |parents| vec![spent_outpoint(parents)]);

    // The single parent commits at height 1 (genesis is 0), and the spender at height 2.
    let spent = spent_outputs_for_block(None::<Arc<Chain>>, &state.db, Height(2).into())
        .expect("block is in the best chain");

    let utxo = spent
        .get(&spent_outpoint(&parents))
        .expect("the spent output is resolved");

    assert_eq!(
        utxo.output.value(),
        spent_value,
        "the resolved output is the one the input spends (index 1), not another index",
    );
    assert_eq!(
        utxo.height,
        Height(1),
        "height is the parent's creation height, not the spending block's height",
    );
    assert!(
        utxo.from_coinbase,
        "the spent output was created by a coinbase transaction",
    );
}

/// Every input spends a small output of a distinct large parent. The lookup must not keep all
/// the parents in memory at once: peak heap should stay near one parent, not all of them.
#[test]
fn spent_outputs_memory_does_not_hold_every_parent() {
    let _init_guard = zebra_test::init();

    const PARENTS: usize = 200;
    const PADDING: usize = 100_000;

    // Output 0 is small and gets spent; output 1 is large padding that is never spent.
    let parent_outputs = (0..PARENTS)
        .map(|_| {
            vec![
                small_output(),
                Output::new(
                    Amount::try_from(1).unwrap(),
                    Script::new(&vec![0x6a; PADDING]),
                ),
            ]
        })
        .collect();
    let (state, _parents, _spender) = commit_chain(parent_outputs, |parents| {
        parents
            .iter()
            .map(|parent| OutPoint {
                hash: parent.transactions[0].hash(),
                index: 0,
            })
            .collect()
    });

    let (spent, heap) = measure(|| {
        spent_outputs_for_block(
            None::<Arc<Chain>>,
            &state.db,
            Height(PARENTS as u32 + 1).into(),
        )
    });

    let spent = spent.expect("block is in the best chain");
    assert_eq!(spent.len(), PARENTS, "every spent output is resolved");

    // Holding every parent would peak around PARENTS * PADDING (20 MB).
    assert!(
        heap.peak < 10 * PADDING,
        "peak heap {} bytes should stay within a few parents ({PADDING} bytes each)",
        heap.peak,
    );
}

/// Every input spends a different output of the same parent. The work should be linear in the
/// inputs and the parent's outputs, not their product.
#[test]
fn spent_outputs_work_is_linear_for_a_shared_parent() {
    let _init_guard = zebra_test::init();

    const OUTPUTS: usize = 2_000;

    let parent_outputs = vec![(0..OUTPUTS).map(|_| small_output()).collect()];
    let (state, parents, _spender) = commit_chain(parent_outputs, |parents| {
        let hash = parents[0].transactions[0].hash();
        (0..OUTPUTS as u32)
            .map(|index| OutPoint { hash, index })
            .collect()
    });

    let (spent, heap) =
        measure(|| spent_outputs_for_block(None::<Arc<Chain>>, &state.db, Height(2).into()));

    let spent = spent.expect("block is in the best chain");
    assert_eq!(spent.len(), OUTPUTS, "every spent output is resolved");

    // Converting the parent's outputs once per input would allocate ~OUTPUTS times the parent.
    let parent_size = parents[0].transactions[0].zcash_serialized_size();
    assert!(
        heap.total < 50 * parent_size,
        "total allocated {} bytes should be a small multiple of the parent ({parent_size} bytes)",
        heap.total,
    );
}
