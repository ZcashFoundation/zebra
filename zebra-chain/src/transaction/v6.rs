//! V6 (ZIP 248) transaction strict-parsing policy for Zebra.
//!
//! librustzcash's V6 parser round-trips unknown bundle types opaquely, so that
//! future ZIPs can add new bundle types without breaking older deserializers.
//! Zebra enforces a stricter policy: a V6 transaction is rejected if it
//! contains any bundle type that this minimal NU7 deployment does not
//! implement. Zebra nodes reach end-of-support before new bundle types are
//! deployed on mainnet, so opaque round-trip would only hide protocol
//! violations rather than enable forward compatibility.
//!
//! Allowed bundle types for this minimal NU7:
//!
//! - `Transparent` (0)
//! - `Sapling` (2)
//! - `Orchard` (3)
//! - `Fee` (4), value-only
//! - `Zip233Nsm` (5), value-only
//!
//! `Reserved` (1), `KeyRotation` (6), and `LockboxDisbursement` (7) are
//! rejected along with any unrecognized wire ID.

use zcash_primitives::transaction::{self as zp_tx, zip248};

use crate::serialization::SerializationError;

/// Check that a V6 transaction only contains bundle types permitted by
/// Zebra's minimal NU7 policy.
///
/// `Reserved` (bundle type 1) is already rejected by librustzcash during
/// parsing, so this only needs to cover types librustzcash accepts but
/// Zebra does not: opaque unknowns, and the types reserved for features
/// not implemented here (`KeyRotation`, `LockboxDisbursement`).
pub(crate) fn reject_unknown_v6_bundles<A: zp_tx::Authorization>(
    data: &zp_tx::TransactionData<A>,
) -> Result<(), SerializationError> {
    let bundles = data.bundles();
    if bundles.unknown_bundles().next().is_some() {
        return Err(SerializationError::Parse(
            "v6 transaction contains an unrecognized bundle type",
        ));
    }

    let vp = data.value_pool_deltas();
    if vp.unknown_iter().next().is_some() {
        return Err(SerializationError::Parse(
            "v6 transaction contains a value pool delta for an unrecognized bundle type",
        ));
    }

    for (key, _) in vp.iter() {
        match key.bundle_type {
            zip248::BundleType::Transparent
            | zip248::BundleType::Sapling
            | zip248::BundleType::Orchard
            | zip248::BundleType::Fee
            | zip248::BundleType::Zip233Nsm => {}
            zip248::BundleType::KeyRotation => {
                return Err(SerializationError::Parse(
                    "v6 transaction contains a KeyRotation bundle (ZIP 270, not supported)",
                ));
            }
            zip248::BundleType::LockboxDisbursement => {
                return Err(SerializationError::Parse(
                    "v6 transaction contains a LockboxDisbursement bundle (not supported)",
                ));
            }
            zip248::BundleType::Reserved => {
                return Err(SerializationError::Parse(
                    "v6 transaction references the reserved bundle type 1",
                ));
            }
            zip248::BundleType::Sprout | zip248::BundleType::Tze => {
                return Err(SerializationError::Parse(
                    "v6 transaction contains an in-memory-only bundle type on the wire",
                ));
            }
            _ => {
                return Err(SerializationError::Parse(
                    "v6 transaction contains a bundle type not supported by this release",
                ));
            }
        }
    }

    Ok(())
}
