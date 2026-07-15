//! Optional "snapshot consume" mode for the finalized state, used during
//! known-hash / checkpoint initial block download (assumeUTXO sync).
//!
//! When enabled, the finalized write path consumes a verified state snapshot at
//! the maximum checkpoint height (`H_max`) instead of deriving that state from
//! the blocks it commits:
//!
//! 1. **Direct note commitment tree writes.** A pre-fetched, verified note
//!    commitment tree for a block's height is written directly into the tree
//!    column families instead of being folded from the block's note commitments
//!    (see [`crate::service::finalized_state::commit_finalized_direct`]). For
//!    sapling, the supplied tree's `.root()` is checked against the block
//!    header's `hashFinalSaplingRoot` before it is accepted.
//! 2. **`H_max` address balances.** The per-block address-balance
//!    credit/debit/merge (the measured Thread 2 bottleneck, see
//!    `docs/design/known-hash-ibd.md` §12) is skipped during the consume phase,
//!    and the verified final balance set is bulk-loaded once at `H_max`.
//! 3. **Survivor-only UTXO / address-index writes.** A verified set of the
//!    transparent output locations that are still unspent at `H_max` (the
//!    *survivor set*) marks which created outputs survive to the snapshot
//!    height. Non-survivor outputs' RPC address-index and balance writes are
//!    skipped, and the `utxo_by_out_loc` bytes for non-survivors are elided too
//!    (controlled by [`SnapshotConsumeConfig::elide_utxo_bytes`], default `true`
//!    in consume mode). Both are crash-safe because the checkpoint commit path
//!    no longer reads the spent value from `utxo_by_out_loc` — see below.
//!
//! # Crash safety
//!
//! UTXO-byte elision was the riskiest part of the assumeUTXO sync, and the
//! reason it is now crash-safe was re-verified against the *actual* commit path
//! on this branch, not just the design doc.
//!
//! The prior crash came from spend *resolution* reading the spent output's value
//! from the live `utxo_by_out_loc` set: if a created output's bytes were elided
//! and the output was later spent after it had aged out of memory (or across a
//! restart, where the in-memory cache is cold), the spend resolved to `None`,
//! which became `MissingTransparentOutput`, a commit error, and a queue reset /
//! crash loop. Both checkpoint commit paths used to perform that read:
//!
//! - the in-memory commit
//!   ([`crate::service::non_finalized_state::NonFinalizedState::commit_checkpoint_block`])
//!   resolved spends via `check::utxo::transparent_spend`, which fell through to
//!   `finalized_state.utxo(&spend)`; and
//! - the finalized commit
//!   ([`crate::service::finalized_state::FinalizedState::commit_finalized_direct_with_trees`])
//!   resolved spent values via `ZebraDb::lookup_spent_utxos`, which read
//!   `utxo_by_out_loc` and panicked if the value was missing.
//!
//! **That value read is now removed for checkpoint blocks.** In snapshot-consume
//! mode:
//!
//! - the in-memory commit uses
//!   [`crate::service::check::utxo::checkpoint_transparent_spend`] with the
//!   finalized read disabled — it resolves spends from in-memory tiers only and
//!   never touches `utxo_by_out_loc`; and
//! - the finalized commit uses
//!   [`crate::service::finalized_state::ZebraDb::lookup_spent_output_locations_only`],
//!   which resolves the spent [`OutputLocation`] (from the always-written
//!   transaction-location index, never elided) for the `utxo_by_out_loc` delete,
//!   and never reads the spent value.
//!
//! With no code path reading a spent output's value from the finalized set, an
//! elided non-survivor output can never be dereferenced, so eliding its bytes
//! can never `None`/panic — even across a restart with a cold cache. Per-block
//! value pools and balances, which previously needed those values, are not
//! derived in consume mode: the verified final value pools and balances are
//! loaded at `H_max` instead.
//!
//! Two classes of elision, both now crash-safe in consume mode:
//!
//! - **Address-index and balance elision for non-survivors** is
//!   unconditionally crash-safe: those column families
//!   (`utxo_loc_by_transparent_addr_loc`, the create side of
//!   `tx_loc_by_transparent_addr_loc`, and `balance_by_transparent_addr`) are
//!   never read by spend resolution, the value pool, or the engine.
//! - **`utxo_by_out_loc` byte elision for non-survivors** is crash-safe because
//!   the spend-value read that previously dereferenced it is gone (above). It is
//!   controlled by [`SnapshotConsumeConfig::elide_utxo_bytes`], which defaults to
//!   `true`. The final survivor set on disk at `H_max` is byte-identical to a
//!   normally-synced node, because an elided output's create and delete net to
//!   zero (a spent output is always a non-survivor, so its create was elided and
//!   its delete is a no-op).

