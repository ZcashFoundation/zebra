//! Reading address transaction IDs.
//!
//! In the functions in this module:
//!
//! The block write task commits blocks to the finalized state before updating
//! `chain` with a cached copy of the best non-finalized chain from
//! `NonFinalizedState.chain_set`. Then the block commit task can commit additional blocks to
//! the finalized state after we've cloned the `chain`.
//!
//! This means that some blocks can be in both:
//! - the cached [`Chain`], and
//! - the shared finalized [`ZebraDb`] reference.

use std::{
    collections::{BTreeMap, HashSet},
    ops::RangeInclusive,
};

use zebra_chain::{
    block::{self, Height},
    transaction, transparent,
};

use crate::{
    service::{
        finalized_state::ZebraDb, non_finalized_state::Chain, read::FINALIZED_STATE_QUERY_RETRIES,
    },
    BoxError, TransactionLocation,
};

/// The finalized chain tips observed at the start and end of a finalized address tx ID read.
struct FinalizedTipBounds {
    start: (Height, block::Hash),
    end: (Height, block::Hash),
}

/// Returns the transaction IDs that sent or received funds from the supplied [`transparent::Address`]es,
/// within `query_height_range`, in chain order, at most `max_entries` of them, along with the
/// tip `(height, hash)` of the chain snapshot that produced them (or `None` if the state is empty).
///
/// If the addresses do not exist in the non-finalized `chain` or finalized `db`,
/// or the `query_height_range` is totally outside both the `chain` and `db` range,
/// returns an empty list with the snapshot tip.
#[cfg(test)]
pub fn transparent_tx_ids<C>(
    chain: Option<C>,
    db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
    query_height_range: RangeInclusive<Height>,
    max_entries: Option<usize>,
) -> Result<
    (
        BTreeMap<TransactionLocation, transaction::Hash>,
        Option<(block::Height, block::Hash)>,
    ),
    BoxError,
>
where
    C: AsRef<Chain>,
{
    transparent_tx_ids_with(
        || chain.as_ref(),
        db,
        addresses,
        query_height_range,
        max_entries,
    )
}

/// Returns the transaction IDs that sent or received funds from the supplied
/// [`transparent::Address`]es, within `query_height_range`, in chain order, at most
/// `max_entries` of them, along with the tip `(height, hash)` of the chain snapshot that
/// produced them (or `None` if the state is empty).
///
/// Re-samples `latest_chain` on every retry attempt so retries after a concurrent finalization
/// or fork switch observe the updated best chain. If the addresses do not exist in the
/// non-finalized chain or finalized `db`, or `query_height_range` is outside both, returns an
/// empty list with the snapshot tip.
pub fn transparent_tx_ids_with<C>(
    mut latest_chain: impl FnMut() -> Option<C>,
    db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
    query_height_range: RangeInclusive<Height>,
    max_entries: Option<usize>,
) -> Result<
    (
        BTreeMap<TransactionLocation, transaction::Hash>,
        Option<(block::Height, block::Hash)>,
    ),
    BoxError,
>
where
    C: AsRef<Chain>,
{
    let mut tx_id_error = None;

    // Retry the finalized tx ID query if it was interrupted by a finalizing block,
    // and the non-finalized chain doesn't overlap the changed heights or belongs to
    // a stale fork.
    //
    // TODO: refactor this into a generic retry(finalized_closure, process_and_check_closure) fn
    for _ in 0..=FINALIZED_STATE_QUERY_RETRIES {
        let chain = latest_chain();

        let (finalized_tx_ids, finalized_tip_bounds) = match finalized_transparent_tx_ids(
            db,
            &addresses,
            query_height_range.clone(),
            max_entries,
        ) {
            Ok(result) => result,
            Err(error) => {
                tx_id_error = Some(Err(error));
                continue;
            }
        };

        // Apply the non-finalized tx ID changes.
        let chain_tx_id_changes = chain_transparent_tx_id_changes(
            chain.as_ref(),
            &addresses,
            finalized_tip_bounds,
            query_height_range.clone(),
            max_entries,
        );

        // If the tx IDs are valid, return them, otherwise, retry or return an error.
        match chain_tx_id_changes {
            Ok((chain_tx_id_changes, tip)) => {
                let tx_ids =
                    apply_tx_id_changes(finalized_tx_ids, chain_tx_id_changes, max_entries);

                return Ok((tx_ids, tip));
            }

            Err(error) => tx_id_error = Some(Err(error)),
        }
    }

    tx_id_error.expect("unexpected missing error: attempts should set error or return")
}

