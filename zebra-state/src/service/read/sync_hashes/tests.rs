//! Tests for the `get-hashes` margin, truncation and output counts.

use super::*;

/// A distinct hash for each height.
fn hash_at(height: Height) -> Option<block::Hash> {
    let mut bytes = [0; 32];
    bytes[..4].copy_from_slice(&height.0.to_le_bytes());
    bytes[31] = 0xff;
    Some(block::Hash(bytes))
}

/// The metadata of a chain whose block at height `h` has `h + 1` outputs
/// (genesis has one), for the heights in `heights`.
fn counts(heights: RangeInclusive<u32>) -> Vec<BlockCounts> {
    heights
        .map(|h| BlockCounts {
            size: 1_000 + h,
            txs: 1 + h % 3,
            notes: 2 * h,
            // 1 + 2 + … + (h + 1).
            cumulative_txouts: u64::from(h + 1) * u64::from(h + 2) / 2,
        })
        .collect()
}

fn range(start: u32, end: u32) -> Option<RangeInclusive<Height>> {
    Some(Height(start)..=Height(end))
}

#[test]
fn margin_ends_the_range_below_the_tip() {
    let _init_guard = zebra_test::init();
    let margin = MAX_BLOCK_REORG_HEIGHT;

    // No chain, or a chain no longer than the margin, serves nothing.
    assert_eq!(served_range(None, Height(0), 10), None);
    assert_eq!(served_range(Some(Height(margin - 1)), Height(0), 10), None);

    // The first servable height is genesis, at a tip exactly the margin above it.
    assert_eq!(
        served_range(Some(Height(margin)), Height(0), 10),
        range(0, 0)
    );

    // A request that ends below the margin is served in full.
    let tip = Height(margin + 5_000);
    assert_eq!(served_range(Some(tip), Height(100), 50), range(100, 149));

    // A request that crosses the margin is cut there, as a prefix.
    assert_eq!(
        served_range(Some(tip), Height(4_990), 50),
        range(4_990, 5_000)
    );

    // A request that starts above the margin, or asks for nothing, serves nothing.
    assert_eq!(served_range(Some(tip), Height(5_001), 50), None);
    assert_eq!(served_range(Some(tip), Height(100), 0), None);
}

#[test]
fn margin_arithmetic_cannot_overflow() {
    let _init_guard = zebra_test::init();

    let tip = Height(u32::MAX);
    let max_served = u32::MAX - MAX_BLOCK_REORG_HEIGHT;
    assert_eq!(
        served_range(Some(tip), Height(max_served - 1), u32::MAX),
        range(max_served - 1, max_served)
    );
    assert_eq!(served_range(Some(tip), Height(u32::MAX), 50_000), None);
}

#[test]
fn entries_describe_each_height() {
    let _init_guard = zebra_test::init();

    // From genesis: genesis's output count is its cumulative count.
    let served = entries(range(0, 3).unwrap(), false, counts(0..=3), hash_at);
    assert_eq!(served.len(), 4);
    for (k, entry) in served.iter().enumerate() {
        let h = u32::try_from(k).expect("a small index");
        assert_eq!(
            Some(entry.hash),
            hash_at(Height(h)),
            "entry k is height start + k"
        );
        assert_eq!(entry.size, u64::from(1_000 + h));
        assert_eq!(entry.txs, u64::from(1 + h % 3));
        assert_eq!(entry.notes, u64::from(2 * h));
        assert_eq!(
            entry.txouts,
            u64::from(h + 1),
            "txouts = cum[h] - cum[h - 1]"
        );
    }

    // From a later height: the scan's first row is the height below, used only for its count.
    let served = entries(range(10, 12).unwrap(), true, counts(9..=12), hash_at);
    assert_eq!(served.len(), 3);
    assert_eq!(Some(served[0].hash), hash_at(Height(10)));
    assert_eq!(
        served.iter().map(|entry| entry.txouts).collect::<Vec<_>>(),
        [11, 12, 13]
    );
}

