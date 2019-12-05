//! Contains impls of `ZcashSerialize`, `ZcashDeserialize` for all of the
//! transaction types, so that all of the serialization logic is in one place.

use std::io;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

use super::Transaction;

impl ZcashSerialize for Transaction {
    fn zcash_serialize<W: io::Write>(&self, _writer: W) -> Result<(), SerializationError> {
        unimplemented!();
    }
}

impl ZcashDeserialize for Transaction {
    fn zcash_deserialize<R: io::Read>(_reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}
