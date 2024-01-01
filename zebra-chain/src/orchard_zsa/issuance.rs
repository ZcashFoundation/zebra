//! Orchard ZSA issuance related functionality.

use std::fmt::Debug;

use orchard_zsa::issuance::{IssueBundle, Signed};

/// Wrapper for `IssueBundle` used in the context of Transaction V6. This allows the implementation of
/// a Serde serializer for unit tests within this crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueData(IssueBundle<Signed>);

#[cfg(any(test, feature = "proptest-impl"))]
impl serde::Serialize for IssueData {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // TODO: FIXME: implement Serde serialization here
        ().serialize(serializer)
    }
}