#[test]
fn missing_metadata_truncates_to_a_prefix() {
    let _init_guard = zebra_test::init();

    // The upgrade has reached part of the range: the entries stop at the first missing height.
    let served = entries(range(10, 20).unwrap(), true, counts(9..=14), hash_at);
    assert_eq!(served.len(), 5);
    assert_eq!(Some(served[4].hash), hash_at(Height(14)));

    // Only the height below the range: nothing is known about the range itself.
    assert!(entries(range(10, 20).unwrap(), true, counts(9..=9), hash_at).is_empty());

    // Not even the height below the range: no output count can be computed.
    assert!(entries(range(10, 20).unwrap(), true, Vec::new(), hash_at).is_empty());
    assert!(entries(range(0, 20).unwrap(), false, Vec::new(), hash_at).is_empty());
}

#[test]
fn missing_hashes_truncate_to_a_prefix() {
    let _init_guard = zebra_test::init();

    // The finalized state does not have height 13 yet.
    let hash = |height: Height| (height.0 < 13).then(|| hash_at(height)).flatten();
    let served = entries(range(10, 20).unwrap(), true, counts(9..=20), hash);
    assert_eq!(served.len(), 3);
}

#[test]
fn decreasing_output_counts_truncate_to_a_prefix() {
    let _init_guard = zebra_test::init();

    let mut corrupt = counts(9..=20);
    corrupt[3].cumulative_txouts = 0;
    let served = entries(range(10, 20).unwrap(), true, corrupt, hash_at);
    assert_eq!(served.len(), 2, "heights 10 and 11 precede the corrupt row");
}

/// `sync_hashes` reads its entries from a finalized state: the hashes from the
/// hash index, the output counts from the cumulative sync metadata (with the
/// scan starting one height below `start`), and nothing above the margin
/// below the tip.
#[test]
fn sync_hashes_reads_the_finalized_state() {
    use std::sync::Arc;

    use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserializeInto};

    use crate::{
        service::finalized_state::FinalizedState, tests::FakeChainHelper, CheckpointVerifiedBlock,
        Config,
    };

    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let mut state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("an ephemeral state opens");

    // A chain a little longer than the margin, so the first heights are served.
    let served_heights = 20;
    let tip = MAX_BLOCK_REORG_HEIGHT + served_heights;
    let mut block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .expect("the genesis vector is a valid block");
    let mut txouts = Vec::new();
    for height in 0..=tip {
        if height > 0 {
            block = block.make_fake_child();

            // Output counts that vary by height, so that an entry paired with the wrong height's
            // cumulative count has the wrong `txouts`.
            let child = Arc::make_mut(&mut block);
            let coinbase = Arc::make_mut(&mut child.transactions[0]);
            let output = coinbase.outputs()[0].clone();
            let outputs = height % 4 + 1;
            coinbase.set_outputs(vec![output; usize::try_from(outputs).expect("at most 4")]);
        }
        txouts.push(
            block
                .transactions
                .iter()
                .map(|tx| tx.outputs().len())
                .sum::<usize>(),
        );
        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block.clone()).into(),
                None,
                "sync hashes test",
            )
            .expect("each block is a child of the tip");
    }
    assert_eq!(state.db.finalized_tip_height(), Some(Height(tip)));

    let check = |start: u32, count: u32, expected_end: Option<u32>| {
        let served = sync_hashes(None::<Arc<Chain>>, &state.db, Height(start), count);
        let expected: Vec<u32> = expected_end.map_or(Vec::new(), |end| (start..=end).collect());
        assert_eq!(
            served.len(),
            expected.len(),
            "start {start} count {count}: entries stop at tip - MAX_BLOCK_REORG_HEIGHT"
        );
        for (entry, height) in served.iter().zip(expected) {
            assert_eq!(Some(entry.hash), state.db.hash(Height(height)), "{height}");
            assert_eq!(
                entry.txouts,
                u64::try_from(txouts[usize::try_from(height).expect("a small height")])
                    .expect("a small count"),
                "{height}"
            );
            assert_eq!(entry.txs, 1, "{height}");
        }
    };

    // From genesis, which has no block below it, to the margin.
    check(0, 50_000, Some(served_heights));
    // From above genesis, where the scan starts one height early.
    check(7, 5, Some(11));
    check(15, 100, Some(served_heights));
    // Above the margin, nothing is served.
    check(served_heights + 1, 10, None);
}
