//! Authenticates key hashes and validates viewing keys.

use std::fmt;

use zcash_primitives::{sapling::SaplingIvk, zip32::DiversifiableFullViewingKey};

const KEY_HASH_BYTE_SIZE: usize = 32;

#[derive(Debug, Clone)]
pub enum ViewingKey {
    SaplingIvk(Box<SaplingIvk>),
    DiversifiableFullViewingKey(Box<DiversifiableFullViewingKey>),
}

impl ViewingKey {
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::SaplingIvk(ivk) => ivk.to_repr().to_vec(),
            Self::DiversifiableFullViewingKey(dfvk) => dfvk.to_bytes().to_vec(),
        }
    }
}

impl From<DiversifiableFullViewingKey> for ViewingKey {
    fn from(value: DiversifiableFullViewingKey) -> Self {
        Self::DiversifiableFullViewingKey(Box::new(value))
    }
}

#[derive(Debug, PartialOrd, Ord, PartialEq, Eq)]
pub struct KeyHash([u8; KEY_HASH_BYTE_SIZE]);

impl From<&ViewingKey> for KeyHash {
    fn from(viewing_key: &ViewingKey) -> Self {
        KeyHash::new(viewing_key)
    }
}

pub struct ViewingKeyWithHash {
    pub key: ViewingKey,
    pub hash: KeyHash,
}

impl From<ViewingKey> for ViewingKeyWithHash {
    fn from(key: ViewingKey) -> Self {
        Self {
            hash: KeyHash::new(&key),
            key,
        }
    }
}

impl From<&ViewingKey> for ViewingKeyWithHash {
    fn from(viewing_key: &ViewingKey) -> Self {
        viewing_key.clone().into()
    }
}

impl KeyHash {
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

impl fmt::Display for KeyHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&bs58::encode(self.0).into_string())
    }
}

impl std::str::FromStr for KeyHash {
    type Err = bs58::decode::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut output = [0u8; KEY_HASH_BYTE_SIZE];
        bs58::decode(s).onto(&mut output)?;
        Ok(Self(output))
    }
}
