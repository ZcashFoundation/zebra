//! Note and value commitments.

use std::{fmt, io};

use hex::{FromHex, FromHexError, ToHex};

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

#[cfg(test)]
mod test_vectors;

/// The randomness used in the Pedersen Hash for note commitment.
///
/// Equivalent to `sapling_crypto::note::CommitmentRandomness`,
/// but we can't use it directly as it is not public.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CommitmentRandomness(jubjub::Fr);

/// A Sapling value commitment.
///
/// Stores raw bytes and lazily converts to the validated `sapling_crypto` type
/// on first access via [`inner()`](ValueCommitment::inner). This avoids expensive
/// Jubjub curve point decompression during deserialization from the finalized
/// state database.
///
/// Consensus safety is preserved because `zebra-consensus` independently
/// re-validates all curve points through `librustzcash`'s `Transaction::read()`.
#[derive(Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValueCommitment([u8; 32]);

impl fmt::Debug for ValueCommitment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("ValueCommitment")
            .field(&hex::encode(self.0))
            .finish()
    }
}

impl ValueCommitment {
    /// Return the raw serialized bytes of this value commitment.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// Return the hash bytes in big-endian byte-order suitable for printing out byte by byte.
    ///
    /// Zebra displays commitment value in big-endian byte-order,
    /// following the convention set by zcashd.
    pub fn bytes_in_display_order(&self) -> [u8; 32] {
        let mut reversed_bytes = self.0;
        reversed_bytes.reverse();
        reversed_bytes
    }

    /// Decode the inner `sapling_crypto::value::ValueCommitment` type.
    ///
    /// This performs Jubjub curve point decompression and small-order checks,
    /// which is expensive. Only call when the decoded point is actually needed
    /// (e.g. for binding verification key computation).
    ///
    /// # Panics
    ///
    /// Panics if the bytes do not represent a valid, non-small-order Jubjub point.
    /// This is safe because bytes were either validated before storage or come from
    /// trusted (already-validated) sources.
    pub fn inner(&self) -> sapling_crypto::value::ValueCommitment {
        sapling_crypto::value::ValueCommitment::from_bytes_not_small_order(&self.0)
            .into_option()
            .expect("previously validated or from trusted storage")
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

        Ok(ValueCommitment(bytes))
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl From<jubjub::ExtendedPoint> for ValueCommitment {
    /// Convert a Jubjub point into a ValueCommitment.
    fn from(extended_point: jubjub::ExtendedPoint) -> Self {
        let bytes = jubjub::AffinePoint::from(extended_point).to_bytes();
        ValueCommitment(bytes)
    }
}

impl ZcashDeserialize for ValueCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = [0u8; 32];
        reader.read_exact(&mut bytes)?;
        Ok(ValueCommitment(bytes))
    }
}

impl ZcashSerialize for ValueCommitment {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0)?;
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
