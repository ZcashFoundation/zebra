//! Deserializes hex-encoded inputs such as the one required
//! for the `submitblock` RPC method.

use std::fmt;

/// Deserialize hex-encoded strings to bytes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct HexData(
    #[serde(serialize_with = "hex::serialize")]
    #[schemars(with = "String")]
    pub Vec<u8>,
);

impl<'de> serde::Deserialize<'de> for HexData {
    // The type parameter has the name that serde's derive gave it, so this method's public
    // signature is the one that the derived implementation had.
    fn deserialize<__D>(deserializer: __D) -> Result<Self, __D::Error>
    where
        __D: serde::Deserializer<'de>,
    {
        struct HexDataVisitor;

        impl serde::de::Visitor<'_> for HexDataVisitor {
            type Value = HexData;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a hex encoded string")
            }

            fn visit_str<E>(self, hex: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match decode_hex(hex.as_bytes()) {
                    Some(bytes) => Ok(HexData(bytes)),
                    // The `hex` crate reports what is wrong with the string.
                    None => hex::decode(hex).map(HexData).map_err(E::custom),
                }
            }
        }

        deserializer.deserialize_str(HexDataVisitor)
    }
}

/// The entry in [`HEX_DIGITS`] for a byte that isn't a hex digit.
///
/// Hex digit values only use the low four bits, so a bitwise OR of entries equals this value if
/// any of them is invalid.
const INVALID_HEX_DIGIT: u8 = 0xff;

/// The value of each ASCII hex digit, and [`INVALID_HEX_DIGIT`] for every other byte.
const HEX_DIGITS: [u8; 256] = {
    let mut digits = [INVALID_HEX_DIGIT; 256];

    let mut byte = u8::MIN;
    loop {
        // Widening a byte to an index can't truncate it.
        digits[byte as usize] = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => INVALID_HEX_DIGIT,
        };

        if byte == u8::MAX {
            break;
        }
        byte += 1;
    }

    digits
};

/// Decodes `hex`, or returns `None` if it isn't an even number of hex digits.
///
/// Blocks are mostly proofs and ciphertexts, so their hex digits are unpredictable. Comparing
/// each digit against the digit ranges, like the `hex` crate does, mispredicts a branch for many
/// of them, which makes decoding a block several times slower than looking the digits up.
fn decode_hex(hex: &[u8]) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }

    let mut any_digit = 0;
    let bytes = hex
        .chunks_exact(2)
        .map(|pair| {
            let high = HEX_DIGITS[usize::from(pair[0])];
            let low = HEX_DIGITS[usize::from(pair[1])];
            any_digit |= high | low;

            (high << 4) | (low & 0x0f)
        })
        .collect();

    (any_digit != INVALID_HEX_DIGIT).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes a JSON string as [`HexData`].
    fn from_json(hex: &str) -> Result<Vec<u8>, String> {
        serde_json::from_value(serde_json::Value::String(hex.to_string()))
            .map(|HexData(bytes)| bytes)
            .map_err(|error| error.to_string())
    }

    /// Decodes a JSON string with the `hex` crate, like [`HexData`] did before it had its own
    /// decoder.
    fn from_json_with_hex_crate(hex: &str) -> Result<Vec<u8>, String> {
        #[derive(serde::Deserialize)]
        struct Reference(#[serde(with = "hex")] Vec<u8>);

        serde_json::from_value(serde_json::Value::String(hex.to_string()))
            .map(|Reference(bytes)| bytes)
            .map_err(|error| error.to_string())
    }

    /// Every byte decodes from lowercase and uppercase hex, and encodes back to lowercase hex.
    #[test]
    fn hex_data_round_trips_every_byte() {
        let _init_guard = zebra_test::init();

        let bytes: Vec<u8> = (u8::MIN..=u8::MAX).collect();
        let lowercase = hex::encode(&bytes);

        assert_eq!(from_json(&lowercase), Ok(bytes.clone()));
        assert_eq!(from_json(&lowercase.to_uppercase()), Ok(bytes.clone()));
        assert_eq!(from_json(""), Ok(Vec::new()));

        assert_eq!(
            serde_json::to_value(HexData(bytes)).expect("hex data serializes to a string"),
            serde_json::Value::String(lowercase),
        );
    }

    /// Strings that aren't hex are rejected with the errors of the `hex` crate, wherever the
    /// first invalid character is.
    #[test]
    fn hex_data_rejects_invalid_strings_like_the_hex_crate() {
        let _init_guard = zebra_test::init();

        let valid = "00ff10ab";
        for invalid in [
            "0", "abc", "0g", "g0", " 0", "0x00", "zz", "é0", "00ff1/", "00:f",
        ] {
            assert!(from_json_with_hex_crate(invalid).is_err());
            assert_eq!(from_json(invalid), from_json_with_hex_crate(invalid));

            // An invalid character after valid digits is still found.
            let invalid_suffix = format!("{valid}{invalid}");
            assert!(from_json_with_hex_crate(&invalid_suffix).is_err());
            assert_eq!(
                from_json(&invalid_suffix),
                from_json_with_hex_crate(&invalid_suffix),
            );
        }

        // Every byte that isn't a hex digit is rejected in both positions of a pair.
        for byte in u8::MIN..=u8::MAX {
            if byte.is_ascii_hexdigit() {
                continue;
            }

            assert_eq!(decode_hex(&[byte, b'0']), None);
            assert_eq!(decode_hex(&[b'0', byte]), None);
        }
    }
}
