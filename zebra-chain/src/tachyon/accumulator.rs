//! Tachyon polynomial accumulator.
//!
//! **IMPORTANT**: Unlike Sapling/Orchard which use Merkle trees, Tachyon uses a
//! **polynomial accumulator** where tachygrams are roots of a polynomial.
//! There is NO tree Root type.
//!
//! From the spec:
//! > "The accumulator will be a commitment to a polynomial with roots at the
//! > committed values, hashed with the previous accumulator value."
//!
//! This enables efficient set membership and non-membership proofs using
//! polynomial evaluation, which integrates with the Ragu PCD system.
//!
//! **TODO**: This module is a placeholder. The actual accumulator implementation
//! will be provided by the Ragu library integration.

use std::{
    fmt,
    hash::{Hash, Hasher},
    io,
};

use group::ff::PrimeField;
use halo2::pasta::pallas;

use crate::serialization::{
    serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
};

/// Tachyon accumulator anchor (placeholder).
///
/// This represents the current state of the polynomial accumulator.
/// Unlike a Merkle tree root, this is a polynomial commitment where
/// tachygrams are the roots of the committed polynomial.
///
/// **TODO**: This is a placeholder type. The actual anchor representation
/// will be defined by the Ragu accumulator integration.
#[derive(Clone, Copy, Eq, Serialize, Deserialize)]
pub struct Anchor(#[serde(with = "serde_helpers::Base")] pub pallas::Base);

impl Anchor {
    /// The size of a serialized Anchor in bytes.
    pub const SIZE: usize = 32;

    /// Create an anchor from a pallas::Base value.
    pub fn from_base(base: pallas::Base) -> Self {
        Self(base)
    }

    /// Get the underlying pallas::Base value.
    pub fn to_base(&self) -> pallas::Base {
        self.0
    }

    /// Create an anchor from bytes.
    pub fn try_from_bytes(bytes: [u8; 32]) -> Result<Self, &'static str> {
        let base = pallas::Base::from_repr(bytes);
        if base.is_some().into() {
            Ok(Self(base.unwrap()))
        } else {
            Err("Invalid pallas::Base value for accumulator anchor")
        }
    }

    /// Convert to bytes.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_repr()
    }
}

impl fmt::Debug for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("tachyon::accumulator::Anchor")
            .field(&hex::encode(self.0.to_repr()))
            .finish()
    }
}

impl fmt::Display for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0.to_repr()))
    }
}

impl Default for Anchor {
    fn default() -> Self {
        // The empty accumulator anchor.
        // TODO: This should be the commitment to the identity polynomial,
        // computed based on the Ragu accumulator specification.
        Self(pallas::Base::zero())
    }
}

impl PartialEq for Anchor {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Hash for Anchor {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_repr().hash(state);
    }
}

impl From<Anchor> for [u8; 32] {
    fn from(anchor: Anchor) -> Self {
        anchor.0.to_repr()
    }
}

impl TryFrom<[u8; 32]> for Anchor {
    type Error = SerializationError;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let base = pallas::Base::from_repr(bytes);
        if base.is_some().into() {
            Ok(Self(base.unwrap()))
        } else {
            Err(SerializationError::Parse(
                "Invalid pallas::Base value for Tachyon accumulator anchor",
            ))
        }
    }
}

impl ZcashSerialize for Anchor {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0.to_repr())
    }
}

impl ZcashDeserialize for Anchor {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Self::try_from(reader.read_32_bytes()?)
    }
}

// Note: The actual polynomial accumulator implementation will be provided by
// the Ragu library. The accumulator state is:
//
//   new_anchor = hash(polynomial_commitment(tachygrams), previous_anchor)
//
// Where polynomial_commitment creates a polynomial P(x) such that
// P(tachygram) = 0 for all committed tachygrams.
//
// Set membership proofs show that a tachygram is a root of the polynomial.
// Set non-membership proofs show that a tachygram is NOT a root.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_serialization_roundtrip() {
        let _init_guard = zebra_test::init();

        let anchor = Anchor::from_base(pallas::Base::from(12345u64));

        let mut bytes = Vec::new();
        anchor.zcash_serialize(&mut bytes).unwrap();

        assert_eq!(bytes.len(), Anchor::SIZE);

        let anchor2 = Anchor::zcash_deserialize(&bytes[..]).unwrap();
        assert_eq!(anchor, anchor2);
    }

    #[test]
    fn anchor_default() {
        let _init_guard = zebra_test::init();

        let anchor = Anchor::default();
        assert_eq!(anchor.to_base(), pallas::Base::zero());
    }
}
