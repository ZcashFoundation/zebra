//! Versioned signature types for transaction V6+.
//!
//! ZIP 246 introduces sighash versioning to allow future upgrades to signature algorithms.
//! Each signature is prefixed with sighash metadata (version + optional associated data).

use std::io;

use crate::serialization::{
    CompactSizeMessage, SerializationError, TrustedPreallocate, ZcashDeserialize,
    ZcashDeserializeInto, ZcashSerialize,
};

/// Sighash version byte.
///
/// Each transaction version defines its own set of sighash versions.
/// For V6 transactions, only version 0 is currently defined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SighashVersion(pub u8);

impl SighashVersion {
    /// Sighash version 0 (default for V6 transactions).
    pub const V0: Self = SighashVersion(0);
}

/// Sighash metadata for V6+ transaction signatures.
///
/// Serialization format: `CompactSize(len) || version || associated_data`
///
/// For V6 transactions with version 0, `associated_data` is always empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SighashInfo {
    /// Sighash algorithm version.
    pub version: SighashVersion,

    /// Optional data specific to this sighash version.
    pub associated_data: Vec<u8>,
}

impl SighashInfo {
    /// Create V0 sighash info for transaction V6 (empty `associated_data`).
    pub fn for_tx_v6() -> Self {
        SighashInfo {
            version: SighashVersion::V0,
            associated_data: Vec::new(),
        }
    }

    /// Validate this is V0 format for transaction V6.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Version is not 0
    /// - `associated_data` is not empty
    pub fn as_tx_v6(&self) -> Result<(), SerializationError> {
        if self.version != SighashVersion::V0 {
            return Err(SerializationError::Parse(
                "V6 transactions only support sighash version 0",
            ));
        }
        if !self.associated_data.is_empty() {
            return Err(SerializationError::Parse(
                "V6 sighash v0 must have empty associated_data",
            ));
        }
        Ok(())
    }
}

impl ZcashSerialize for SighashInfo {
    fn zcash_serialize<W: io::Write>(&self, mut w: W) -> io::Result<()> {
        // Total length = 1 byte version + N bytes associated_data
        let total_len = 1usize
            .checked_add(self.associated_data.len())
            .expect("len fits in MAX_PROTOCOL_MESSAGE_LEN");
        CompactSizeMessage::try_from(total_len)
            .expect("len fits in MAX_PROTOCOL_MESSAGE_LEN")
            .zcash_serialize(&mut w)?;
        w.write_all(&[self.version.0])?;
        w.write_all(&self.associated_data)?;
        Ok(())
    }
}

impl ZcashDeserialize for SighashInfo {
    fn zcash_deserialize<R: io::Read>(mut r: R) -> Result<Self, SerializationError> {
        let len: usize = (&mut r)
            .zcash_deserialize_into::<CompactSizeMessage>()?
            .into();

        // Validate length
        if len == 0 {
            return Err(SerializationError::Parse(
                "sighashInfo length must be at least 1",
            ));
        }

        // Read version
        let mut version = [0u8; 1];
        r.read_exact(&mut version)?;

        // Read associated_data
        let assoc_len = len - 1;
        let mut associated_data = vec![0u8; assoc_len];
        if assoc_len > 0 {
            r.read_exact(&mut associated_data)?;
        }

        Ok(SighashInfo {
            version: SighashVersion(version[0]),
            associated_data,
        })
    }
}

/// A signature with versioned sighash metadata.
///
/// Serialization format: `sighashInfo || signature`
///
/// Used in V6+ transactions to allow future signature algorithm upgrades.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionedSig<Sig> {
    /// Sighash version and associated data.
    pub sighash_info: SighashInfo,

    /// The actual signature.
    pub signature: Sig,
}

impl<Sig> VersionedSig<Sig> {
    /// Create a versioned signature for transaction V6 (sighash v0).
    pub fn for_tx_v6(signature: Sig) -> Self {
        Self {
            sighash_info: SighashInfo::for_tx_v6(),
            signature,
        }
    }

