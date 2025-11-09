//! OrchardZSA issuance related functionality.

use std::{fmt::Debug, io};

// For pallas::Base::from_repr only
use group::ff::PrimeField;

use halo2::pasta::pallas;

use orchard::{
    issuance::{IssueBundle, Signed},
    note::ExtractedNoteCommitment,
};

use zcash_primitives::transaction::components::issuance::{read_bundle, write_bundle};

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

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
                // TODO: FIXME: Make `ExtractedNoteCommitment::inner` public in `orchard` (this would
                // eliminate the need for the workaround of converting `pallas::Base` from bytes
                // here), or introduce a new public method in `orchard::issuance::IssueBundle` to
                // retrieve note commitments directly from `orchard`.
                pallas::Base::from_repr(ExtractedNoteCommitment::from(note.commitment()).to_bytes())
                    .unwrap()
            })
        })
    }
}

impl ZcashSerialize for Option<IssueData> {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        write_bundle(self.as_ref().map(|issue_data| &issue_data.0), writer)
    }
}

impl ZcashDeserialize for Option<IssueData> {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        Ok(read_bundle(reader)?.map(IssueData))
    }
}

#[cfg(any(test, feature = "proptest-impl", feature = "elasticsearch"))]
impl serde::Serialize for IssueData {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        unimplemented!("Serde serialization for IssueData functionality is not needed for Zebra");
    }
}
