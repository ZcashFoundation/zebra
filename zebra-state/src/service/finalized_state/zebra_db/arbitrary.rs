//! Arbitrary value generation and test harnesses for high-level typed database access.

#![allow(dead_code)]

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
    /// This is a test-only method, because it allows write access
    /// and raw read access to the RocksDB instance.
    pub fn db(&self) -> &DiskDb {
        &self.db
    }

    /// Allow to set up a fake value pool in the database for testing purposes.
    pub fn set_finalized_value_pool(&self, fake_value_pool: ValueBalance<NonNegative>) {
        let mut batch = DiskWriteBatch::new();
        let value_pool_cf = self.db().cf_handle("tip_chain_value_pool").unwrap();

        batch.zs_insert(&value_pool_cf, (), fake_value_pool);
        self.db().write(batch).unwrap();
    }

    /// Artificially prime the note commitment tree anchor sets with anchors
    /// referenced in a block, for testing purposes _only_.
    pub fn populate_with_anchors(&self, block: &Block) {
        let mut batch = DiskWriteBatch::new();

        let sprout_anchors = self.db().cf_handle("sprout_anchors").unwrap();
        let sapling_anchors = self.db().cf_handle("sapling_anchors").unwrap();
        let orchard_anchors = self.db().cf_handle("orchard_anchors").unwrap();

        for transaction in block.transactions.iter() {
            // Sprout
            for joinsplit in transaction.sprout_groth16_joinsplits() {
                batch.zs_insert(
                    &sprout_anchors,
                    joinsplit.anchor,
                    sprout::tree::NoteCommitmentTree::default(),
                );
            }

            // Sapling
            for anchor in transaction.sapling_anchors() {
                batch.zs_insert(&sapling_anchors, anchor, ());
            }

            // Orchard
            if let Some(orchard_shielded_data) = transaction.orchard_shielded_data() {
                batch.zs_insert(&orchard_anchors, orchard_shielded_data.shared_anchor, ());
            }
        }

        self.db().write(batch).unwrap();
    }
}
