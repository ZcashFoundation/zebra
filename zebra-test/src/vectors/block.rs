//! Block test vectors

use hex::FromHex;
use lazy_static::lazy_static;

use std::collections::BTreeMap;
use std::iter::FromIterator;

lazy_static! {

    /// All block test vectors
    pub static ref BLOCKS: Vec<&'static [u8]> = MAINNET_BLOCKS.iter().chain(TESTNET_BLOCKS.iter()).map(|(_height, block)| *block).collect();

    // Update these lists of blocks when you add new block test vectors to
    // this file

    /// Mainnet blocks, indexed by height
    pub static ref MAINNET_BLOCKS: BTreeMap<u32, &'static [u8]> = BTreeMap::from_iter(
        [
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
            (347_499, BLOCK_MAINNET_347499_BYTES.as_ref()),
            // Overwinter
            (347_500, BLOCK_MAINNET_347500_BYTES.as_ref()),
            (347_501, BLOCK_MAINNET_347501_BYTES.as_ref()),
            (415_000, BLOCK_MAINNET_415000_BYTES.as_ref()),
            (419_199, BLOCK_MAINNET_419199_BYTES.as_ref()),
            // Sapling
            (419_200, BLOCK_MAINNET_419200_BYTES.as_ref()),
            (419_201, BLOCK_MAINNET_419201_BYTES.as_ref()),
            (434_873, BLOCK_MAINNET_434873_BYTES.as_ref()),
            (653_599, BLOCK_MAINNET_653599_BYTES.as_ref()),
            // Blossom
            (653_600, BLOCK_MAINNET_653600_BYTES.as_ref()),
            (653_601, BLOCK_MAINNET_653601_BYTES.as_ref()),
            (902_999, BLOCK_MAINNET_902999_BYTES.as_ref()),
            // Heartwood
            (903_000, BLOCK_MAINNET_903000_BYTES.as_ref()),
            (903_001, BLOCK_MAINNET_903001_BYTES.as_ref()),
            // TODO: Canopy and First Halving, see #1099
        ].iter().cloned()
    );

    /// Testnet blocks, indexed by height
    pub static ref TESTNET_BLOCKS: BTreeMap<u32, &'static [u8]> = BTreeMap::from_iter(
        [
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
            (207_499, BLOCK_TESTNET_207499_BYTES.as_ref()),
            // Overwinter
            (207_500, BLOCK_TESTNET_207500_BYTES.as_ref()),
            (207_501, BLOCK_TESTNET_207501_BYTES.as_ref()),
            (279_999, BLOCK_TESTNET_279999_BYTES.as_ref()),
            // Sapling
            (280_000, BLOCK_TESTNET_280000_BYTES.as_ref()),
            (280_001, BLOCK_TESTNET_280001_BYTES.as_ref()),
            (583_999, BLOCK_TESTNET_583999_BYTES.as_ref()),
            // Blossom
            (584_000, BLOCK_TESTNET_584000_BYTES.as_ref()),
            (584_001, BLOCK_TESTNET_584001_BYTES.as_ref()),
            (903_799, BLOCK_TESTNET_903799_BYTES.as_ref()),
            // Heartwood
            (903_800, BLOCK_TESTNET_903800_BYTES.as_ref()),
            (903_801, BLOCK_TESTNET_903801_BYTES.as_ref()),
            (1_028_499, BLOCK_TESTNET_1028499_BYTES.as_ref()),
            // Canopy
            (1_028_500, BLOCK_TESTNET_1028500_BYTES.as_ref()),
            (1_028_501, BLOCK_TESTNET_1028501_BYTES.as_ref()),
            (1_095_000, BLOCK_TESTNET_1095000_BYTES.as_ref()),
            // TODO: First Halving, see #1104
        ].iter().cloned()
    );

    // Mainnet

    // Genesis/BeforeOverwinter
    // for i in `seq 0 9`; do
    //     zcash-cli getblock $i 0 > block-main--000-00$i.txt
    // done
    pub static ref BLOCK_MAINNET_GENESIS_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-000.txt").trim())
            .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_1_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-main-0-000-001.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_2_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-main-0-000-002.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_3_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-main-0-000-003.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_4_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-main-0-000-004.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_5_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-main-0-000-005.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_6_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-main-0-000-006.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_7_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-main-0-000-007.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_8_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-main-0-000-008.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_9_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-main-0-000-009.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // zcash-cli getblock 10 0 > block-main-0-000-010.txt
    pub static ref BLOCK_MAINNET_10_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-000-010.txt").trim())
            .expect("Block bytes are in valid hex representation");

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
    // for i in 419199 419200 419201; do
    //     zcash-cli getblock $i 0 > block-main-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
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
    // this one has a bad version field
    // zcash-cli getblock 434873 0 > block-main-0-434-873.txt
    pub static ref BLOCK_MAINNET_434873_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-434-873.txt").trim())
            .expect("Block bytes are in valid hex representation");

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

    // TODO: Canopy transition, after mainnet canopy activation
    // for i in 1046399 1046400 1046401; do
    //     zcash-cli getblock $i 0 > block-main-$[i/1000000]-0$[i/1000%1000]-$[i%1000].txt
    // done

    // TODO: one more Canopy Mainnet block
    //       (so that we have at least 3 blocks from Canopy)
    // Note: don't use the highest block, it must be below the reorg limit!

    // Testnet

    // Genesis/BeforeOverwinter
    // for i in `seq 0 9`; do
    //     zcash-cli -testnet getblock $i 0 > block-test-0-000-00$i.txt
    // done
    pub static ref BLOCK_TESTNET_GENESIS_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-000.txt").trim())
            .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_1_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-test-0-000-001.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_2_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-test-0-000-002.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_3_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-test-0-000-003.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_4_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-test-0-000-004.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_5_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-test-0-000-005.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_6_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-test-0-000-006.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_7_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-test-0-000-007.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_8_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-test-0-000-008.txt").trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_9_BYTES: Vec<u8> = <Vec<u8>>::from_hex(include_str!("block-test-0-000-009.txt").trim())
        .expect("Block bytes are in valid hex representation");
    // zcash-cli -testnet getblock 10 0 > block-test-0-000-010.txt
    pub static ref BLOCK_TESTNET_10_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-010.txt").trim())
            .expect("Block bytes are in valid hex representation");

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
    pub static ref BLOCK_TESTNET_279999_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-279-999.txt").trim())
            .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_280000_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-280-000.txt").trim())
            .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_280001_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-280-001.txt").trim())
            .expect("Block bytes are in valid hex representation");

    // Blossom transition
    // i=583999
    // zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-$[i%1000].txt
    // for i in 584000 584001; do
    //     zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-$[i/1000%1000]-00$[i%1000].txt
    // done
    pub static ref BLOCK_TESTNET_583999_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-583-999.txt").trim())
            .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_584000_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-584-000.txt").trim())
            .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_584001_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-584-001.txt").trim())
            .expect("Block bytes are in valid hex representation");

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
    // One more Canopy block
    // (so that we have at least 3 blocks from Canopy)
    // i=1095000
    // zcash-cli -testnet getblock $i 0 > block-test-$[i/1000000]-0$[i/1000%1000]-00$[i%1000].txt
    pub static ref BLOCK_TESTNET_1095000_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-1-095-000.txt").trim())
            .expect("Block bytes are in valid hex representation");
}

#[cfg(test)]
mod test {
    use super::*;

    use std::collections::HashSet;

    #[test]
    fn block_test_vectors_unique() {
        let block_count = BLOCKS.len();
        let block_set: HashSet<_> = BLOCKS.iter().collect();

        // putting the same block in two files is an easy mistake to make
        assert_eq!(
            block_count,
            block_set.len(),
            "block test vectors must be unique"
        );
    }

    /// Make sure we use all the test vectors in the lists.
    ///
    /// We're using lazy_static! and combinators, so it would be easy to make this mistake.
    #[test]
    fn block_test_vectors_count() {
        assert!(
            BLOCKS.len() > 50,
            "there should be a reasonable number of block test vectors"
        );
    }
}
