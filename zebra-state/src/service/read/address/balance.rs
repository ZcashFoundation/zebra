//! Reading address balances.
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

use std::collections::HashSet;

use zebra_chain::{
    amount::{self, Amount, NegativeAllowed, NonNegative},
    block::{self, Height},
    transparent,
};

use crate::{
    service::{
        finalized_state::ZebraDb, non_finalized_state::Chain, read::FINALIZED_STATE_QUERY_RETRIES,
    },
    BoxError,
};

/// Returns the total transparent balance and received balance for the supplied
/// [`transparent::Address`]es, along with the tip `(height, hash)` of the chain snapshot
/// that produced them (or `None` if the state is empty).
///
/// If the addresses do not exist in the non-finalized `chain` or finalized `db`, returns zero
/// with the snapshot tip.
#[cfg(test)]
pub fn transparent_balance<C>(
    chain: Option<C>,
    db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
) -> Result<((Amount<NonNegative>, u64), Option<(Height, block::Hash)>), BoxError>
where
    C: AsRef<Chain>,
{
    transparent_balance_with(|| chain.as_ref(), db, addresses)
}

/// Like [`transparent_balance`], but re-samples `latest_chain` on every retry attempt so retries
/// after a concurrent finalization or fork switch observe the updated best chain.
pub fn transparent_balance_with<C>(
    mut latest_chain: impl FnMut() -> Option<C>,
    db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
) -> Result<((Amount<NonNegative>, u64), Option<(Height, block::Hash)>), BoxError>
where
    C: AsRef<Chain>,
{
    let mut balance_error = None;

    // Retry the finalized balance query if it was interrupted by a finalizing block
    // or if the sampled non-finalized chain belongs to a stale fork.
    //
    // TODO: refactor this into a generic retry(finalized_closure, process_and_check_closure) fn
    for _ in 0..=FINALIZED_STATE_QUERY_RETRIES {
        let chain = latest_chain();

        let (finalized_balance, finalized_tip) = match finalized_transparent_balance(db, &addresses)
        {
            Ok(result) => result,
            Err(error) => {
                balance_error = Some(Err(error));
                continue;
            }
        };

        match chain_transparent_balance_change(chain.as_ref(), &addresses, finalized_tip) {
            Ok((chain_balance_change, tip)) => {
                let balance = apply_balance_change(finalized_balance, chain_balance_change)?;
                return Ok((balance, tip));
            }
            Err(error) => balance_error = Some(Err(error)),
        }
    }

    balance_error.expect("unexpected missing error: attempts should set error or return")
}

/// Returns the total transparent balance for `addresses` in the finalized chain,
/// and the finalized tip `(height, hash)` the balances were queried at.
///
/// If the addresses do not exist in the finalized `db`, returns zero.
fn finalized_transparent_balance(
    db: &ZebraDb,
    addresses: &HashSet<transparent::Address>,
) -> Result<((Amount<NonNegative>, u64), Option<(Height, block::Hash)>), BoxError> {
    // # Correctness
    //
    // The StateService can commit additional blocks while we are querying address balances.

    // Check if the finalized state changed while we were querying it.
    let original_finalized_tip = db.tip();

    let finalized_balance = db.partial_finalized_transparent_balance(addresses);

    let finalized_tip = db.tip();

    if original_finalized_tip != finalized_tip {
        // Correctness: Some balances might be from before the block, and some after.
        return Err("unable to get balance: state was committing a block".into());
    }

    Ok((finalized_balance, finalized_tip))
}

/// Returns `true` if `chain` extends `finalized_tip` on the same fork.
fn chain_matches_finalized_tip(chain: &Chain, finalized_tip: (Height, block::Hash)) -> bool {
    let non_finalized_root = chain.non_finalized_root_height();

    if finalized_tip.0 < non_finalized_root {
        debug_assert_eq!(finalized_tip.0 .0 + 1, non_finalized_root.0);
        chain
            .blocks
            .values()
            .next()
            .expect("non-finalized chain has at least one block")
            .block
            .header
            .previous_block_hash
            == finalized_tip.1
    } else {
        chain.hash_by_height(finalized_tip.0) == Some(finalized_tip.1)
    }
}

