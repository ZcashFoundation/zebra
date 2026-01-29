//! Tachyon nullifiers with epoch "flavoring".
//!
//! Unlike Orchard nullifiers, Tachyon nullifiers include an epoch ID that
//! enables oblivious syncing. The epoch "flavor" allows wallets to delegate
//! scanning to untrusted services using constrained PRF keys.

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

use super::epoch::EpochId;

/// A Tachyon nullifier with epoch flavor.
///
/// The nullifier value `nf` is derived as:
/// ```text
/// nf = F_nk(psi || flavor)
/// ```
/// where:
/// - `F_nk` is a PRF keyed on the user's nullifier key
/// - `psi` is a nullifier trapdoor (user-controlled randomness)
/// - `flavor` is the epoch ID
///
/// Unlike Orchard, Tachyon nullifiers do not require global uniqueness
/// of the input randomness, which enables new privacy features.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Nullifier {
    /// The 32-byte nullifier value (element of pallas::Base).
    #[serde(with = "serde_helpers::Base")]
    nf: pallas::Base,

    /// The epoch "flavor" for oblivious syncing.
    epoch: EpochId,
}

impl Nullifier {
    /// The size of a serialized Nullifier in bytes (32 for nf + 4 for epoch).
    pub const SIZE: usize = 36;

    /// Create a new Nullifier from its components.
    pub fn new(nf: pallas::Base, epoch: EpochId) -> Self {
        Self { nf, epoch }
    }

    /// Get the nullifier value.
    pub fn nf(&self) -> pallas::Base {
        self.nf
    }

    /// Get the epoch "flavor".
    pub fn epoch(&self) -> EpochId {
        self.epoch
    }

    /// Get the nullifier value as bytes.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.nf.to_repr()
    }

    /// Try to create a Nullifier from bytes and an epoch.
    pub fn try_from_bytes(bytes: [u8; 32], epoch: EpochId) -> Result<Self, SerializationError> {
        let possible_base = pallas::Base::from_repr(bytes);

        if possible_base.is_some().into() {
            Ok(Self {
                nf: possible_base.unwrap(),
                epoch,
            })
        } else {
            Err(SerializationError::Parse(
                "Invalid pallas::Base value for Tachyon Nullifier",
            ))
        }
    }
}

impl fmt::Debug for Nullifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("tachyon::Nullifier")
            .field("nf", &hex::encode(self.nf.to_repr()))
            .field("epoch", &self.epoch.value())
            .finish()
    }
}

impl PartialEq for Nullifier {
    fn eq(&self, other: &Self) -> bool {
        self.nf == other.nf && self.epoch == other.epoch
    }
}

impl Eq for Nullifier {}

impl Hash for Nullifier {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.nf.to_repr().hash(state);
        self.epoch.0.hash(state);
    }
}

impl ZcashSerialize for Nullifier {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.nf.to_repr())?;
        self.epoch.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for Nullifier {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let nf_bytes = reader.read_32_bytes()?;
        let possible_base = pallas::Base::from_repr(nf_bytes);

        if possible_base.is_none().into() {
            return Err(SerializationError::Parse(
                "Invalid pallas::Base value for Tachyon Nullifier",
            ));
        }

        let epoch = EpochId::zcash_deserialize(&mut reader)?;

        Ok(Self {
            nf: possible_base.unwrap(),
            epoch,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nullifier_serialization_roundtrip() {
        let _init_guard = zebra_test::init();

        let nf = Nullifier::new(pallas::Base::from(12345u64), EpochId::new(42));

        let mut bytes = Vec::new();
        nf.zcash_serialize(&mut bytes).unwrap();

        assert_eq!(bytes.len(), Nullifier::SIZE);

        let nf2 = Nullifier::zcash_deserialize(&bytes[..]).unwrap();
        assert_eq!(nf, nf2);
    }
}
