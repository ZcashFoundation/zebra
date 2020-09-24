//! Block test vectors

use hex::FromHex;
use lazy_static::lazy_static;

lazy_static! {
    // Update this list of test blocks when you add a new block test vector to
    // this file
    pub static ref TEST_BLOCKS: Vec<&'static [u8]> = vec![
        // Mainnet
        &BLOCK_MAINNET_GENESIS_BYTES,
        &BLOCK_MAINNET_1_BYTES,
        &BLOCK_MAINNET_2_BYTES,
        &BLOCK_MAINNET_3_BYTES,
        &BLOCK_MAINNET_4_BYTES,
        &BLOCK_MAINNET_5_BYTES,
        &BLOCK_MAINNET_6_BYTES,
        &BLOCK_MAINNET_7_BYTES,
        &BLOCK_MAINNET_8_BYTES,
        &BLOCK_MAINNET_9_BYTES,
        &BLOCK_MAINNET_10_BYTES,
        &BLOCK_MAINNET_347499_BYTES,
        &BLOCK_MAINNET_347500_BYTES,
        &BLOCK_MAINNET_347501_BYTES,
        &BLOCK_MAINNET_415000_BYTES,
        &BLOCK_MAINNET_419199_BYTES,
        &BLOCK_MAINNET_419200_BYTES,
        &BLOCK_MAINNET_419201_BYTES,
        &BLOCK_MAINNET_434873_BYTES,
        &BLOCK_MAINNET_653599_BYTES,
        &BLOCK_MAINNET_653600_BYTES,
        &BLOCK_MAINNET_653601_BYTES,
        &BLOCK_MAINNET_902999_BYTES,
        &BLOCK_MAINNET_903000_BYTES,
        &BLOCK_MAINNET_903001_BYTES,
        // Testnet
        &BLOCK_TESTNET_GENESIS_BYTES,
        &BLOCK_TESTNET_1_BYTES,
        &BLOCK_TESTNET_2_BYTES,
        &BLOCK_TESTNET_3_BYTES,
        &BLOCK_TESTNET_4_BYTES,
        &BLOCK_TESTNET_5_BYTES,
        &BLOCK_TESTNET_6_BYTES,
        &BLOCK_TESTNET_7_BYTES,
        &BLOCK_TESTNET_8_BYTES,
        &BLOCK_TESTNET_9_BYTES,
        &BLOCK_TESTNET_10_BYTES,
    ];

    // Mainnet

    // Genesis/BeforeOverwinter
    // for i in `seq 0 9`; do
    //     zcash-cli getblock $i 0 > block-main-0-000-00$i.txt
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

    // Overwinter
    // for i in 347499 347500 347501; do
    //     zcash-cli getblock $i 0 > block-main-0-347-$[i%1000].txt
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

    // Sapling
    // for i in 419199 419200 419201; do
    //     zcash-cli getblock $i 0 > block-main-0-419-$[i%1000].txt
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
    // zcash-cli getblock 434873 0 > block-main-0-434-873.txt
    pub static ref BLOCK_MAINNET_434873_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-434-873.txt").trim())
            .expect("Block bytes are in valid hex representation");

    // Blossom
    // for i in 653599 653600 653601; do
    //     zcash-cli getblock $i 0 > block-main-0-653-$[i%1000].txt
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

    // Heartwood
    // zcash-cli getblock 902999 0 > block-main-0-902-999.txt
    pub static ref BLOCK_MAINNET_902999_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-902-999.txt").trim())
            .expect("Block bytes are in valid hex representation");
    // for i in 903000 903001; do
    //     zcash-cli getblock $i 0 > block-main-0-$[i/1000]-00$[i%10].txt
    // done
    pub static ref BLOCK_MAINNET_903000_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-903-000.txt").trim())
            .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_903001_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-main-0-903-001.txt").trim())
            .expect("Block bytes are in valid hex representation");

    // TODO: Canopy, after mainnet canopy activation
    // for i in 1046399 1046400 1046401; do
    //     zcash-cli getblock $i 0 > block-main-$[i/1000000]-$[i/1000]-$[i%1000].txt
    // done

    // Testnet

    // Genesis/BeforeOverwinter
    // for i in `seq 0 9`; do
    //     zcash-cli getblock $i 0 > block-test-0-000-00$i.txt
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
    // zcash-cli getblock 10 0 > block-test-0-000-010.txt
    pub static ref BLOCK_TESTNET_10_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("block-test-0-000-010.txt").trim())
            .expect("Block bytes are in valid hex representation");
}