/// Returns the [`transaction::Hash`]es for `addresses` in the finalized chain `query_height_range`,
/// and the finalized tip `(height, hash)` bounds observed before and after the query.
///
/// If the addresses do not exist in the finalized `db`, returns an empty list.
fn finalized_transparent_tx_ids(
    db: &ZebraDb,
    addresses: &HashSet<transparent::Address>,
    query_height_range: RangeInclusive<Height>,
    limit: Option<usize>,
) -> Result<
    (
        BTreeMap<TransactionLocation, transaction::Hash>,
        Option<FinalizedTipBounds>,
    ),
    BoxError,
> {
    // # Correctness
    //
    // The StateService can commit additional blocks while we are querying transaction IDs.

    // Check if the finalized state changed while we were querying it.
    let start_finalized_tip = db.tip();

    let finalized_tx_ids =
        db.partial_finalized_transparent_tx_ids(addresses, query_height_range, limit);

    let end_finalized_tip = db.tip();

    let finalized_tip_bounds = match (start_finalized_tip, end_finalized_tip) {
        (Some(start), Some(end)) => Some(FinalizedTipBounds { start, end }),
        (None, None) => None,
        (None, Some(_)) | (Some(_), None) => {
            return Err("unable to get tx IDs: \
                        finalized state transitioned between empty and populated during query"
                .into());
        }
    };

    Ok((finalized_tx_ids, finalized_tip_bounds))
}

/// Returns the extra transaction IDs for `addresses` in the non-finalized chain `query_height_range`,
/// matching or overlapping the transaction IDs for the `finalized_tip_bounds`,
/// at most `max_entries` of them, the first in chain order, along with the tip `(height, hash)`
/// of the resulting consistent snapshot (or `None` if the state is empty).
///
/// If the addresses do not exist in the non-finalized `chain`, returns an empty list.
fn chain_transparent_tx_id_changes<C>(
    chain: Option<C>,
    addresses: &HashSet<transparent::Address>,
    finalized_tip_bounds: Option<FinalizedTipBounds>,
    query_height_range: RangeInclusive<Height>,
    max_entries: Option<usize>,
) -> Result<
    (
        BTreeMap<TransactionLocation, transaction::Hash>,
        Option<(Height, block::Hash)>,
    ),
    BoxError,
