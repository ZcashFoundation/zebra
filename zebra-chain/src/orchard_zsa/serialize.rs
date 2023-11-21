//! Serialization and deserialization for Orchard ZSA specific types.

use crate::serialization::TrustedPreallocate;

use crate::amount::Amount;

use orchard_zsa::note::AssetBase;

// The size of AssetBase in bytes
const ASSET_BASE_SIZE: u64 = 32;

// The size of Amount in bytes
const AMOUNT_SIZE: u64 = 8;

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
