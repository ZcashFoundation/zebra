//! The durable construction record, stored with each atomic block batch.
//!
//! The `spentness_artifact` column family holds one row, keyed by the
//! artifact's SHA-256 (its pinned `spentness_hash`):
//!
//! ```text
//! Applying { height, next_ordinal, omitted, resolved }  ->  Complete
//! ```
//!
//! The value is fixed-width and big-endian:
//!
//! | offset | len | field |
//! |---|---|---|
//! | 0  | 1 | record version, [`RECORD_VERSION`] |
//! | 1  | 1 | phase: `0` applying, `1` complete |
//! | 2  | 4 | `height`: the last block committed with hints |
//! | 6  | 8 | `next_ordinal`: the artifact bit of the next block's first output |
//! | 14 | 8 | `omitted`: outputs omitted after genesis |
//! | 22 | 8 | `resolved`: spends resolved to omitted outputs |
//!
//! A `Complete` record keeps the final counts, so an operator can audit it.
//!
//! Ported from `zakura/feat/spentness-writer` (`a909419a59`, `spentness/record.rs`)
//! without its JSON encoding, which `zebra-state` only has as an optional
//! dependency, and without its rollback floor: Zebra's finalized state never
//! rolls back.

use zebra_chain::block::Height;

use super::{SpentnessError, SPENTNESS_ARTIFACT};
use crate::service::finalized_state::{
    disk_db::{DiskWriteBatch, ReadDisk, WriteDisk},
    disk_format::RawBytes,
    zebra_db::ZebraDb,
};

/// The record format version.
const RECORD_VERSION: u8 = 1;

/// The exact record length.
const RECORD_LEN: usize = 30;

/// The construction phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Phase {
    /// Blocks through `height` are committed with hints, and `H` is not reached.
    Applying,
    /// The block at `H` is committed, and every omitted output was spent.
    Complete,
}

/// The durable construction progress for one artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Progress {
    pub(super) phase: Phase,
    pub(super) height: Height,
    pub(super) next_ordinal: u64,
    pub(super) omitted: u64,
    pub(super) resolved: u64,
}

impl Progress {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(RECORD_LEN);
        bytes.push(RECORD_VERSION);
        bytes.push(match self.phase {
            Phase::Applying => 0,
            Phase::Complete => 1,
        });
        bytes.extend_from_slice(&self.height.0.to_be_bytes());
        bytes.extend_from_slice(&self.next_ordinal.to_be_bytes());
        bytes.extend_from_slice(&self.omitted.to_be_bytes());
        bytes.extend_from_slice(&self.resolved.to_be_bytes());
        bytes
    }

    #[allow(clippy::unwrap_in_result)]
    fn decode(bytes: &[u8]) -> Result<Self, SpentnessError> {
        const BAD: SpentnessError = SpentnessError::Inconsistent("malformed construction record");

        let bytes: &[u8; RECORD_LEN] = bytes.try_into().map_err(|_| BAD)?;
        if bytes[0] != RECORD_VERSION {
            return Err(BAD);
        }
        let phase = match bytes[1] {
            0 => Phase::Applying,
            1 => Phase::Complete,
            _ => return Err(BAD),
        };
        let u64_at = |offset: usize| {
            u64::from_be_bytes(
                bytes[offset..offset + 8]
                    .try_into()
                    .expect("the record has a fixed length, checked above"),
            )
        };

        Ok(Self {
            phase,
            height: Height(u32::from_be_bytes(
                bytes[2..6]
                    .try_into()
                    .expect("the record has a fixed length, checked above"),
            )),
            next_ordinal: u64_at(6),
            omitted: u64_at(14),
            resolved: u64_at(22),
        })
    }
}

/// Reads the construction record and the artifact hash it is keyed by, or
/// `None` for a database that was never hinted.
#[allow(clippy::unwrap_in_result)]
pub(super) fn read_progress(db: &ZebraDb) -> Result<Option<([u8; 32], Progress)>, SpentnessError> {
    let cf = db
        .db
        .cf_handle(SPENTNESS_ARTIFACT)
        .expect("the spentness_artifact column family is declared");
    let Some((key, value)) = db.db.zs_first_key_value::<_, RawBytes, RawBytes>(&cf) else {
        return Ok(None);
    };
    let commitment: [u8; 32] = key
        .raw_bytes()
        .as_slice()
        .try_into()
        .map_err(|_| SpentnessError::Inconsistent("malformed construction record key"))?;

    Ok(Some((commitment, Progress::decode(value.raw_bytes())?)))
}

impl DiskWriteBatch {
    /// Stores `progress` for the artifact `commitment` with the other writes in this batch.
    pub(super) fn prepare_spentness_progress(
        &mut self,
        db: &ZebraDb,
        commitment: [u8; 32],
        progress: Progress,
    ) {
        let cf = db
            .db
            .cf_handle(SPENTNESS_ARTIFACT)
            .expect("the spentness_artifact column family is declared");
        self.zs_insert(
            &cf,
            RawBytes::new_raw_bytes(commitment.to_vec()),
            RawBytes::new_raw_bytes(progress.encode()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trips() {
        for phase in [Phase::Applying, Phase::Complete] {
            let progress = Progress {
                phase,
                height: Height(419_200),
                next_ordinal: u64::MAX - 1,
                omitted: 7,
                resolved: 5,
            };
            let bytes = progress.encode();
            assert_eq!(bytes.len(), RECORD_LEN);
            assert_eq!(Progress::decode(&bytes), Ok(progress));
        }
    }

    #[test]
    fn record_rejects_malformed_bytes() {
        let bytes = Progress {
            phase: Phase::Applying,
            height: Height(1),
            next_ordinal: 2,
            omitted: 3,
            resolved: 4,
        }
        .encode();

        assert!(Progress::decode(&bytes[..RECORD_LEN - 1]).is_err());

        let mut version = bytes.clone();
        version[0] = RECORD_VERSION + 1;
        assert!(Progress::decode(&version).is_err());

        let mut phase = bytes;
        phase[1] = 2;
        assert!(Progress::decode(&phase).is_err());
    }
}