>
where
    C: AsRef<Chain>,
{
    let address_count = addresses.len();

    let Some(FinalizedTipBounds {
        start: start_tip,
        end: end_tip,
    }) = finalized_tip_bounds
    else {
        assert!(
            chain.is_none(),
            "unexpected non-finalized chain when finalized state is empty"
        );

        debug!(
            ?address_count,
            "chain address tx ID query: state is empty, no tx IDs available",
        );

        return Ok((BTreeMap::new(), None));
    };

    // # Correctness
    //
    // We can compensate for inconsistent finalized reads across blocks or addresses by
    // merging the overlapping non-finalized transaction IDs from the matching chain.
    //
    // We also require a consistent snapshot tip even for a single address, because
    // `ZebraDb::address_tx_locations` performs separate un-snapshotted RocksDB reads;
    // a concurrent finalized block commit between those reads and the start/end tip checks
    // would otherwise make the returned tip ambiguous with respect to that block's tx IDs.

    // Check if the finalized and non-finalized states match or overlap.
    let required_min_non_finalized_root = start_tip.0 .0 + 1;

    // Work out if we need to compensate for finalized query results from multiple heights:
    // - Ok contains the consistent finalized tip (no need to compensate)
    // - Err contains the required non-finalized chain overlap
    let finalized_tip_status = required_min_non_finalized_root..=end_tip.0 .0;
    let finalized_tip_status = if finalized_tip_status.is_empty() {
        Ok(end_tip)
    } else {
        Err(finalized_tip_status)
    };

    let Some(chain) = chain else {
        if let Ok(finalized_tip) = finalized_tip_status {
            debug!(
                ?finalized_tip_status,
                ?required_min_non_finalized_root,
                ?address_count,
                "chain address tx ID query: \
                 finalized chain is consistent, and non-finalized chain is empty",
            );

            return Ok((BTreeMap::new(), Some(finalized_tip)));
        } else {
            // We can't compensate for inconsistent database queries,
            // because the non-finalized chain is empty.
            debug!(
                ?finalized_tip_status,
                ?required_min_non_finalized_root,
                ?address_count,
                "chain address tx ID query: \
                 finalized tip query was inconsistent, but non-finalized chain is empty",
            );

            return Err("unable to get tx IDs: \
                        state was committing a block, and non-finalized chain is empty"
                .into());
        }
    };

    let chain = chain.as_ref();

    let non_finalized_root = chain.non_finalized_root_height();
    let non_finalized_tip = chain.non_finalized_tip();

    assert!(
        non_finalized_root.0 <= required_min_non_finalized_root,
        "unexpected chain gap: the best chain is updated after its previous root is finalized",
    );

    match finalized_tip_status {
        Ok(finalized_tip) => {
            // If we've already committed this entire chain, ignore its tx ID changes and
            // return the finalized tip from `db` (not from the discarded `chain`).
            if finalized_tip.0 >= non_finalized_tip.0 {
                debug!(
                    ?non_finalized_root,
                    ?non_finalized_tip,
                    ?finalized_tip_status,
                    ?address_count,
                    "chain address tx ID query: \
                     non-finalized blocks have all been finalized, no new tx ID changes",
                );

                return Ok((BTreeMap::new(), Some(finalized_tip)));
            }
        }

        Err(ref required_non_finalized_overlap) => {
            // We can't compensate for inconsistent database queries,
            // because the non-finalized chain is below the inconsistent query range.
            if *required_non_finalized_overlap.end() > non_finalized_tip.0 .0 {
                debug!(
                    ?non_finalized_root,
                    ?non_finalized_tip,
                    ?finalized_tip_status,
                    ?address_count,
                    "chain address tx ID query: \
                     finalized tip query was inconsistent, \
                     and some inconsistent blocks are missing from the non-finalized chain",
                );

                return Err("unable to get tx IDs: \
                            state was committing a block, \
                            that is missing from the non-finalized chain"
                    .into());
            }

            // Correctness: some finalized tx IDs might come from different heights,
            // but we've just checked they can be corrected by merging the non-finalized
            // chain, once we verify below that the chain matches the finalized fork.
            assert!(
                required_non_finalized_overlap
                    .clone()
                    .all(|height| chain.blocks.contains_key(&Height(height))),
                "tx ID query inconsistency: \
                 chain must contain required overlap blocks",
            );
        }
    }

    // Verify that `chain` extends `end_tip` on the same fork before merging its tx IDs.
    // Because `non_finalized_root.0 <= start_tip.0.0 + 1 <= end_tip.0.0 + 1` and we already
    // returned above when `end_tip.0 >= non_finalized_tip.0` (with `start_tip == end_tip`)
    // or `end_tip.0 > non_finalized_tip.0` (with `start_tip < end_tip`), either `end_tip` is
    // the immediate parent of `non_finalized_root`, or `end_tip` itself is in `chain.blocks`.
    let chain_matches_finalized_tip = if end_tip.0 < non_finalized_root {
        debug_assert_eq!(end_tip.0 .0 + 1, non_finalized_root.0);
        chain
            .blocks
            .values()
            .next()
            .expect("non-finalized chain has at least one block")
            .block
            .header
            .previous_block_hash
            == end_tip.1
    } else {
        chain.hash_by_height(end_tip.0) == Some(end_tip.1)
    };

    if !chain_matches_finalized_tip {
        debug!(
            ?non_finalized_root,
            ?non_finalized_tip,
            ?end_tip,
            ?address_count,
            "chain address tx ID query: \
             non-finalized chain belongs to a different fork than finalized tip",
        );

        return Err("unable to get tx IDs: \
                    non-finalized chain does not match finalized tip"
            .into());
    }

    Ok((
        chain.partial_transparent_tx_ids(addresses, query_height_range, max_entries),
        Some(non_finalized_tip),
    ))
}

