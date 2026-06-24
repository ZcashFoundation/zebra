//! Ironwood shielded pool types (NU6.3 onward).
//!
//! The Ironwood pool reuses the Orchard Action + Halo2 proof system, so its on-chain data
//! structures are structurally identical to Orchard's. Rather than duplicating those types, this
//! module provides thin newtypes over the Orchard types. This keeps the Ironwood pool
//! *type-distinct* from Orchard — it has its own note commitment tree, nullifier set, and chain
//! value pool — while reusing all of Orchard's wire-format and proof-verification machinery.
//!
//! The Ironwood *state* (nullifier set, note commitment tree, anchors, and chain value pool) is
//! always present, so the on-disk database format is stable; it simply stays empty until NU6.3
//! transactions appear on-chain.

use crate::orchard;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

/// An Ironwood nullifier.
///
/// Wraps [`orchard::Nullifier`] (Ironwood reuses the Orchard nullifier construction). The Ironwood
/// and Orchard nullifier sets are *disjoint* even when their bit patterns coincide: they live in
/// separate column families and are checked separately. Keeping Ironwood nullifiers in their own
/// type lets the duplicate-nullifier detection distinguish the two pools at the type level.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub struct Nullifier(pub orchard::Nullifier);

impl From<orchard::Nullifier> for Nullifier {
    fn from(nullifier: orchard::Nullifier) -> Self {
        Self(nullifier)
    }
}

/// Ironwood shielded data: a v6 Orchard-protocol bundle committed to the Ironwood pool.
///
/// Wraps [`orchard::ShieldedDataV6`] (which itself carries the NU6.3 flag-byte format that permits
/// the `enableCrossAddress` flag). The Ironwood bundle shares the exact wire format of the v6
/// Orchard bundle; this newtype keeps the two type-distinct so they cannot be accidentally
/// interchanged, and so the Ironwood bundle can commit into a separate note commitment tree and
/// nullifier set.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ShieldedData(orchard::ShieldedDataV6);

impl ShieldedData {
    /// Wraps a v6 Orchard-protocol bundle as Ironwood shielded data.
    pub fn new(shielded_data: orchard::ShieldedDataV6) -> Self {
        Self(shielded_data)
    }

    /// Returns the inner Orchard [`ShieldedData`](orchard::ShieldedData) backing this Ironwood
    /// bundle (the v6 Orchard bundle shape that Ironwood reuses).
    pub fn data(&self) -> &orchard::ShieldedData {
        self.0.data()
    }
}
