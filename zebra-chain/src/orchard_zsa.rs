//! OrchardZSA related functionality.

// TODO: FIXME: remove pub(crate) later if possible
#[cfg(any(test, feature = "proptest-impl"))]
pub(crate) mod arbitrary;

/// TODO: FIXME: feature = "proptest-impl" and pub are needed to access test vectors from
/// other crates, remove it later if possible
#[cfg(any(test, feature = "proptest-impl"))]
pub mod tests;

mod burn;
mod issuance;

pub(crate) use burn::{Burn, BurnItem, NoBurn};
pub(crate) use issuance::IssueData;