/// Returns the total transparent balance change for `addresses` in the non-finalized chain,
/// matching the balance for `finalized_tip`, along with the tip `(height, hash)` of the
/// resulting consistent snapshot (or `None` if the state is empty).
///
/// If the addresses do not exist in the non-finalized `chain`, returns zero.
fn chain_transparent_balance_change<C>(
    chain: Option<C>,
    addresses: &HashSet<transparent::Address>,
    finalized_tip: Option<(Height, block::Hash)>,
) -> Result<
    (
        (Amount<NegativeAllowed>, u64),
        Option<(Height, block::Hash)>,
    ),
    BoxError,
>
where
    C: AsRef<Chain>,
{
    let Some(finalized_tip) = finalized_tip else {
        assert!(
            chain.is_none(),
            "unexpected non-finalized chain when finalized state is empty"
        );
        return Ok(((Amount::zero(), 0), None));
    };

    let Some(chain) = chain else {
        return Ok(((Amount::zero(), 0), Some(finalized_tip)));
    };

    let chain = chain.as_ref();
    let required_chain_root = (finalized_tip.0 + 1).expect("finalized tip height is below MAX");

    assert!(
        chain.non_finalized_root_height() <= required_chain_root,
        "unexpected chain gap: the best chain is updated after its previous root is finalized"
    );

    let non_finalized_tip = chain.non_finalized_tip();

    // If we've already committed this entire chain (or finalized a rival block at or above it),
    // ignore its balance changes and return the finalized tip from `db`.
    if non_finalized_tip.0 < required_chain_root {
        return Ok(((Amount::zero(), 0), Some(finalized_tip)));
    }

    if !chain_matches_finalized_tip(chain, finalized_tip) {
        return Err(
            "unable to get balance: non-finalized chain does not match finalized tip".into(),
        );
    }

    // Correctness: the finalized balance already includes the blocks below the required root,
    // so only the blocks from that root are added, and each block is counted once.
    Ok((
        chain.partial_transparent_balance_change_from(addresses, required_chain_root),
        Some(non_finalized_tip),
    ))
}

/// Add the supplied finalized and non-finalized balances together,
/// and return the result.
fn apply_balance_change(
    (finalized_balance, finalized_received): (Amount<NonNegative>, u64),
    (chain_balance_change, chain_received_change): (Amount<NegativeAllowed>, u64),
) -> amount::Result<(Amount<NonNegative>, u64)> {
    let balance = finalized_balance.constrain()? + chain_balance_change;
    // Addresses could receive more than the max money supply by sending to themselves,
    // use u64::MAX if the addition overflows.
    let received = finalized_received.saturating_add(chain_received_change);
    Ok((balance?.constrain()?, received))
}

#[cfg(test)]
mod tests {
    use std::{
        ops::RangeInclusive,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    use zebra_chain::{
        block::Block,
        parameters::{Network::Mainnet, NetworkKind},
        serialization::ZcashDeserializeInto,
    };

    use super::*;
    use crate::{
        arbitrary::Prepare,
        service::{finalized_state::FinalizedState, non_finalized_state::NonFinalizedState},
        tests::setup::new_state_with_mainnet_genesis,
        CheckpointVerifiedBlock, Config,
    };

    fn mainnet_block(height: u32) -> Arc<Block> {
        zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&height]
            .zcash_deserialize_into()
            .expect("mainnet test vector block should deserialize")
    }

    fn coinbase_output_1_addr(block: &Block) -> transparent::Address {
        block.transactions[0].outputs()[1]
            .address(&Mainnet)
            .expect("coinbase output 1 should have an address")
    }

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

