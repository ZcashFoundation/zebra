//! Note and value commitments.

use std::io;

use hex::{FromHex, FromHexError, ToHex};

use crate::serialization::{serde_helpers, SerializationError, ZcashDeserialize, ZcashSerialize};

pub mod pedersen_hashes;

#[cfg(test)]
mod test_vectors;

/// The randomness used in the Pedersen Hash for note commitment.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CommitmentRandomness(jubjub::Fr);

/// A wrapper for the Sapling value commitment type.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ValueCommitment(
    #[serde(with = "serde_helpers::ValueCommitment")] pub sapling_crypto::value::ValueCommitment,
);

impl PartialEq for ValueCommitment {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_inner() == other.0.as_inner()
    }
}
impl Eq for ValueCommitment {}

impl ValueCommitment {
    /// Return the hash bytes in big-endian byte-order suitable for printing out byte by byte.
    ///
    /// Zebra displays commitment value in big-endian byte-order,
    /// following the convention set by zcashd.
    pub fn bytes_in_display_order(&self) -> [u8; 32] {
        let mut reversed_bytes = self.0.to_bytes();
        reversed_bytes.reverse();
        reversed_bytes
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

        ValueCommitment(value_commitment)
    }
}

impl ZcashDeserialize for sapling_crypto::value::ValueCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut buf = [0u8; 32];
        reader.read_exact(&mut buf)?;

        let value_commitment: Option<sapling_crypto::value::ValueCommitment> =
            sapling_crypto::value::ValueCommitment::from_bytes_not_small_order(&buf).into_option();

        value_commitment.ok_or(SerializationError::Parse("invalid ValueCommitment bytes"))
    }
}

impl ZcashDeserialize for ValueCommitment {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        let value_commitment = sapling_crypto::value::ValueCommitment::zcash_deserialize(reader)?;
        Ok(Self(value_commitment))
    }
}

impl ZcashSerialize for ValueCommitment {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0.to_bytes())?;
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
