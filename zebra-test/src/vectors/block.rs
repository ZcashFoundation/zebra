//! Block test vectors

#![allow(missing_docs)]

use hex::FromHex;
use lazy_static::lazy_static;

use std::collections::BTreeMap;

trait ReverseCollection {
    /// Return a reversed copy of this collection
    fn rev(self) -> Self;
}

impl ReverseCollection for [u8; 32] {
    fn rev(mut self) -> [u8; 32] {
        self.reverse();
        self
    }
}

lazy_static! {
    /// All block test vectors
    pub static ref BLOCKS: Vec<&'static [u8]> = MAINNET_BLOCKS
        .iter()
        .chain(TESTNET_BLOCKS.iter())
        .map(|(_height, block)| *block)
        .collect();

    /// Continuous mainnet blocks, indexed by height
    ///
    /// Contains the continuous blockchain from genesis onwards.  Stops at the
    /// first gap in the chain.
    pub static ref CONTINUOUS_MAINNET_BLOCKS: BTreeMap<u32, &'static [u8]> = MAINNET_BLOCKS
        .iter()
        .enumerate()
        .take_while(|(i, (height, _block))| *i == **height as usize)
        .map(|(_i, (height, block))| (*height, *block))
        .collect();

    /// Continuous testnet blocks, indexed by height
    ///
    /// Contains the continuous blockchain from genesis onwards.  Stops at the
    /// first gap in the chain.
    pub static ref CONTINUOUS_TESTNET_BLOCKS: BTreeMap<u32, &'static [u8]> = TESTNET_BLOCKS
        .iter()
        .enumerate()
        .take_while(|(i, (height, _block))| *i == **height as usize)
        .map(|(_i, (height, block))| (*height, *block))
        .collect();

    // Update these lists of blocks when you add new block test vectors to
    // this file
    //
    // We use integer heights in these maps, to avoid a dependency on zebra_chain

    /// Mainnet blocks, indexed by height
    ///
    /// This is actually a bijective map, the tests ensure that values are unique.
    pub static ref MAINNET_BLOCKS: BTreeMap<u32, &'static [u8]> = [
            // Genesis
            (0, BLOCK_MAINNET_GENESIS_BYTES.as_ref()),

            // BeforeOverwinter
            (1, BLOCK_MAINNET_1_BYTES.as_ref()),
            (2, BLOCK_MAINNET_2_BYTES.as_ref()),
            (3, BLOCK_MAINNET_3_BYTES.as_ref()),
            (4, BLOCK_MAINNET_4_BYTES.as_ref()),
            (5, BLOCK_MAINNET_5_BYTES.as_ref()),
            (6, BLOCK_MAINNET_6_BYTES.as_ref()),
            (7, BLOCK_MAINNET_7_BYTES.as_ref()),
            (8, BLOCK_MAINNET_8_BYTES.as_ref()),
            (9, BLOCK_MAINNET_9_BYTES.as_ref()),
            (10, BLOCK_MAINNET_10_BYTES.as_ref()),
            (202, BLOCK_MAINNET_202_BYTES.as_ref()),
            // The first block that contains a tx with a JoinSplit.
            (396, BLOCK_MAINNET_396_BYTES.as_ref()),
            (347_499, BLOCK_MAINNET_347499_BYTES.as_ref()),

            // Overwinter
            (347_500, BLOCK_MAINNET_347500_BYTES.as_ref()),
            (347_501, BLOCK_MAINNET_347501_BYTES.as_ref()),
            (415_000, BLOCK_MAINNET_415000_BYTES.as_ref()),
            (419_199, BLOCK_MAINNET_419199_BYTES.as_ref()),

            // Sapling
            (419_200, BLOCK_MAINNET_419200_BYTES.as_ref()),
            (419_201, BLOCK_MAINNET_419201_BYTES.as_ref()),
            (419_202, BLOCK_MAINNET_419202_BYTES.as_ref()),

            // A bad version field
            (434_873, BLOCK_MAINNET_434873_BYTES.as_ref()),
            (653_599, BLOCK_MAINNET_653599_BYTES.as_ref()),

            // Blossom
            (653_600, BLOCK_MAINNET_653600_BYTES.as_ref()),
            (653_601, BLOCK_MAINNET_653601_BYTES.as_ref()),
            (902_999, BLOCK_MAINNET_902999_BYTES.as_ref()),

            // Heartwood
            (903_000, BLOCK_MAINNET_903000_BYTES.as_ref()),
            (903_001, BLOCK_MAINNET_903001_BYTES.as_ref()),

            // Shielded coinbase x3
            (949_496, BLOCK_MAINNET_949496_BYTES.as_ref()),
            (975_066, BLOCK_MAINNET_975066_BYTES.as_ref()),
            (982_681, BLOCK_MAINNET_982681_BYTES.as_ref()),

            // Last Heartwood
            (1_046_399, BLOCK_MAINNET_1046399_BYTES.as_ref()),

            // Canopy and First Coinbase Halving
            (1_046_400, BLOCK_MAINNET_1046400_BYTES.as_ref()),
            (1_046_401, BLOCK_MAINNET_1046401_BYTES.as_ref()),
            (1_180_900, BLOCK_MAINNET_1180900_BYTES.as_ref()),
        ].iter().cloned().collect();

    /// Mainnet final Sprout roots, indexed by height.
    ///
    /// If there are no Sprout inputs or outputs in a block, the final Sprout root is the same as
    /// the previous block.
    pub static ref MAINNET_FINAL_SPROUT_ROOTS: BTreeMap<u32, &'static [u8; 32]> = [
            // Genesis
            (0, SPROUT_FINAL_ROOT_MAINNET_0_BYTES.as_ref().try_into().unwrap()),
            // The first block that contains a tx with a JoinSplit.
            (396, SPROUT_FINAL_ROOT_MAINNET_396_BYTES.as_ref().try_into().unwrap()),

            // Overwinter
            (347_499, SPROUT_FINAL_ROOT_MAINNET_347499_BYTES.as_ref().try_into().unwrap()),
            (347_500, SPROUT_FINAL_ROOT_MAINNET_347500_BYTES.as_ref().try_into().unwrap()),
            (347_501, SPROUT_FINAL_ROOT_MAINNET_347501_BYTES.as_ref().try_into().unwrap()),
        ].iter().cloned().collect();

    /// Mainnet final Sapling roots, indexed by height
    ///
    /// Pre-Sapling roots are all-zeroes.  If there are no Sapling Outputs in a block, the final
    /// Sapling root is the same as the previous block.
    pub static ref MAINNET_FINAL_SAPLING_ROOTS: BTreeMap<u32, &'static [u8; 32]> = [
            // Sapling
            (419_200, SAPLING_FINAL_ROOT_MAINNET_419200_BYTES.as_ref().try_into().unwrap()),
            (419_201, SAPLING_FINAL_ROOT_MAINNET_419201_BYTES.as_ref().try_into().unwrap()),
            (419_202, SAPLING_FINAL_ROOT_MAINNET_419202_BYTES.as_ref().try_into().unwrap()),
            // A bad version field
            (434_873, SAPLING_FINAL_ROOT_MAINNET_434873_BYTES.as_ref().try_into().unwrap()),
            (653_599, SAPLING_FINAL_ROOT_MAINNET_653599_BYTES.as_ref().try_into().unwrap()),
            // Blossom
            (653_600, SAPLING_FINAL_ROOT_MAINNET_653600_BYTES.as_ref().try_into().unwrap()),
            (653_601, SAPLING_FINAL_ROOT_MAINNET_653601_BYTES.as_ref().try_into().unwrap()),
            (902_999, SAPLING_FINAL_ROOT_MAINNET_902999_BYTES.as_ref().try_into().unwrap()),
            // Heartwood
            (903_000, SAPLING_FINAL_ROOT_MAINNET_903000_BYTES.as_ref().try_into().unwrap()),
            (903_001, SAPLING_FINAL_ROOT_MAINNET_903001_BYTES.as_ref().try_into().unwrap()),
            // Shielded coinbase x3
            (949_496, SAPLING_FINAL_ROOT_MAINNET_949496_BYTES.as_ref().try_into().unwrap()),
            (975_066, SAPLING_FINAL_ROOT_MAINNET_975066_BYTES.as_ref().try_into().unwrap()),
            (982_681, SAPLING_FINAL_ROOT_MAINNET_982681_BYTES.as_ref().try_into().unwrap()),
            // Last Heartwood
            (1_046_399, SAPLING_FINAL_ROOT_MAINNET_1046399_BYTES.as_ref().try_into().unwrap()),
            // Canopy and First Coinbase Halving
            (1_046_400, SAPLING_FINAL_ROOT_MAINNET_1046400_BYTES.as_ref().try_into().unwrap()),
            (1_046_401, SAPLING_FINAL_ROOT_MAINNET_1046401_BYTES.as_ref().try_into().unwrap()),
            (1_180_900, SAPLING_FINAL_ROOT_MAINNET_1180900_BYTES.as_ref().try_into().unwrap()),
        ].iter().cloned().collect();

    /// Testnet blocks, indexed by height
    ///
    /// This is actually a bijective map, the tests ensure that values are unique.
    pub static ref TESTNET_BLOCKS: BTreeMap<u32, &'static [u8]> = [
            // Genesis
            (0, BLOCK_TESTNET_GENESIS_BYTES.as_ref()),
            // BeforeOverwinter
            (1, BLOCK_TESTNET_1_BYTES.as_ref()),
            (2, BLOCK_TESTNET_2_BYTES.as_ref()),
            (3, BLOCK_TESTNET_3_BYTES.as_ref()),
            (4, BLOCK_TESTNET_4_BYTES.as_ref()),
            (5, BLOCK_TESTNET_5_BYTES.as_ref()),
            (6, BLOCK_TESTNET_6_BYTES.as_ref()),
            (7, BLOCK_TESTNET_7_BYTES.as_ref()),
            (8, BLOCK_TESTNET_8_BYTES.as_ref()),
            (9, BLOCK_TESTNET_9_BYTES.as_ref()),
            (10, BLOCK_TESTNET_10_BYTES.as_ref()),
            // The first block that contains a tx with a JoinSplit.
            (2_259, BLOCK_TESTNET_2259_BYTES.as_ref()),
            // A large block
            (141_042, BLOCK_TESTNET_141042_BYTES.as_ref()),
            (207_499, BLOCK_TESTNET_207499_BYTES.as_ref()),
            // Overwinter
            (207_500, BLOCK_TESTNET_207500_BYTES.as_ref()),
            (207_501, BLOCK_TESTNET_207501_BYTES.as_ref()),
            (279_999, BLOCK_TESTNET_279999_BYTES.as_ref()),
            // Sapling
            (280_000, BLOCK_TESTNET_280000_BYTES.as_ref()),
            (280_001, BLOCK_TESTNET_280001_BYTES.as_ref()),
            (299_187, BLOCK_TESTNET_299187_BYTES.as_ref()),
            // Minimum-difficulty blocks x2
            // See zebra_chain's MINIMUM_DIFFICULTY_HEIGHTS for a full list
            (299_188, BLOCK_TESTNET_299188_BYTES.as_ref()),
            (299_189, BLOCK_TESTNET_299189_BYTES.as_ref()),
            (299_201, BLOCK_TESTNET_299201_BYTES.as_ref()),
            // Minimum-difficulty block
            (299_202, BLOCK_TESTNET_299202_BYTES.as_ref()),
            (583_999, BLOCK_TESTNET_583999_BYTES.as_ref()),
            // Blossom
            (584_000, BLOCK_TESTNET_584000_BYTES.as_ref()),
            (584_001, BLOCK_TESTNET_584001_BYTES.as_ref()),
            (903_799, BLOCK_TESTNET_903799_BYTES.as_ref()),
            // Heartwood
            (903_800, BLOCK_TESTNET_903800_BYTES.as_ref()),
            (903_801, BLOCK_TESTNET_903801_BYTES.as_ref()),
            // Shielded coinbase x2
            (914_678, BLOCK_TESTNET_914678_BYTES.as_ref()),
            (925_483, BLOCK_TESTNET_925483_BYTES.as_ref()),
            (1_028_499, BLOCK_TESTNET_1028499_BYTES.as_ref()),
            // Canopy
            (1_028_500, BLOCK_TESTNET_1028500_BYTES.as_ref()),
            (1_028_501, BLOCK_TESTNET_1028501_BYTES.as_ref()),
            (1_095_000, BLOCK_TESTNET_1095000_BYTES.as_ref()),
            // Shielded coinbase
            (1_101_629, BLOCK_TESTNET_1101629_BYTES.as_ref()),
            // Last Pre-Halving
            (1_115_999, BLOCK_TESTNET_1115999_BYTES.as_ref()),
            // First Coinbase Halving
            (1_116_000, BLOCK_TESTNET_1116000_BYTES.as_ref()),
            (1_116_001, BLOCK_TESTNET_1116001_BYTES.as_ref()),
            (1_326_100, BLOCK_TESTNET_1326100_BYTES.as_ref()),
            (1_599_199, BLOCK_TESTNET_1599199_BYTES.as_ref()),
        ].iter().cloned().collect();

    /// Testnet final Sprout roots, indexed by height.
    ///
    /// If there are no Sprout inputs or outputs in a block, the final Sprout root is the same as
    /// the previous block.
    pub static ref TESTNET_FINAL_SPROUT_ROOTS: BTreeMap<u32, &'static [u8; 32]> = [
        // Genesis
        (0, SPROUT_FINAL_ROOT_TESTNET_0_BYTES.as_ref().try_into().unwrap()),
        // The first block that contains a tx with a JoinSplit.
        (2259, SPROUT_FINAL_ROOT_TESTNET_2259_BYTES.as_ref().try_into().unwrap()),
    ].iter().cloned().collect();

    /// Testnet final Sapling roots, indexed by height
    ///
    /// Pre-sapling roots are all-zeroes.  If there are no Sapling Outputs in a block, the final
    /// sapling root is the same as the previous block.
    pub static ref TESTNET_FINAL_SAPLING_ROOTS: BTreeMap<u32, &'static [u8; 32]> = [
            // Sapling
            (280_000, SAPLING_FINAL_ROOT_TESTNET_280000_BYTES.as_ref().try_into().unwrap()),
            (280_001, SAPLING_FINAL_ROOT_TESTNET_280001_BYTES.as_ref().try_into().unwrap()),
            (299_187, SAPLING_FINAL_ROOT_TESTNET_299187_BYTES.as_ref().try_into().unwrap()),
            // Minimum-difficulty blocks x2
            // See zebra_chain's MINIMUM_DIFFICULTY_HEIGHTS for a full list
            (299_188, SAPLING_FINAL_ROOT_TESTNET_299188_BYTES.as_ref().try_into().unwrap()),
            (299_189, SAPLING_FINAL_ROOT_TESTNET_299189_BYTES.as_ref().try_into().unwrap()),
            (299_201, SAPLING_FINAL_ROOT_TESTNET_299201_BYTES.as_ref().try_into().unwrap()),
            // Minimum-difficulty block
            (299_202, SAPLING_FINAL_ROOT_TESTNET_299202_BYTES.as_ref().try_into().unwrap()),
            (583_999, SAPLING_FINAL_ROOT_TESTNET_583999_BYTES.as_ref().try_into().unwrap()),
            // Blossom
            (584_000, SAPLING_FINAL_ROOT_TESTNET_584000_BYTES.as_ref().try_into().unwrap()),
            (584_001, SAPLING_FINAL_ROOT_TESTNET_584001_BYTES.as_ref().try_into().unwrap()),
            (903_799, SAPLING_FINAL_ROOT_TESTNET_903799_BYTES.as_ref().try_into().unwrap()),
            // Heartwood
            (903_800, SAPLING_FINAL_ROOT_TESTNET_903800_BYTES.as_ref().try_into().unwrap()),
            (903_801, SAPLING_FINAL_ROOT_TESTNET_903801_BYTES.as_ref().try_into().unwrap()),
            // Shielded coinbase x2
            (914_678, SAPLING_FINAL_ROOT_TESTNET_914678_BYTES.as_ref().try_into().unwrap()),
            (925_483, SAPLING_FINAL_ROOT_TESTNET_925483_BYTES.as_ref().try_into().unwrap()),
            (1_028_499, SAPLING_FINAL_ROOT_TESTNET_1028499_BYTES.as_ref().try_into().unwrap()),
            // Canopy
            (1_028_500, SAPLING_FINAL_ROOT_TESTNET_1028500_BYTES.as_ref().try_into().unwrap()),
            (1_028_501, SAPLING_FINAL_ROOT_TESTNET_1028501_BYTES.as_ref().try_into().unwrap()),
            (1_095_000, SAPLING_FINAL_ROOT_TESTNET_1095000_BYTES.as_ref().try_into().unwrap()),
            // Shielded coinbase
            (1_101_629, SAPLING_FINAL_ROOT_TESTNET_1101629_BYTES.as_ref().try_into().unwrap()),
            // Last Pre-Halving
            (1_115_999, SAPLING_FINAL_ROOT_TESTNET_1115999_BYTES.as_ref().try_into().unwrap()),
            // First Coinbase Halving
            (1_116_000, SAPLING_FINAL_ROOT_TESTNET_1116000_BYTES.as_ref().try_into().unwrap()),
            (1_116_001, SAPLING_FINAL_ROOT_TESTNET_1116001_BYTES.as_ref().try_into().unwrap()),
            (1_326_100, SAPLING_FINAL_ROOT_TESTNET_1326100_BYTES.as_ref().try_into().unwrap()),
            (1_599_199, SAPLING_FINAL_ROOT_TESTNET_1599199_BYTES.as_ref().try_into().unwrap()),
        ].iter().cloned().collect();

    // Mainnet

    // Genesis/BeforeOverwinter
    // for i in `seq 0 9`; do
    //     zcash-cli getblock $i 0 > block-main--000-00$i.txt
    // done
    pub static ref BLOCK_MAINNET_GENESIS_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-000.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_1_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-001.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_2_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-002.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_3_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-003.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_4_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-004.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_5_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-005.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_6_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-006.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_7_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-007.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_8_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-008.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_9_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-009.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // zcash-cli getblock 10 0 > block-main-0-000-010.txt
    pub static ref BLOCK_MAINNET_10_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-010.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_202_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-202.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // zcash-cli getblock 395 0 > block-main-0-000-395.txt
    pub static ref BLOCK_MAINNET_395_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-395.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // zcash-cli getblock 396 0 > block-main-0-000-396.txt
    pub static ref BLOCK_MAINNET_396_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-396.txt").trim())
        .expect("Block bytes are in valid hex representation");

    /// This contains an encoding of block 202 but with an improperly encoded
    /// coinbase height.
    pub static ref BAD_BLOCK_MAINNET_202_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-202-bad.txt").trim())
        .expect("Block bytes are in valid hex representation");


    // # Anchors for Sprout, starting at Genesis.
    //
    // for i in 0 396; do
    //     zcash-cli z_gettreestate "$i" | \
    //     jq --arg i "$i" \
    //     --raw-output \
    //     '"pub static ref SPROUT_FINAL_ROOT_MAINNET_\($i)_BYTES: [u8; 32] = <[u8; 32]>::from_hex(\"\(.sprout.commitments.finalRoot)\").expect(\"final root bytes are in valid hex representation\").rev();"'
    //     done
    pub static ref SPROUT_FINAL_ROOT_MAINNET_0_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("59d2cde5e65c1414c32ba54f0fe4bdb3d67618125286e6a191317917c812c6d7")
        .expect("final root bytes are in valid hex representation").rev();
    // The first block that contains a tx with a JoinSplit.
    pub static ref SPROUT_FINAL_ROOT_MAINNET_396_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("6a5710d1ca7d079baf1ce6ed1ea1b0756e219e9f3ebb9c0ec5b8ca1ff81c8f06")
        .expect("final root bytes are in valid hex representation").rev();

    // # Anchors for the Overwinter transition, which is still in the Sprout pool.
    //
    // for i in 347499 347500 347501; do
    //     zcash-cli z_gettreestate "$i" | \
    //     jq --arg i "$i" \
    //     --raw-output \
    //     '"pub static ref SPROUT_FINAL_ROOT_MAINNET_\($i)_BYTES: [u8; 32] = <[u8; 32]>::from_hex(\"\(.sprout.commitments.finalRoot)\").expect(\"final root bytes are in valid hex representation\").rev();"'
    //     done
    pub static ref SPROUT_FINAL_ROOT_MAINNET_347499_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("ce01f64025aba7c0e30a29f239f0eecd3cc18e5b1e575ca018c789a99482724f")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SPROUT_FINAL_ROOT_MAINNET_347500_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("ce01f64025aba7c0e30a29f239f0eecd3cc18e5b1e575ca018c789a99482724f")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SPROUT_FINAL_ROOT_MAINNET_347501_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("db036e080299a7401fd816789b5ea1b092ba3dab21e0f1d44161fffa149c65c1")
        .expect("final root bytes are in valid hex representation").rev();

    // Overwinter transition
    // for i in 347499 347500 347501; do
    //     zcash-cli getblock $i 0 > block-main-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // done
    pub static ref BLOCK_MAINNET_347499_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-347-499.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_347500_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-347-500.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_347501_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-347-501.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // zcash-cli getblock 415000 0 > block-main-0-415-000.txt
    pub static ref BLOCK_MAINNET_415000_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-415-000.txt").trim())
        .expect("Block bytes are in valid hex representation");

    // Sapling transition
    // for i in 419199 419200 419201 419202; do
    //     zcash-cli getblock $i 0 > block-main-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // done
    //
    // zcashd provides final sapling roots in big-endian order, but Zebra stores
    // that field in block order internally.
    //
    // for i in `cat post_sapling_mainnet_heights`; do
    //     zcash-cli z_gettreestate "$i" | \
    //         jq --arg i "$i" \
    //            --raw-output \
    //            '"pub static ref SAPLING_FINAL_ROOT_MAINNET_\($i)_BYTES: [u8; 32] = <[u8; 32]>::from_hex(\"\(.sapling.commitments.finalRoot)\").expect(\"final root bytes are in valid hex representation\").rev();"'
    // done
    pub static ref BLOCK_MAINNET_419199_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-419-199.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_419200_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-419-200.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_419201_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-419-201.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_419202_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-419-202.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_MAINNET_419200_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("3e49b5f954aa9d3545bc6c37744661eea48d7c34e3000d82b7f0010c30f4c2fb")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_MAINNET_419201_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("638d7e5ba37ab7921c51a4f3ae1b32d71c605a0ed9be7477928111a637f7421b")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_MAINNET_419202_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("54393f89293c8af01eb985398f5a984c446dd2974bf6ab63fdacbaf32d27a107")
        .expect("final root bytes are in valid hex representation").rev();

    // this one has a bad version field
    // zcash-cli getblock 434873 0 > block-main-0-434-873.txt
    pub static ref BLOCK_MAINNET_434873_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-434-873.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_MAINNET_434873_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("56e33199bc41d146cb24d24a65db35101248a1d12fff33affef56f90081a9517")
        .expect("final root bytes are in valid hex representation").rev();

    // Blossom transition
    // for i in 653599 653600 653601; do
    //     zcash-cli getblock $i 0 > block-main-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // done
    pub static ref BLOCK_MAINNET_653599_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-653-599.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_653600_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-653-600.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_653601_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-653-601.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_MAINNET_653599_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("3d532d101b9171769423a9f45a65b6312e28e7aa92b627cb81810f7a6fe21c6a")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_MAINNET_653600_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("3d532d101b9171769423a9f45a65b6312e28e7aa92b627cb81810f7a6fe21c6a")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_MAINNET_653601_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("612c62e54ef55f7bf8a60281debf1df904bf1fa6d1fa65d9656302b44ea98427")
        .expect("final root bytes are in valid hex representation").rev();

    // Heartwood transition
    // i=902999
    // zcash-cli getblock $i 0 > block-main-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    pub static ref BLOCK_MAINNET_902999_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-902-999.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // for i in 903000 903001; do
    //     zcash-cli getblock $i 0 > block-main-$[i/1000000]-$[i/1000%1000]-00$[i%1000].txt
    // done
    pub static ref BLOCK_MAINNET_903000_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-903-000.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_903001_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-903-001.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_MAINNET_902999_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("49df1a6e62458b0226b9d6c0c28fb7e94d9ca840582878b10d0117fd028b4e91")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_MAINNET_903000_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("11e48300f0e2296d5c413340b26426eddada1155155f4e959ebe307396976c79")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_MAINNET_903001_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("14e3c2b8af239bc4e17486573c20824292d5e1a6670dedf58bf865159e389cce")
        .expect("final root bytes are in valid hex representation").rev();

    // Shielded coinbase
    // for i in 949496 982681; do
    //     zcash-cli getblock $i 0 > block-main-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // done
    // i=975066
    // zcash-cli getblock $i 0 > block-main-$[i/1000000]-$[i/1000%1000]-0$[i%1000].txt
    // First shielded coinbase block
    pub static ref BLOCK_MAINNET_949496_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-949-496.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // Largest shielded coinbase block so far (in bytes)
    pub static ref BLOCK_MAINNET_975066_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-975-066.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // Last shielded coinbase block so far
    pub static ref BLOCK_MAINNET_982681_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-982-681.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_MAINNET_949496_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("4db238913a86284f5bddb9bcfe76f96a46d097ec681aad1da846cc276bfa7263")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_MAINNET_975066_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("2f62381b6decd0e0f937f6aa23faa7d19444b784701be93ad7e4df31bd4da1f9")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_MAINNET_982681_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("4fd1cb6d1e5c479baa44fcb7c3a1c6fafdaa54c0456d254918cd63839805848d")
        .expect("final root bytes are in valid hex representation").rev();

    // Canopy transition and Coinbase Halving
    // (On mainnet, Canopy happens at the same block as the first coinbase halving)
    // for i in 1046399 1046400; do
    //     zcash-cli getblock $i 0 > block-main-$[i/1000000]-0$[i/1000%1000]-$[i%1000].txt
    // done
    pub static ref BLOCK_MAINNET_1046399_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-1-046-399.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_1046400_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-1-046-400.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // Block 1046401 is 72 kB in size (currently the second-largest test vector), so we store it in binary.
    // i=1046401
    // zcash-cli getblock $i 0 | xxd -revert -plain > block-main-$[i/1000000]-0$[i/1000%1000]-$[i%1000].bin
    pub static ref BLOCK_MAINNET_1046401_BYTES: Vec<u8> =
        include_bytes!("block-main-1-046-401.bin")
        .to_vec();
    pub static ref SAPLING_FINAL_ROOT_MAINNET_1046399_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("0f12c92f737e84142792bddc82e36481de4a7679d5778a27389793933c8742e1")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_MAINNET_1046400_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("0f12c92f737e84142792bddc82e36481de4a7679d5778a27389793933c8742e1")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_MAINNET_1046401_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("1cb7a61a31354384957eea0b98661e1cf85a5de8e43f0e3bef522c8b375b26cb")
        .expect("final root bytes are in valid hex representation").rev();

    // One more Canopy/Post-Halving block
    // (so that we have at least 3 blocks after canopy/halving)
    // i=1180900
    // zcash-cli getblock $i 0 > block-main-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    pub static ref BLOCK_MAINNET_1180900_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-1-180-900.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_MAINNET_1180900_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("4a51c1b879f49637873ac4b261e9c625e16d9400b22d8aa4f27cd6fd1138ddda")
        .expect("final root bytes are in valid hex representation").rev();

    // Testnet

    // Genesis/BeforeOverwinter
    // for i in `seq 0 9`; do
    //     zcash-cli -testnet getblock $i 0 > block-test-0-000-00$i.txt
    // done
    pub static ref BLOCK_TESTNET_GENESIS_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-000.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_1_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-001.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_2_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-002.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_3_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-003.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_4_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-004.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_5_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-005.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_6_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-006.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_7_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-007.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_8_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-008.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_9_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-009.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // zcash-cli -testnet getblock 10 0 > block-test-0-000-010.txt
    pub static ref BLOCK_TESTNET_10_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-010.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // zcash-cli -testnet getblock 2259 0 > block-test-0-002-259.txt
    pub static ref BLOCK_TESTNET_2259_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-002-259.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // A large block
    // i=141042
    // zcash-cli -testnet getblock $i 0 | xxd -revert -plain > block-test-$[i/1000000]-$[i/1000%1000]-0$[i%1000].bin
    //
    // Block 141042 is almost ~2 MB in size (the maximum size for a block),
    // and contains 1 coinbase reward transaction and 20 transactions.
    // In each transaction, there is one joinsplit, with 244 inputs and 0 outputs.
    // https://zcash.readthedocs.io/en/latest/rtd_pages/shield_coinbase.html
    //
    // We store large blocks as binary, to reduce disk and network usage.
    // (git compresses blocks in transit and in its index, so there is not much need for extra compression.)
    pub static ref BLOCK_TESTNET_141042_BYTES: Vec<u8> = include_bytes!("block-test-0-141-042.bin").to_vec();

    // # Anchors for Sprout, starting at Genesis.
    //
    // for i in 0 396; do
    //     zcash-cli z_gettreestate "$i" | \
    //     jq --arg i "$i" \
    //     --raw-output \
    //     '"pub static ref SPROUT_FINAL_ROOT_TESTNET_\($i)_BYTES: [u8; 32] = <[u8; 32]>::from_hex(\"\(.sprout.commitments.finalRoot)\").expect(\"final root bytes are in valid hex representation\").rev();"'
    //     done
    pub static ref SPROUT_FINAL_ROOT_TESTNET_0_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("59d2cde5e65c1414c32ba54f0fe4bdb3d67618125286e6a191317917c812c6d7")
        .expect("final root bytes are in valid hex representation").rev();
    // The first block that contains a tx with a JoinSplit.
    pub static ref SPROUT_FINAL_ROOT_TESTNET_2259_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("2985231c8b3fb5624299fd7289c33667b0270a3fcde420c9047a6bad41f07733")
        .expect("final root bytes are in valid hex representation").rev();

    // Overwinter transition
    // for i in 207499 207500 207501; do
    //     zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // done
    pub static ref BLOCK_TESTNET_207499_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-207-499.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_207500_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-207-500.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_207501_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-207-501.txt").trim())
        .expect("Block bytes are in valid hex representation");

    // Sapling transition
    // i=279999
    // zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // for i in 280000 280001; do
    //     zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-00$[i%1000].txt
    // done
    //
    // for i in `cat post_sapling_testnet_heights`; do
    //     zcash-cli -testnet z_gettreestate "$i" | \
    //         jq --arg i "$i" \
    //            --raw-output \
    //            '"pub static ref SAPLING_FINAL_ROOT_TESTNET_\($i)_BYTES: [u8; 32] = <[u8; 32]>::from_hex(\"\(.sapling.commitments.finalRoot)\").expect(\"final root bytes are in valid hex representation\").rev();"'
    // done
    pub static ref BLOCK_TESTNET_279999_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-279-999.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_280000_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-280-000.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_280001_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-280-001.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_TESTNET_280000_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("3e49b5f954aa9d3545bc6c37744661eea48d7c34e3000d82b7f0010c30f4c2fb")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_280001_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("3e49b5f954aa9d3545bc6c37744661eea48d7c34e3000d82b7f0010c30f4c2fb")
        .expect("final root bytes are in valid hex representation").rev();

    // The first minimum difficulty blocks 299188, 299189, 299202 and their previous blocks for context
    // (pre-Blossom minimum difficulty)
    //
    // for i in 299187 299188 299189 299201 299202; do
    //     zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // done
    pub static ref BLOCK_TESTNET_299187_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-299-187.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_299188_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-299-188.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_299189_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-299-189.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_299201_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-299-201.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_299202_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-299-202.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_TESTNET_299187_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("6815df99f9f7ec9486a0b3a4e992ff9348dba7101c2ac91be41ceab392aa5521")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_299188_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("6815df99f9f7ec9486a0b3a4e992ff9348dba7101c2ac91be41ceab392aa5521")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_299189_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("6815df99f9f7ec9486a0b3a4e992ff9348dba7101c2ac91be41ceab392aa5521")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_299201_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("6815df99f9f7ec9486a0b3a4e992ff9348dba7101c2ac91be41ceab392aa5521")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_299202_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("6815df99f9f7ec9486a0b3a4e992ff9348dba7101c2ac91be41ceab392aa5521")
        .expect("final root bytes are in valid hex representation").rev();

    // Blossom transition
    // i=583999
    // zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // for i in 584000 584001; do
    //     zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-00$[i%1000].txt
    // done
    pub static ref BLOCK_TESTNET_583999_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-583-999.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // The first post-Blossom minimum difficulty block is the activation block
    pub static ref BLOCK_TESTNET_584000_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-584-000.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_584001_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-584-001.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_TESTNET_583999_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("13746c0c426cdddd05f85d86231f8bc647f5b024277c606c309ef707d85fd652")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_584000_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("13746c0c426cdddd05f85d86231f8bc647f5b024277c606c309ef707d85fd652")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_584001_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("13746c0c426cdddd05f85d86231f8bc647f5b024277c606c309ef707d85fd652")
        .expect("final root bytes are in valid hex representation").rev();

    // Heartwood transition
    // for i in 903799 903800 903801; do
    //     zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // done
    pub static ref BLOCK_TESTNET_903799_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-903-799.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_903800_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-903-800.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_903801_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-903-801.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_TESTNET_903799_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("626444395cd5963d3dba2652ee5bd73ef57555cca4d9f0d52e887a3bf44488e2")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_903800_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("626444395cd5963d3dba2652ee5bd73ef57555cca4d9f0d52e887a3bf44488e2")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_903801_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("626444395cd5963d3dba2652ee5bd73ef57555cca4d9f0d52e887a3bf44488e2")
        .expect("final root bytes are in valid hex representation").rev();

    // Shielded coinbase
    // for i in 914678 925483; do
    //     zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // done
    // First shielded coinbase block
    pub static ref BLOCK_TESTNET_914678_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-914-678.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // Largest shielded coinbase block so far (in bytes)
    pub static ref BLOCK_TESTNET_925483_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-925-483.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_TESTNET_914678_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("0ba286a3fb00d8a63b6ea52064bcc58ffb859aaa881745157d9a67d20afd7a8d")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_925483_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("271370ff86c2a9cc452334098d3337cddffc478e66a356dc0c00aebb58a4b6ac")
        .expect("final root bytes are in valid hex representation").rev();

    // Canopy transition
    // for i in 1028499 1028500 1028501; do
    //     zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-0$[i/1000%1000]-$[i%1000].txt
    // done
    pub static ref BLOCK_TESTNET_1028499_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-1-028-499.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_1028500_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-1-028-500.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_1028501_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-1-028-501.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_TESTNET_1028499_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("580adc0253cd0545250039267b7b49445ca0550df735920b7466bba1a64f7cf7")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_1028500_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("580adc0253cd0545250039267b7b49445ca0550df735920b7466bba1a64f7cf7")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_1028501_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("580adc0253cd0545250039267b7b49445ca0550df735920b7466bba1a64f7cf7")
        .expect("final root bytes are in valid hex representation").rev();

    // One more Canopy block
    // (so that we have at least 3 blocks from Canopy)
    // i=1095000
    // zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-0$[i/1000%1000]-00$[i%1000].txt
    pub static ref BLOCK_TESTNET_1095000_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-1-095-000.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_TESTNET_1095000_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("1781d73666ceb7675323130defd5fae426f1ee7d5fbb83adc9393aa8ff7e8a8d")
        .expect("final root bytes are in valid hex representation").rev();

    // Shielded coinbase + Canopy
    // i=1101629
    // zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-0$[i/1000%1000]-$[i%1000].txt
    // Last shielded coinbase block so far
    pub static ref BLOCK_TESTNET_1101629_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-1-101-629.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_TESTNET_1101629_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("14c7c827cd05c73c052c2a9faa6d7fc7b45ec2e386d8894eb65c420175c43745")
        .expect("final root bytes are in valid hex representation").rev();

    // Testnet Coinbase Halving
    // i=1115999
    // zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // for i in 1116000 1116001; do
    //     zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-00$[i%1000].txt
    // done
    pub static ref BLOCK_TESTNET_1115999_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-1-115-999.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_1116000_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-1-116-000.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_1116001_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-1-116-001.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_TESTNET_1115999_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("14b41a6dd7cd3c113d98f72543c4f57ff4e444bd5995366e0da420169c861f37")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_1116000_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("14b41a6dd7cd3c113d98f72543c4f57ff4e444bd5995366e0da420169c861f37")
        .expect("final root bytes are in valid hex representation").rev();
    pub static ref SAPLING_FINAL_ROOT_TESTNET_1116001_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("14b41a6dd7cd3c113d98f72543c4f57ff4e444bd5995366e0da420169c861f37")
        .expect("final root bytes are in valid hex representation").rev();

    // One more Post-Halving block
    // (so that we have at least 3 blocks after the halving)
    // i=1326100
    // zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    pub static ref BLOCK_TESTNET_1326100_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-1-326-100.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_TESTNET_1326100_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("2b30b19f4254709fe365bd0b381b2e3d9d0c933eb4dba4dd1d07f0f6e196a183")
        .expect("final root bytes are in valid hex representation").rev();

    // Nu5 transition
    // for i in 1599199 1599200 1599201; do
    //     zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // done
    pub static ref BLOCK_TESTNET_1599199_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-1-599-199.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref SAPLING_FINAL_ROOT_TESTNET_1599199_BYTES: [u8; 32] =
        <[u8; 32]>::from_hex("4de75d10def701ad22ecc17517a3adc8789ea8c214ac5bfc917b8924377e6c89")
        .expect("final root bytes are in valid hex representation").rev();

    // Sapling note commitment tree.
    pub static ref SAPLING_TREESTATE_MAINNET_419201_STRING: String =
        String::from(include_str!("sapling-treestate-main-0-419-201.txt"));
}