    #[test]
    fn empty_state_returns_none_tip() {
        let _init_guard = zebra_test::init();

        let config = Config::ephemeral();
        let finalized_state = FinalizedState::new_with_debug(&config, &Mainnet, true, false)
            .expect("opening ephemeral state should succeed");
        let addr = transparent::Address::from_pub_key_hash(NetworkKind::Mainnet, [1; 20]);

        let ((balance, received), tip) = transparent_balance(
            None::<Arc<Chain>>,
            &finalized_state.db,
            HashSet::from([addr]),
        )
        .expect("empty state query should succeed");

        assert_eq!(balance, Amount::<NonNegative>::zero());
        assert_eq!(received, 0);
        assert_eq!(tip, None);
    }

    #[test]
    fn finalized_only_state_returns_finalized_tip() {
        let _init_guard = zebra_test::init();

        let (mut finalized_state, _non_finalized_state, _genesis) =
            new_state_with_mainnet_genesis();
        let block_1 = mainnet_block(1);
        let block_1_hash = block_1.hash();
        let addr = coinbase_output_1_addr(&block_1);
        let expected_value = u64::from(block_1.transactions[0].outputs()[1].value);
        let unused_addr = transparent::Address::from_pub_key_hash(NetworkKind::Mainnet, [2; 20]);

        finalized_state
            .commit_finalized_direct(CheckpointVerifiedBlock::from(block_1).into(), None, "test")
            .expect("block 1 commit should succeed");

        let ((balance, received), tip) = transparent_balance(
            None::<Arc<Chain>>,
            &finalized_state.db,
            HashSet::from([addr]),
        )
        .expect("finalized query should succeed");
        assert_eq!(u64::from(balance), expected_value);
        assert_eq!(received, expected_value);
        assert_eq!(tip, Some((Height(1), block_1_hash)));

        let ((unused_balance, unused_received), unused_tip) = transparent_balance(
            None::<Arc<Chain>>,
            &finalized_state.db,
            HashSet::from([unused_addr]),
        )
        .expect("unused address query should succeed");
        assert_eq!(unused_balance, Amount::<NonNegative>::zero());
        assert_eq!(unused_received, 0);
        assert_eq!(unused_tip, Some((Height(1), block_1_hash)));
    }

    #[test]
    fn finalized_and_non_finalized_returns_non_finalized_tip() {
        let _init_guard = zebra_test::init();

        let (mut finalized_state, mut non_finalized_state, _genesis) =
            new_state_with_mainnet_genesis();
        commit_finalized_mainnet_blocks(&mut finalized_state, 1..=5);

        let block_6 = mainnet_block(6);
        let block_7 = mainnet_block(7);
        let addr = coinbase_output_1_addr(&block_6);
        let tip_7 = (Height(7), block_7.hash());

        non_finalized_state
            .commit_new_chain(block_6.prepare(), &finalized_state.db)
            .expect("block 6 commit should succeed");
        non_finalized_state
            .commit_block(block_7.prepare(), &finalized_state.db)
            .expect("block 7 commit should succeed");

        let ((balance, received), tip) = transparent_balance(
            non_finalized_state.best_chain(),
            &finalized_state.db,
            HashSet::from([addr]),
        )
        .expect("combined query should succeed");

        let expected: u64 = (1..=7)
            .map(|h| u64::from(mainnet_block(h).transactions[0].outputs()[1].value))
            .sum();
        assert_eq!(u64::from(balance), expected);
        assert_eq!(received, expected);
        assert_eq!(tip, Some(tip_7));
    }

    #[test]
    fn superseded_rival_fork_chain_uses_finalized_db_tip() {
        let _init_guard = zebra_test::init();

        let (mut finalized_state, mut non_finalized_state, _genesis) =
            new_state_with_mainnet_genesis();
        commit_finalized_mainnet_blocks(&mut finalized_state, 1..=5);

        let block_6 = mainnet_block(6);
        non_finalized_state
            .commit_new_chain(block_6.clone().prepare(), &finalized_state.db)
            .expect("block 6 commit should succeed");
        let stale_chain = non_finalized_state
            .best_chain()
            .expect("stale chain should exist");

        let rival_addr = transparent::Address::from_pub_key_hash(NetworkKind::Mainnet, [8; 20]);
        let rival_6 = make_rival_block(&block_6, rival_addr, None);
        let rival_6_hash = rival_6.hash();
        let rival_value = u64::from(rival_6.transactions[0].outputs()[1].value);
        finalized_state
            .commit_finalized_direct(CheckpointVerifiedBlock::from(rival_6).into(), None, "test")
            .expect("rival block 6 commit should succeed");

        let ((balance, received), tip) = transparent_balance(
            Some(stale_chain),
            &finalized_state.db,
            HashSet::from([rival_addr]),
        )
        .expect("superseded chain should fall back cleanly to finalized state");

        assert_eq!(tip, Some((Height(6), rival_6_hash)));
        assert_eq!(u64::from(balance), rival_value);
        assert_eq!(received, rival_value);
    }

