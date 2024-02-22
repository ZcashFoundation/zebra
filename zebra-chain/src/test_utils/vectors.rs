//! Network methods for fetching blockchain vectors.
//!

#[cfg(any(test, feature = "proptest-impl"))]
use std::collections::BTreeMap;

#[cfg(any(test, feature = "proptest-impl"))]
use crate::{
    block::Block,
    parameters::Network,
    serialization::{SerializationError, ZcashDeserializeInto},
};

#[cfg(any(test, feature = "proptest-impl"))]
use zebra_test::vectors::{
    BLOCK_MAINNET_1046400_BYTES, BLOCK_MAINNET_653599_BYTES, BLOCK_MAINNET_982681_BYTES,
    BLOCK_TESTNET_1116000_BYTES, BLOCK_TESTNET_583999_BYTES, BLOCK_TESTNET_925483_BYTES,
    CONTINUOUS_MAINNET_BLOCKS, CONTINUOUS_TESTNET_BLOCKS, MAINNET_BLOCKS,
    MAINNET_FINAL_SAPLING_ROOTS, MAINNET_FINAL_SPROUT_ROOTS,
    SAPLING_FINAL_ROOT_MAINNET_1046400_BYTES, SAPLING_FINAL_ROOT_TESTNET_1116000_BYTES,
    TESTNET_BLOCKS, TESTNET_FINAL_SAPLING_ROOTS, TESTNET_FINAL_SPROUT_ROOTS,
};

/// Network methods for fetching blockchain vectors.
#[cfg(any(test, feature = "proptest-impl"))]
impl Network {
    /// Returns true if network is of type Mainnet.
    pub fn is_mainnet(&self) -> bool {
        matches!(self, Network::Mainnet)
    }

    /// Returns iterator over blocks.
    pub fn get_block_iter(&self) -> std::collections::btree_map::Iter<'static, u32, &'static [u8]> {
        if self.is_mainnet() {
            MAINNET_BLOCKS.iter()
        } else {
            TESTNET_BLOCKS.iter()
        }
    }

    /// Returns genesis block for chain.
    pub fn get_gen_block(&self) -> std::option::Option<&&[u8]> {
        if self.is_mainnet() {
            MAINNET_BLOCKS.get(&0)
        } else {
            TESTNET_BLOCKS.get(&0)
        }
    }

    /// Returns block bytes
    pub fn get_block_bytes(&self, version: u32) -> Result<Block, SerializationError> {
        if self.is_mainnet() {
            match version {
                653_599 => BLOCK_MAINNET_653599_BYTES.zcash_deserialize_into(),
                982_681 => BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into(),
                _ => Err(SerializationError::UnsupportedVersion(version)),
            }
        } else {
            match version {
                583_999 => BLOCK_TESTNET_583999_BYTES.zcash_deserialize_into(),
                925_483 => BLOCK_TESTNET_925483_BYTES.zcash_deserialize_into(),
                _ => Err(SerializationError::UnsupportedVersion(version)),
            }
        }
    }

    /// Returns iterator over blockchain.
    pub fn get_blockchain_iter(&self) -> std::collections::btree_map::Iter<'_, u32, &[u8]> {
        if self.is_mainnet() {
            CONTINUOUS_MAINNET_BLOCKS.iter()
        } else {
            CONTINUOUS_TESTNET_BLOCKS.iter()
        }
    }

    /// Returns BTreemap of blockchain.
    pub fn get_blockchain_map(&self) -> &BTreeMap<u32, &'static [u8]> {
        if self.is_mainnet() {
            &CONTINUOUS_MAINNET_BLOCKS
        } else {
            &CONTINUOUS_TESTNET_BLOCKS
        }
    }

    /// Returns iterator over blocks and sapling roots.
    pub fn get_block_sapling_roots_iter(
        &self,
    ) -> (
        std::collections::btree_map::Iter<'_, u32, &[u8]>,
        std::collections::BTreeMap<u32, &[u8; 32]>,
    ) {
        if self.is_mainnet() {
            (MAINNET_BLOCKS.iter(), MAINNET_FINAL_SAPLING_ROOTS.clone())
        } else {
            (TESTNET_BLOCKS.iter(), TESTNET_FINAL_SAPLING_ROOTS.clone())
        }
    }

    /// Returns BTreemap of blocks and sapling roots.
    pub fn get_block_sapling_roots_map(
        &self,
    ) -> (
        &std::collections::BTreeMap<u32, &'static [u8]>,
        &std::collections::BTreeMap<u32, &'static [u8; 32]>,
    ) {
        if self.is_mainnet() {
            (&*MAINNET_BLOCKS, &*MAINNET_FINAL_SAPLING_ROOTS)
        } else {
            (&*TESTNET_BLOCKS, &*TESTNET_FINAL_SAPLING_ROOTS)
        }
    }

    /// Returns block and sapling root bytes
    pub fn get_block_sapling_roots_bytes(
        &self,
        version: u32,
    ) -> Result<(&[u8], [u8; 32]), SerializationError> {
        if self.is_mainnet() {
            match version {
                1_046_400_ => Ok((
                    &BLOCK_MAINNET_1046400_BYTES[..],
                    *SAPLING_FINAL_ROOT_MAINNET_1046400_BYTES,
                )),
                _ => Err(SerializationError::UnsupportedVersion(version)),
            }
        } else {
            match version {
                1_116_000 => Ok((
                    &BLOCK_TESTNET_1116000_BYTES[..],
                    *SAPLING_FINAL_ROOT_TESTNET_1116000_BYTES,
                )),
                _ => Err(SerializationError::UnsupportedVersion(version)),
            }
        }
    }

    /// Returns BTreemap of blocks and sprout roots, and last split height.
    pub fn get_block_sprout_roots_height(
        &self,
    ) -> (
        &std::collections::BTreeMap<u32, &'static [u8]>,
        &std::collections::BTreeMap<u32, &'static [u8; 32]>,
        u32,
    ) {
        // The mainnet block height at which the first JoinSplit occurred.
        const MAINNET_FIRST_JOINSPLIT_HEIGHT: u32 = 396;

        // The testnet block height at which the first JoinSplit occurred.
        const TESTNET_FIRST_JOINSPLIT_HEIGHT: u32 = 2259;
        if self.is_mainnet() {
            (
                &*MAINNET_BLOCKS,
                &*MAINNET_FINAL_SPROUT_ROOTS,
                MAINNET_FIRST_JOINSPLIT_HEIGHT,
            )
        } else {
            (
                &*TESTNET_BLOCKS,
                &*TESTNET_FINAL_SPROUT_ROOTS,
                TESTNET_FIRST_JOINSPLIT_HEIGHT,
            )
        }
    }
}
