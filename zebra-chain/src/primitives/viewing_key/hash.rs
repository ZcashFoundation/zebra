use std::fmt;

use super::ViewingKey;

/// The size of [`KeyHash`]
const KEY_HASH_BYTE_SIZE: usize = 32;

/// The hash of a viewing key for use as an identifier in zebra-scan, and
/// for authorizing remote access via the scanner's gRPC interface.
#[derive(Debug, PartialOrd, Ord, PartialEq, Eq)]
pub struct ViewingKeyHash([u8; KEY_HASH_BYTE_SIZE]);

impl From<&ViewingKey> for ViewingKeyHash {
    fn from(viewing_key: &ViewingKey) -> Self {
        ViewingKeyHash::new(viewing_key)
    }
}

/// A [`ViewingKey`] and its [`KeyHash`]
pub struct ViewingKeyWithHash {
    /// The [`ViewingKey`]
    pub key: ViewingKey,

    /// The [`KeyHash`] for the above `key`
    pub hash: ViewingKeyHash,
}

impl ViewingKeyHash {
    /// Creates a new [`KeyHash`] for the provided viewing key
    pub fn new(viewing_key: &ViewingKey) -> Self {
        Self(
            blake2b_simd::Params::new()
                .hash_length(KEY_HASH_BYTE_SIZE)
                .to_state()
                .update(&viewing_key.to_bytes())
                .finalize()
                .as_bytes()
                .try_into()
                .expect("hash length should be KEY_HASH_BYTE_SIZE bytes"),
        )
    }
}

impl fmt::Display for ViewingKeyHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&bs58::encode(self.0).into_string())
    }
}

impl std::str::FromStr for ViewingKeyHash {
    type Err = bs58::decode::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut output = [0u8; KEY_HASH_BYTE_SIZE];
        bs58::decode(s).onto(&mut output)?;
        Ok(Self(output))
    }
}

impl From<ViewingKey> for ViewingKeyWithHash {
    fn from(key: ViewingKey) -> Self {
        Self {
            hash: ViewingKeyHash::new(&key),
            key,
        }
    }
}

impl From<&ViewingKey> for ViewingKeyWithHash {
    fn from(viewing_key: &ViewingKey) -> Self {
        viewing_key.clone().into()
    }
}
