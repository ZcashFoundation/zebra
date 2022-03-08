//! Transparent transfer serialization formats for finalized data.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use zebra_chain::{
    block::Height,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transparent,
};

use crate::service::finalized_state::disk_format::{FromDisk, IntoDisk};

impl IntoDisk for transparent::Utxo {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        let mut bytes = vec![0; 5];
        bytes[0..4].copy_from_slice(&self.height.0.to_be_bytes());
        bytes[4] = self.from_coinbase as u8;
        self.output
            .zcash_serialize(&mut bytes)
            .expect("serialization to vec doesn't fail");
        bytes
    }
}

impl FromDisk for transparent::Utxo {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let (meta_bytes, output_bytes) = bytes.as_ref().split_at(5);
        let height = Height(u32::from_be_bytes(meta_bytes[0..4].try_into().unwrap()));
        let from_coinbase = meta_bytes[4] == 1u8;
        let output = output_bytes
            .zcash_deserialize_into()
            .expect("db has serialized data");
        Self {
            output,
            height,
            from_coinbase,
        }
    }
}

impl IntoDisk for transparent::OutPoint {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }
}
