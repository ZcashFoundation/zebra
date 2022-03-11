//! Shared state reading code.
//!
//! Used by [`StateService`] and [`ReadStateService`]
//! to read from the best [`Chain`] in the [`NonFinalizedState`],
//! and the database in the [`FinalizedState`].

use std::sync::Arc;

use zebra_chain::block::Block;

use crate::{
    service::{finalized_state::ZebraDb, non_finalized_state::Chain},
    HashOrHeight,
};

/// Return the block identified by either its `height` or `hash` if it exists
/// in the non-finalized `chain` or finalized `db`.
pub(crate) fn block(
    chain: Option<&Arc<Chain>>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<Block>> {
    chain
        .and_then(|chain| chain.block(hash_or_height))
        .map(|contextual| contextual.block.clone())
        .or_else(|| db.block(hash_or_height))
}