    #[test]
    fn stale_fork_chain_above_finalized_tip_is_rejected_and_retried() {
        let _init_guard = zebra_test::init();

        let (mut finalized_state, mut non_finalized_state, _genesis) =
            new_state_with_mainnet_genesis();
        commit_finalized_mainnet_blocks(&mut finalized_state, 1..=5);
        let block_6 = mainnet_block(6);
        let block_7 = mainnet_block(7);

        non_finalized_state
            .commit_new_chain(block_6.clone().prepare(), &finalized_state.db)
            .expect("block 6 commit should succeed");
        non_finalized_state
            .commit_block(block_7.clone().prepare(), &finalized_state.db)
            .expect("block 7 commit should succeed");
        let stale_chain = non_finalized_state
            .best_chain()
            .expect("stale chain should exist");

        let rival_addr = transparent::Address::from_pub_key_hash(NetworkKind::Mainnet, [9; 20]);
        let rival_6 = make_rival_block(&block_6, rival_addr, None);
        let rival_6_hash = rival_6.hash();
        let rival_6_val = u64::from(rival_6.transactions[0].outputs()[1].value);
        finalized_state
            .commit_finalized_direct(CheckpointVerifiedBlock::from(rival_6).into(), None, "test")
            .expect("rival block 6 commit should succeed");

        let err = transparent_balance(
            Some(stale_chain.clone()),
            &finalized_state.db,
            HashSet::from([rival_addr]),
        )
        .expect_err("stale fork chain extending past finalized tip must fail");
        assert!(err
            .to_string()
            .contains("non-finalized chain does not match finalized tip"));

        let rival_7 = make_rival_block(&block_7, rival_addr, Some(rival_6_hash));
        let rival_7_hash = rival_7.hash();
        let rival_7_val = u64::from(rival_7.transactions[0].outputs()[1].value);
        let mut rival_non_finalized = NonFinalizedState::new(&Mainnet);
        rival_non_finalized
            .commit_new_chain(rival_7.prepare(), &finalized_state.db)
            .expect("rival block 7 commit should succeed");
        let valid_chain = rival_non_finalized
            .best_chain()
            .expect("valid chain should exist");

        let attempts = AtomicUsize::new(0);
        let ((balance, received), tip) = transparent_balance_with(
            || {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Some(stale_chain.clone())
                } else {
                    Some(valid_chain.clone())
                }
            },
            &finalized_state.db,
            HashSet::from([rival_addr]),
        )
        .expect("re-sampling latest_chain should succeed on retry");

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(tip, Some((Height(7), rival_7_hash)));
        assert_eq!(u64::from(balance), rival_6_val + rival_7_val);
        assert_eq!(received, rival_6_val + rival_7_val);
    }

    #[test]
    fn parent_hash_mismatch_below_non_finalized_root_is_rejected() {
        let _init_guard = zebra_test::init();

        let rival_addr = transparent::Address::from_pub_key_hash(NetworkKind::Mainnet, [9; 20]);
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

        let err = transparent_balance(
            Some(fork_a_chain),
            &parent_mismatch_db.db,
            HashSet::from([rival_addr]),
        )
        .expect_err("root block whose previous_block_hash mismatches finalized tip must fail");
        assert!(err
            .to_string()
            .contains("non-finalized chain does not match finalized tip"));
    }
}