    /// Extract signature after validating it's V0 for V6 transactions.
    ///
    /// # Errors
    ///
    /// Returns an error if the sighash info is not valid for V6.
    pub fn as_tx_v6(self) -> Result<Sig, SerializationError> {
        self.sighash_info.as_tx_v6()?;
        Ok(self.signature)
    }
}

impl<Sig> ZcashSerialize for VersionedSig<Sig>
where
    Sig: ZcashSerialize,
{
    fn zcash_serialize<W: io::Write>(&self, mut w: W) -> io::Result<()> {
        self.sighash_info.zcash_serialize(&mut w)?;
        self.signature.zcash_serialize(&mut w)
    }
}

impl<Sig> ZcashDeserialize for VersionedSig<Sig>
where
    Sig: ZcashDeserialize,
{
    fn zcash_deserialize<R: io::Read>(mut r: R) -> Result<Self, SerializationError> {
        let sighash_info = SighashInfo::zcash_deserialize(&mut r)?;
        let signature = Sig::zcash_deserialize(&mut r)?;
        Ok(VersionedSig {
            sighash_info,
            signature,
        })
    }
}

impl<Sig> TrustedPreallocate for VersionedSig<Sig>
where
    Sig: TrustedPreallocate,
{
    fn max_allocation() -> u64 {
        // Min size of VersionedSig is min Sig size + 2 bytes (CompactSize(1) + version),
        // so we can safely use Sig::max_allocation.
        Sig::max_allocation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sighash_info_v0_roundtrip() {
        let info = SighashInfo::for_tx_v6();

        let bytes = info.zcash_serialize_to_vec().unwrap();
        assert_eq!(bytes, &[0x01, 0x00]); // CompactSize(1), version 0

        let parsed = SighashInfo::zcash_deserialize(&bytes[..]).unwrap();
        assert_eq!(parsed, info);
        assert!(parsed.as_tx_v6().is_ok());
    }

    #[test]
    fn sighash_info_with_data() {
        let info = SighashInfo {
            version: SighashVersion(1),
            associated_data: vec![0xaa, 0xbb],
        };

        let bytes = info.zcash_serialize_to_vec().unwrap();
        assert_eq!(bytes, &[0x03, 0x01, 0xaa, 0xbb]); // CompactSize(3), v1, data

        let parsed = SighashInfo::zcash_deserialize(&bytes[..]).unwrap();
        assert_eq!(parsed, info);

        // Should fail V6 validation
        assert!(parsed.as_tx_v6().is_err());
    }

    #[test]
    fn sighash_info_rejects_empty() {
        let bytes = [0x00]; // CompactSize(0)
        assert!(SighashInfo::zcash_deserialize(&bytes[..]).is_err());
    }

    #[test]
    fn versioned_sig_v6() {
        // Mock signature type
        #[derive(Clone, Debug, PartialEq, Eq)]
        struct TestSig([u8; 4]);

        impl ZcashSerialize for TestSig {
            fn zcash_serialize<W: io::Write>(&self, mut w: W) -> io::Result<()> {
                w.write_all(&self.0)
            }
        }

        impl ZcashDeserialize for TestSig {
            fn zcash_deserialize<R: io::Read>(mut r: R) -> Result<Self, SerializationError> {
                let mut buf = [0u8; 4];
                r.read_exact(&mut buf)?;
                Ok(TestSig(buf))
            }
        }

        let sig = TestSig([0x11, 0x22, 0x33, 0x44]);
        let versioned = VersionedSig::for_tx_v6(sig.clone());

        let bytes = versioned.zcash_serialize_to_vec().unwrap();
        // sighash info bytes (CompactSize(1) + version 0) + signature bytes
        assert_eq!(bytes, &[0x01, 0x00, 0x11, 0x22, 0x33, 0x44]);

        let parsed = VersionedSig::<TestSig>::zcash_deserialize(&bytes[..]).unwrap();
        assert_eq!(parsed.signature, sig);

        let extracted = parsed.as_tx_v6().unwrap();
        assert_eq!(extracted, sig);
    }
}
