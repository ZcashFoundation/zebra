//! Network methods for fetching blockchain vectors.
//!

use std::{collections::BTreeMap, ops::RangeBounds};

use crate::{
    amount::Amount,
    block::Block,
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{UnminedTx, VerifiedUnminedTx},
};

use zebra_test::vectors::{
    BLOCK_MAINNET_1046400_BYTES, BLOCK_MAINNET_653599_BYTES, BLOCK_MAINNET_982681_BYTES,
    BLOCK_TESTNET_1116000_BYTES, BLOCK_TESTNET_583999_BYTES, BLOCK_TESTNET_925483_BYTES,
    CONTINUOUS_MAINNET_BLOCKS, CONTINUOUS_TESTNET_BLOCKS, MAINNET_BLOCKS,
    MAINNET_FINAL_ORCHARD_ROOTS, MAINNET_FINAL_SAPLING_ROOTS, MAINNET_FINAL_SPROUT_ROOTS,
    SAPLING_FINAL_ROOT_MAINNET_1046400_BYTES, SAPLING_FINAL_ROOT_TESTNET_1116000_BYTES,
    TESTNET_BLOCKS, TESTNET_FINAL_ORCHARD_ROOTS, TESTNET_FINAL_SAPLING_ROOTS,
    TESTNET_FINAL_SPROUT_ROOTS,
};

/// Network methods for fetching blockchain vectors.
impl Network {
    /// Returns true if network is of type Mainnet.
    pub fn is_mainnet(&self) -> bool {
        matches!(self, Network::Mainnet)
    }

    /// Returns iterator over blocks.
    pub fn block_iter(&self) -> std::collections::btree_map::Iter<'static, u32, &'static [u8]> {
        if self.is_mainnet() {
            MAINNET_BLOCKS.iter()
        } else {
            TESTNET_BLOCKS.iter()
        }
    }

    /// Returns iterator over deserialized blocks.
    pub fn block_parsed_iter(&self) -> impl Iterator<Item = Block> {
        self.block_iter().map(|(_, block_bytes)| {
            block_bytes
                .zcash_deserialize_into::<Block>()
                .expect("block is structurally valid")
        })
    }

    /// Returns iterator over verified unmined transactions in the provided block height range.
    pub fn unmined_transactions_in_blocks(
        &self,
        block_height_range: impl RangeBounds<u32>,
    ) -> impl DoubleEndedIterator<Item = VerifiedUnminedTx> {
        let blocks = self.block_iter();

        // Deserialize the blocks that are selected based on the specified `block_height_range`.
        let selected_blocks = blocks
            .filter(move |(&height, _)| block_height_range.contains(&height))
            .map(|(_, block)| {
                block
                    .zcash_deserialize_into::<Block>()
                    .expect("block test vector is structurally valid")
            });

        // Extract the transactions from the blocks and wrap each one as an unmined transaction.
        // Use a fake zero miner fee and sigops, because we don't have the UTXOs to calculate
        // the correct fee.
        selected_blocks
            .flat_map(|block| block.transactions)
            .map(UnminedTx::from)
            // Skip transactions that fail ZIP-317 mempool checks
            .filter_map(|transaction| {
                VerifiedUnminedTx::new(
                    transaction,
                    Amount::try_from(1_000_000).expect("valid amount"),
                    0,
                )
                .ok()
            })
    }

    /// Returns blocks indexed by height in a [`BTreeMap`].
    ///
    /// Returns Mainnet blocks if `self` is set to Mainnet, and Testnet blocks otherwise.
    pub fn block_map(&self) -> BTreeMap<u32, &'static [u8]> {
        if self.is_mainnet() {
            zebra_test::vectors::MAINNET_BLOCKS.clone()
        } else {
            zebra_test::vectors::TESTNET_BLOCKS.clone()
        }
    }

    /// Returns genesis block for chain.
    pub fn gen_block(&self) -> std::option::Option<&&[u8]> {
        if self.is_mainnet() {
            MAINNET_BLOCKS.get(&0)
        } else {
            TESTNET_BLOCKS.get(&0)
        }
    }

    /// Returns block bytes
    pub fn test_block(&self, main_height: u32, test_height: u32) -> Option<Block> {
        match (self.is_mainnet(), main_height, test_height) {
            (true, 653_599, _) => BLOCK_MAINNET_653599_BYTES.zcash_deserialize_into().ok(),
            (true, 982_681, _) => BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into().ok(),
            (false, _, 583_999) => BLOCK_TESTNET_583999_BYTES.zcash_deserialize_into().ok(),
            (false, _, 925_483) => BLOCK_TESTNET_925483_BYTES.zcash_deserialize_into().ok(),
            _ => None,
        }
    }

    /// Returns iterator over blockchain.
    pub fn blockchain_iter(&self) -> std::collections::btree_map::Iter<'_, u32, &[u8]> {
        if self.is_mainnet() {
            CONTINUOUS_MAINNET_BLOCKS.iter()
        } else {
            CONTINUOUS_TESTNET_BLOCKS.iter()
        }
    }

    /// Returns BTreemap of blockchain.
    pub fn blockchain_map(&self) -> &BTreeMap<u32, &'static [u8]> {
        if self.is_mainnet() {
            &CONTINUOUS_MAINNET_BLOCKS
        } else {
            &CONTINUOUS_TESTNET_BLOCKS
        }
    }

    /// Returns a [`BTreeMap`] of heights and Sapling anchors for this network.
    pub fn sapling_anchors(&self) -> std::collections::BTreeMap<u32, &[u8; 32]> {
        if self.is_mainnet() {
            MAINNET_FINAL_SAPLING_ROOTS.clone()
        } else {
            TESTNET_FINAL_SAPLING_ROOTS.clone()
        }
    }

    /// Returns a [`BTreeMap`] of heights and Orchard anchors for this network.
    pub fn orchard_anchors(&self) -> std::collections::BTreeMap<u32, &[u8; 32]> {
        if self.is_mainnet() {
            MAINNET_FINAL_ORCHARD_ROOTS.clone()
        } else {
            TESTNET_FINAL_ORCHARD_ROOTS.clone()
        }
    }

    /// Returns BTreemap of blocks and sapling roots.
    pub fn block_sapling_roots_map(
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
    pub fn test_block_sapling_roots(
        &self,
        main_height: u32,
        test_height: u32,
    ) -> Option<(&[u8], [u8; 32])> {
        match (self.is_mainnet(), main_height, test_height) {
            (true, 1_046_400, _) => Some((
                &BLOCK_MAINNET_1046400_BYTES[..],
                *SAPLING_FINAL_ROOT_MAINNET_1046400_BYTES,
            )),
            (false, _, 1_116_000) => Some((
                &BLOCK_TESTNET_1116000_BYTES[..],
                *SAPLING_FINAL_ROOT_TESTNET_1116000_BYTES,
            )),
            _ => None,
        }
    }

    /// Returns BTreemap of blocks and sprout roots, and last split height.
    pub fn block_sprout_roots_height(
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
