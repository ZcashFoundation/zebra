//! Provides high-level access to the database `known_hash_chunk` column family.
//!
//! This column family stores known-hash chunks (the v2 chunk byte format defined
//! in [`zebra_chain::parameters::known_hashes::chunk_v2`]) that have been
//! downloaded from a peer and verified against the SHA-256 constants pinned in
//! `zebra-chain`. See `docs/design/p2p-snapshot-distribution.md` for the overall
//! architecture: the chunk *hashes* stay in `zebra-chain` as the trust root, but
//! the verified chunk *bytes* live here in the state database rather than in
//! shipped asset files.
//!
//! The key is the `u32` chunk index, serialized big-endian so RocksDB's
//! lexicographic key ordering matches the numeric chunk order. The value is the
//! verified v2 chunk bytes, stored opaquely (this module does not re-parse them).
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use crate::service::finalized_state::{
    disk_db::DiskWriteBatch, disk_format::RawBytes, zebra_db::ZebraDb, TypedColumnFamily,
};

/// The name of the known-hash chunk column family.
///
/// This constant should be used so the compiler can detect typos.
pub const KNOWN_HASH_CHUNK: &str = "known_hash_chunk";

/// The type for reading and writing known-hash chunks from the database.
///
/// The key is the `u32` chunk index (big-endian on disk for sort order), and the
/// value is the verified v2 chunk bytes stored opaquely as [`RawBytes`].
///
/// This type should be used so the compiler can detect incorrectly typed accesses
/// to the column family.
pub type KnownHashChunkCf<'cf> = TypedColumnFamily<'cf, u32, RawBytes>;

impl ZebraDb {
    // Column family convenience methods

    /// Returns a typed handle to the `known_hash_chunk` column family, or `None`
    /// if it is not open.
    ///
    /// The column family is absent in read-only (secondary) opens of a database
    /// created before it existed, because a secondary instance cannot create new
    /// column families (see `DiskDb::construct_column_families`). Read-only
    /// consumers don't need it: they generate chunks from state on demand
    /// ([`crate::known_hash_chunk_bytes`]) rather than reading the download cache.
    pub(crate) fn known_hash_chunk_cf(&self) -> Option<KnownHashChunkCf<'_>> {
        KnownHashChunkCf::new(&self.db, KNOWN_HASH_CHUNK)
    }

    // Read methods

    /// Returns the verified known-hash chunk bytes stored at `index`, if present
    /// (and if the column family is open).
    ///
    /// The bytes are the v2 chunk format
    /// ([`zebra_chain::parameters::known_hashes::chunk_v2`]) as they were
    /// downloaded and verified before being written via
    /// [`DiskWriteBatch::write_known_hash_chunk`]. Returns `None` when the column
    /// family is not open (read-only DB without it) or has no chunk at `index`.
    pub fn known_hash_chunk(&self, index: u32) -> Option<Vec<u8>> {
        self.known_hash_chunk_cf()?
            .zs_get(&index)
            .map(|raw| raw.raw_bytes().clone())
    }

    // Write methods

    /// Writes the verified known-hash chunk `bytes` at `index`, overwriting any
    /// existing chunk for that index.
    ///
    /// The caller is responsible for verifying `bytes` against the pinned
    /// SHA-256 constant for `index` before calling this method; this method
    /// stores the bytes opaquely.
    ///
    /// This writes its own single-key batch atomically.
    pub fn write_known_hash_chunk(&self, index: u32, bytes: &[u8]) -> Result<(), rocksdb::Error> {
        let mut batch = DiskWriteBatch::new();
        batch.prepare_known_hash_chunk(self, index, bytes);
        self.write_batch(batch)
    }
}

impl DiskWriteBatch {
    /// Prepares a database batch entry that stores the verified known-hash chunk
    /// `bytes` at `index`, overwriting any existing chunk for that index.
    ///
    /// The caller is responsible for verifying `bytes` against the pinned
    /// SHA-256 constant for `index` before calling this method.
    ///
    /// The batch is modified by this method and written by the caller.
    pub fn prepare_known_hash_chunk(&mut self, db: &ZebraDb, index: u32, bytes: &[u8]) {
        // The batch is modified by this method and written by the caller.
        // Writing only happens on a read/write database, where the column family
        // always exists (a read-only/secondary instance never writes).
        let _ = db
            .known_hash_chunk_cf()
            .expect("the known_hash_chunk column family exists on a read/write database")
            .with_batch_for_writing(self)
            .zs_insert(&index, &RawBytes::new_raw_bytes(bytes.to_vec()));
    }
}

#[cfg(test)]
mod tests {
    use zebra_chain::parameters::Network;

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
            // Skip format upgrades: this test only touches the known-hash chunk CF.
            true,
            STATE_COLUMN_FAMILIES_IN_CODE
                .iter()
                .map(ToString::to_string),
            false,
        )
    }

    /// Writing a known-hash chunk blob and reading it back returns the same bytes.
    #[test]
    fn known_hash_chunk_write_then_read_round_trips() {
        let _init_guard = zebra_test::init();

        let db = new_ephemeral_zebra_db(&Network::Mainnet);

        // An absent index reads as None.
        assert_eq!(db.known_hash_chunk(0), None, "empty CF returns None");

        // Two chunks with distinct indices and contents, including an empty blob.
        let chunk_0: Vec<u8> = (0u8..=255).cycle().take(1000).collect();
        let chunk_7: Vec<u8> = vec![0xAB; 64];
        let chunk_empty: Vec<u8> = Vec::new();

        db.write_known_hash_chunk(0, &chunk_0)
            .expect("ephemeral db accepts the write");
        db.write_known_hash_chunk(7, &chunk_7)
            .expect("ephemeral db accepts the write");
        db.write_known_hash_chunk(42, &chunk_empty)
            .expect("ephemeral db accepts the write");

        assert_eq!(
            db.known_hash_chunk(0).as_deref(),
            Some(chunk_0.as_slice()),
            "chunk 0 round-trips",
        );
        assert_eq!(
            db.known_hash_chunk(7).as_deref(),
            Some(chunk_7.as_slice()),
            "chunk 7 round-trips",
        );
        assert_eq!(
            db.known_hash_chunk(42).as_deref(),
            Some(chunk_empty.as_slice()),
            "empty chunk round-trips",
        );

        // A different, never-written index is still absent.
        assert_eq!(
            db.known_hash_chunk(1),
            None,
            "unwritten index between written ones returns None",
        );

        // Overwriting an existing index replaces its contents.
        let chunk_0_v2: Vec<u8> = vec![0x11, 0x22, 0x33];
        db.write_known_hash_chunk(0, &chunk_0_v2)
            .expect("ephemeral db accepts the overwrite");
        assert_eq!(
            db.known_hash_chunk(0).as_deref(),
            Some(chunk_0_v2.as_slice()),
            "overwriting an index replaces its contents",
        );
    }
}
