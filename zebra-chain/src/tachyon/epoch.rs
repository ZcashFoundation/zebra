//! Epoch identifiers for Tachyon nullifier "flavoring".
//!
//! The epoch ID enables oblivious syncing by partitioning nullifiers into
//! time-based epochs. Wallets can delegate scanning to untrusted services
//! using constrained PRF keys that only work for specific epoch ranges.

use std::io;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::{
    block,
    serialization::{SerializationError, ZcashDeserialize, ZcashSerialize},
};

/// Epoch identifier for nullifier "flavor".
///
/// The epoch ID is derived from block height and enables partitioned wallet
/// scanning. Oblivious syncing services can be given constrained PRF keys
/// that only work for epochs up to a certain point, preventing them from
/// learning about future transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EpochId(pub u32);

impl EpochId {
    /// The size of a serialized EpochId in bytes.
    pub const SIZE: usize = 4;

    /// Default epoch length in blocks.
    ///
    /// This determines how many blocks are in each epoch. A shorter epoch
    /// length provides more granular delegation but increases bandwidth
    /// for constrained key distribution.
    ///
    /// TODO: This should be a consensus parameter.
    pub const DEFAULT_EPOCH_LENGTH: u32 = 1000;

    /// Create an EpochId from a raw value.
    pub const fn new(epoch: u32) -> Self {
        Self(epoch)
    }

    /// Get the raw epoch value.
    pub const fn value(&self) -> u32 {
        self.0
    }

    /// Compute the epoch ID from a block height.
    ///
    /// Uses the default epoch length. For custom epoch lengths, use
    /// `from_height_with_length`.
    pub fn from_height(height: block::Height) -> Self {
        Self::from_height_with_length(height, Self::DEFAULT_EPOCH_LENGTH)
    }

    /// Compute the epoch ID from a block height with a custom epoch length.
    pub fn from_height_with_length(height: block::Height, epoch_length: u32) -> Self {
        debug_assert!(epoch_length > 0, "epoch length must be positive");
        Self(height.0 / epoch_length)
    }

    /// Get the first block height of this epoch.
    pub fn start_height(&self) -> block::Height {
        self.start_height_with_length(Self::DEFAULT_EPOCH_LENGTH)
    }

    /// Get the first block height of this epoch with a custom epoch length.
    pub fn start_height_with_length(&self, epoch_length: u32) -> block::Height {
        block::Height(self.0.saturating_mul(epoch_length))
    }
}

impl From<u32> for EpochId {
    fn from(epoch: u32) -> Self {
        Self(epoch)
    }
}

impl From<EpochId> for u32 {
    fn from(epoch: EpochId) -> Self {
        epoch.0
    }
}

impl ZcashSerialize for EpochId {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(self.0)
    }
}

impl ZcashDeserialize for EpochId {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self(reader.read_u32::<LittleEndian>()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_from_height() {
        let _init_guard = zebra_test::init();

        // Block 0 is epoch 0
        assert_eq!(EpochId::from_height(block::Height(0)), EpochId::new(0));

        // Block 999 is still epoch 0
        assert_eq!(EpochId::from_height(block::Height(999)), EpochId::new(0));

        // Block 1000 is epoch 1
        assert_eq!(EpochId::from_height(block::Height(1000)), EpochId::new(1));

        // Block 2500 is epoch 2
        assert_eq!(EpochId::from_height(block::Height(2500)), EpochId::new(2));
    }

    #[test]
    fn epoch_start_height() {
        let _init_guard = zebra_test::init();

        assert_eq!(EpochId::new(0).start_height(), block::Height(0));
        assert_eq!(EpochId::new(1).start_height(), block::Height(1000));
        assert_eq!(EpochId::new(5).start_height(), block::Height(5000));
    }
}
