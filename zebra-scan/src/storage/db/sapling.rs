//! Sapling-specific database reading and writing.

use zebra_state::AsColumnFamilyRef;

use crate::storage::Storage;

/// The name of the sapling transaction IDs result column family.
///
/// This constant should be used so the compiler can detect typos.
pub const SAPLING_TX_IDS: &str = "sapling_tx_ids";

impl Storage {
    /// Returns a handle to the `sapling_tx_ids` column family.
    pub fn sapling_tx_ids_cf(&self) -> impl AsColumnFamilyRef + '_ {
        self.db.cf_handle(SAPLING_TX_IDS).unwrap()
    }
}