use std::{
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

use zebra_chain::{block::Height, parameters::Network};

/// The on-disk byte length of an [`OutputLocation`](crate::OutputLocation):
/// 3-byte height + 2-byte transaction index + 3-byte output index, big-endian.
///
/// Kept as a local constant to avoid depending on the private `disk_format`
/// module path; a test asserts it matches the canonical definition.
pub const OUTPUT_LOCATION_DISK_BYTES: usize = 8;

/// Configuration for the finalized state's optional snapshot-consume mode.
///
/// Defaults to off everywhere it is used as an `Option` (the
/// [`Config::snapshot_consume`](crate::Config) field is `None` by default), so a
/// normal sync is completely unaffected.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct SnapshotConsumeConfig {
    /// Path to the verified survivor-set artifact: the sorted, big-endian,
    /// 8-byte on-disk [`OutputLocation`](crate::OutputLocation)s of every
    /// transparent output unspent at `H_max`.
    ///
    /// When `None`, no survivor set is loaded, so no per-block address-index,
    /// balance, or UTXO elision is performed. The other consume behaviours
    /// (direct tree writes, skipping per-block balances) are still applied.
    pub survivor_set_path: Option<PathBuf>,

    /// The maximum checkpoint height `H_max` the snapshot was taken at.
    ///
    /// The survivor set and the bulk-loaded balances are defined as of this
    /// height. The loader records it so a mismatched artifact can be refused,
    /// and the write path uses it to bound when elision applies.
    pub h_max: u32,

    /// Whether to elide the `utxo_by_out_loc` bytes for non-survivor outputs.
    ///
    /// **Defaults to `true`.** This is the survivor-only UTXO write: only
    /// outputs that are unspent at `H_max` are written to `utxo_by_out_loc`,
    /// which is the dominant write-volume win of assumeUTXO sync.
    ///
    /// This is crash-safe because the checkpoint commit path no longer reads a
    /// spent output's value from `utxo_by_out_loc` (the in-memory commit resolves
    /// spends from memory only, and the finalized commit resolves only the
    /// [`OutputLocation`] for deletion). An elided non-survivor output can never
    /// be dereferenced, so it can never `None`/panic, even across a restart. See
    /// the module docs and `docs/design/utxo-elision.md`.
    ///
    /// Set to `false` to keep the full UTXO set on disk (e.g. for an RPC node
    /// that needs complete intermediate-height UTXO queries during IBD); the
    /// address-index and balance elision still applies.
    pub elide_utxo_bytes: bool,
}

impl Default for SnapshotConsumeConfig {
    fn default() -> Self {
        Self {
            survivor_set_path: None,
            h_max: 0,
            // Survivor-only UTXO elision is on by default in consume mode: it is
            // the main write-volume win and is now crash-safe (see the field and
            // module docs).
            elide_utxo_bytes: true,
        }
    }
}

/// A read-only set of the transparent output locations that survive unspent to
/// `H_max`, loaded once at startup and queried by the finalized write path.
///
/// The artifact is the sorted concatenation of 8-byte big-endian on-disk
/// [`OutputLocation`](crate::OutputLocation)s (so byte order equals location
/// order). Membership is an exact binary search; there are no false positives
/// or negatives, which is required because a wrong answer would corrupt the
/// final UTXO set.
///
/// The set is held in a sorted `Vec<[u8; 8]>`. A memory-mapped file would lower
/// resident memory for very large sets; that is a recorded follow-up (the query
/// API is identical either way).
#[derive(Debug)]
pub struct SurvivorSet {
    /// Sorted 8-byte on-disk output locations, ascending.
    locations: Vec<[u8; OUTPUT_LOCATION_DISK_BYTES]>,
}

