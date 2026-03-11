//! Reading note commitment trees.
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

use std::{collections::BTreeMap, sync::Arc};

use zebra_chain::{
    orchard, sapling,
    subtree::{NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
};

use crate::{
    service::{finalized_state::ZebraDb, non_finalized_state::Chain},
    HashOrHeight,
};

// Doc-only items
#[allow(unused_imports)]
use zebra_chain::subtree::NoteCommitmentSubtree;

/// Returns the Sapling
/// [`NoteCommitmentTree`](sapling::tree::NoteCommitmentTree) specified by a
/// hash or height, if it exists in the non-finalized `chain` or finalized `db`.
pub fn sapling_tree<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<sapling::tree::NoteCommitmentTree>>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // Since sapling treestates are the same in the finalized and non-finalized
    // state, we check the most efficient alternative first. (`chain` is always
    // in memory, but `db` stores blocks on disk, with a memory cache.)
    chain
        .and_then(|chain| chain.as_ref().sapling_tree(hash_or_height))
        .or_else(|| db.sapling_tree_by_hash_or_height(hash_or_height))
}

/// Returns a list of Sapling [`NoteCommitmentSubtree`]s with indexes in the provided range.
///
/// If there is no subtree at the first index in the range, the returned list is empty.
/// Otherwise, subtrees are continuous up to the finalized tip.
///
/// See [`subtrees`] for more details.
pub fn sapling_subtrees<C>(
    chain: Option<C>,
    db: &ZebraDb,
    range: impl std::ops::RangeBounds<NoteCommitmentSubtreeIndex> + Clone,
) -> BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<sapling_crypto::Node>>
where
    C: AsRef<Chain>,
{
    subtrees(
        chain,
        range,
        |chain, range| chain.sapling_subtrees_in_range(range),
        |range| db.sapling_subtree_list_by_index_range(range),
    )
}

/// Returns the Orchard
/// [`NoteCommitmentTree`](orchard::tree::NoteCommitmentTree) specified by a
/// hash or height, if it exists in the non-finalized `chain` or finalized `db`.
pub fn orchard_tree<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<orchard::tree::NoteCommitmentTree>>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // Since orchard treestates are the same in the finalized and non-finalized
    // state, we check the most efficient alternative first. (`chain` is always
    // in memory, but `db` stores blocks on disk, with a memory cache.)
    chain
        .and_then(|chain| chain.as_ref().orchard_tree(hash_or_height))
        .or_else(|| db.orchard_tree_by_hash_or_height(hash_or_height))
}

/// Returns a list of Orchard [`NoteCommitmentSubtree`]s with indexes in the provided range.
///
/// If there is no subtree at the first index in the range, the returned list is empty.
/// Otherwise, subtrees are continuous up to the finalized tip.
///
/// See [`subtrees`] for more details.
pub fn orchard_subtrees<C>(
    chain: Option<C>,
    db: &ZebraDb,
    range: impl std::ops::RangeBounds<NoteCommitmentSubtreeIndex> + Clone,
) -> BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<orchard::tree::Node>>
where
    C: AsRef<Chain>,
{
    subtrees(
        chain,
        range,
        |chain, range| chain.orchard_subtrees_in_range(range),
        |range| db.orchard_subtree_list_by_index_range(range),
    )
}

/// Returns a list of [`NoteCommitmentSubtree`]s in the provided range.
///
/// If there is no subtree at the first index in the range, the returned list is empty.
/// Otherwise, subtrees are continuous up to the finalized tip.
///
/// Accepts a `chain` from the non-finalized state, a `range` of subtree indexes to retrieve,
/// a `read_chain` function for retrieving the `range` of subtrees from `chain`, and
/// a `read_disk` function for retrieving the `range` from [`ZebraDb`].
///
/// Returns a consistent set of subtrees for the supplied chain fork and database.
/// Avoids reading the database if the subtrees are present in memory.
///
/// # Correctness
///
/// APIs that return single subtrees can't be used for `read_chain` and `read_disk`, because they
/// can create an inconsistent list of subtrees after concurrent non-finalized and finalized updates.
fn subtrees<C, Range, Node, ChainSubtreeFn, DbSubtreeFn>(
    chain: Option<C>,
    range: Range,
    read_chain: ChainSubtreeFn,
    read_disk: DbSubtreeFn,
) -> BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<Node>>
where
    C: AsRef<Chain>,
    Node: PartialEq,
    Range: std::ops::RangeBounds<NoteCommitmentSubtreeIndex> + Clone,
    ChainSubtreeFn: FnOnce(
        &Chain,
        Range,
    )
        -> BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<Node>>,
    DbSubtreeFn:
        FnOnce(Range) -> BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<Node>>,
{
    use std::ops::Bound::*;

    let start_index = match range.start_bound().cloned() {
        Included(start_index) => start_index,
        Excluded(start_index) => (start_index.0 + 1).into(),
        Unbounded => 0.into(),
    };

    // # Correctness
    //
    // After `chain` was cloned, the StateService can commit additional blocks to the finalized state `db`.
    // Usually, the subtrees of these blocks are consistent. But if the `chain` is a different fork to `db`,
    // then the trees can be inconsistent. In that case, if `chain` does not contain a subtree at the first
    // index in the provided range, we ignore all the trees in `chain` after the first inconsistent tree,
    // because we know they will be inconsistent as well. (It is cryptographically impossible for tree roots
    // to be equal once the leaves have diverged.)

    let results = match chain.map(|chain| read_chain(chain.as_ref(), range.clone())) {
        Some(chain_results) if chain_results.contains_key(&start_index) => return chain_results,
        Some(chain_results) => {
            let mut db_results = read_disk(range);

            // Check for inconsistent trees in the fork.
            for (chain_index, chain_subtree) in chain_results {
                // If there's no matching index, just update the list of trees.
                let Some(db_subtree) = db_results.get(&chain_index) else {
                    db_results.insert(chain_index, chain_subtree);
                    continue;
                };

                // We have an outdated chain fork, so skip this subtree and all remaining subtrees.
                if &chain_subtree != db_subtree {
                    break;
                }
                // Otherwise, the subtree is already in the list, so we don't need to add it.
            }

            db_results
        }
        None => read_disk(range),
    };

    // Check that we got the start subtree
    if results.contains_key(&start_index) {
        results
    } else {
        BTreeMap::new()
    }
}

/// Get the history tree of the provided chain.
pub fn history_tree<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<zebra_chain::history_tree::HistoryTree>>
where
    C: AsRef<Chain>,
{
    chain
        .and_then(|chain| chain.as_ref().history_tree(hash_or_height))
        .or_else(|| Some(db.history_tree()))
}
