//! Note and value commitments.

use std::{fmt, io, sync::OnceLock};

use hex::{FromHex, FromHexError, ToHex};
use serde::{Deserialize, Serialize};

use crate::serialization::{
    is_trusted_source, SerializationError, ZcashDeserialize, ZcashSerialize,
};

#[cfg(test)]
mod test_vectors;

/// The randomness used in the Pedersen Hash for note commitment.
///
/// Equivalent to `sapling_crypto::note::CommitmentRandomness`,
/// but we can't use it directly as it is not public.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CommitmentRandomness(jubjub::Fr);

/// A wrapper for the `sapling_crypto::value::ValueCommitment` type with lazy validation.
///
/// Stores raw bytes and defers the expensive point decompression and small-order check
/// until the inner value is actually needed. This allows blocks read from trusted storage
/// to skip validation at deserialization time.
pub struct ValueCommitment {
    bytes: [u8; 32],
    inner: OnceLock<sapling_crypto::value::ValueCommitment>,
}

impl ValueCommitment {
    /// Create from an already-validated inner value.
    pub fn from_validated(inner: sapling_crypto::value::ValueCommitment) -> Self {
        let bytes = inner.to_bytes();
        let lock = OnceLock::new();
        lock.set(inner)
            .expect("freshly created OnceLock should be empty");
        Self { bytes, inner: lock }
    }

    /// Create from raw bytes without validation. Validation is deferred until `inner()` is called.
    pub fn from_bytes_unchecked(bytes: [u8; 32]) -> Self {
        Self {
            bytes,
            inner: OnceLock::new(),
        }
    }

    /// Try to get the validated inner `sapling_crypto::value::ValueCommitment`, performing
    /// lazy validation if needed.
    ///
    /// Returns an error if the stored bytes do not represent a valid ValueCommitment.
    pub fn try_inner(&self) -> Result<&sapling_crypto::value::ValueCommitment, SerializationError> {
        // Fast path: already validated
        if let Some(inner) = self.inner.get() {
            return Ok(inner);
        }

        let vc = sapling_crypto::value::ValueCommitment::from_bytes_not_small_order(&self.bytes)
            .into_option()
            .ok_or(SerializationError::Parse("invalid ValueCommitment bytes"))?;

        Ok(self.inner.get_or_init(|| vc))
    }

    /// Get the validated inner `sapling_crypto::value::ValueCommitment`, performing
    /// lazy validation if needed.
    ///
    /// # Panics
    ///
    /// Panics if the stored bytes do not represent a valid ValueCommitment.
    /// This should never happen for data that was previously validated and stored.
    pub fn inner(&self) -> &sapling_crypto::value::ValueCommitment {
        self.try_inner()
            .expect("lazy ValueCommitment validation failed on previously stored bytes")
    }

    /// Return the raw bytes (zero-cost, no validation).
    pub fn to_bytes(&self) -> [u8; 32] {
        self.bytes
    }

    /// Return the hash bytes in big-endian byte-order suitable for printing out byte by byte.
    ///
    /// Zebra displays commitment value in big-endian byte-order,
    /// following the convention set by zcashd.
    pub fn bytes_in_display_order(&self) -> [u8; 32] {
        let mut reversed_bytes = self.bytes;
        reversed_bytes.reverse();
        reversed_bytes
    }
}

impl Clone for ValueCommitment {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes,
            inner: self.inner.clone(),
        }
    }
}

impl fmt::Debug for ValueCommitment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValueCommitment")
            .field("bytes", &hex::encode(self.bytes))
            .finish()
    }
}

impl PartialEq for ValueCommitment {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for ValueCommitment {}

impl Serialize for ValueCommitment {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Serialize as {"bytes": [u8; 32]} for compatibility with the
        // previous serde remote derive format.
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ValueCommitment", 1)?;
        state.serialize_field("bytes", &self.bytes)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for ValueCommitment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Deserialize from bytes and always validate (JSON/serde is untrusted).
        #[derive(Deserialize)]
        struct Helper {
            bytes: [u8; 32],
        }
        let helper = Helper::deserialize(deserializer)?;
        let inner =
            sapling_crypto::value::ValueCommitment::from_bytes_not_small_order(&helper.bytes)
                .into_option()
                .ok_or_else(|| serde::de::Error::custom("invalid ValueCommitment bytes"))?;
        Ok(Self::from_validated(inner))
    }
}

impl ToHex for &ValueCommitment {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl FromHex for ValueCommitment {
    type Error = FromHexError;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        // Parse hex string to 32 bytes
        let mut bytes = <[u8; 32]>::from_hex(hex)?;
        // Convert from big-endian (display) to little-endian (internal)
        bytes.reverse();

        Self::zcash_deserialize(io::Cursor::new(&bytes))
            .map_err(|_| FromHexError::InvalidStringLength)
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl From<jubjub::ExtendedPoint> for ValueCommitment {
    /// Convert a Jubjub point into a ValueCommitment.
    ///
    /// # Panics
    ///
    /// Panics if the given point does not correspond to a valid ValueCommitment.
    fn from(extended_point: jubjub::ExtendedPoint) -> Self {
        let bytes = jubjub::AffinePoint::from(extended_point).to_bytes();

        let value_commitment =
            sapling_crypto::value::ValueCommitment::from_bytes_not_small_order(&bytes)
                .into_option()
                .expect("invalid ValueCommitment bytes");

        ValueCommitment::from_validated(value_commitment)
    }
}

impl ZcashDeserialize for ValueCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut buf = [0u8; 32];
        reader.read_exact(&mut buf)?;

        if is_trusted_source() {
            Ok(Self::from_bytes_unchecked(buf))
        } else {
            let vc = sapling_crypto::value::ValueCommitment::from_bytes_not_small_order(&buf)
                .into_option()
                .ok_or(SerializationError::Parse("invalid ValueCommitment bytes"))?;
            Ok(Self::from_validated(vc))
        }
    }
}

impl ZcashSerialize for ValueCommitment {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.bytes)?;
        Ok(())
    }
}

impl ZcashDeserialize for sapling_crypto::note::ExtractedNoteCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut buf = [0u8; 32];
        reader.read_exact(&mut buf)?;

        let extracted_note_commitment: Option<sapling_crypto::note::ExtractedNoteCommitment> =
            sapling_crypto::note::ExtractedNoteCommitment::from_bytes(&buf).into_option();

        extracted_note_commitment.ok_or(SerializationError::Parse(
            "invalid ExtractedNoteCommitment bytes",
        ))
    }
}