/// Returns the combined finalized and non-finalized transaction IDs, truncated to `max_entries`.
fn apply_tx_id_changes(
    finalized_tx_ids: BTreeMap<TransactionLocation, transaction::Hash>,
    chain_tx_ids: BTreeMap<TransactionLocation, transaction::Hash>,
    max_entries: Option<usize>,
) -> BTreeMap<TransactionLocation, transaction::Hash> {
    // Correctness: compensate for inconsistent tx IDs finalized blocks across multiple addresses,
    // by combining them with overlapping non-finalized block tx IDs.
    let tx_ids: BTreeMap<_, _> = finalized_tx_ids.into_iter().chain(chain_tx_ids).collect();

    // Truncate after merging, so the survivors are the first in chain order.
    match max_entries {
        Some(max_entries) => tx_ids.into_iter().take(max_entries).collect(),
        None => tx_ids,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use zebra_chain::{
        block::Block,
        parameters::{Network::Mainnet, NetworkKind},
        serialization::ZcashDeserializeInto,
    };

    use super::*;
    use crate::{
        arbitrary::Prepare,
        service::{
            finalized_state::FinalizedState, non_finalized_state::NonFinalizedState,
            read::ADDRESS_HEIGHTS_FULL_RANGE,
        },
        tests::setup::new_state_with_mainnet_genesis,
        CheckpointVerifiedBlock, Config,
    };

    /// Mutates `block` into a distinct rival block with a different block hash and coinbase
    /// output address, optionally overriding `previous_block_hash`.
    fn make_rival_block(
        block: &Block,
        rival_address: transparent::Address,
        previous_block_hash: Option<block::Hash>,
    ) -> Arc<Block> {
        let mut rival = block.clone();
        let header = Arc::make_mut(&mut rival.header);
        header.nonce.0[0] ^= 0xff;
        if let Some(prev_hash) = previous_block_hash {
            header.previous_block_hash = prev_hash;
        }

        let coinbase = Arc::make_mut(&mut rival.transactions[0]);
        let mut outputs = coinbase.outputs().to_vec();
        outputs[1].lock_script = rival_address.script();
        coinbase.set_outputs(outputs);

        Arc::new(rival)
    }

    /// Deserializes the mainnet test vector block at `height`.
    fn mainnet_block(height: u32) -> Arc<Block> {
        zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&height]
            .zcash_deserialize_into()
            .expect("mainnet test vector block should deserialize")
    }

    /// Commits mainnet test vector blocks in `heights` directly into `finalized_state`.
    fn commit_finalized_mainnet_blocks(
        finalized_state: &mut FinalizedState,
        heights: RangeInclusive<u32>,
    ) {
        for height in heights {
            finalized_state
                .commit_finalized_direct(
                    CheckpointVerifiedBlock::from(mainnet_block(height)).into(),
                    None,
                    "test",
                )
                .expect("finalized commit should succeed");
        }
    }

    #[test]
    fn empty_state_returns_none_tip() {
        let _init_guard = zebra_test::init();

        let config = Config::ephemeral();
        let finalized_state = FinalizedState::new_with_debug(&config, &Mainnet, true, false)
            .expect("opening ephemeral state should succeed");
        let addr = transparent::Address::from_pub_key_hash(NetworkKind::Mainnet, [1; 20]);

        let (tx_ids, tip) = transparent_tx_ids(
            None::<Arc<Chain>>,
            &finalized_state.db,
            HashSet::from([addr]),
            ADDRESS_HEIGHTS_FULL_RANGE,
            None,
        )
        .expect("empty state query should succeed");

        assert!(tx_ids.is_empty());
        assert_eq!(tip, None);
    }

    #[test]
    fn finalized_only_state_returns_finalized_tip_even_when_empty_or_out_of_range() {
        let _init_guard = zebra_test::init();

        let (mut finalized_state, _non_finalized_state, _genesis) =
            new_state_with_mainnet_genesis();
        let block_1: Arc<Block> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&1]
            .zcash_deserialize_into()
            .expect("block 1 should deserialize");
        let block_1_hash = block_1.hash();
        let addr = block_1.transactions[0].outputs()[1]
            .address(&Mainnet)
            .expect("coinbase output 1 should have an address");
        let unused_addr = transparent::Address::from_pub_key_hash(NetworkKind::Mainnet, [2; 20]);

        finalized_state
            .commit_finalized_direct(CheckpointVerifiedBlock::from(block_1).into(), None, "test")
            .expect("block 1 commit should succeed");

        let (tx_ids, tip) = transparent_tx_ids(
            None::<Arc<Chain>>,
            &finalized_state.db,
            HashSet::from([addr]),
            ADDRESS_HEIGHTS_FULL_RANGE,
            None,
        )
        .expect("finalized query should succeed");
        assert_eq!(tx_ids.len(), 1);
        assert_eq!(tip, Some((Height(1), block_1_hash)));

        let (empty_tx_ids, tip) = transparent_tx_ids(
            None::<Arc<Chain>>,
            &finalized_state.db,
            HashSet::from([unused_addr]),
            ADDRESS_HEIGHTS_FULL_RANGE,
            None,
        )
        .expect("unused address query should succeed");
        assert!(empty_tx_ids.is_empty());
        assert_eq!(tip, Some((Height(1), block_1_hash)));

        #[allow(clippy::reversed_empty_ranges)]
        let empty_range = Height(5)..=Height(3);
        let (out_of_range_tx_ids, tip) = transparent_tx_ids(
            None::<Arc<Chain>>,
            &finalized_state.db,
            HashSet::from([addr]),
            empty_range,
            None,
        )
        .expect("empty height range query should succeed");
        assert!(out_of_range_tx_ids.is_empty());
        assert_eq!(tip, Some((Height(1), block_1_hash)));
    }

    #[test]
    fn finalized_and_non_finalized_returns_non_finalized_tip() {
        let _init_guard = zebra_test::init();

        let (mut finalized_state, mut non_finalized_state, _genesis) =
            new_state_with_mainnet_genesis();
        let mut tip_7 = None;
        let mut addr = None;

        for (&height, bytes) in zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS.range(1..=7) {
            let block: Arc<Block> = bytes
                .zcash_deserialize_into()
                .expect("block should deserialize");
            if addr.is_none() {
                addr = block.transactions[0].outputs()[1].address(&Mainnet);
            }
            if height <= 5 {
                finalized_state
                    .commit_finalized_direct(
                        CheckpointVerifiedBlock::from(block).into(),
                        None,
                        "test",
                    )
                    .expect("finalized commit should succeed");
            } else if height == 6 {
                non_finalized_state
                    .commit_new_chain(block.prepare(), &finalized_state.db)
                    .expect("non-finalized root commit should succeed");
            } else {
                tip_7 = Some((Height(height), block.hash()));
                non_finalized_state
                    .commit_block(block.prepare(), &finalized_state.db)
                    .expect("non-finalized block commit should succeed");
            }
        }

        let (tx_ids, tip) = transparent_tx_ids(
            non_finalized_state.best_chain(),
            &finalized_state.db,
            HashSet::from([addr.expect("address should exist")]),
            ADDRESS_HEIGHTS_FULL_RANGE,
            None,
        )
        .expect("combined query should succeed");

        assert_eq!(tx_ids.len(), 7);
        assert_eq!(tip, tip_7);
    }

    #[test]
    fn superseded_rival_fork_chain_uses_finalized_db_tip_not_chain_hash() {
        let _init_guard = zebra_test::init();

        let (mut finalized_state, mut non_finalized_state, _genesis) =
            new_state_with_mainnet_genesis();
        for (&_height, bytes) in zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS.range(1..=5) {
            let block: Arc<Block> = bytes
                .zcash_deserialize_into()
                .expect("block should deserialize");
            finalized_state
                .commit_finalized_direct(CheckpointVerifiedBlock::from(block).into(), None, "test")
                .expect("finalized commit should succeed");
        }

        let block_6: Arc<Block> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&6]
            .zcash_deserialize_into()
            .expect("block 6 should deserialize");
        let original_addr = block_6.transactions[0].outputs()[1]
            .address(&Mainnet)
            .expect("coinbase output 1 should have an address");
        non_finalized_state
            .commit_new_chain(block_6.clone().prepare(), &finalized_state.db)
            .expect("non-finalized commit should succeed");
        let stale_chain = non_finalized_state
            .best_chain()
            .expect("non-finalized chain should exist");

        // Finalize a rival block 6 directly into `db` so `finalized_tip >= stale_chain.tip`.
        let rival_addr = transparent::Address::from_pub_key_hash(NetworkKind::Mainnet, [8; 20]);
        let rival_6 = make_rival_block(&block_6, rival_addr, None);
        let rival_6_hash = rival_6.hash();
        let rival_6_txid = rival_6.transactions[0].hash();
        assert_ne!(rival_6_hash, block_6.hash());
        finalized_state
            .commit_finalized_direct(CheckpointVerifiedBlock::from(rival_6).into(), None, "test")
            .expect("rival block 6 commit should succeed");

        let (tx_ids, tip) = transparent_tx_ids(
            Some(stale_chain),
            &finalized_state.db,
            HashSet::from([original_addr, rival_addr]),
            ADDRESS_HEIGHTS_FULL_RANGE,
            None,
        )
        .expect("superseded chain should fall back cleanly to finalized state");

        assert_eq!(tip, Some((Height(6), rival_6_hash)));
        assert!(tx_ids.values().any(|txid| *txid == rival_6_txid));
        assert!(!tx_ids
            .values()
            .any(|txid| *txid == block_6.transactions[0].hash()));
    }

    #[test]
    fn stale_fork_chain_above_finalized_tip_is_rejected_and_retried_via_latest_chain() {
        let _init_guard = zebra_test::init();

        let (mut finalized_state, mut non_finalized_state, _genesis) =
            new_state_with_mainnet_genesis();
        commit_finalized_mainnet_blocks(&mut finalized_state, 1..=5);
        let block_6 = mainnet_block(6);
        let block_7 = mainnet_block(7);
        let original_addr = block_6.transactions[0].outputs()[1]
            .address(&Mainnet)
            .expect("coinbase output 1 should have an address");

        non_finalized_state
            .commit_new_chain(block_6.clone().prepare(), &finalized_state.db)
            .expect("block 6 commit should succeed");
        non_finalized_state
            .commit_block(block_7.clone().prepare(), &finalized_state.db)
            .expect("block 7 commit should succeed");
        let stale_chain = non_finalized_state
            .best_chain()
            .expect("stale chain should exist");

        // Finalize a rival block 6 so `stale_chain` at heights 6..=7 disagrees with `db` at 6.
        let rival_addr = transparent::Address::from_pub_key_hash(NetworkKind::Mainnet, [9; 20]);
        let rival_6 = make_rival_block(&block_6, rival_addr, None);
        let rival_6_hash = rival_6.hash();
        let rival_6_txid = rival_6.transactions[0].hash();
        finalized_state
            .commit_finalized_direct(CheckpointVerifiedBlock::from(rival_6).into(), None, "test")
            .expect("rival block 6 commit should succeed");

        let err = transparent_tx_ids(
            Some(stale_chain.clone()),
            &finalized_state.db,
            HashSet::from([original_addr, rival_addr]),
            ADDRESS_HEIGHTS_FULL_RANGE,
            None,
        )
        .expect_err("stale fork chain extending past finalized tip must fail");
        assert!(err
            .to_string()
            .contains("non-finalized chain does not match finalized tip"));

        // Build a matching non-finalized chain on top of rival block 6, and verify
        // `transparent_tx_ids_with` recovers on retry by re-sampling `latest_chain`.
        let rival_7 = make_rival_block(&block_7, rival_addr, Some(rival_6_hash));
        let rival_7_hash = rival_7.hash();
        let rival_7_txid = rival_7.transactions[0].hash();
        let mut rival_non_finalized = NonFinalizedState::new(&Mainnet);
        rival_non_finalized
            .commit_new_chain(rival_7.prepare(), &finalized_state.db)
            .expect("rival block 7 commit should succeed");
        let valid_chain = rival_non_finalized
            .best_chain()
            .expect("valid chain should exist");

        let attempts = AtomicUsize::new(0);
        let (tx_ids, tip) = transparent_tx_ids_with(
            || {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Some(stale_chain.clone())
                } else {
                    Some(valid_chain.clone())
                }
            },
            &finalized_state.db,
            HashSet::from([original_addr, rival_addr]),
            ADDRESS_HEIGHTS_FULL_RANGE,
            None,
        )
        .expect("re-sampling latest_chain should succeed on retry");

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(tip, Some((Height(7), rival_7_hash)));
        assert!(tx_ids.values().any(|txid| *txid == rival_6_txid));
        assert!(tx_ids.values().any(|txid| *txid == rival_7_txid));
        assert!(!tx_ids
            .values()
            .any(|txid| *txid == block_6.transactions[0].hash()));
        assert!(!tx_ids
            .values()
            .any(|txid| *txid == block_7.transactions[0].hash()));

        // Also cover the `end_tip.0 < non_finalized_root` branch where the finalized tip is the
        // parent height of `non_finalized_root`, with a mismatched block hash.
        let (mut parent_mismatch_db, _nfs, _gen) = new_state_with_mainnet_genesis();
        commit_finalized_mainnet_blocks(&mut parent_mismatch_db, 1..=4);
        let rival_5 = make_rival_block(&mainnet_block(5), rival_addr, None);
        parent_mismatch_db
            .commit_finalized_direct(CheckpointVerifiedBlock::from(rival_5).into(), None, "test")
            .expect("finalized commit should succeed");

        let mut fork_a_finalized = new_state_with_mainnet_genesis().0;
        commit_finalized_mainnet_blocks(&mut fork_a_finalized, 1..=5);
        let mut fork_a_non_finalized = NonFinalizedState::new(&Mainnet);
        fork_a_non_finalized
            .commit_new_chain(mainnet_block(6).prepare(), &fork_a_finalized.db)
            .expect("fork A block 6 commit should succeed");
        let fork_a_chain = fork_a_non_finalized
            .best_chain()
            .expect("fork A chain should exist");
        let parent_err = transparent_tx_ids(
            Some(fork_a_chain),
            &parent_mismatch_db.db,
            HashSet::from([original_addr]),
            ADDRESS_HEIGHTS_FULL_RANGE,
            None,
        )
        .expect_err("root block whose previous_block_hash mismatches finalized tip must fail");
        assert!(parent_err
            .to_string()
            .contains("non-finalized chain does not match finalized tip"));
    }

    #[test]
    fn single_address_rejects_inconsistent_finalized_tip_bounds() {
        let _init_guard = zebra_test::init();

        let (mut finalized_state, mut non_finalized_4_state, _genesis) =
            new_state_with_mainnet_genesis();

        for (&_height, bytes) in zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS.range(1..=3) {
            let block: Arc<Block> = bytes
                .zcash_deserialize_into()
                .expect("block should deserialize");
            finalized_state
                .commit_finalized_direct(CheckpointVerifiedBlock::from(block).into(), None, "test")
                .expect("finalized commit should succeed");
        }

        let block_4: Arc<Block> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&4]
            .zcash_deserialize_into()
            .expect("block 4 should deserialize");
        let block_4_hash = block_4.hash();
        let single_addr = block_4.transactions[0].outputs()[1]
            .address(&Mainnet)
            .expect("address should exist");
        non_finalized_4_state
            .commit_new_chain(block_4.prepare(), &finalized_state.db)
            .expect("block 4 commit should succeed");
        let chain_4 = non_finalized_4_state
            .best_chain()
            .cloned()
            .expect("chain 4 should exist");

        let mut non_finalized_5_state = non_finalized_4_state;
        let block_5: Arc<Block> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&5]
            .zcash_deserialize_into()
            .expect("block 5 should deserialize");
        let block_5_hash = block_5.hash();
        non_finalized_5_state
            .commit_block(block_5.prepare(), &finalized_state.db)
            .expect("block 5 commit should succeed");
        let chain_5 = non_finalized_5_state
            .best_chain()
            .cloned()
            .expect("chain 5 should exist");

        let single_addr_set = HashSet::from([single_addr]);
        let tip_4 = (Height(4), block_4_hash);
        let tip_5 = (Height(5), block_5_hash);

        // Even with 1 address, an inconsistent finalized tip bound with no non-finalized chain
        // must fail rather than return an ambiguous tip.
        let err = chain_transparent_tx_id_changes(
            None::<Arc<Chain>>,
            &single_addr_set,
            Some(FinalizedTipBounds {
                start: tip_4,
                end: tip_5,
            }),
            ADDRESS_HEIGHTS_FULL_RANGE,
            None,
        )
        .expect_err("single address must not bypass inconsistent finalized tip bounds");
        assert!(err.to_string().contains("non-finalized chain is empty"));

        // Even with 1 address, a chain that doesn't reach `end_tip` must fail.
        let err = chain_transparent_tx_id_changes(
            Some(&chain_4),
            &single_addr_set,
            Some(FinalizedTipBounds {
                start: tip_4,
                end: tip_5,
            }),
            ADDRESS_HEIGHTS_FULL_RANGE,
            None,
        )
        .expect_err("single address must not bypass missing overlap block in chain");
        assert!(err
            .to_string()
            .contains("missing from the non-finalized chain"));

        // Once the chain covers `end_tip` with matching hash, it succeeds with `Some(tip_5)`.
        let (_changes, tip) = chain_transparent_tx_id_changes(
            Some(&chain_5),
            &single_addr_set,
            Some(FinalizedTipBounds {
                start: tip_4,
                end: tip_5,
            }),
            ADDRESS_HEIGHTS_FULL_RANGE,
            None,
        )
        .expect("single address with covering matching chain should succeed");
        assert_eq!(tip, Some(tip_5));
    }
}
