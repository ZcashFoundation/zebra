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

use crate::service::finalized_state::disk_format::{FromDisk, IntoDisk, IntoDiskFixedLen};

impl IntoDisk for transparent::Utxo {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        let height_bytes = self.height.as_bytes().to_vec();
        let coinbase_flag_bytes = [self.from_coinbase as u8].to_vec();
        let output_bytes = self
            .output
            .zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail");

        [height_bytes, coinbase_flag_bytes, output_bytes].concat()
    }
}

impl FromDisk for transparent::Utxo {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let height_len = Height::fixed_byte_len();

        let (height_bytes, rest_bytes) = bytes.as_ref().split_at(height_len);
        let (coinbase_flag_bytes, output_bytes) = rest_bytes.split_at(1);

        let height = Height::from_bytes(height_bytes);
        let from_coinbase = coinbase_flag_bytes[0] == 1u8;
        let output = output_bytes
            .zcash_deserialize_into()
            .expect("db has valid serialized data");

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
        // TODO: serialize the index into a smaller number of bytes (#3152)
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }
}
