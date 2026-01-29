//! Tachyon actions (Tachyactions).
//!
//! A Tachyaction is a simplified version of an Orchard Action, designed for
//! block-level proof aggregation and out-of-band payment distribution.
//!
//! Key differences from Orchard Actions:
//! - No encrypted ciphertexts (payment secrets distributed out-of-band)
//! - Nullifier includes epoch "flavor" for oblivious syncing
//! - Proof is aggregated at block level, not per-action

use std::io;

use group::ff::PrimeField;
use halo2::pasta::pallas;
use reddsa::{orchard::SpendAuth, Signature};

use crate::serialization::{
    serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
};

use super::{commitment::ValueCommitment, nullifier::Nullifier};

/// A Tachyon action description.
///
/// Tachyactions are simpler than Orchard actions because:
/// 1. No ciphertexts - secrets distributed out-of-band
/// 2. Proofs aggregated at block level via Ragu PCD
/// 3. Nullifiers have epoch "flavor" for oblivious syncing
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tachyaction {
    /// Value commitment to net value (input - output).
    pub cv: ValueCommitment,

    /// Nullifier with epoch flavor.
    pub nullifier: Nullifier,

    /// The x-coordinate of the note commitment for the output note.
    #[serde(with = "serde_helpers::Base")]
    pub cm_x: pallas::Base,

    /// Randomized spend authorization key.
    pub rk: reddsa::VerificationKeyBytes<SpendAuth>,
}

impl Tachyaction {
    /// The size of a serialized Tachyaction in bytes.
    ///
    /// cv: 32 + nullifier: 36 (32 + 4) + cm_x: 32 + rk: 32 = 132 bytes
    ///
    /// This is significantly smaller than Orchard actions (~580 bytes)
    /// due to the absence of encrypted ciphertexts.
    pub const SIZE: usize = 132;
}

impl ZcashSerialize for Tachyaction {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.cv.zcash_serialize(&mut writer)?;
        self.nullifier.zcash_serialize(&mut writer)?;
        writer.write_all(&self.cm_x.to_repr())?;
        writer.write_all(&<[u8; 32]>::from(self.rk))?;
        Ok(())
    }
}

impl ZcashDeserialize for Tachyaction {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let cv = ValueCommitment::zcash_deserialize(&mut reader)?;
        let nullifier = Nullifier::zcash_deserialize(&mut reader)?;

        let cm_x_bytes = reader.read_32_bytes()?;
        let cm_x = pallas::Base::from_repr(cm_x_bytes);
        if cm_x.is_none().into() {
            return Err(SerializationError::Parse(
                "Invalid pallas::Base value for cm_x",
            ));
        }

        let rk = reader.read_32_bytes()?.into();

        Ok(Self {
            cv,
            nullifier,
            cm_x: cm_x.unwrap(),
            rk,
        })
    }
}

/// An authorized Tachyaction with spend authorization signature.
///
/// Every Tachyaction that spends a note must have a corresponding
/// spend authorization signature from the randomized key `rk`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizedTachyaction {
    /// The action description.
    pub action: Tachyaction,

    /// The spend authorization signature.
    pub spend_auth_sig: Signature<SpendAuth>,
}

impl AuthorizedTachyaction {
    /// The size of a serialized AuthorizedTachyaction in bytes.
    ///
    /// Tachyaction: 132 + signature: 64 = 196 bytes
    pub const SIZE: usize = Tachyaction::SIZE + 64;

    /// Split into parts for serialization.
    pub fn into_parts(self) -> (Tachyaction, Signature<SpendAuth>) {
        (self.action, self.spend_auth_sig)
    }

    /// Combine parts from deserialization.
    pub fn from_parts(action: Tachyaction, spend_auth_sig: Signature<SpendAuth>) -> Self {
        Self {
            action,
            spend_auth_sig,
        }
    }
}

impl ZcashSerialize for AuthorizedTachyaction {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.action.zcash_serialize(&mut writer)?;
        writer.write_all(&<[u8; 64]>::from(self.spend_auth_sig))?;
        Ok(())
    }
}

impl ZcashDeserialize for AuthorizedTachyaction {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let action = Tachyaction::zcash_deserialize(&mut reader)?;

        let mut sig_bytes = [0u8; 64];
        reader.read_exact(&mut sig_bytes)?;
        let spend_auth_sig = Signature::from(sig_bytes);

        Ok(Self {
            action,
            spend_auth_sig,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tachyaction_size() {
        // Verify our size calculations are correct
        assert_eq!(
            Tachyaction::SIZE,
            32 + 36 + 32 + 32, // cv + nullifier + cm_x + rk
        );
        assert_eq!(
            AuthorizedTachyaction::SIZE,
            Tachyaction::SIZE + 64, // action + signature
        );
    }
}