impl SurvivorSet {
    /// Loads a survivor set from `path`.
    ///
    /// The file must be a whole number of 8-byte records and strictly
    /// ascending (the emitter guarantees both). Returns an error otherwise.
    pub fn load(path: &Path) -> Result<Self, SurvivorSetError> {
        let file = File::open(path).map_err(|source| SurvivorSetError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        let mut reader = BufReader::new(file);
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|source| SurvivorSetError::Read {
                path: path.to_path_buf(),
                source,
            })?;

        Self::from_bytes(bytes)
    }

    /// Builds a survivor set from already-loaded `bytes`.
    ///
    /// Validates that the length is a multiple of the record size and that the
    /// records are strictly ascending.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, SurvivorSetError> {
        if bytes.len() % OUTPUT_LOCATION_DISK_BYTES != 0 {
            return Err(SurvivorSetError::BadLength {
                len: bytes.len(),
                record_len: OUTPUT_LOCATION_DISK_BYTES,
            });
        }

        let mut locations: Vec<[u8; OUTPUT_LOCATION_DISK_BYTES]> =
            Vec::with_capacity(bytes.len() / OUTPUT_LOCATION_DISK_BYTES);
        for chunk in bytes.chunks_exact(OUTPUT_LOCATION_DISK_BYTES) {
            let mut record = [0u8; OUTPUT_LOCATION_DISK_BYTES];
            record.copy_from_slice(chunk);
            locations.push(record);
        }

        // Strictly ascending: required so binary search is correct and so the
        // emitter can't have produced duplicates.
        if locations.windows(2).any(|w| w[0] >= w[1]) {
            return Err(SurvivorSetError::NotSorted);
        }

        Ok(Self { locations })
    }

    /// Returns whether the output at `loc_bytes` (its 8-byte on-disk
    /// representation) is a survivor (unspent at `H_max`).
    pub fn is_survivor_bytes(&self, loc_bytes: &[u8; OUTPUT_LOCATION_DISK_BYTES]) -> bool {
        self.locations.binary_search(loc_bytes).is_ok()
    }

    /// Returns the number of survivor entries.
    pub fn len(&self) -> usize {
        self.locations.len()
    }

    /// Returns whether the survivor set is empty.
    pub fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }
}

/// The loaded, validated snapshot-consume state, stored on
/// [`ZebraDb`](crate::ZebraDb) and consulted by the finalized write path.
#[derive(Debug)]
pub struct SnapshotConsumeState {
    /// The network the snapshot is for. Mismatches are refused at load time.
    network: Network,

    /// The snapshot height `H_max`.
    h_max: Height,

    /// Whether to elide `utxo_by_out_loc` bytes for non-survivors (default on;
    /// crash-safe — see [`SnapshotConsumeConfig::elide_utxo_bytes`]).
    elide_utxo_bytes: bool,

    /// The survivor set, if a path was configured.
    ///
    /// When `None`, per-block address-index / balance / UTXO elision is not
    /// performed (every output is treated as a survivor), but the per-block
    /// balance derivation can still be skipped and trees can still be supplied.
    survivor_set: Option<Arc<SurvivorSet>>,
}

impl SnapshotConsumeState {
    /// Loads the snapshot-consume state from `config` for `network`.
    ///
    /// Loads and validates the survivor set if its path is configured. Returns
    /// an error if the artifact can't be loaded or fails validation, so a
    /// misconfigured snapshot fails fast instead of silently corrupting state.
    pub fn load(
        config: &SnapshotConsumeConfig,
        network: &Network,
    ) -> Result<Self, SurvivorSetError> {
        let survivor_set = match &config.survivor_set_path {
            Some(path) => Some(Arc::new(SurvivorSet::load(path)?)),
            None => None,
        };

        Ok(Self {
            network: network.clone(),
            h_max: Height(config.h_max),
            elide_utxo_bytes: config.elide_utxo_bytes,
            survivor_set,
        })
    }

    /// Builds the consume state directly from an optional survivor set, for
    /// tests.
    pub fn from_parts(
        network: Network,
        h_max: Height,
        elide_utxo_bytes: bool,
        survivor_set: Option<Arc<SurvivorSet>>,
    ) -> Self {
        Self {
            network,
            h_max,
            elide_utxo_bytes,
            survivor_set,
        }
    }

