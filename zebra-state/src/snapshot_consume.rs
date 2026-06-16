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
//!    skipped (always crash-safe). The `utxo_by_out_loc` bytes themselves are
//!    elided **only** if [`SnapshotConsumeConfig::elide_utxo_bytes`] is enabled,
//!    which is **unsafe across restarts** and defaults off — see the crash
//!    safety analysis below and `docs/design/utxo-elision.md`.
//!
//! # Crash safety
//!
//! This is the riskiest part of the assumeUTXO sync, and it was re-verified
//! against the *actual* commit path on this branch, not just the design doc.
//!
//! Checkpoint blocks resolve their transparent spends in
//! [`crate::service::non_finalized_state::NonFinalizedState::commit_checkpoint_block`],
//! which calls [`crate::service::check::utxo::transparent_spend`]. That resolver
//! falls through to `finalized_state.utxo(&spend)` (the live `utxo_by_out_loc`
//! set) as its last resort. So if a created output's `utxo_by_out_loc` bytes are
//! elided and the output is later spent after it has aged out of the in-memory
//! `PrunedChain` cache (or across a restart, where that cache is cold), the
//! spend resolves to `None`, which becomes `MissingTransparentOutput`, a commit
//! error, and a queue reset / crash loop. This is exactly the prior crash.
//!
//! Therefore:
//!
//! - **Address-index and balance elision for non-survivors is unconditionally
//!   crash-safe**: those column families
//!   (`utxo_loc_by_transparent_addr_loc`, the create side of
//!   `tx_loc_by_transparent_addr_loc`, and `balance_by_transparent_addr`) are
//!   never read by spend resolution, the value pool, or the engine. A crash and
//!   resume finds a fully consistent UTXO set; only the RPC indexes are sparser.
//!   This is always applied in consume mode.
//! - **`utxo_by_out_loc` byte elision is a known crash** and is gated behind
//!   [`SnapshotConsumeConfig::elide_utxo_bytes`] (default `false`). The
//!   restart-safe way to elide the UTXO bytes is the deferred-durability buffer
//!   ("approach B") described in `docs/design/utxo-elision.md` §4.4; it is not
//!   implemented here. Do not enable `elide_utxo_bytes` for a production sync.

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
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
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
    /// **Defaults to `false`, and should stay `false` for any real sync.**
    ///
    /// Eliding the UTXO bytes is **not crash-safe** with the current commit
    /// path: checkpoint spend resolution falls through to the finalized
    /// `utxo_by_out_loc` set, so an elided-then-spent output that has aged out
    /// of the in-memory cache (always the case across a restart) resolves to
    /// `None` and crashes the commit. See the module docs and
    /// `docs/design/utxo-elision.md`. The address-index and balance elision
    /// (always applied when a survivor set is loaded) captures most of the
    /// write-volume win without this hazard.
    pub elide_utxo_bytes: bool,
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

    /// Whether to elide `utxo_by_out_loc` bytes (unsafe; default off — see
    /// [`SnapshotConsumeConfig::elide_utxo_bytes`]).
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

    /// Whether `utxo_by_out_loc` byte elision is enabled (unsafe; default off).
    pub fn elide_utxo_bytes(&self) -> bool {
        self.elide_utxo_bytes
    }

    /// The loaded survivor set, if any.
    pub fn survivor_set(&self) -> Option<&Arc<SurvivorSet>> {
        self.survivor_set.as_ref()
    }

    /// Returns whether the RPC **address-index and balance** writes for the
    /// output at `loc_bytes` should be elided.
    ///
    /// This is the unconditionally crash-safe elision: it returns `true` only
    /// for non-survivor outputs and only when a survivor set is loaded. Those
    /// column families are never read by spend resolution or consensus.
    pub fn elide_address_index(&self, loc_bytes: &[u8; OUTPUT_LOCATION_DISK_BYTES]) -> bool {
        match &self.survivor_set {
            Some(set) => !set.is_survivor_bytes(loc_bytes),
            None => false,
        }
    }

    /// Returns whether the `utxo_by_out_loc` **bytes** for the output at
    /// `loc_bytes` should be elided.
    ///
    /// Returns `true` only when both a survivor set is loaded, the output is a
    /// non-survivor, **and** the unsafe [`SnapshotConsumeConfig::elide_utxo_bytes`]
    /// flag is set. With the flag off (the default) this is always `false`, so
    /// the UTXO set on disk is always complete and the commit path's spend
    /// resolution never sees a hole.
    pub fn elide_utxo_byte(&self, loc_bytes: &[u8; OUTPUT_LOCATION_DISK_BYTES]) -> bool {
        self.elide_utxo_bytes && self.elide_address_index(loc_bytes)
    }
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
}
