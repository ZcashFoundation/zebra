//! Versioned signature types for transaction V6+.
//!
//! ZIP 246 introduces sighash versioning to allow future signature algorithm upgrades.
//! Signatures are prefixed with sighash metadata (version + optional associated data).

use std::io;

use byteorder::{ReadBytesExt, WriteBytesExt};

use crate::serialization::{
    CompactSizeMessage, SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize,
};

/// Sighash version 0 for V6 transactions (zero-sized type as V0 has no metadata).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SighashInfoV0;

#[allow(clippy::unwrap_in_result)]
impl ZcashSerialize for SighashInfoV0 {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> io::Result<()> {
        // Sighash V0 has no associated data, so length is always 1 (just the version byte)
        CompactSizeMessage::try_from(1)
            .expect("1 is always a valid CompactSize")
            .zcash_serialize(&mut writer)?;

        // Version 0
        writer.write_u8(0)?;

        Ok(())
    }
}

impl ZcashDeserialize for SighashInfoV0 {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let length = usize::from(CompactSizeMessage::zcash_deserialize(&mut reader)?);

        if length != 1 {
            return Err(SerializationError::Parse(
                "invalid sighash V0: length must be 1",
            ));
        }

        let version = reader.read_u8()?;
        if version != 0 {
            return Err(SerializationError::Parse(
                "invalid sighash V0: version byte must be 0",
            ));
        }

        Ok(Self)
    }
}

/// A signature with sighash version 0 prefix for V6 transactions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VersionedSigV0<Sig>(Sig);

impl<Sig> VersionedSigV0<Sig> {
    /// Wrap a signature with sighash V0 metadata.
    pub(crate) fn new(signature: Sig) -> Self {
        Self(signature)
    }

    /// Extract the underlying signature.
    pub(crate) fn into_signature(self) -> Sig {
        self.0
    }
}

impl<Sig> ZcashSerialize for VersionedSigV0<Sig>
where
    Sig: ZcashSerialize,
{
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> io::Result<()> {
        SighashInfoV0.zcash_serialize(&mut writer)?;
        self.0.zcash_serialize(&mut writer)
    }
}

impl<Sig> ZcashDeserialize for VersionedSigV0<Sig>
where
    Sig: ZcashDeserialize,
{
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        SighashInfoV0::zcash_deserialize(&mut reader)?;
        let signature = Sig::zcash_deserialize(&mut reader)?;
        Ok(Self(signature))
    }
}

impl<Sig> TrustedPreallocate for VersionedSigV0<Sig>
where
    Sig: TrustedPreallocate,
{
    fn max_allocation() -> u64 {
        // Sighash info adds only 2 bytes overhead, so signature's max allocation is safe
        Sig::max_allocation()
    }
}

#[cfg(test)]
mod tests {
    use redjubjub::{Signature, SpendAuth};

    use super::*;

    #[test]
    fn sighash_info_v0_roundtrip() {
        let info = SighashInfoV0;

        let bytes = info.zcash_serialize_to_vec().unwrap();
        assert_eq!(bytes, &[0x01, 0x00]); // CompactSize(1), version 0

        let parsed = SighashInfoV0::zcash_deserialize(&bytes[..]).unwrap();
        assert_eq!(parsed, info);
    }

    #[test]
    fn sighash_info_v0_rejects_wrong_length() {
        let bytes = [0x02, 0x00]; // CompactSize(2) - wrong length
        assert!(SighashInfoV0::zcash_deserialize(&bytes[..]).is_err());
    }

    #[test]
    fn sighash_info_v0_rejects_wrong_version() {
        let bytes = [0x01, 0x01]; // CompactSize(1), version 1 - wrong version
        assert!(SighashInfoV0::zcash_deserialize(&bytes[..]).is_err());
    }

    #[test]
    fn versioned_sig_v0_roundtrip() {
        // Create a test signature using real Sapling SpendAuth signature type (64 bytes)
        // Using fixed bytes for deterministic testing (not a cryptographically valid signature)
        let sig_bytes = [0x11u8; 64]; // Arbitrary 64-byte pattern
        let original_sig = Signature::<SpendAuth>::from(sig_bytes);

        let versioned_sig = VersionedSigV0::new(original_sig);
        let serialized_bytes = versioned_sig.zcash_serialize_to_vec().unwrap();

        // Expected format: [CompactSize(1), version(0), sig_bytes...]
        // 0x01 = CompactSize encoding of length 1 (just the version byte)
        // 0x00 = sighash version 0
        // followed by 64 bytes of the signature
        assert_eq!(serialized_bytes.len(), 1 + 1 + 64); // CompactSize + version + sig
        assert_eq!(serialized_bytes[0], 0x01); // CompactSize(1)
        assert_eq!(serialized_bytes[1], 0x00); // version 0
        assert_eq!(&serialized_bytes[2..], &sig_bytes[..]); // signature bytes

        let deserialized_sig =
            VersionedSigV0::<Signature<SpendAuth>>::zcash_deserialize(&serialized_bytes[..])
                .unwrap();
        assert_eq!(deserialized_sig.into_signature(), original_sig);
    }

    #[test]
    fn versioned_sig_v0_rejects_invalid_sighash() {
        let mut bytes = vec![0x01, 0x01]; // Invalid: CompactSize(1), version 1
        bytes.extend_from_slice(&[0u8; 64]); // Add dummy signature

        assert!(VersionedSigV0::<Signature<SpendAuth>>::zcash_deserialize(&bytes[..]).is_err());
    }
}
