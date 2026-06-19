//! Ironwood shielded pool types (NU6.3 onward).
//!
//! The Ironwood pool reuses the Orchard Action + Halo2 proof system, so its on-chain data
//! structures are structurally identical to Orchard's. Rather than duplicating those types, this
//! module provides thin newtypes over the Orchard types. This keeps the Ironwood pool
//! *type-distinct* from Orchard — it has its own note commitment tree, nullifier set, and chain
//! value pool — while reusing all of Orchard's wire-format and proof-verification machinery.
//!
//! This module is only compiled for the experimental NU6.3 / v6-transaction build
//! (`--cfg zcash_unstable="nu6.3"` with the `tx_v6` feature).

use crate::orchard;

/// Ironwood shielded data: a v6 Orchard-protocol bundle committed to the Ironwood pool.
///
/// Wraps [`orchard::ShieldedDataV6`] (which itself carries the NU6.3 flag-byte format that permits
/// the `enableCrossAddress` flag). The Ironwood bundle shares the exact wire format of the v6
/// Orchard bundle; this newtype keeps the two type-distinct so they cannot be accidentally
/// interchanged, and so the Ironwood bundle can commit into a separate note commitment tree and
/// nullifier set.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ShieldedData(pub orchard::ShieldedDataV6);
