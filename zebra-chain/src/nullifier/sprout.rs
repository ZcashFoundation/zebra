//! Sprout Nullifier Set types and impls
#![allow(clippy::unit_arg)]
use crate::serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize};
use serde::{Deserialize, Serialize};
use std::io;

/// A Nullifier Set for Sprout transactions
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Nullifier([u8; 32]);

impl From<[u8; 32]> for Nullifier {
    fn from(buf: [u8; 32]) -> Self {
        Self(buf)
    }
}

impl ZcashDeserialize for Nullifier {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let bytes = reader.read_32_bytes()?;

        Ok(Self(bytes))
    }
}

impl ZcashSerialize for Nullifier {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])
    }
}
