//! Provides high-level access to the database `spentness_hint` column family.
//!
//! This column family stores spentness-hint artifacts (the `ZSH1` byte format
//! from [`zebra_chain::parameters::spentness_hints`]) that have been verified
//! against the SHA-256 hash pinned in the release's [`MaxCheckpoint`] trust
//! root — either captured by this node when its sync frontier passed the
//! pinned height, or downloaded from a peer and verified before being
//! persisted. The pinned *hash* stays in `zebra-chain` as the trust root; the
//! verified artifact *bytes* live here in the state database so the node can
//! serve them over the v2 network protocol's `get-object` requests.
//!
//! The key is the [`Height`] of the final checkpoint the artifact covers,
//! serialized big-endian so RocksDB's lexicographic key ordering matches the
//! height order. The value is the serialized artifact, stored opaquely (this
//! module does not re-parse it).
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.
//!
//! [`MaxCheckpoint`]: zebra_chain::parameters::spentness_hints::MaxCheckpoint

use zebra_chain::block::Height;

use crate::service::finalized_state::{
    disk_db::DiskWriteBatch, disk_format::RawBytes, zebra_db::ZebraDb, TypedColumnFamily,
};

/// The name of the spentness-hint column family.
///
/// This constant should be used so the compiler can detect typos.
pub const SPENTNESS_HINT: &str = "spentness_hint";

/// The type for reading and writing spentness-hint artifacts from the database.
///
/// The key is the final checkpoint [`Height`] the artifact covers, and the
/// value is the verified artifact bytes stored opaquely as [`RawBytes`].
///
/// This type should be used so the compiler can detect incorrectly typed accesses
/// to the column family.
pub type SpentnessHintCf<'cf> = TypedColumnFamily<'cf, Height, RawBytes>;

impl ZebraDb {
    // Column family convenience methods

    /// Returns a typed handle to the `spentness_hint` column family, or `None`
    /// if it is not open.
    ///
    /// The column family is absent in read-only (secondary) opens of a database
    /// created before it existed, because a secondary instance cannot create new
    /// column families (see `DiskDb::construct_column_families`). Read-only
    /// consumers don't need it.
    pub(crate) fn spentness_hint_cf(&self) -> Option<SpentnessHintCf<'_>> {
        SpentnessHintCf::new(&self.db, SPENTNESS_HINT)
    }

    // Read methods

    /// Returns the verified spentness-hint artifact bytes stored for the final
    /// checkpoint `height`, if present (and if the column family is open).
    ///
    /// The bytes are the serialized artifact as it was captured or downloaded
    /// and verified before being written via
    /// [`ZebraDb::write_spentness_hint`]. Returns `None` when the column
    /// family is not open (read-only DB without it) or has no artifact at
    /// `height`.
    pub fn spentness_hint(&self, height: Height) -> Option<Vec<u8>> {
        self.spentness_hint_cf()?
            .zs_get(&height)
            .map(|raw| raw.raw_bytes().clone())
    }

    // Write methods

    /// Writes the verified spentness-hint artifact `bytes` at `height`,
    /// overwriting any existing artifact for that height.
    ///
    /// The caller is responsible for verifying `bytes` against the pinned
    /// SHA-256 constant for `height` before calling this method — captured
    /// artifacts are self-verifying because they are a deterministic function
    /// of this node's own finalized state. This method stores the bytes
    /// opaquely.
    ///
    /// This writes its own single-key batch atomically.
    pub fn write_spentness_hint(&self, height: Height, bytes: &[u8]) -> Result<(), rocksdb::Error> {
        let mut batch = DiskWriteBatch::new();
        batch.prepare_spentness_hint(self, height, bytes);
        self.write_batch(batch)
    }
}

impl DiskWriteBatch {
    /// Prepares a database batch entry that stores the verified spentness-hint
    /// artifact `bytes` at `height`, overwriting any existing artifact for
    /// that height.
    ///
    /// The caller is responsible for verifying `bytes` against the pinned
    /// SHA-256 constant for `height` before calling this method.
    ///
    /// The batch is modified by this method and written by the caller.
    pub fn prepare_spentness_hint(&mut self, db: &ZebraDb, height: Height, bytes: &[u8]) {
        // Writing only happens on a read/write database, where the column family
        // always exists (a read-only/secondary instance never writes).
        let _ = db
            .spentness_hint_cf()
            .expect("the spentness_hint column family exists on a read/write database")
            .with_batch_for_writing(self)
            .zs_insert(&height, &RawBytes::new_raw_bytes(bytes.to_vec()));
    }
}

#[cfg(test)]
mod tests {
    use zebra_chain::{block::Height, parameters::Network};

    use crate::{
        constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
        service::finalized_state::{ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE},
        Config,
    };

    /// Opens an ephemeral [`ZebraDb`] with all current column families.
    fn new_ephemeral_zebra_db(network: &Network) -> ZebraDb {
        ZebraDb::new(
            &Config::ephemeral(),
            STATE_DATABASE_KIND,
            &state_database_format_version_in_code(),
            network,
            // Skip format upgrades: this test only touches the spentness-hint CF.
            true,
            STATE_COLUMN_FAMILIES_IN_CODE
                .iter()
                .map(ToString::to_string),
            false,
        )
        .expect("test database opens")
    }

    /// Writing a spentness-hint artifact and reading it back returns the same
    /// bytes.
    #[test]
    fn spentness_hint_write_then_read_round_trips() {
        let _init_guard = zebra_test::init();

        let db = new_ephemeral_zebra_db(&Network::Mainnet);

        // An absent height reads as None.
        assert_eq!(db.spentness_hint(Height(0)), None, "empty CF returns None");

        let artifact: Vec<u8> = (0u8..=255).cycle().take(1000).collect();
        let other: Vec<u8> = vec![0xAB; 64];

        db.write_spentness_hint(Height(4_200_000), &artifact)
            .expect("ephemeral db accepts the write");
        db.write_spentness_hint(Height(3_000_000), &other)
            .expect("ephemeral db accepts the write");

        assert_eq!(
            db.spentness_hint(Height(4_200_000)).as_deref(),
            Some(artifact.as_slice()),
            "artifact round-trips at its height",
        );
        assert_eq!(
            db.spentness_hint(Height(3_000_000)).as_deref(),
            Some(other.as_slice()),
            "artifact round-trips at a different height",
        );

        // A different, never-written height is still absent.
        assert_eq!(
            db.spentness_hint(Height(1)),
            None,
            "unwritten height returns None",
        );

        // Overwriting an existing height replaces its contents.
        let artifact_v2: Vec<u8> = vec![0x11, 0x22, 0x33];
        db.write_spentness_hint(Height(4_200_000), &artifact_v2)
            .expect("ephemeral db accepts the overwrite");
        assert_eq!(
            db.spentness_hint(Height(4_200_000)).as_deref(),
            Some(artifact_v2.as_slice()),
            "overwriting a height replaces its contents",
        );
    }
}