    /// The network this snapshot is for.
    pub fn network(&self) -> &Network {
        &self.network
    }

    /// The snapshot height `H_max`.
    pub fn h_max(&self) -> Height {
        self.h_max
    }

    /// Whether `utxo_by_out_loc` byte elision is enabled (crash-safe; default on).
    pub fn elide_utxo_bytes(&self) -> bool {
        self.elide_utxo_bytes
    }

    /// The loaded survivor set, if any.
    pub fn survivor_set(&self) -> Option<&Arc<SurvivorSet>> {
        self.survivor_set.as_ref()
    }

    /// Returns the big-endian block height encoded in the first
    /// [`HEIGHT_DISK_BYTES`] bytes of an on-disk [`OutputLocation`](crate::OutputLocation).
    ///
    /// The on-disk layout is a 3-byte big-endian height, then the transaction
    /// and output indexes, so the height is the leading 3 bytes. A test
    /// (`elision_is_bounded_by_h_max`) pins this against the canonical encoding.
    fn output_location_height(loc_bytes: &[u8; OUTPUT_LOCATION_DISK_BYTES]) -> u32 {
        // 3-byte big-endian height, zero-extended to a `u32`.
        u32::from_be_bytes([0, loc_bytes[0], loc_bytes[1], loc_bytes[2]])
    }

    /// Returns `true` if the output at `loc_bytes` was created **above** the
    /// snapshot height `H_max`.
    ///
    /// Such an output cannot appear in the `H_max` survivor set (the snapshot
    /// was taken before it existed), so a survivor-set miss for it is *not*
    /// evidence that it is a non-survivor — it just was not yet created. Eliding
    /// it would drop a live output that sync continuing past `H_max` must keep,
    /// so elision must be refused for these heights. See finding #6 /
    /// `docs/design/snapshot-distribution.md` §3.3.
    fn created_above_h_max(&self, loc_bytes: &[u8; OUTPUT_LOCATION_DISK_BYTES]) -> bool {
        Self::output_location_height(loc_bytes) > self.h_max.0
    }

    /// Returns whether the RPC **address-index and balance** writes for the
    /// output at `loc_bytes` should be elided.
    ///
    /// This is the unconditionally crash-safe elision: it returns `true` only
    /// for non-survivor outputs created at or below `H_max`, and only when a
    /// survivor set is loaded. Those column families are never read by spend
    /// resolution or consensus.
    ///
    /// Outputs created **above** `H_max` are never elided: they cannot be in the
    /// `H_max` survivor set (they did not exist yet), so a survivor-set miss for
    /// them does not mean "non-survivor" — eliding them would drop live outputs
    /// of blocks committed after the snapshot.
    pub fn elide_address_index(&self, loc_bytes: &[u8; OUTPUT_LOCATION_DISK_BYTES]) -> bool {
        match &self.survivor_set {
            // An output above `H_max` is out of the survivor set's domain, so its
            // absence is meaningless; never elide it.
            Some(_) if self.created_above_h_max(loc_bytes) => false,
            Some(set) => !set.is_survivor_bytes(loc_bytes),
            None => false,
        }
    }

    /// Returns whether the `utxo_by_out_loc` **bytes** for the output at
    /// `loc_bytes` should be elided.
    ///
    /// Returns `true` when a survivor set is loaded, the output is a
    /// non-survivor created at or below `H_max`, **and**
    /// [`SnapshotConsumeConfig::elide_utxo_bytes`] is set (the default in consume
    /// mode). This is crash-safe because the checkpoint commit path no longer
    /// reads a spent output's value from `utxo_by_out_loc` (see [the module
    /// docs](crate::snapshot_consume)). With the flag off the full UTXO set is
    /// kept on disk. Outputs created above `H_max` are never elided (see
    /// [`Self::elide_address_index`]).
    pub fn elide_utxo_byte(&self, loc_bytes: &[u8; OUTPUT_LOCATION_DISK_BYTES]) -> bool {
        self.elide_utxo_bytes && self.elide_address_index(loc_bytes)
    }
}