#[cfg(test)]
mod test {
    use super::*;

    use std::collections::HashSet;

    use crate::init;

    #[test]
    fn block_test_vectors_unique() {
        let _init_guard = init();

        let block_count = BLOCKS.len();
        let block_set: HashSet<_> = BLOCKS.iter().collect();

        // putting the same block in two files is an easy mistake to make
        assert_eq!(
            block_count,
            block_set.len(),
            "block test vectors must be unique"
        );

        // final sapling roots can be duplicated if a block has no sapling spends or outputs
    }

    /// Make sure we use all the test vectors in the lists.
    ///
    /// We're using lazy_static! and combinators, so it would be easy to make this mistake.
    #[test]
    fn block_test_vectors_count() {
        let _init_guard = init();

        assert!(
            MAINNET_BLOCKS.len() > 30,
            "there should be a reasonable number of mainnet block test vectors"
        );

        assert!(
            TESTNET_BLOCKS.len() > 30,
            "there should be a reasonable number of testnet block test vectors"
        );

        assert!(
            MAINNET_FINAL_SAPLING_ROOTS.len() > 10,
            "there should be a reasonable number of mainnet final sapling root test vectors"
        );

        assert!(
            TESTNET_FINAL_SAPLING_ROOTS.len() > 10,
            "there should be a reasonable number of testnet final sapling root test vectors"
        );
    }
}
