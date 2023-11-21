//! Orchard ZSA burn related functionality.

// FIXME: add docs and comments

use std::io;

use crate::serialization::{
    ReadZcashExt, SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize,
};

use crate::amount::Amount;

use orchard_zsa::note::AssetBase;

// FIXME: TODO: implement tests
/*
#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
#[cfg(test)]
mod tests;
*/

// The size of AssetBase in bytes
const ASSET_BASE_SIZE: u64 = 32;

// The size of Amount in bytes
const AMOUNT_SIZE: u64 = 8;

impl ZcashSerialize for AssetBase {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.to_bytes())
    }
}

impl ZcashDeserialize for AssetBase {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Option::from(AssetBase::from_bytes(&reader.read_32_bytes()?))
            .ok_or_else(|| SerializationError::Parse("Invalid AssetBase!"))
    }
}

impl TrustedPreallocate for AssetBase {
    fn max_allocation() -> u64 {
        ASSET_BASE_SIZE
    }
}

impl TrustedPreallocate for Amount {
    fn max_allocation() -> u64 {
        AMOUNT_SIZE
    }
}
