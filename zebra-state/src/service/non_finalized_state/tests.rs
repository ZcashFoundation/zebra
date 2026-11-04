#![allow(clippy::unwrap_in_result)]

mod prop;
mod vectors;

impl super::NonFinalizedState {
    /// Publishes a sparse fixture's parent and trees without revalidating its synthetic history.
    pub(crate) fn set_test_snapshot_parent(
        &mut self,
        parent: std::sync::Arc<zebra_chain::block::Block>,
        db: &super::ZebraDb,
    ) {
        use crate::arbitrary::Prepare;

        let height = parent.coinbase_height().unwrap();
        let hash = parent.hash();
        let mut chain = super::Chain::new(
            &self.network,
            height,
            db.note_commitment_trees_for_tip(),
            db.history_tree(),
            db.finalized_value_pool(),
        );
        chain.height_by_hash.insert(hash, height);
        chain.blocks.insert(
            height,
            std::sync::Arc::new(parent.prepare().test_with_zero_chain_pool_change()),
        );
        self.chain_set.clear();
        self.chain_set.insert(std::sync::Arc::new(chain));
    }
}
