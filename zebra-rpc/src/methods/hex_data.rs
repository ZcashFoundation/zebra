//! Hex conversion for serialized RPC blocks and transactions.

use std::{fmt, marker::PhantomData};

use hex::FromHexError;
use serde::{de, Deserializer, Serializer};

#[cfg(test)]
mod tests;

const DIGITS: &[u8; 16] = b"0123456789abcdef";

const DECODE: [u8; 256] = {
    let mut table = [u8::MAX; 256];
    let mut digit = 0;
    while digit < DIGITS.len() {
        // The digit is below 16, and widening ASCII bytes to indices is lossless.
        table[DIGITS[digit] as usize] = digit as u8;
        if digit >= 10 {
            table[b'A' as usize + digit - 10] = digit as u8;
        }
        digit += 1;
    }
    table
};

/// Encode bytes as lowercase hex.
pub(super) fn encode(bytes: &[u8]) -> String {
    const_hex::encode(bytes)
}

/// Decode hex with the same byte-indexed errors and precedence as the `hex` crate.
pub(super) fn decode(bytes: &[u8]) -> Result<Vec<u8>, FromHexError> {
    // Adapted from Zakura (MIT OR Apache-2.0): const-hex accepts `0x`, unlike the scalar decoder.
    if matches!(bytes, [b'0', b'x' | b'X', ..]) {
        return decode_scalar(bytes);
    }

    const_hex::decode(bytes).or_else(|_| decode_scalar(bytes))
}

/// Original decoder, retained for exact error compatibility on invalid input.
fn decode_scalar(bytes: &[u8]) -> Result<Vec<u8>, FromHexError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(FromHexError::OddLength);
    }

    let mut decoded = vec![0; bytes.len() / 2];
    for (index, (pair, byte)) in bytes.chunks_exact(2).zip(&mut decoded).enumerate() {
        let high = DECODE[usize::from(pair[0])];
        let low = DECODE[usize::from(pair[1])];
        if high | low == u8::MAX {
            let offset = usize::from(high != u8::MAX);
            return Err(FromHexError::InvalidHexCharacter {
                c: char::from(pair[offset]),
                index: index * 2 + offset,
            });
        }
        *byte = high << 4 | low;
    }
    Ok(decoded)
}

/// Serialize byte-backed RPC data as a hex string.
pub(super) fn serialize<S, T>(data: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: AsRef<[u8]>,
{
    serializer.serialize_str(&encode(data.as_ref()))
}

/// Deserialize a hex string into byte-backed RPC data, matching `hex::deserialize`.
pub(super) fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: From<Vec<u8>>,
{
    struct HexVisitor<T>(PhantomData<T>);

    impl<T: From<Vec<u8>>> de::Visitor<'_> for HexVisitor<T> {
        type Value = T;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a hex encoded string")
        }

        fn visit_str<E: de::Error>(self, data: &str) -> Result<T, E> {
            decode(data.as_bytes()).map(T::from).map_err(E::custom)
        }
    }

    deserializer.deserialize_str(HexVisitor(PhantomData))
}

/// `#[serde(with = ...)]` helpers for byte fields in RPC responses: SIMD encoding,
/// and the `hex` crate's decoding so accepted input is unchanged.
pub(crate) mod bytes {
    use serde::{Deserializer, Serializer};

    /// Serialize bytes as a lowercase hex string.
    pub(crate) fn serialize<S: Serializer, T: AsRef<[u8]>>(
        data: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::encode(data.as_ref()))
    }

    /// Deserialize a hex string, as `hex::deserialize` does.
    pub(crate) fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: hex::FromHex,
        <T as hex::FromHex>::Error: std::fmt::Display,
    {
        hex::deserialize(deserializer)
    }
}

/// `serde_with` adapter for byte fields in RPC responses: SIMD encoding, and
/// `serde_with::hex::Hex` decoding so accepted input is unchanged.
pub(crate) struct Hex;

impl<T: AsRef<[u8]>> serde_with::SerializeAs<T> for Hex {
    fn serialize_as<S: Serializer>(source: &T, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode(source.as_ref()))
    }
}

impl<'de, T> serde_with::DeserializeAs<'de, T> for Hex
where
    serde_with::hex::Hex: serde_with::DeserializeAs<'de, T>,
{
    fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<T, D::Error> {
        <serde_with::hex::Hex as serde_with::DeserializeAs<'de, T>>::deserialize_as(deserializer)
    }
}

/// Deserialize hex-encoded strings to bytes.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, schemars::JsonSchema,
)]
pub struct HexData(
    #[serde(with = "crate::methods::hex_data")]
    #[schemars(with = "String")]
    pub Vec<u8>,
);
