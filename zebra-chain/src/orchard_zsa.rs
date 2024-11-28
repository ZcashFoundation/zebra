//! Orchard ZSA related functionality.

// FIXME: remove pub(crate) later if possible
#[cfg(any(test, feature = "proptest-impl"))]
pub(crate) mod arbitrary;

/// FIXME: feature = "proptest-impl" and pub are needed to access test vectors from another crates,
/// remove it then
#[cfg(any(test, feature = "proptest-impl"))]
pub mod tests;

pub mod asset_state;
mod burn;
mod issuance;

pub(crate) use burn::{Burn, BurnItem, NoBurn};
pub(crate) use issuance::IssueData;

pub use asset_state::{AssetBase, AssetState, AssetStateChange, IssuedAssets, IssuedAssetsChange};
