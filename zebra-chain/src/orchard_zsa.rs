//! Orchard ZSA related functionality.

// FIXME: remove pub(crate) later if possible
#[cfg(any(test, feature = "proptest-impl"))]
pub(crate) mod arbitrary;

/// FIXME: feature = "proptest-impl" and pub are needed to access test vectors from another crates,
/// remove it then
#[cfg(any(test, feature = "proptest-impl"))]
pub mod tests;

mod burn;
mod issuance;

pub(crate) use burn::{Burn, NoBurn};
pub(crate) use issuance::IssueData;
