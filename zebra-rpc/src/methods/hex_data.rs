//! Hex conversion for serialized RPC blocks and transactions.

use std::{fmt, marker::PhantomData};

use hex::FromHexError;
use serde::{de, Deserializer, Serializer};

#[cfg(test)]
mod tests;

const DIGITS: &[u8; 16] = b"0123456789abcdef";

const ENCODE: [[u8; 2]; 256] = {
    let mut table = [[0; 2]; 256];
    let mut byte = 0;
    while byte < table.len() {
        table[byte] = [DIGITS[byte >> 4], DIGITS[byte & 0xf]];
        byte += 1;
    }
    table
};

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

/// Encode bytes as lowercase hex in a single preallocated buffer.
pub(super) fn encode(bytes: &[u8]) -> String {
    let mut encoded = vec![0; bytes.len() * 2];
    for (byte, pair) in bytes.iter().zip(encoded.chunks_exact_mut(2)) {
        pair.copy_from_slice(&ENCODE[usize::from(*byte)]);
    }
    String::from_utf8(encoded).expect("the encoding table contains only ASCII hex digits")
}

/// Decode hex with the same byte-indexed errors and precedence as the `hex` crate.
pub(super) fn decode(bytes: &[u8]) -> Result<Vec<u8>, FromHexError> {
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

/// Deserialize hex-encoded strings to bytes.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, schemars::JsonSchema,
)]
pub struct HexData(
    #[serde(with = "crate::methods::hex_data")]
    #[schemars(with = "String")]
    pub Vec<u8>,
);