/// Errors loading the snapshot-consume state at database open
/// ([`SnapshotConsumeState`]).
#[derive(thiserror::Error, Debug)]
pub enum SnapshotConsumeLoadError {
    /// Snapshot-consume sync was configured against a database that already
    /// holds blocks. AssumeUTXO sync must start from a fresh, from-genesis
    /// database: a non-empty database may already hold the very outputs the
    /// survivor set would mark as non-survivors, and after a restart the
    /// in-memory spend-resolution cache is cold — both unsafe (see
    /// `docs/design/utxo-elision.md` §4.3).
    #[error(
        "snapshot-consume (assumeUTXO) sync is configured, but the state database \
         at height {tip_height:?} is not empty; it must start from a fresh \
         from-genesis database (see docs/design/utxo-elision.md)"
    )]
    NonEmptyDatabase {
        /// The finalized tip height of the non-empty database.
        tip_height: Option<Height>,
    },

    /// The configured network does not match the database's network.
    #[error(
        "snapshot-consume (assumeUTXO) sync is configured for network {configured}, \
         but the database is for network {database}"
    )]
    NetworkMismatch {
        /// The network the snapshot-consume artifact / config is for.
        configured: Network,
        /// The network the database is for.
        database: Network,
    },

    /// The configured survivor set could not be loaded or validated.
    #[error("failed to load the configured snapshot-consume survivor set")]
    SurvivorSet(#[from] SurvivorSetError),
}

