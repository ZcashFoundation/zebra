//! Reading note commitment trees.

use std::sync::Arc;

use zebra_chain::{orchard, sapling};

use crate::{
    service::{finalized_state::ZebraDb, non_finalized_state::Chain},
    HashOrHeight,
};

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
    // The StateService commits blocks to the finalized state before updating
    // the latest chain, and it can commit additional blocks after we've cloned
    // this `chain` variable.
    //
    // Since sapling treestates are the same in the finalized and non-finalized
    // state, we check the most efficient alternative first. (`chain` is always
    // in memory, but `db` stores blocks on disk, with a memory cache.)
    chain
        .and_then(|chain| chain.as_ref().sapling_tree(hash_or_height))
        .or_else(|| db.sapling_tree(hash_or_height))
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
    // The StateService commits blocks to the finalized state before updating
    // the latest chain, and it can commit additional blocks after we've cloned
    // this `chain` variable.
    //
    // Since orchard treestates are the same in the finalized and non-finalized
    // state, we check the most efficient alternative first. (`chain` is always
    // in memory, but `db` stores blocks on disk, with a memory cache.)
    chain
        .and_then(|chain| chain.as_ref().orchard_tree(hash_or_height))
        .or_else(|| db.orchard_tree(hash_or_height))
}
