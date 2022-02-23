//! Arbitrary value generation and test harnesses for the finalized state.

#![allow(dead_code)]

use std::sync::Arc;

use proptest::prelude::*;

use zebra_chain::{
    amount::NonNegative,
    block::{self, Block},
    sprout,
    value_balance::ValueBalance,
};

use crate::service::finalized_state::{
    disk_db::{DiskWriteBatch, WriteDisk},
    disk_format::{FromDisk, IntoDisk, TransactionLocation},
    FinalizedState,
};

impl Arbitrary for TransactionLocation {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (any::<block::Height>(), any::<u32>())
            .prop_map(|(height, index)| Self { height, index })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

pub fn round_trip<T>(input: T) -> T
where
    T: IntoDisk + FromDisk,
{
    let bytes = input.as_bytes();
    T::from_bytes(bytes)
}

pub fn assert_round_trip<T>(input: T)
where
    T: IntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
{
    let before = input.clone();
    let after = round_trip(input);
    assert_eq!(before, after);
}

pub fn round_trip_ref<T>(input: &T) -> T
where
    T: IntoDisk + FromDisk,
{
    let bytes = input.as_bytes();
    T::from_bytes(bytes)
}

pub fn assert_round_trip_ref<T>(input: &T)
where
    T: IntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
{
    let before = input;
    let after = round_trip_ref(input);
    assert_eq!(before, &after);
}

pub fn round_trip_arc<T>(input: Arc<T>) -> T
where
    T: IntoDisk + FromDisk,
{
    let bytes = input.as_bytes();
    T::from_bytes(bytes)
}

pub fn assert_round_trip_arc<T>(input: Arc<T>)
where
    T: IntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
{
    let before = input.clone();
    let after = round_trip_arc(input);
    assert_eq!(*before, after);
}

/// The round trip test covers types that are used as value field in a rocksdb
/// column family. Only these types are ever deserialized, and so they're the only
/// ones that implement both `IntoDisk` and `FromDisk`.
pub fn assert_value_properties<T>(input: T)
where
    T: IntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
{
    assert_round_trip_ref(&input);
    assert_round_trip_arc(Arc::new(input.clone()));
    assert_round_trip(input);
}

impl FinalizedState {
    /// Allow to set up a fake value pool in the database for testing purposes.
    pub fn set_finalized_value_pool(&self, fake_value_pool: ValueBalance<NonNegative>) {
        let mut batch = DiskWriteBatch::new();
        let value_pool_cf = self.db.cf_handle("tip_chain_value_pool").unwrap();

        batch.zs_insert(value_pool_cf, (), fake_value_pool);
        self.db.write(batch).unwrap();
    }

    /// Artificially prime the note commitment tree anchor sets with anchors
    /// referenced in a block, for testing purposes _only_.
    pub fn populate_with_anchors(&self, block: &Block) {
        let mut batch = DiskWriteBatch::new();

        let sprout_anchors = self.db.cf_handle("sprout_anchors").unwrap();
        let sapling_anchors = self.db.cf_handle("sapling_anchors").unwrap();
        let orchard_anchors = self.db.cf_handle("orchard_anchors").unwrap();

        for transaction in block.transactions.iter() {
            // Sprout
            for joinsplit in transaction.sprout_groth16_joinsplits() {
                batch.zs_insert(
                    sprout_anchors,
                    joinsplit.anchor,
                    sprout::tree::NoteCommitmentTree::default(),
                );
            }

            // Sapling
            for anchor in transaction.sapling_anchors() {
                batch.zs_insert(sapling_anchors, anchor, ());
            }

            // Orchard
            if let Some(orchard_shielded_data) = transaction.orchard_shielded_data() {
                batch.zs_insert(orchard_anchors, orchard_shielded_data.shared_anchor, ());
            }
        }

        self.db.write(batch).unwrap();
    }
}
