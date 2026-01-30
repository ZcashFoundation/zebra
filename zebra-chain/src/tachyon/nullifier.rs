//! Tachyon nullifiers with epoch "flavoring".
//!
//! Unlike Orchard nullifiers, Tachyon nullifiers include an epoch ID that
//! enables oblivious syncing. The epoch "flavor" allows wallets to delegate
//! scanning to untrusted services using constrained PRF keys.
//!
//! This module provides [`FlavoredNullifier`], a serializable wrapper that bundles
//! a [`tachyon::Nullifier`] with its associated [`Epoch`](tachyon::Epoch) for
//! blockchain storage.

use std::{
    fmt,
    hash::{Hash, Hasher},
    io,
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize};

/// A Tachyon nullifier bundled with its epoch flavor for blockchain serialization.
///
/// This type pairs a [`tachyon::Nullifier`] with an [`tachyon::Epoch`] to enable:
/// - Blockchain serialization (both fields need to be persisted)
/// - Oblivious syncing (epoch enables constrained PRF key delegation)
///
/// The nullifier value `nf` is derived as:
/// ```text
/// nf = F_nk(psi || flavor)
/// ```
/// where:
/// - `F_nk` is a PRF keyed on the user's nullifier key
/// - `psi` is a nullifier trapdoor (user-controlled randomness)
/// - `flavor` is the epoch
///
/// Unlike Orchard, Tachyon nullifiers do not require global uniqueness
/// of the input randomness, which enables new privacy features.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct FlavoredNullifier {
    /// The nullifier value as bytes (for serde compatibility).
    nf_bytes: [u8; 32],

    /// The epoch "flavor" as u64 (for serde compatibility).
    epoch: u64,
}

impl FlavoredNullifier {
    /// The size of a serialized FlavoredNullifier in bytes (32 for nf + 8 for epoch).
    pub const SIZE: usize = 40;

    /// Create a new FlavoredNullifier from its components.
    pub fn new(nullifier: tachyon::Nullifier, epoch: tachyon::Epoch) -> Self {
        Self {
            nf_bytes: nullifier.to_bytes(),
            epoch: epoch.as_u64(),
        }
    }

    /// Get the nullifier value.
    pub fn nullifier(&self) -> tachyon::Nullifier {
        tachyon::Nullifier::from_bytes(&self.nf_bytes).expect("valid nullifier bytes")
    }

    /// Get the epoch "flavor".
    pub fn epoch(&self) -> tachyon::Epoch {
        tachyon::Epoch::new(self.epoch)
    }

    /// Get the nullifier value as bytes.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.nf_bytes
    }

    /// Try to create a FlavoredNullifier from bytes and an epoch.
    pub fn try_from_bytes(
        bytes: [u8; 32],
        epoch: tachyon::Epoch,
    ) -> Result<Self, SerializationError> {
        // Validate the bytes represent a valid field element
        tachyon::Nullifier::from_bytes(&bytes).ok_or(SerializationError::Parse(
            "Invalid field element for Tachyon Nullifier",
        ))?;
        Ok(Self {
            nf_bytes: bytes,
            epoch: epoch.as_u64(),
        })
    }
}

impl fmt::Debug for FlavoredNullifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlavoredNullifier")
            .field("nf", &hex::encode(self.nf_bytes))
            .field("epoch", &self.epoch)
            .finish()
    }
}

impl PartialEq for FlavoredNullifier {
    fn eq(&self, other: &Self) -> bool {
        self.nf_bytes == other.nf_bytes && self.epoch == other.epoch
    }
}

impl Eq for FlavoredNullifier {}

impl Hash for FlavoredNullifier {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.nf_bytes.hash(state);
        self.epoch.hash(state);
    }
}

impl ZcashSerialize for FlavoredNullifier {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.nf_bytes)?;
        writer.write_u64::<LittleEndian>(self.epoch)?;
        Ok(())
    }
}

impl ZcashDeserialize for FlavoredNullifier {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let nf_bytes = reader.read_32_bytes()?;

        // Validate the bytes represent a valid field element
        tachyon::Nullifier::from_bytes(&nf_bytes).ok_or(SerializationError::Parse(
            "Invalid field element for Tachyon Nullifier",
        ))?;

        let epoch = reader.read_u64::<LittleEndian>()?;

        Ok(Self { nf_bytes, epoch })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tachyon::primitives::Fp;

    #[test]
    fn flavored_nullifier_serialization_roundtrip() {
        let _init_guard = zebra_test::init();

        let nf = FlavoredNullifier::new(
            tachyon::Nullifier::from_field(Fp::from(12345u64)),
            tachyon::Epoch::new(42),
        );

        let mut bytes = Vec::new();
        nf.zcash_serialize(&mut bytes).unwrap();

        assert_eq!(bytes.len(), FlavoredNullifier::SIZE);

        let nf2 = FlavoredNullifier::zcash_deserialize(&bytes[..]).unwrap();
        assert_eq!(nf, nf2);
    }
}
