use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct Stamp {}

impl ZcashSerialize for Stamp {
    fn zcash_serialize<W: io::Write>(&self, _writer: W) -> Result<(), io::Error> {
        // Empty stamp placeholder - no serialization needed yet
        Ok(())
    }
}

impl ZcashDeserialize for Stamp {
    fn zcash_deserialize<R: io::Read>(_reader: R) -> Result<Self, SerializationError> {
        // Empty stamp placeholder - no deserialization needed yet
        Ok(Stamp {})
    }
}