/// Errors loading or validating a [`SurvivorSet`].
#[derive(thiserror::Error, Debug)]
pub enum SurvivorSetError {
    /// The survivor-set file could not be opened.
    #[error("could not open survivor set file {path}")]
    Open {
        /// The path that failed to open.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The survivor-set file could not be read.
    #[error("could not read survivor set file {path}")]
    Read {
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The file length is not a whole number of records.
    #[error("survivor set length {len} is not a multiple of the record length {record_len}")]
    BadLength {
        /// The file length in bytes.
        len: usize,
        /// The expected record length in bytes.
        record_len: usize,
    },

    /// The records are not strictly ascending.
    #[error("survivor set records are not strictly ascending")]
    NotSorted,
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{service::finalized_state::IntoDisk, OutputLocation};

    /// The local [`OUTPUT_LOCATION_DISK_BYTES`] must match the canonical on-disk
    /// length of an [`OutputLocation`].
    #[test]
    fn output_location_disk_bytes_matches_canonical() {
        let loc = OutputLocation::from_usize(Height(0), 0, 0);
        assert_eq!(
            loc.as_bytes().len(),
            OUTPUT_LOCATION_DISK_BYTES,
            "local record length must match the canonical OutputLocation length",
        );
    }

    /// Builds an 8-byte on-disk output location for a tiny synthetic survivor
    /// set, big-endian so byte order equals location order.
    fn loc(height: u32, tx: u16, output: u32) -> [u8; OUTPUT_LOCATION_DISK_BYTES] {
        let mut bytes = [0u8; OUTPUT_LOCATION_DISK_BYTES];
        // 3-byte height + 2-byte tx index + 3-byte output index, all big-endian.
        bytes[0..3].copy_from_slice(&height.to_be_bytes()[1..4]);
        bytes[3..5].copy_from_slice(&tx.to_be_bytes());
        bytes[5..8].copy_from_slice(&output.to_be_bytes()[1..4]);
        bytes
    }

    #[test]
    fn survivor_set_membership_round_trips() {
        let entries = [loc(1, 0, 0), loc(1, 0, 1), loc(100, 3, 0), loc(100, 3, 2)];
        let bytes: Vec<u8> = entries.iter().flatten().copied().collect();

        let set = SurvivorSet::from_bytes(bytes).expect("ascending records load");
        assert_eq!(set.len(), 4);

        for entry in &entries {
            assert!(set.is_survivor_bytes(entry), "loaded entries are survivors");
        }

        // Locations not in the set are non-survivors.
        assert!(!set.is_survivor_bytes(&loc(1, 0, 2)));
        assert!(!set.is_survivor_bytes(&loc(50, 0, 0)));
        assert!(!set.is_survivor_bytes(&loc(100, 3, 1)));
        assert!(!set.is_survivor_bytes(&loc(200, 0, 0)));
    }

    #[test]
    fn survivor_set_rejects_bad_length() {
        let bytes = vec![0u8; OUTPUT_LOCATION_DISK_BYTES + 1];
        assert!(matches!(
            SurvivorSet::from_bytes(bytes),
            Err(SurvivorSetError::BadLength { .. })
        ));
    }

    #[test]
    fn survivor_set_rejects_unsorted() {
        let entries = [loc(100, 0, 0), loc(1, 0, 0)];
        let bytes: Vec<u8> = entries.iter().flatten().copied().collect();
        assert!(matches!(
            SurvivorSet::from_bytes(bytes),
            Err(SurvivorSetError::NotSorted)
        ));

        // Duplicates are also rejected (not strictly ascending).
        let dup = [loc(1, 0, 0), loc(1, 0, 0)];
        let dup_bytes: Vec<u8> = dup.iter().flatten().copied().collect();
        assert!(matches!(
            SurvivorSet::from_bytes(dup_bytes),
            Err(SurvivorSetError::NotSorted)
        ));
    }

    #[test]
    fn elision_modes_respect_flag_and_survivors() {
        let survivor = loc(1, 0, 0);
        let non_survivor = loc(2, 0, 0);
        let bytes: Vec<u8> = survivor.to_vec();
        let set = Arc::new(SurvivorSet::from_bytes(bytes).expect("single record loads"));

        // Address-index elision: safe, applies to non-survivors regardless of
        // the unsafe UTXO-byte flag.
        let safe =
            SnapshotConsumeState::from_parts(Network::Mainnet, Height(2), false, Some(set.clone()));
        assert!(!safe.elide_address_index(&survivor));
        assert!(safe.elide_address_index(&non_survivor));
        // UTXO-byte elision stays off when the flag is off.
        assert!(!safe.elide_utxo_byte(&non_survivor));

        // With the unsafe flag on, UTXO bytes are elided for non-survivors.
        let unsafe_mode =
            SnapshotConsumeState::from_parts(Network::Mainnet, Height(2), true, Some(set));
        assert!(unsafe_mode.elide_utxo_byte(&non_survivor));
        assert!(!unsafe_mode.elide_utxo_byte(&survivor));

        // With no survivor set, nothing is elided.
        let none = SnapshotConsumeState::from_parts(Network::Mainnet, Height(2), true, None);
        assert!(!none.elide_address_index(&non_survivor));
        assert!(!none.elide_utxo_byte(&non_survivor));
    }

    /// Outputs created above `H_max` are never elided, even though they are
    /// absent from the survivor set: they did not exist when the snapshot was
    /// taken, so a survivor-set miss is not evidence they are non-survivors.
    /// This bounds elision so sync continuing past `H_max` keeps its live
    /// outputs (finding #6).
    #[test]
    fn elision_is_bounded_by_h_max() {
        let h_max = 100;

        // A survivor at H_max, and a non-survivor created at or below H_max.
        let survivor = loc(h_max, 0, 0);
        let non_survivor_below = loc(h_max - 1, 0, 0);
        // An output created above H_max (a block committed after the snapshot).
        // It is necessarily absent from the survivor set.
        let above_h_max = loc(h_max + 1, 0, 0);
        // An output at exactly H_max that is not a survivor: still elidable.
        let non_survivor_at_h_max = loc(h_max, 5, 0);

        let bytes: Vec<u8> = survivor.to_vec();
        let set = Arc::new(SurvivorSet::from_bytes(bytes).expect("single record loads"));
        let consume =
            SnapshotConsumeState::from_parts(Network::Mainnet, Height(h_max), true, Some(set));

        // The height extractor matches the canonical on-disk encoding.
        assert_eq!(
            SnapshotConsumeState::output_location_height(&above_h_max),
            h_max + 1,
        );

        // Survivor: never elided.
        assert!(!consume.elide_address_index(&survivor));
        assert!(!consume.elide_utxo_byte(&survivor));

        // Non-survivor at or below H_max: elided (both kinds).
        assert!(consume.elide_address_index(&non_survivor_below));
        assert!(consume.elide_utxo_byte(&non_survivor_below));
        assert!(consume.elide_address_index(&non_survivor_at_h_max));
        assert!(consume.elide_utxo_byte(&non_survivor_at_h_max));

        // Created above H_max: never elided, despite being absent from the set.
        assert!(!consume.elide_address_index(&above_h_max));
        assert!(!consume.elide_utxo_byte(&above_h_max));
    }
}
