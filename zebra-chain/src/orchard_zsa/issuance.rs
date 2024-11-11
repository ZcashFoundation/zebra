//! Orchard ZSA issuance related functionality.

use std::{fmt::Debug, io};

use halo2::pasta::pallas;

// For pallas::Base::from_repr only
use group::ff::PrimeField;

use zcash_primitives::transaction::components::issuance::{read_v6_bundle, write_v6_bundle};

use orchard::{
    issuance::{IssueAction, IssueBundle, Signed},
    note::ExtractedNoteCommitment,
    Note,
};

use crate::{
    block::MAX_BLOCK_BYTES,
    serialization::{SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize},
};

use super::burn::ASSET_BASE_SIZE;

/// Wrapper for `IssueBundle` used in the context of Transaction V6. This allows the implementation of
/// a Serde serializer for unit tests within this crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueData(IssueBundle<Signed>);

impl IssueData {
    /// Returns a reference to the inner `IssueBundle<Signed>`.
    pub fn inner(&self) -> &IssueBundle<Signed> {
        &self.0
    }
}

impl From<IssueBundle<Signed>> for IssueData {
    fn from(inner: IssueBundle<Signed>) -> Self {
        Self(inner)
    }
}

impl IssueData {
    pub(crate) fn note_commitments(&self) -> impl Iterator<Item = pallas::Base> + '_ {
        self.0.actions().iter().flat_map(|action| {
            action.notes().iter().map(|note| {
                // FIXME: Make `ExtractedNoteCommitment::inner` public in `orchard` (this would
                // eliminate the need for the workaround of converting `pallas::Base` from bytes
                // here), or introduce a new public method in `orchard::issuance::IssueBundle` to
                // retrieve note commitments directly from `orchard`.
                pallas::Base::from_repr(ExtractedNoteCommitment::from(note.commitment()).to_bytes())
                    .unwrap()
            })
        })
    }
}

// Sizes of the serialized values for types in bytes (used for TrustedPreallocate impls)
// FIXME: are those values correct (43, 32 etc.)?
//const ISSUANCE_VALIDATING_KEY_SIZE: u64 = 32;
const ADDRESS_SIZE: u64 = 43;
const NULLIFIER_SIZE: u64 = 32;
const NOTE_VALUE_SIZE: u64 = 4;
const RANDOM_SEED_SIZE: u64 = 32;
// FIXME: is this a correct way to calculate (simple sum of sizes of components)?
const NOTE_SIZE: u64 =
    ADDRESS_SIZE + NOTE_VALUE_SIZE + ASSET_BASE_SIZE + NULLIFIER_SIZE + RANDOM_SEED_SIZE;

impl TrustedPreallocate for Note {
    fn max_allocation() -> u64 {
        // FIXME: is this a correct calculation way?
        // The longest Vec<Note> we receive from an honest peer must fit inside a valid block.
        // Since encoding the length of the vec takes at least one byte, we use MAX_BLOCK_BYTES - 1
        (MAX_BLOCK_BYTES - 1) / NOTE_SIZE
    }
}

impl TrustedPreallocate for IssueAction {
    fn max_allocation() -> u64 {
        // FIXME: impl correct calculation
        10
    }
}

impl ZcashSerialize for Option<IssueData> {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        write_v6_bundle(self.as_ref().map(|issue_data| &issue_data.0), writer)
    }
}

// FIXME: We can't split IssueData out of Option<IssueData> deserialization,
// because the counts are read along with the arrays.
impl ZcashDeserialize for Option<IssueData> {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        Ok(read_v6_bundle(reader)?.map(IssueData))
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl serde::Serialize for IssueData {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // TODO: FIXME: implement Serde serialization here
        "(IssueData)".serialize(serializer)
    }
}
