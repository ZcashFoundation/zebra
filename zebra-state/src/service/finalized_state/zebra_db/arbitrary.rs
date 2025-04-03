//! Arbitrary value generation and test harnesses for high-level typed database access.

#![allow(unused_imports)]

use std::ops::Deref;

use zebra_chain::{amount::NonNegative, block::Block, sprout, value_balance::ValueBalance};

use crate::service::finalized_state::{
    disk_db::{DiskDb, DiskWriteBatch, WriteDisk},
    ZebraDb,
};

// Enable older test code to automatically access the inner database via Deref coercion.
impl Deref for ZebraDb {
    type Target = DiskDb;

    fn deref(&self) -> &Self::Target {
        self.db()
    }
}

impl ZebraDb {
    /// Returns the inner database.
    ///
    /// This is a test-only and shielded-scan-only method, because it allows write access
    /// and raw read access to the RocksDB instance.
    pub fn db(&self) -> &DiskDb {
        &self.db
    }

    /// Allow to set up a fake value pool in the database for testing purposes.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn set_finalized_value_pool(&self, fake_value_pool: ValueBalance<NonNegative>) {
        let mut batch = DiskWriteBatch::new();
        let value_pool_cf = self.db().cf_handle("tip_chain_value_pool").unwrap();

        batch.zs_insert(&value_pool_cf, (), fake_value_pool);
        self.db().write(batch).unwrap();
    }

    /// Artificially prime the note commitment tree anchor sets with anchors
    /// referenced in a block, for testing purposes _only_.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn populate_with_anchors(&self, block: &Block) {
        let mut batch = DiskWriteBatch::new();

        let sprout_anchors = self.db().cf_handle("sprout_anchors").unwrap();
        let sapling_anchors = self.db().cf_handle("sapling_anchors").unwrap();
        let orchard_anchors = self.db().cf_handle("orchard_anchors").unwrap();

        let sprout_tree = sprout::tree::NoteCommitmentTree::default();
        // Calculate the root so we pass the tree with a cached root to the database. We need to do
        // that because the database checks if the tree has a cached root.
        sprout_tree.root();

        for transaction in block.transactions.iter() {
            // Sprout
            for joinsplit in transaction.sprout_groth16_joinsplits() {
                batch.zs_insert(&sprout_anchors, joinsplit.anchor, sprout_tree.clone());
            }

            // Sapling
            for anchor in transaction.sapling_anchors() {
                batch.zs_insert(&sapling_anchors, anchor, ());
            }

            // Orchard
            for shared_anchor in transaction.orchard_shared_anchors() {
                batch.zs_insert(&orchard_anchors, shared_anchor, ());
            }
        }

        self.db().write(batch).unwrap();
    }
}
